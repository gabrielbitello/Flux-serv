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
pub const SCHEMA_VERSION: i64 = 4;

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

/// v2 (10, F4): token de deploy POR PROJETO. Guarda SÓ o sha256 (lowercase
/// hex, 64 chars) — o valor claro vive no GitHub Secrets e é impresso uma
/// vez pelo `flux project token`; regenerar = INSERT OR REPLACE = hash
/// anterior revogado, um hash por projeto.
const SCHEMA_V2: &str = r#"
CREATE TABLE IF NOT EXISTS deploy_tokens(
    project TEXT PRIMARY KEY,
    token_hash TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
"#;

/// v3 (sync do mutagen): estado das sessões do `flux watch` — o watch roda no
/// HOST Windows, reporta ao fluxd (POST /sync/report) e o painel web lê/limpa
/// via GET /sync/status. `rescan_requested` é a fila de rescan POR PROJETO:
/// o painel seta, o watch consome e limpa com `rescan_applied` no relato
/// seguinte. `updated_at` em RFC3339 UTC (contrato do /sync/status); os
/// DEFAULT permitem a linha "só com flag" nascer do rescan antes do
/// primeiro relato do watch.
const SCHEMA_V3: &str = r#"
CREATE TABLE IF NOT EXISTS sync_status(
    project TEXT PRIMARY KEY,
    session TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'unknown',
    paused INTEGER NOT NULL DEFAULT 0,
    files INTEGER,
    problems INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    rescan_requested INTEGER NOT NULL DEFAULT 0,
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now'))
);
"#;

/// v4 ([domains], 09): hosts custom declarados no `flux.toml` — o ManagedHost
/// resolve o header `Host` aqui ANTES da derivação `{name}{suffix}.{zone}`.
/// `dev` = host de dev (sufixo -d). Um host por linha; re-registro substitui
/// as linhas do projeto (transação em `set_project_hosts`).
const SCHEMA_V4: &str = r#"
CREATE TABLE IF NOT EXISTS project_hosts(
    host TEXT PRIMARY KEY,
    project TEXT NOT NULL,
    dev INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_project_hosts_project ON project_hosts(project);
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

    /// Relógio da expiração temporária (14): `(last_interaction_dev,
    /// created_at)`, ambos epoch segundos ou `None`. O sweep usa o que
    /// existir (interação vence registro) — ambos ausentes = não expira.
    pub fn temporary_clock(&self, name: &str) -> Result<(Option<i64>, Option<i64>), StateError> {
        self.with(|c| {
            let created: Option<i64> = c
                .query_row(
                    "SELECT CAST(strftime('%s', created_at) AS INTEGER) FROM projects WHERE name = ?1",
                    params![name],
                    |r| r.get(0),
                )
                .optional()?;
            let interacao: Option<i64> = c
                .query_row(
                    "SELECT CAST(strftime('%s', last_interaction) AS INTEGER)
                     FROM sleep_state WHERE project = ?1 AND env = 'dev'",
                    params![name],
                    |r| r.get(0),
                )
                .optional()?;
            Ok((interacao, created))
        })
    }

    /// Apaga TODA a trilha do projeto no estado (14, teardown temporário).
    pub fn purge_project(&self, name: &str) -> Result<(), StateError> {
        self.with(|c| {
            for sql in [
                "DELETE FROM sleep_state WHERE project = ?1",
                "DELETE FROM migration_state WHERE project = ?1",
                "DELETE FROM active_color WHERE project = ?1",
                "DELETE FROM deploy_log WHERE project = ?1",
                // Estado de sync do watch (v3) também é trilha: sem isso o
                // painel mostraria sync de projeto temporário já apagado.
                "DELETE FROM sync_status WHERE project = ?1",
                // Hosts custom ([domains], v4): host órfão nunca pode continuar
                // resolvendo para projeto que não existe mais.
                "DELETE FROM project_hosts WHERE project = ?1",
                "DELETE FROM projects WHERE name = ?1",
            ] {
                c.execute(sql, params![name])?;
            }
            Ok(())
        })
    }

    /// Grava os hosts custom de `[domains]` (09, v4) — substitui as linhas do
    /// projeto no re-registro (delete+insert sob o MESMO lock do estado).
    /// Lista vazia = sem override (linhas apagadas).
    pub fn set_project_hosts(
        &self,
        name: &str,
        hosts: &[(String, bool)],
    ) -> Result<(), StateError> {
        self.with(|c| {
            c.execute(
                "DELETE FROM project_hosts WHERE project = ?1",
                params![name],
            )?;
            for (host, dev) in hosts {
                c.execute(
                    "INSERT INTO project_hosts(host, project, dev) VALUES (?1, ?2, ?3)",
                    params![host, name, *dev as i64],
                )?;
            }
            Ok(())
        })
    }

    /// `(prod, dev)` overrides do projeto (09) — `None` no lado sem override
    /// (a exibição cai na derivação `{name}{suffix}.{zone}`).
    pub fn project_hosts(
        &self,
        name: &str,
    ) -> Result<(Option<String>, Option<String>), StateError> {
        self.with(|c| {
            let mut prod = None;
            let mut dev = None;
            let mut stmt = c.prepare("SELECT host, dev FROM project_hosts WHERE project = ?1")?;
            let mut rows = stmt.query(params![name])?;
            while let Some(r) = rows.next()? {
                let host: String = r.get(0)?;
                let is_dev: i64 = r.get(1)?;
                if is_dev != 0 {
                    dev = Some(host);
                } else {
                    prod = Some(host);
                }
            }
            Ok((prod, dev))
        })
    }

    /// Resolve host custom EXATO (09): header `Host` cru (minúsculo, sem
    /// porta) → `(projeto, dev)`. Consulta do `ManagedHost::parse` — só bate
    /// exato; derivação por sufixo continua valendo para os demais hosts.
    pub fn host_custom(&self, host: &str) -> Result<Option<(String, bool)>, StateError> {
        self.with(|c| {
            c.query_row(
                "SELECT project, dev FROM project_hosts WHERE host = ?1",
                params![host],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? != 0)),
            )
            .optional()
        })
    }

    /// Cor ativa do blue-green de prod (10): `blue|green`, `None` quando o
    /// projeto não usa cores. A leitura guia proxy/wake/sleep na Prod.
    pub fn active_color(&self, name: &str) -> Result<Option<String>, StateError> {
        self.with(|c| {
            c.query_row(
                "SELECT color FROM active_color WHERE project = ?1",
                params![name],
                |r| r.get(0),
            )
            .optional()
        })
    }

    /// Define a cor ativa (troca ATÔMICA do blue-green, 10).
    pub fn set_active_color(&self, name: &str, color: &str) -> Result<(), StateError> {
        self.with(|c| {
            c.execute(
                "INSERT INTO active_color(project, color) VALUES (?1, ?2)
                 ON CONFLICT(project) DO UPDATE SET color = excluded.color",
                params![name, color],
            )?;
            Ok(())
        })
    }

    /// Conteúdo bruto do `flux.toml` registrado (o sweep de temporários
    /// re-parseia para saber TTL). `None` = projeto não registrado.
    pub fn project_toml(&self, name: &str) -> Result<Option<String>, StateError> {
        self.with(|c| {
            c.query_row(
                "SELECT toml FROM projects WHERE name = ?1",
                params![name],
                |r| r.get(0),
            )
            .optional()
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

    // ------------------------------------- token de deploy por projeto (10, F4)

    /// Grava o sha256 (lowercase hex, 64 chars) do token de deploy do
    /// projeto. INSERT OR REPLACE = regenerar revoga o hash anterior (um
    /// hash por projeto) sem tocar os tokens dos outros projetos.
    pub fn set_deploy_token_hash(
        &self,
        name: &str,
        token_hash_hex: &str,
    ) -> Result<(), StateError> {
        self.with(|c| {
            c.execute(
                "INSERT OR REPLACE INTO deploy_tokens(project, token_hash) VALUES (?1, ?2)",
                params![name, token_hash_hex],
            )?;
            Ok(())
        })
    }

    /// Hash do token de deploy do projeto; `None` = sem token configurado.
    /// Qualquer OUTRA falha de leitura propaga `Err` — fail-closed: o
    /// handler recusa (503) em vez de deixar a rota abrir sem token.
    pub fn deploy_token_hash(&self, name: &str) -> Result<Option<String>, StateError> {
        self.with(|c| {
            c.query_row(
                "SELECT token_hash FROM deploy_tokens WHERE project = ?1",
                params![name],
                |r| r.get::<_, String>(0),
            )
            .optional()
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
            conn.execute_batch(SCHEMA_V2)?;
            conn.execute_batch(SCHEMA_V3)?;
            conn.execute_batch(SCHEMA_V4)?;
            conn.execute(
                "INSERT INTO settings(k, v) VALUES ('schema_version', ?1)",
                params![SCHEMA_VERSION],
            )?;
        }
        // v1 → atual (10): acrescenta `deploy_tokens` e o que veio depois sem
        // tocar dado nenhum — o DDL é só CREATE TABLE IF NOT EXISTS, seguro
        // reexecutar.
        Some(1) => {
            conn.execute_batch(SCHEMA_V2)?;
            conn.execute_batch(SCHEMA_V3)?;
            conn.execute_batch(SCHEMA_V4)?;
            conn.execute(
                "UPDATE settings SET v = ?1 WHERE k = 'schema_version'",
                params![SCHEMA_VERSION],
            )?;
        }
        // v2 → atual (sync): acrescenta `sync_status` do watch; banco que já
        // tem token de deploy (todo fluxd em produção hoje).
        Some(2) => {
            conn.execute_batch(SCHEMA_V3)?;
            conn.execute_batch(SCHEMA_V4)?;
            conn.execute(
                "UPDATE settings SET v = ?1 WHERE k = 'schema_version'",
                params![SCHEMA_VERSION],
            )?;
        }
        // v3 → atual ([domains]): acrescenta `project_hosts`.
        Some(3) => {
            conn.execute_batch(SCHEMA_V4)?;
            conn.execute(
                "UPDATE settings SET v = ?1 WHERE k = 'schema_version'",
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

    /// `[domains]` (09, v4): gravação substitui, consulta exata resolve,
    /// purge apaga a trilha — host órfão nunca resolve.
    #[test]
    fn hosts_custom_grava_resolve_e_purga() {
        let path = tmp_db("hosts-custom");
        let _ = std::fs::remove_file(&path);
        let st = State::open(&path).unwrap();
        st.register_project("vipers", "name = \"vipers\"", "h")
            .unwrap();

        st.set_project_hosts(
            "vipers",
            &[
                ("vip.bitello.xyz".into(), false),
                ("vip-dev.bitello.xyz".into(), true),
            ],
        )
        .unwrap();
        let (prod, dev) = st.project_hosts("vipers").unwrap();
        assert_eq!(prod.as_deref(), Some("vip.bitello.xyz"));
        assert_eq!(dev.as_deref(), Some("vip-dev.bitello.xyz"));
        assert_eq!(
            st.host_custom("vip.bitello.xyz").unwrap(),
            Some(("vipers".into(), false))
        );
        assert_eq!(
            st.host_custom("vip-dev.bitello.xyz").unwrap(),
            Some(("vipers".into(), true))
        );
        assert_eq!(st.host_custom("outro.bitello.xyz").unwrap(), None);

        // Re-registro SUBSTITUI: dev some, prod troca.
        st.set_project_hosts("vipers", &[("novo.bitello.xyz".into(), false)])
            .unwrap();
        assert_eq!(st.host_custom("vip.bitello.xyz").unwrap(), None);
        assert_eq!(st.host_custom("vip-dev.bitello.xyz").unwrap(), None);
        assert_eq!(
            st.host_custom("novo.bitello.xyz").unwrap(),
            Some(("vipers".into(), false))
        );

        // Purge: nada de host apontando para projeto apagado.
        st.purge_project("vipers").unwrap();
        assert_eq!(st.host_custom("novo.bitello.xyz").unwrap(), None);
        assert_eq!(st.project_hosts("vipers").unwrap(), (None, None));
        std::fs::remove_file(&path).ok();
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

    /// Banco v1 REAL (criado na mão com o DDL antigo) migra para a versão
    /// atual sem perder projects/settings — o caso de quem atualizou o fluxd
    /// antes do `flux project token` (v2) e do sync do watch (v3) existirem.
    #[test]
    fn migra_v1_para_atual_preservando_projects_e_settings() {
        let path = tmp_db("v1-para-v2");
        let _ = std::fs::remove_file(&path);
        {
            let c = Connection::open(&path).unwrap();
            c.execute_batch(SCHEMA_V1).unwrap();
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
                "INSERT INTO settings(k, v) VALUES ('proxy_target/vipers/dev', '127.0.0.1:9000')",
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
            "versão sobe para a atual"
        );
        let projetos: i64 = st
            .with(|c| c.query_row("SELECT COUNT(*) FROM projects", [], |r| r.get(0)))
            .unwrap();
        assert_eq!(projetos, 1, "projeto registrado no v1 sobrevive");
        let proxy: String = st
            .with(|c| {
                c.query_row(
                    "SELECT v FROM settings WHERE k = 'proxy_target/vipers/dev'",
                    [],
                    |r| r.get(0),
                )
            })
            .unwrap();
        assert_eq!(proxy, "127.0.0.1:9000", "settings preservados");
        let tokens: i64 = st
            .with(|c| {
                c.query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='deploy_tokens'",
                    [],
                    |r| r.get(0),
                )
            })
            .unwrap();
        assert_eq!(tokens, 1, "deploy_tokens criada pela migração");
        let sync_t: i64 = st
            .with(|c| {
                c.query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='sync_status'",
                    [],
                    |r| r.get(0),
                )
            })
            .unwrap();
        assert_eq!(sync_t, 1, "sync_status criada pela migração");
        // Reabertura idempotente: versão atual reconhecida sem erro e sem
        // subir mais.
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
        assert_eq!(
            versao2,
            SCHEMA_VERSION.to_string(),
            "reabrir na versão atual é no-op"
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// Banco v2 REAL (todo fluxd em produção hoje, com deploy_tokens) migra
    /// para a atual criando `sync_status` — é o caminho que a VM da 07 roda
    /// quando recebe o fluxd com sync do watch.
    #[test]
    fn migra_v2_para_atual_criando_sync_status() {
        let path = tmp_db("v2-para-atual");
        let _ = std::fs::remove_file(&path);
        {
            let c = Connection::open(&path).unwrap();
            c.execute_batch(SCHEMA_V1).unwrap();
            c.execute_batch(SCHEMA_V2).unwrap();
            c.execute(
                "INSERT INTO settings(k, v) VALUES ('schema_version', '2')",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO projects(name, toml, toml_hash) VALUES ('vipa', 'name = \"vipa\"', 'hash')",
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
        assert_eq!(versao, SCHEMA_VERSION.to_string(), "v2 sobe para a atual");
        let projetos: i64 = st
            .with(|c| c.query_row("SELECT COUNT(*) FROM projects", [], |r| r.get(0)))
            .unwrap();
        assert_eq!(projetos, 1, "projeto do v2 sobrevive");
        // A tabela nova já aceita gravação (upsert do sync.rs fala com ela).
        st.with(|c| {
            c.execute(
                "INSERT INTO sync_status(project, rescan_requested) VALUES ('vipa', 1)",
                [],
            )?;
            Ok(())
        })
        .unwrap();
        let flag: i64 = st
            .with(|c| {
                c.query_row(
                    "SELECT rescan_requested FROM sync_status WHERE project = 'vipa'",
                    [],
                    |r| r.get(0),
                )
            })
            .unwrap();
        assert_eq!(flag, 1, "flag gravada na tabela recém-criada");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// Adulteração da versão continua recusada com erro claro — agora
    /// cobrindo os dois caminhos do guard: valor não-numérico e versão
    /// mais nova que a suportada.
    #[test]
    fn versao_adulterada_e_recusada_nos_dois_caminhos() {
        for valor in ["nao-e-numero", "99"] {
            let path = tmp_db("adulterada");
            let _ = std::fs::remove_file(&path);
            {
                let c = Connection::open(&path).unwrap();
                c.execute_batch(SCHEMA_V1).unwrap();
                c.execute(
                    "INSERT INTO settings(k, v) VALUES ('schema_version', ?1)",
                    params![valor],
                )
                .unwrap();
            }
            let err = State::open(&path).unwrap_err();
            let msg = err.to_string();
            if valor == "nao-e-numero" {
                assert!(
                    msg.contains("adulterado"),
                    "valor '{valor}' deve recusar com 'adulterado', erro: {msg}"
                );
            } else {
                assert!(
                    msg.contains("mais novo"),
                    "valor '{valor}' deve recusar como muito novo, erro: {msg}"
                );
            }
            let _ = std::fs::remove_dir_all(path.parent().unwrap());
        }
    }

    /// Roundtrip do token de deploy: grava, lê igual; regenerar troca o hash
    /// (revoga o anterior); projeto B nunca vê o token do projeto A.
    #[test]
    fn deploy_token_roundtrip_regenera_e_nao_toca_outro_projeto() {
        let st = State::in_memory().unwrap();
        st.set_deploy_token_hash("vipers", "hash-antigo-do-vipers")
            .unwrap();
        assert_eq!(
            st.deploy_token_hash("vipers").unwrap().as_deref(),
            Some("hash-antigo-do-vipers"),
            "hash gravado volta igual"
        );
        st.set_deploy_token_hash("vipers", "hash-novo-do-vipers")
            .unwrap();
        assert_eq!(
            st.deploy_token_hash("vipers").unwrap().as_deref(),
            Some("hash-novo-do-vipers"),
            "regenerar substitui (revoga o hash anterior)"
        );
        st.set_deploy_token_hash("djm", "hash-do-djm").unwrap();
        assert_eq!(
            st.deploy_token_hash("djm").unwrap().as_deref(),
            Some("hash-do-djm"),
            "outro projeto tem o próprio hash"
        );
        assert_eq!(
            st.deploy_token_hash("vipers").unwrap().as_deref(),
            Some("hash-novo-do-vipers"),
            "regenerar do djm não toca o vipers"
        );
        assert!(
            st.deploy_token_hash("sem-token").unwrap().is_none(),
            "projeto sem token = None (não erro)"
        );
    }
}
