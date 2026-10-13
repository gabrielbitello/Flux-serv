//! Estado do fluxd: SQLite único em WAL (`/srv/flux/state/flux.sqlite3`, 07).
//! NADA de estado de projeto fora daqui + Docker + arquivos. Corrompido =
//! recusa com erro claro apontando o backup, nunca recria em silêncio
//! (critério de aceite 5 da 07).

use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use flux_common::error::FluxError;
use flux_common::protocol::{ContainerState, Env};
use rusqlite::{Connection, OptionalExtension, params};

/// Versão do schema guardada em `settings.schema_version`.
pub const SCHEMA_VERSION: i64 = 1;

/// Linha de sono de um projeto/ambiente (epoch segundos; `None` = nunca).
pub type SleepRow = (Option<i64>, Option<i64>, Option<i64>, Option<String>);

const SCHEMA_V1: &str = r#"
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

#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error(transparent)]
    Flux(#[from] FluxError),
    #[error("SQLite: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error(
        "mutex do estado envenenado após pânico interno — reinicie o fluxd e verifique os logs"
    )]
    Poisoned,
}

pub struct State {
    conn: Mutex<Connection>,
}

impl std::fmt::Debug for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("State").finish_non_exhaustive()
    }
}

impl State {
    pub fn open(path: &Path) -> Result<Self, StateError> {
        let conn = Connection::open(path).map_err(|e| {
            tracing::error!(path = %path.display(), "falha abrindo banco de estado: {e}");
            StateError::Sql(e)
        })?;
        Self::init(conn, Some(path))
    }

    /// Em memória — testes.
    pub fn in_memory() -> Result<Self, StateError> {
        let conn = Connection::open_in_memory()?;
        Self::init(conn, None)
    }

    fn init(conn: Connection, path: Option<&Path>) -> Result<Self, StateError> {
        let path_label = path
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "<memória>".into());
        let err = |e: rusqlite::Error| corrupt_or_other(e, &path_label);

        if path.is_some() {
            conn.pragma_update(None, "journal_mode", "WAL")
                .map_err(err)?;
            conn.pragma_update(None, "synchronous", "NORMAL")
                .map_err(err)?;
        }
        conn.pragma_update(None, "foreign_keys", "ON")
            .map_err(err)?;

        match conn.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0)) {
            Ok(status) if status == "ok" => {}
            Ok(status) => {
                tracing::error!(path = %path_label, "integrity_check reportou: {status}");
                return Err(FluxError::StateCorrupt { path: path_label }.into());
            }
            Err(e) => return Err(err(e)),
        }

        migrate(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Acesso exclusivo à conexão. SQLite rápido + handlers tokio: mutex síncrono
    /// basta até a fase de sampler; se virar contenda, vira canal dedicado.
    pub fn with<T>(
        &self,
        f: impl FnOnce(&Connection) -> rusqlite::Result<T>,
    ) -> Result<T, StateError> {
        let conn = self.lock()?;
        f(&conn).map_err(StateError::Sql)
    }

    fn lock(&self) -> Result<MutexGuard<'_, Connection>, StateError> {
        self.conn.lock().map_err(|_| StateError::Poisoned)
    }

    /// Registra projeto (idempotente) — usado por testes e, na fase F6, pelo
    /// `fluxd ctl register`.
    pub fn register_project(
        &self,
        name: &str,
        toml_src: &str,
        toml_hash: &str,
    ) -> Result<(), StateError> {
        self.with(|c| {
            c.execute(
                "INSERT INTO projects(name, toml, toml_hash) VALUES (?1, ?2, ?3)
                 ON CONFLICT(name) DO UPDATE SET toml = excluded.toml, toml_hash = excluded.toml_hash",
                params![name, toml_src, toml_hash],
            )?;
            Ok(())
        })
    }

    /// Define estado de sono de um projeto/ambiente (testes + sleep manager F4).
    pub fn set_env_state(
        &self,
        name: &str,
        env: Env,
        mode: ContainerState,
    ) -> Result<(), StateError> {
        self.with(|c| {
            c.execute(
                "INSERT INTO sleep_state(project, env, mode) VALUES (?1, ?2, ?3)
                 ON CONFLICT(project, env) DO UPDATE SET mode = excluded.mode",
                params![name, env.to_string(), mode.as_str()],
            )?;
            Ok(())
        })
    }

    /// Define alvo de proxy `host:porta` do container (testes; a fase F3 troca
    /// por resolução via bollard com cache).
    pub fn set_proxy_target(&self, name: &str, env: Env, target: &str) -> Result<(), StateError> {
        self.with(|c| {
            c.execute(
                "INSERT INTO settings(k, v) VALUES (?1, ?2)
                 ON CONFLICT(k) DO UPDATE SET v = excluded.v",
                params![format!("proxy_target/{name}/{env}"), target],
            )?;
            Ok(())
        })
    }

    // ------------------------------------------------- sinais de sono (spec 04)

    /// Lista de projetos registrados (o sweep itera sobre estes).
    pub fn list_projects(&self) -> Result<Vec<String>, StateError> {
        self.with(|c| {
            let mut stmt = c.prepare("SELECT name FROM projects ORDER BY name")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            rows.collect()
        })
    }

    /// Linha de `sleep_state` de um projeto/ambiente — `(last_interaction,
    /// lease_until, cooldown_until, mode)`, tudo como epoch segundos
    /// (`NULL` quando nunca houve sinal).
    pub fn sleep_row(&self, name: &str, env: Env) -> Result<SleepRow, StateError> {
        self.with(|c| {
            c.query_row(
                "SELECT
                    CAST(strftime('%s', last_interaction) AS INTEGER),
                    CAST(strftime('%s', lease_until) AS INTEGER),
                    CAST(strftime('%s', cooldown_until) AS INTEGER),
                    mode
                 FROM sleep_state WHERE project = ?1 AND env = ?2",
                params![name, env.to_string()],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get::<_, Option<String>>(3)?,
                    ))
                },
            )
            .optional()
            .map(|o| o.unwrap_or((None, None, None, None)))
        })
    }

    /// Sinal de interação (request proxyado, lease, CPU alta): zera o timer e
    /// garante a linha existente.
    pub fn touch_interaction(&self, name: &str, env: Env) -> Result<(), StateError> {
        self.with(|c| {
            c.execute(
                "INSERT INTO sleep_state(project, env, last_interaction, mode)
                 VALUES (?1, ?2, datetime('now'), 'awake')
                 ON CONFLICT(project, env) DO UPDATE
                 SET last_interaction = datetime('now')",
                params![name, env.to_string()],
            )?;
            Ok(())
        })
    }

    /// Grava o estado observado no Docker + `sleeping_since` quando dorme.
    pub fn record_mode(
        &self,
        name: &str,
        env: Env,
        mode: ContainerState,
    ) -> Result<(), StateError> {
        self.with(|c| {
            c.execute(
                "INSERT INTO sleep_state(project, env, mode, sleeping_since)
                 VALUES (?1, ?2, ?3, CASE WHEN ?3 IN ('sleeping','paused')
                     THEN datetime('now') ELSE NULL END)
                 ON CONFLICT(project, env) DO UPDATE SET
                    mode = excluded.mode,
                    sleeping_since = excluded.sleeping_since",
                params![name, env.to_string(), mode.as_str()],
            )?;
            Ok(())
        })
    }

    /// Cooldown anti-flapping (04): imune a dormir por N segundos após acordar.
    pub fn set_cooldown(&self, name: &str, env: Env, secs: i64) -> Result<(), StateError> {
        self.with(|c| {
            c.execute(
                "INSERT INTO sleep_state(project, env, cooldown_until, mode)
                 VALUES (?1, ?2, datetime('now', '+' || ?3 || ' seconds'), 'awake')
                 ON CONFLICT(project, env) DO UPDATE SET
                    cooldown_until = datetime('now', '+' || ?3 || ' seconds')",
                params![name, env.to_string(), secs],
            )?;
            Ok(())
        })
    }

    /// Lease de sessão (CLI/painel): mantém acordado enquanto renova o TTL.
    pub fn set_lease(&self, name: &str, env: Env, ttl_secs: i64) -> Result<(), StateError> {
        self.with(|c| {
            c.execute(
                "INSERT INTO sleep_state(project, env, lease_until, mode)
                 VALUES (?1, ?2, datetime('now', '+' || ?3 || ' seconds'), 'awake')
                 ON CONFLICT(project, env) DO UPDATE SET
                    lease_until = datetime('now', '+' || ?3 || ' seconds')",
                params![name, env.to_string(), ttl_secs],
            )?;
            Ok(())
        })
    }

    // --------------------------------------------- migrations (spec 05, F5)

    /// `(arquivo, sha256)` já aplicados para o projeto/ambiente.
    pub fn migration_rows(
        &self,
        name: &str,
        env: Env,
    ) -> Result<Vec<(String, String)>, StateError> {
        self.with(|c| {
            let mut stmt = c.prepare(
                "SELECT file, sha256 FROM migration_state
                 WHERE project = ?1 AND env = ?2 ORDER BY file",
            )?;
            let rows = stmt.query_map(params![name, env.to_string()], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?;
            rows.collect()
        })
    }

    /// Grava hash aplicado de uma migration (após sucesso do runner).
    pub fn record_migration(
        &self,
        name: &str,
        env: Env,
        file: &str,
        sha256: &str,
    ) -> Result<(), StateError> {
        self.with(|c| {
            c.execute(
                "INSERT INTO migration_state(project, env, file, sha256, applied_at)
                 VALUES (?1, ?2, ?3, ?4, datetime('now'))
                 ON CONFLICT(project, env, file) DO UPDATE SET
                    sha256 = excluded.sha256, applied_at = excluded.applied_at",
                params![name, env.to_string(), file, sha256],
            )?;
            Ok(())
        })
    }

    /// Limpa o estado de migrations do projeto/ambiente — só no rebuild de dev
    /// (`migrate:fresh` reaplica tudo e o estado é regravado em seguida).
    pub fn clear_migrations(&self, name: &str, env: Env) -> Result<(), StateError> {
        self.with(|c| {
            c.execute(
                "DELETE FROM migration_state WHERE project = ?1 AND env = ?2",
                params![name, env.to_string()],
            )?;
            Ok(())
        })
    }
}

/// Corrupção de arquivo vira `StateCorrupt` (com o backup no erro) em QUALQUER
/// operação de abertura — nunca um "SQLite: ..." cru sem caminho de recuperação.
fn corrupt_or_other(e: rusqlite::Error, path_label: &str) -> StateError {
    match e.sqlite_error_code() {
        Some(rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase) => {
            FluxError::StateCorrupt {
                path: path_label.to_owned(),
            }
            .into()
        }
        _ => StateError::Sql(e),
    }
}

fn migrate(conn: &Connection) -> Result<(), StateError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS settings(k TEXT PRIMARY KEY, v TEXT NOT NULL);",
    )?;
    let raw: Option<String> = conn
        .query_row(
            "SELECT v FROM settings WHERE k = 'schema_version'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    let v = raw
        .as_deref()
        .map(|s| s.parse::<i64>())
        .transpose()
        .map_err(|e| {
            FluxError::Other(format!(
                "schema_version no estado não é número ({e}) — banco foi adulterado"
            ))
        })?;
    match v {
        None => {
            conn.execute_batch(SCHEMA_V1)?;
            conn.execute(
                "INSERT INTO settings(k, v) VALUES ('schema_version', ?1)",
                params![SCHEMA_VERSION],
            )?;
        }
        Some(found) if found == SCHEMA_VERSION => {}
        Some(found) if found < SCHEMA_VERSION => {
            return Err(FluxError::Other(format!(
                "schema {found} é mais antigo que o suportado ({SCHEMA_VERSION}) e não há migrador — erro de programação: versões de schema só sobem"
            )).into());
        }
        Some(found) => {
            return Err(FluxError::SchemaTooNew {
                found,
                supported: SCHEMA_VERSION,
            }
            .into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_db(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("fluxd-test-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("state.sqlite3")
    }

    #[test]
    fn migra_e_reabre_sem_perder_dados() {
        let path = tmp_db("reabre");
        let _ = std::fs::remove_file(&path);
        {
            let st = State::open(&path).unwrap();
            st.register_project("vipers", "name = \"vipers\"", "hash")
                .unwrap();
        }
        let st = State::open(&path).unwrap();
        let n: i64 = st
            .with(|c| c.query_row("SELECT COUNT(*) FROM projects", [], |r| r.get(0)))
            .unwrap();
        assert_eq!(n, 1);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn versao_de_schema_mais_nova_e_recusada() {
        let st = State::in_memory().unwrap();
        st.with(|c| {
            c.execute(
                "UPDATE settings SET v = '99' WHERE k = 'schema_version'",
                [],
            )?;
            Ok(())
        })
        .unwrap();
        let path = tmp_db("toonew");
        let _ = std::fs::remove_file(&path);
        drop(st);
        // reescreve o arquivo com schema 99 e tenta abrir
        {
            let c = Connection::open(&path).unwrap();
            c.execute_batch(SCHEMA_V1).unwrap();
            c.execute(
                "INSERT INTO settings(k, v) VALUES ('schema_version', '99')",
                [],
            )
            .unwrap();
        }
        let err = State::open(&path).unwrap_err();
        assert!(err.to_string().contains("mais novo"), "erro: {err}");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn arquivo_corrompido_e_recusado_com_backup_no_erro() {
        let path = tmp_db("corrompido");
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, b"isto definitivamente nao e um banco sqlite").unwrap();
        let err = State::open(&path).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("restaure de /srv/flux/backups/"),
            "erro: {msg}"
        );
        assert!(msg.contains("nunca recria em silêncio"), "erro: {msg}");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn memoria_e_usada_pelos_testes() {
        let st = State::in_memory().unwrap();
        st.set_env_state("vipers", Env::Dev, ContainerState::Sleeping)
            .unwrap();
        st.set_proxy_target("vipers", Env::Dev, "127.0.0.1:9000")
            .unwrap();
        let mode: String = st
            .with(|c| {
                c.query_row(
                    "SELECT mode FROM sleep_state WHERE project='vipers' AND env='dev'",
                    [],
                    |r| r.get(0),
                )
            })
            .unwrap();
        assert_eq!(mode, "sleeping");
    }
}
