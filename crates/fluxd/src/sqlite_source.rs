//! Fonte de verdade no SQLite para a decisão do router (07). Consultas por
//! request são chave→valor simples; erro de leitura é LOGADO e degrada para
//! "Unknown" (que vira página de espera/503 acordando), nunca pânico.

use flux_common::protocol::{ContainerState, Env};
use rusqlite::{OptionalExtension, params};

use crate::decision::HostSource;
use crate::state::State;

pub struct SqliteSource<'a> {
    st: &'a State,
}

impl<'a> SqliteSource<'a> {
    pub fn new(st: &'a State) -> Self {
        Self { st }
    }
}

impl HostSource for SqliteSource<'_> {
    fn project_exists(&self, name: &str) -> bool {
        match self.st.with(|c| {
            c.query_row(
                "SELECT 1 FROM projects WHERE name = ?1",
                params![name],
                |_| Ok(()),
            )
            .optional()
        }) {
            Ok(Some(())) => true,
            Ok(None) => false,
            Err(e) => {
                tracing::error!("consulta de projeto {name:?} falhou: {e}");
                false
            }
        }
    }

    fn env_state(&self, name: &str, env: Env) -> ContainerState {
        let r = self.st.with(|c| {
            c.query_row(
                "SELECT mode FROM sleep_state WHERE project = ?1 AND env = ?2",
                params![name, env.to_string()],
                |row| row.get::<_, String>(0),
            )
            .optional()
        });
        match r {
            Ok(Some(mode)) => ContainerState::from_str_label(&mode),
            Ok(None) => ContainerState::Unknown,
            Err(e) => {
                tracing::error!("consulta de estado {name:?}/{env} falhou: {e}");
                ContainerState::Unknown
            }
        }
    }

    fn branch_exists(&self, _name: &str, _branch: &str) -> bool {
        // O registro de branches locais chega do `flux watch` (02) na fase F11;
        // sem a fonte, toda branch declarada é tratada como existente — mentir
        // "branch faltante" sem fonte seria falso positivo.
        true
    }

    /// Hosts custom de `[domains]` (09): batida exata na tabela `project_hosts`.
    fn host_custom(&self, host: &str) -> Option<(String, bool)> {
        match self.st.host_custom(host) {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("consulta de host custom {host:?} falhou: {e}");
                None
            }
        }
    }

    fn proxy_target(&self, name: &str, env: Env) -> Option<String> {
        match self.st.with(|c| {
            c.query_row(
                "SELECT v FROM settings WHERE k = ?1",
                params![format!("proxy_target/{name}/{env}")],
                |row| row.get::<_, String>(0),
            )
            .optional()
        }) {
            Ok(v) => v,
            Err(e) => {
                tracing::error!("consulta de proxy_target {name:?}/{env} falhou: {e}");
                None
            }
        }
    }

    fn release_flags(&self, name: &str) -> (bool, bool) {
        let r = self.st.with(|c| {
            c.query_row(
                "SELECT has_robots, has_sitemap FROM active_color WHERE project = ?1",
                params![name],
                |row| Ok((row.get::<_, i64>(0)? != 0, row.get::<_, i64>(1)? != 0)),
            )
            .optional()
        });
        match r {
            Ok(Some(flags)) => flags,
            Ok(None) => (false, false),
            Err(e) => {
                tracing::error!("consulta de release {name:?} falhou: {e}");
                (false, false)
            }
        }
    }
}
