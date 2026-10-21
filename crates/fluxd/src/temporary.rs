//! Modo dev temporário (14): projeto com `[temporary] enabled = true` no
//! `flux.toml` se apaga SOZINHO da VM após TTL sem interação NENHUMA
//! (default 6 h). O relógio é o `last_interaction` de dev (mesmos sinais do
//! sono, 04) ou, para projeto nunca interagido, o `created_at` do registro —
//! ambos ausentes = NÃO expira (nunca chutar relógio).
//!
//! Teardown toca SÓ coisas da VM, e só do projeto: containers com label
//! compose `flux-{name}*`, diretórios `/srv/flux/projects/{name}*` e a
//! trilha no SQLite. O repo no WINDOWS fica intocado (sync é
//! one-way-replica VM←Windows, 02). Falha de qualquer passo é logada como
//! erro com o que foi feito — nunca silenciosa, e o purge do SQLite roda
//! POR ÚLTIMO (projeto meio-apagado continua visível no painel para
//! investigação).

use std::path::{Path, PathBuf};

use bollard::Docker;
use flux_common::config::ProjectToml;

use crate::events;
use crate::state::State;

/// Nome seguro para virar sufixo de glob de diretório: o CONTRATO de nome de
/// projeto (flux_common::nomes). Qualquer outra coisa (herança de pasta
/// ruim) NUNCA entra em padrão de remoção — o projeto é pulado com erro
/// visível.
fn nome_seguro(name: &str) -> bool {
    flux_common::nomes::projeto_valido(name)
}

/// Varredura da expiração: chamada pelo sweep (60 s) DEPOIS do ciclo de sono.
/// `agora` epoch em segundos (injetável para testes).
pub async fn expire_check(st: &State, docker: Option<Docker>, agora: i64) {
    expire_check_em(
        st,
        docker,
        agora,
        Path::new(crate::paths::PROJECTS_ROOT_PADRAO),
    )
    .await;
}

pub async fn expire_check_em(st: &State, docker: Option<Docker>, agora: i64, raiz: &Path) {
    let Ok(projetos) = st.list_projects() else {
        tracing::error!("temporary: falha listando projetos — sweep de expiração pulado");
        return;
    };
    for nome in projetos {
        let toml_src = match st.project_toml(&nome) {
            Ok(Some(t)) => t,
            Ok(None) => continue,
            Err(e) => {
                tracing::error!("temporary: falha lendo toml de {nome}: {e}");
                continue;
            }
        };
        let Ok(toml) = ProjectToml::parse_str(&toml_src) else {
            tracing::warn!("temporary: toml de {nome} não parseia — pulado");
            continue;
        };
        let Some(temp) = toml.temporary.as_ref().filter(|t| t.enabled()) else {
            continue;
        };
        let ttl = temp.ttl_secs();
        let clock = match st.temporary_clock(&nome) {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("temporary: falha lendo relógio de {nome}: {e}");
                continue;
            }
        };
        // Interação vence registro; ambos ausentes = sem relógio = nunca.
        let Some(ultimo) = clock.0.or(clock.1) else {
            tracing::warn!(
                "temporary: {nome} sem last_interaction e sem created_at — NÃO expira (14)"
            );
            continue;
        };
        let ocioso = agora - ultimo;
        if ocioso <= ttl {
            continue;
        }
        let horas = temp.ttl_hours();
        tracing::info!(
            "temporary: {nome} ocioso há {ocioso}s > TTL {ttl}s — teardown (repo Windows intocado)"
        );
        teardown(st, docker.as_ref(), &nome, raiz, horas, ocioso).await;
    }
}

/// Remove da VM tudo que o projeto criou: containers (compose projects com
/// prefixo `flux-{nome}`), diretórios `{raiz}/{nome}*` e a trilha no SQLite.
async fn teardown(
    st: &State,
    docker: Option<&Docker>,
    nome: &str,
    raiz: &Path,
    ttl_horas: u64,
    ocioso: i64,
) {
    if !nome_seguro(nome) {
        events::event(
            st,
            "error",
            "temporary",
            &format!("teardown RECUSADO: nome '{nome}' tem caracteres fora de [a-z0-9-]"),
        );
        return;
    }
    let mut feito: Vec<String> = Vec::new();
    let mut falhas: Vec<String> = Vec::new();

    // 1. Containers: derruba cada compose project `flux-{nome}…` (dev e
    //    worktrees). Via docker CLI (compose down -v cuida de rede/volume);
    //    a lista vem do engine quando existe, senão nada a derrubar aqui.
    if let Some(d) = docker {
        match compose_projects_do(d, nome).await {
            Ok(projetos) => {
                for projeto in projetos {
                    match compose_down(&projeto).await {
                        Ok(()) => feito.push(format!("compose {projeto} down -v")),
                        Err(e) => falhas.push(format!("compose {projeto}: {e}")),
                    }
                }
            }
            Err(e) => falhas.push(format!("listando compose projects: {e}")),
        }
    }

    // 2. Banco próprio ([db] own, 21/10): o database de DEV do projeto
    //    temporário expira JUNTO — sai com dump de backup (mesma regra do
    //    TTL de 1 semana, só que na hora do teardown, porque a instância
    //    deixou de existir). O CONTAINER do grupo NÃO é do compose do
    //    projeto (`flux-db-{grupo}` ≠ `flux-{nome}-*`) — sobrevive ao
    //    `down -v` e para via dormir_se_ninguem se ficou órfão.
    if let Ok(Some(src)) = st.project_toml(nome)
        && let Some(Ok(cfg)) = crate::banco::config_de(&src, nome)
        && let (Some(d), Some(backup_dir)) =
            (docker, raiz.parent().map(|p| p.join("state/db-backups")))
        && let Ok(db) = crate::banco::database_name(nome, flux_common::protocol::Env::Dev)
    {
        match crate::banco::apagar_database_com_backup(st, &backup_dir, &cfg.grupo, &db).await {
            Ok(caminho) => feito.push(format!(
                "banco próprio: {db} apagado (dump em {})",
                caminho.display()
            )),
            Err(e) => falhas.push(format!("banco próprio {db}: {e}")),
        }
        crate::banco::dormir_se_ninguem(st, Some(d), &cfg.grupo).await;
    }

    // 3. Diretórios de sync na VM (fonte da verdade é o repo Windows).
    for dir in dirs_do_projeto(raiz, nome) {
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => feito.push(format!("rm {}", dir.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => falhas.push(format!("rm {}: {e}", dir.display())),
        }
    }

    // 4. SQLite POR ÚLTIMO — enquanto não some, o painel continua mostrando
    //    o projeto (meio-apagado = visível, nunca fantasma).
    match st.purge_project(nome) {
        Ok(()) => feito.push("estado SQLite".to_owned()),
        Err(e) => falhas.push(format!("purge SQLite: {e}")),
    }

    let msg = format!(
        "dev temporário '{nome}' expirou (ttl {ttl_horas} h, ocioso {ocioso} s sem interação) — \
         apagado da VM: {}; repo Windows intocado; sessão mutagen do projeto fica halted até o \
         próximo `flux up`",
        if feito.is_empty() {
            "nada".to_owned()
        } else {
            feito.join(", ")
        }
    );
    if falhas.is_empty() {
        events::event(st, "warn", "temporary", &msg);
    } else {
        events::event(
            st,
            "error",
            "temporary",
            &format!(
                "{msg}; FALHAS (investigar no painel): {}",
                falhas.join("; ")
            ),
        );
    }
}

/// Compose projects do projeto: containers com label
/// `com.docker.compose.project` começando em `flux-{nome}-` (dev e
/// worktrees; NUNCA casa `flux-{outro}` — o traço final é parte do match).
async fn compose_projects_do(d: &Docker, nome: &str) -> Result<Vec<String>, String> {
    // L3.1: listagem vem da FONTE ÚNICA (docker::listar_label, forma
    // "todos com label") — o filtro de prefixo fica aqui, é o contrato
    // deste teardown (dev + worktrees; NUNCA casa flux-{outro}).
    let containers = crate::docker::listar_label(d, None).await?;
    let prefixo = format!("flux-{nome}-");
    let mut projetos: Vec<String> = containers
        .iter()
        .filter_map(|c| c.labels.as_ref())
        .filter_map(|l| l.get("com.docker.compose.project"))
        .filter(|p| p.starts_with(&prefixo))
        .cloned()
        .collect();
    projetos.sort();
    projetos.dedup();
    Ok(projetos)
}

async fn compose_down(projeto: &str) -> Result<(), String> {
    let saida = tokio::process::Command::new("docker")
        .args(["compose", "-p", projeto, "down", "-v", "--timeout", "10"])
        .output()
        .await
        .map_err(|e| e.to_string())?;
    if saida.status.success() {
        Ok(())
    } else {
        Err(format!(
            "exit {:?}: {}",
            saida.status.code(),
            String::from_utf8_lossy(&saida.stderr).trim()
        ))
    }
}

/// Diretórios candidatos do projeto sob a raiz: `{nome}` e `{nome}@{qualquer
/// coisa}` (worktrees, 02). Glob manual — sem dep nova.
fn dirs_do_projeto(raiz: &Path, nome: &str) -> Vec<PathBuf> {
    let prefixo = nome.to_owned();
    let prefixo_wt = format!("{nome}@");
    let mut achados = Vec::new();
    let Ok(entradas) = std::fs::read_dir(raiz) else {
        return achados;
    };
    for entrada in entradas.flatten() {
        let nome_dir = entrada.file_name();
        let Some(nome_dir) = nome_dir.to_str() else {
            continue;
        };
        if nome_dir == prefixo || nome_dir.starts_with(&prefixo_wt) {
            achados.push(entrada.path());
        }
    }
    achados
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::State;

    fn state_com(nome: &str, toml: &str, criado_ha_secs: i64) -> State {
        let st = State::in_memory().unwrap();
        st.register_project(nome, toml, "h").unwrap();
        // created_at no passado: relógio honesto de registro.
        st.with(|c| {
            c.execute(
                "UPDATE projects SET created_at = datetime('now', ?1) WHERE name = ?2",
                rusqlite::params![format!("-{criado_ha_secs} seconds"), nome],
            )
        })
        .unwrap();
        st
    }

    const TEMPORARIO: &str = "name = \"demo\"\n[temporary]\nenabled = true\nttl_hours = 6\n";

    #[tokio::test]
    async fn temporario_expira_e_e_purgado() {
        let tmp = tempfile::tempdir().unwrap();
        let st = state_com("demo", TEMPORARIO, 7 * 3600); // 7 h atrás, TTL 6 h
        std::fs::create_dir_all(tmp.path().join("demo")).unwrap();
        expire_check_em(&st, None, agora(), tmp.path()).await;
        assert!(
            st.list_projects().unwrap().is_empty(),
            "projeto temporário expirado some do estado"
        );
        assert!(!tmp.path().join("demo").exists(), "diretório da VM apagado");
        let eventos: i64 = st
            .with(|c| {
                c.query_row(
                    "SELECT count(*) FROM events WHERE scope='temporary'",
                    [],
                    |r| r.get(0),
                )
            })
            .unwrap();
        assert!(eventos >= 1, "teardown deixa evento");
    }

    #[tokio::test]
    async fn temporario_fresco_nao_apaga() {
        let tmp = tempfile::tempdir().unwrap();
        let st = state_com("demo", TEMPORARIO, 60); // 1 min atrás
        std::fs::create_dir_all(tmp.path().join("demo")).unwrap();
        expire_check_em(&st, None, agora(), tmp.path()).await;
        assert_eq!(st.list_projects().unwrap().len(), 1, "dentro do TTL fica");
        assert!(tmp.path().join("demo").exists());
    }

    #[tokio::test]
    async fn interacao_renova_o_relogio_mesmo_com_registro_velho() {
        let tmp = tempfile::tempdir().unwrap();
        let st = state_com("demo", TEMPORARIO, 48 * 3600); // registrado há 2 dias
        st.touch_interaction("demo", flux_common::protocol::Env::Dev)
            .unwrap();
        expire_check_em(&st, None, agora(), tmp.path()).await;
        assert_eq!(
            st.list_projects().unwrap().len(),
            1,
            "interação vale mais que registro"
        );
    }

    #[tokio::test]
    async fn created_at_e_nunca_null_relogio_sempre_existe() {
        // Garantia do schema: created_at NOT NULL — o caso "sem relógio
        // nenhum" (clock.0.or(clock.1) == None) é DEFESA do código, não
        // estado alcançável. Projeto registrado-agora não expira de todo.
        let tmp = tempfile::tempdir().unwrap();
        let st = State::in_memory().unwrap();
        st.register_project("fantasma", TEMPORARIO, "h").unwrap();
        let (interacao, criado) = st.temporary_clock("fantasma").unwrap();
        assert!(interacao.is_none(), "nunca interagiu");
        assert!(criado.is_some(), "created_at NOT NULL = relógio garantido");
        expire_check_em(&st, None, agora(), tmp.path()).await;
        assert_eq!(
            st.list_projects().unwrap().len(),
            1,
            "recém-registrado fica"
        );
    }

    #[tokio::test]
    async fn projeto_normal_nunca_e_alvo() {
        let tmp = tempfile::tempdir().unwrap();
        let st = state_com("normal", "name = \"normal\"", 48 * 3600);
        std::fs::create_dir_all(tmp.path().join("normal")).unwrap();
        expire_check_em(&st, None, agora(), tmp.path()).await;
        assert_eq!(
            st.list_projects().unwrap().len(),
            1,
            "sem [temporary] = intocável"
        );
        assert!(tmp.path().join("normal").exists());
    }

    #[tokio::test]
    async fn worktree_dir_e_apagado_junto() {
        let tmp = tempfile::tempdir().unwrap();
        let st = state_com("demo", TEMPORARIO, 7 * 3600);
        std::fs::create_dir_all(tmp.path().join("demo@feature-x")).unwrap();
        expire_check_em(&st, None, agora(), tmp.path()).await;
        assert!(
            !tmp.path().join("demo@feature-x").exists(),
            "worktree demo@feature-x some junto"
        );
    }

    #[tokio::test]
    async fn nome_perigoso_e_recusado_sem_apagar_nada() {
        let st = state_com("a-b", TEMPORARIO, 7 * 3600);
        // Nome com caractere de path NUNCA vira glob (defesa em profundidade:
        // o registro devia barrar; aqui é a segunda cerca).
        st.register_project("../escapou", TEMPORARIO, "h").unwrap();
        st.with(|c| {
            c.execute(
                "UPDATE projects SET created_at = datetime('now', '-7 hours') WHERE name='../escapou'",
                [],
            )
        })
        .unwrap();
        expire_check_em(
            &st,
            None,
            agora(),
            std::path::Path::new("/srv/flux/projects"),
        )
        .await;
        assert!(
            st.list_projects()
                .unwrap()
                .contains(&"../escapou".to_owned()),
            "nome inseguro é pulado com erro, não removido"
        );
        st.purge_project("../escapou").unwrap();
    }

    fn agora() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    }
}
