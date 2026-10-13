//! Pipeline de migrations (F5, spec 05): diff por hash contra
//! `migration_state` (laravel) ou `migrate_cmd` por execução (node/rust — a
//! idempotência é do comando, 06). PROD nunca reconstrói: migration mutada é
//! ERRO e bloqueia. Execução é sempre DENTRO do container, via `docker exec`.
//!
//! Janela de silêncio (5 s sem mutação, inotify) pertence ao fluxo de sync
//! (F2) — quem chama `ensure_schema` já passou pela janela.

use bollard::Docker;
use bollard::exec::CreateExecOptions;
use flux_common::config::ProjectToml;
use flux_common::protocol::Env;
use futures_util::StreamExt;

use crate::docker::{self, find_app_container};
use crate::events;
use crate::state::State;

/// Diff do estado aplicado vs. o que está no disco agora (laravel).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct MigrationPlan {
    pub to_apply: Vec<String>,
    pub mutated: Vec<String>,
    pub deleted: Vec<String>,
}

pub fn plan_migrations(
    applied: &[(String, String)],
    current: &[(String, String)],
) -> MigrationPlan {
    let mut plan = MigrationPlan::default();
    for (file, sha) in current {
        match applied.iter().find(|(f, _)| f == file) {
            None => plan.to_apply.push(file.clone()),
            Some((_, sha_aplicada)) => {
                if sha_aplicada != sha {
                    plan.mutated.push(file.clone());
                }
            }
        }
    }
    plan.deleted = applied
        .iter()
        .filter(|(f, _)| !current.iter().any(|(cf, _)| cf == f))
        .map(|(f, _)| f.clone())
        .collect();
    plan
}

/// Resultado do ciclo de schema — cada variante vira linha de log/painel.
#[derive(Debug, PartialEq, Eq)]
pub enum SchemaOutcome {
    /// laravel: migrations novas aplicadas; dev com mutação → banco
    /// reconstruído (`fresh`), sem distinguir aqui.
    LaravelRan { fresh: bool, aplicadas: usize },
    /// node/rust: `migrate_cmd` rodou (saída anexada).
    MigrateCmd { output: String },
    /// stack sem pipeline de schema — aviso explícito, nunca silêncio (06).
    NoPipeline,
}

/// Executa o ciclo completo de schema do projeto. `files` = (arquivo, sha256)
/// das migrations no disco agora — só laravel usa; vazio nas outras stacks.
/// # Erro
/// Prod com migration mutada — bloqueia SEMPRE (05: imutável, sem flag).
pub async fn ensure_schema(
    docker: &Docker,
    st: &State,
    name: &str,
    env: Env,
    toml_src: &str,
    files: &[(String, String)],
) -> Result<SchemaOutcome, String> {
    let toml = ProjectToml::parse_str(toml_src)
        .map_err(|e| format!("flux.toml de {name} inválido: {e}"))?;
    let framework = toml.framework();
    let seed_on_rebuild = toml
        .db
        .as_ref()
        .and_then(|d| d.seed_on_rebuild)
        .unwrap_or(true);
    let migrate_cmd = toml.db.as_ref().and_then(|d| d.migrate_cmd.clone());

    match framework {
        flux_common::framework::Framework::Laravel => {
            ensure_laravel(docker, st, name, env, files, seed_on_rebuild).await
        }
        _ => match migrate_cmd {
            Some(cmd) if !cmd.is_empty() => {
                let (output, code) = exec_in_container(docker, name, env, &cmd).await?;
                if code != 0 {
                    return Err(format!(
                        "{name}/{env}: migrate_cmd falhou (exit {code}): {output}"
                    ));
                }
                events::event(
                    st,
                    "info",
                    "db",
                    &format!("{name}/{env}: migrate_cmd rodou ({framework})"),
                );
                Ok(SchemaOutcome::MigrateCmd { output })
            }
            _ => {
                let msg = format!(
                    "{name}/{env}: framework {framework} sem [db] migrate_cmd — sem pipeline de schema (aviso, não erro)"
                );
                events::event(st, "warn", "db", &msg);
                Ok(SchemaOutcome::NoPipeline)
            }
        },
    }
}

async fn ensure_laravel(
    docker: &Docker,
    st: &State,
    name: &str,
    env: Env,
    files: &[(String, String)],
    seed_on_rebuild: bool,
) -> Result<SchemaOutcome, String> {
    let applied = st.migration_rows(name, env).map_err(|e| e.to_string())?;
    let plan = plan_migrations(&applied, files);

    if !plan.deleted.is_empty() {
        events::event(
            st,
            "warn",
            "db",
            &format!(
                "{name}/{env}: migrations apagadas do disco (estado mantém hash): {:?}",
                plan.deleted
            ),
        );
    }

    match env {
        Env::Prod => {
            if !plan.mutated.is_empty() {
                return Err(format!(
                    "{name}/prod: migration JÁ APLICADA foi mutada — deploy bloqueado (05): {:?}. Crie migration nova; desfazer a mutação no arquivo original ou `flux db prod-mutated --ack` (auditado)",
                    plan.mutated
                ));
            }
            if plan.to_apply.is_empty() {
                return Ok(SchemaOutcome::LaravelRan {
                    fresh: false,
                    aplicadas: 0,
                });
            }
            let (_, code) =
                exec_in_container(docker, name, env, "php artisan migrate --force").await?;
            if code != 0 {
                return Err(format!("{name}/prod: migrate falhou (exit {code})"));
            }
            for f in &plan.to_apply {
                if let Some((_, sha)) = files.iter().find(|(cf, _)| cf == f) {
                    st.record_migration(name, env, f, sha)
                        .map_err(|e| e.to_string())?;
                }
            }
            events::event(
                st,
                "info",
                "db",
                &format!(
                    "{name}/prod: {} migration(ns) aplicadas",
                    plan.to_apply.len()
                ),
            );
            Ok(SchemaOutcome::LaravelRan {
                fresh: false,
                aplicadas: plan.to_apply.len(),
            })
        }
        Env::Dev => {
            if !plan.mutated.is_empty() {
                // Banco dev é descartável por definição: reconstrói (05).
                let cmd = if seed_on_rebuild {
                    "php artisan migrate:fresh --force --seed"
                } else {
                    "php artisan migrate:fresh --force"
                };
                let (_, code) = exec_in_container(docker, name, env, cmd).await?;
                if code != 0 {
                    return Err(format!("{name}/dev: migrate:fresh falhou (exit {code})"));
                }
                st.clear_migrations(name, env).map_err(|e| e.to_string())?;
                for (f, sha) in files {
                    st.record_migration(name, env, f, sha)
                        .map_err(|e| e.to_string())?;
                }
                events::event(
                    st,
                    "info",
                    "db",
                    &format!(
                        "{name}/dev: banco reconstruído (mutação em {:?}; seed={seed_on_rebuild})",
                        plan.mutated
                    ),
                );
                return Ok(SchemaOutcome::LaravelRan {
                    fresh: true,
                    aplicadas: files.len(),
                });
            }
            if plan.to_apply.is_empty() {
                return Ok(SchemaOutcome::LaravelRan {
                    fresh: false,
                    aplicadas: 0,
                });
            }
            let (_, code) =
                exec_in_container(docker, name, env, "php artisan migrate --force").await?;
            if code != 0 {
                return Err(format!("{name}/dev: migrate falhou (exit {code})"));
            }
            for f in &plan.to_apply {
                if let Some((_, sha)) = files.iter().find(|(cf, _)| cf == f) {
                    st.record_migration(name, env, f, sha)
                        .map_err(|e| e.to_string())?;
                }
            }
            events::event(
                st,
                "info",
                "db",
                &format!(
                    "{name}/dev: {} migration(ns) aplicadas",
                    plan.to_apply.len()
                ),
            );
            Ok(SchemaOutcome::LaravelRan {
                fresh: false,
                aplicadas: plan.to_apply.len(),
            })
        }
    }
}

/// `docker exec` no container app do projeto — o ÚNICO caminho de execução
/// (07: nenhum shell fora daqui, compose via plugin, resto é API).
async fn exec_in_container(
    docker: &Docker,
    name: &str,
    env: Env,
    cmd: &str,
) -> Result<(String, i64), String> {
    let Some(c) = find_app_container(docker, &docker::compose_project(name, env)).await else {
        return Err(format!(
            "{name}/{env}: container app não encontrado para executar: {cmd}"
        ));
    };
    let id =
        c.id.clone()
            .ok_or_else(|| format!("{name}/{env}: container sem id"))?;

    let exec = docker
        .create_exec(
            &id,
            CreateExecOptions {
                cmd: Some(vec!["sh".to_owned(), "-c".to_owned(), cmd.to_owned()]),
                attach_stdout: Some(true),
                attach_stderr: Some(true),
                ..Default::default()
            },
        )
        .await
        .map_err(|e| format!("create_exec: {e}"))?;

    let mut output = String::new();
    match docker.start_exec(&exec.id, None).await {
        Ok(bollard::exec::StartExecResults::Attached {
            output: mut output_stream,
            ..
        }) => {
            while let Some(chunk) = output_stream.next().await {
                match chunk {
                    Ok(bollard::container::LogOutput::StdOut { message })
                    | Ok(bollard::container::LogOutput::StdErr { message }) => {
                        output.push_str(&String::from_utf8_lossy(&message));
                    }
                    Ok(_) => {}
                    Err(e) => return Err(format!("exec stream: {e}")),
                }
            }
        }
        Ok(bollard::exec::StartExecResults::Detached) => {}
        Err(e) => return Err(format!("start_exec: {e}")),
    }

    let inspected = docker
        .inspect_exec(&exec.id)
        .await
        .map_err(|e| format!("inspect_exec: {e}"))?;
    let code = inspected.exit_code.unwrap_or(-1);
    Ok((output.trim_end().to_owned(), code))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair(v: &[(&str, &str)]) -> Vec<(String, String)> {
        v.iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
    }

    #[test]
    fn diff_classifica_novas_mutadas_e_apagadas() {
        let applied = pair(&[
            ("2024_a.php", "hash_a"),
            ("2024_b.php", "hash_b"),
            ("2024_c.php", "hash_c"),
        ]);
        let current = pair(&[
            ("2024_a.php", "hash_a"),
            ("2024_b.php", "hash_MUTADA"),
            ("2024_d.php", "hash_d"),
        ]);
        let plan = plan_migrations(&applied, &current);
        assert_eq!(plan.to_apply, vec!["2024_d.php".to_owned()]);
        assert_eq!(plan.mutated, vec!["2024_b.php".to_owned()]);
        assert_eq!(plan.deleted, vec!["2024_c.php".to_owned()]);
    }

    #[test]
    fn diff_limpo_e_vazio() {
        let applied = pair(&[("a.php", "h1")]);
        let plan = plan_migrations(&applied, &applied);
        assert_eq!(plan, MigrationPlan::default());
        assert_eq!(plan_migrations(&[], &[]), MigrationPlan::default());
    }
}
