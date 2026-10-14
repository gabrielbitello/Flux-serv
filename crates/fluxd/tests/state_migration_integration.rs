//! Testes de integração do migrador de schema do estado do fluxd (state.rs):
//! um banco v1 REAL (DDL antigo + `schema_version = 1`, como um fluxd antigo
//! teria deixado em `/srv/flux/state/flux.sqlite3`) sobe para v2 na abertura
//! sem perder dado nenhum; banco com `schema_version` ADULTERADA (valor
//! não-numérico ou versão mais nova do que o binário entende) é RECUSADO na
//! abertura (guard do `migrate`, state.rs:496-549) — o fluxd nunca abre o
//! que não entende e nunca migra para baixo.
//!
//! Recorte: `SCHEMA_V1` é `const` PRIVADO de state.rs — o DDL antigo é
//! reproduzido aqui como fixture (cópia de state.rs:19-73) para montar o
//! banco antigo de verdade, em arquivo, a partir de fora do crate.

use fluxd::state::{SCHEMA_VERSION, State};
use rusqlite::Connection;

/// DDL do schema v1 — cópia VERBATIM do `SCHEMA_V1` de state.rs (19-73),
/// incluindo `deploy_log` (a tabela de trilha já era da V1; a NOVA da v2 é
/// só `deploy_tokens`).
const SCHEMA_V1_FIXTURE: &str = r#"
CREATE TABLE IF NOT EXISTS projects(
    name TEXT PRIMARY KEY,
    toml TEXT NOT NULL,
    toml_hash TEXT NOT NULL,
    compose_hash TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE TABLE IF NOT EXISTS sleep_state(
    project TEXT NOT NULL,
    env TEXT NOT NULL CHECK(env IN ('dev','prod')),
    last_interaction TEXT,
    sleeping_since TEXT,
    lease_until TEXT,
    cooldown_until TEXT,
    mode TEXT NOT NULL DEFAULT 'awake',
    PRIMARY KEY(project, env)
);
CREATE TABLE IF NOT EXISTS migration_state(
    project TEXT NOT NULL,
    env TEXT NOT NULL,
    file TEXT NOT NULL,
    sha256 TEXT NOT NULL,
    batch INTEGER,
    applied_at TEXT,
    PRIMARY KEY(project, env, file)
);
CREATE TABLE IF NOT EXISTS active_color(
    project TEXT PRIMARY KEY,
    color TEXT NOT NULL,
    release_sha TEXT,
    has_robots INTEGER NOT NULL DEFAULT 0,
    has_sitemap INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS deploy_log(
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    project TEXT NOT NULL,
    env TEXT NOT NULL,
    kind TEXT NOT NULL,
    detail_json TEXT,
    ok INTEGER NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE TABLE IF NOT EXISTS events(
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    level TEXT NOT NULL,
    scope TEXT NOT NULL,
    message TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE TABLE IF NOT EXISTS settings(
    k TEXT PRIMARY KEY,
    v TEXT NOT NULL
);
"#;

/// Banco v1 em ARQUIVO migra para v2 na abertura, preservando o dado
/// (projects, deploy_log, settings), criando a `deploy_tokens` nova e
/// operável — e reabrir o banco já em v2 é no-op (state.rs:527-534).
#[test]
fn banco_v1_em_arquivo_migra_para_v2_preservando_dado() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.sqlite3");
    {
        let c = Connection::open(&path).unwrap();
        c.execute_batch(SCHEMA_V1_FIXTURE).unwrap();
        c.execute(
            "INSERT INTO settings(k, v) VALUES ('schema_version', '1')",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO projects(name, toml, toml_hash) VALUES ('vipers', 'name = \"vipers\"', 'hash')",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO deploy_log(project, env, kind, detail_json, ok) \
             VALUES ('vipers', 'prod', 'aceito', '{}', 1)",
            [],
        )
        .unwrap();
    }

    let st = State::open(&path).unwrap();

    let versao: String = st
        .with(|c| {
            c.query_row(
                "SELECT v FROM settings WHERE k = 'schema_version'",
                [],
                |r| r.get(0),
            )
        })
        .unwrap();
    assert_eq!(
        versao,
        SCHEMA_VERSION.to_string(),
        "versão sobe para a atual do binário"
    );

    let projetos: i64 = st
        .with(|c| c.query_row("SELECT COUNT(*) FROM projects", [], |r| r.get(0)))
        .unwrap();
    assert_eq!(projetos, 1, "projeto registrado no v1 sobrevive");

    let logs: i64 = st
        .with(|c| c.query_row("SELECT COUNT(*) FROM deploy_log", [], |r| r.get(0)))
        .unwrap();
    assert_eq!(logs, 1, "deploy_log (tabela da V1) preservado");

    // A tabela NOVA da v2 existe e é operável no banco migrado.
    st.set_deploy_token_hash("vipers", "hash-do-token").unwrap();
    assert_eq!(
        st.deploy_token_hash("vipers").unwrap().as_deref(),
        Some("hash-do-token"),
        "deploy_tokens criada pela migração e funcional"
    );

    // Reabrir o banco já em v2 é no-op: sem erro e sem subir mais.
    drop(st);
    let st2 = State::open(&path).unwrap();
    let versao2: String = st2
        .with(|c| {
            c.query_row(
                "SELECT v FROM settings WHERE k = 'schema_version'",
                [],
                |r| r.get(0),
            )
        })
        .unwrap();
    assert_eq!(versao2, SCHEMA_VERSION.to_string(), "reabrir em v2 é no-op");
}

/// Adulteração do `schema_version` é recusada na abertura nos DOIS caminhos
/// do guard (state.rs:507-546): valor não-numérico — "o banco foi
/// adulterado" — e versão mais nova do que este binário entende
/// (`SchemaTooNew` — "atualize o fluxd").
#[test]
fn schema_version_adulterada_e_recusada_na_abertura() {
    for (valor, pista) in [("nao-e-numero", "adulterado"), ("99", "atualize o fluxd")] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.sqlite3");
        {
            let c = Connection::open(&path).unwrap();
            c.execute_batch(SCHEMA_V1_FIXTURE).unwrap();
            c.execute(
                "INSERT INTO settings(k, v) VALUES ('schema_version', ?1)",
                [valor],
            )
            .unwrap();
        }
        let err = State::open(&path).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains(pista),
            "valor {valor:?} tem que ser recusado com a causa {pista:?}: {msg}"
        );
    }
}
