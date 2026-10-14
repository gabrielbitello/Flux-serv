//! Pipeline de migrations (F5, spec 05): diff por hash contra
//! `migration_state` (laravel) ou `migrate_cmd` por execução (node/rust — a
//! idempotência é do comando, 06). PROD nunca reconstrói: migration mutada é
//! ERRO e bloqueia. Execução é sempre DENTRO do container, via `docker exec`.
//!
//! Seed (05): a seed só existe no REBUILD de dev (`migrate:fresh` +
//! `db:seed --force` como exec separado, para a saída do seeder aparecer no
//! evento). Precedência: CLI (`Some`) > toml `[db].seed_on_rebuild` > default
//! da spec (true). PROD NUNCA semeia — nem com `Some(true)`: o parâmetro não
//! alcança o caminho de prod (05: "nunca aplicar seed em prod").
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

/// Precedência da seed (05): CLI (`cli`) vence o toml (`toml`), que vence o
/// default da spec — `true` (05, linha 158: "seed_on_rebuild ... default
/// true"; o fresh de dev semeia por padrão). Pura, para teste sem engine.
pub fn resolver_seed(cli: Option<bool>, toml: Option<bool>) -> bool {
    cli.or(toml).unwrap_or(true)
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

/// Executa o ciclo completo de schema do projeto sem override de seed —
/// decide pelo flux.toml (05). Wrapper fino sobre `ensure_schema_com_seed`;
/// a assinatura antiga segue de pé para os chamadores existentes (api.rs).
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
    ensure_schema_com_seed(docker, st, name, env, toml_src, files, None).await
}

/// Como `ensure_schema`, com override de seed da CLI (05: `flux db fresh
/// --seed` / `--no-seed` força uma vez). `seed: Some(v)` vence o toml;
/// `None` = decidir por `[db].seed_on_rebuild`; ausente nos dois = default
/// da spec (true). Prod ignora o parâmetro por inteiro — nunca semeia.
/// # Erro
/// Prod com migration mutada; exec falho de migrate/seed no rebuild de dev.
pub async fn ensure_schema_com_seed(
    docker: &Docker,
    st: &State,
    name: &str,
    env: Env,
    toml_src: &str,
    files: &[(String, String)],
    seed: Option<bool>,
) -> Result<SchemaOutcome, String> {
    let toml = ProjectToml::parse_str(toml_src)
        .map_err(|e| format!("flux.toml de {name} inválido: {e}"))?;
    let framework = toml.framework();
    let seed_on_rebuild = resolver_seed(seed, toml.db.as_ref().and_then(|d| d.seed_on_rebuild));
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
                // Seed é exec SEPARADO (`db:seed --force`) e não a flag
                // `--seed` do fresh, para a saída do seeder aparecer no
                // evento — e para falha de seed não ficar engolida no exit
                // code do migrate:fresh (nada silencioso).
                let (_, code) =
                    exec_in_container(docker, name, env, "php artisan migrate:fresh --force")
                        .await?;
                if code != 0 {
                    return Err(format!("{name}/dev: migrate:fresh falhou (exit {code})"));
                }
                let mut saida_seed = String::new();
                if seed_on_rebuild {
                    let (out, code) =
                        exec_in_container(docker, name, env, "php artisan db:seed --force").await?;
                    if code != 0 {
                        return Err(format!(
                            "{name}/dev: db:seed falhou após o fresh (exit {code}): {out}"
                        ));
                    }
                    saida_seed = out;
                }
                st.clear_migrations(name, env).map_err(|e| e.to_string())?;
                for (f, sha) in files {
                    st.record_migration(name, env, f, sha)
                        .map_err(|e| e.to_string())?;
                }
                // Saída do seeder vai no evento (linha única — o painel é
                // log); sem seed não há o que anexar.
                let cauda_seed = saida_seed
                    .lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty())
                    .collect::<Vec<_>>()
                    .join(" | ");
                let sufixo_seed = if cauda_seed.is_empty() {
                    String::new()
                } else {
                    format!("; saída: {cauda_seed}")
                };
                events::event(
                    st,
                    "info",
                    "db",
                    &format!(
                        "{name}/dev: banco reconstruído (mutação em {:?}; seed={seed_on_rebuild}{sufixo_seed})",
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

    #[test]
    fn precedencia_da_seed_cli_vence_toml_vence_default_da_spec() {
        assert!(
            !resolver_seed(Some(false), Some(true)),
            "CLI --no-seed vence o toml true"
        );
        assert!(
            resolver_seed(Some(true), Some(false)),
            "CLI --seed vence o toml false"
        );
        assert!(resolver_seed(None, Some(true)), "toml true vale sem CLI");
        assert!(!resolver_seed(None, Some(false)), "toml false vale sem CLI");
        assert!(
            resolver_seed(None, None),
            "CLI e toml ausentes = default da spec 05 (true) — fresh de dev semeia por padrão"
        );
    }
}

/// Fresh FORÇADO de dev por comando (`flux db fresh`, pedido do Gabriel
/// 14/10): reconstrói o schema MESMO sem migration mutada, com seed
/// controlável (mesma precedência da pipeline). PROD NUNCA: bloqueio
/// incondicional antes de qualquer efeito (05). Framework não-laravel =
/// erro honesto (não há `fresh` genérico fora do artisan).
pub async fn fresh_dev_forcado(
    docker: &Docker,
    st: &State,
    name: &str,
    toml_src: &str,
    seed: Option<bool>,
) -> Result<String, String> {
    let toml = ProjectToml::parse_str(toml_src)
        .map_err(|e| format!("flux.toml de {name} inválido: {e}"))?;
    if toml.framework() != flux_common::framework::Framework::Laravel {
        return Err(format!(
            "{name}: fresh forçado só existe para laravel (framework atual: {}). \
             Para node/rust, use o migrate_cmd do projeto.",
            toml.framework()
        ));
    }
    let seed_ligado = resolver_seed(seed, toml.db.as_ref().and_then(|d| d.seed_on_rebuild));
    let cmd = if seed_ligado {
        "php artisan migrate:fresh --force --seed"
    } else {
        "php artisan migrate:fresh --force"
    };
    let (output, code) = exec_in_container(docker, name, Env::Dev, cmd).await?;
    if code != 0 {
        return Err(format!("{name}/dev: {cmd} falhou (exit {code}): {output}"));
    }
    // Fresh apagou TODAS as tabelas — a trilha de migrations aplicadas também.
    st.clear_migrations(name, Env::Dev)
        .map_err(|e| e.to_string())?;
    events::event(
        st,
        "warn",
        "db",
        &format!(
            "{name}/dev: RECONSTRUÍDO por comando ({cmd}) — dados de dev apagados, seed={seed_ligado}"
        ),
    );
    Ok(output)
}
