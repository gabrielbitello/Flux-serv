//! Protocolo CLI ↔ API de controle (`:7420`) e corpos de resposta fixos.
//! Corpos de 503 são contratos: parsers do cliente dependem da forma exata
//! (12 — API nunca recebe HTML, nunca 200 com corpo de erro).

use serde::{Deserialize, Serialize};

/// Ambiente de um projeto. Todo projeto tem no máximo dois (00).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Env {
    Dev,
    Prod,
}

impl std::str::FromStr for Env {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "dev" => Ok(Self::Dev),
            "prod" => Ok(Self::Prod),
            other => Err(format!("ambiente inválido: {other:?} (use dev|prod)")),
        }
    }
}

impl std::fmt::Display for Env {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Dev => "dev",
            Self::Prod => "prod",
        })
    }
}

/// Estado do container visto pelo fluxd. A verdade vem do Docker (events +
/// sweep, 07) — estado interno nunca é tratado como verdade absoluta.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerState {
    Awake,
    Paused,
    Sleeping,
    Starting,
    Failed,
    Unknown,
}

impl ContainerState {
    /// Rótulo na coluna `sleep_state.mode` do SQLite (07).
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Awake => "awake",
            Self::Paused => "paused",
            Self::Sleeping => "sleeping",
            Self::Starting => "starting",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
        }
    }

    pub fn from_str_label(s: &str) -> Self {
        match s {
            "awake" => Self::Awake,
            "paused" => Self::Paused,
            "sleeping" => Self::Sleeping,
            "starting" => Self::Starting,
            "failed" => Self::Failed,
            _ => Self::Unknown,
        }
    }
}

/// Corpo do 503 para rota de API com serviço parado/acordando (matriz 03).
#[derive(Debug, Serialize)]
pub struct ServiceWakingBody {
    pub error: &'static str,
    pub retry_after: u64,
}

impl ServiceWakingBody {
    pub fn waking(retry_after: u64) -> Self {
        Self {
            error: "service_waking",
            retry_after,
        }
    }
}

/// Corpo do 503 quando o container falhou de verdade (crash loop/health).
#[derive(Debug, Serialize)]
pub struct ServiceFailedBody {
    pub error: &'static str,
}

impl Default for ServiceFailedBody {
    fn default() -> Self {
        Self {
            error: "service_failed",
        }
    }
}

/// GET /healthz na API de controle.
#[derive(Debug, Serialize)]
pub struct HealthBody {
    pub status: &'static str,
    pub version: &'static str,
}

/// Resumo de projeto para CLI/painel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectSummary {
    pub name: String,
    pub framework: String,
    pub prod_host: String,
    pub dev_host: String,
}

/// GET /projects/{name}/db — estado do banco próprio do projeto (21/10).
/// `own = false` vem com `own: false` e o resto vazio (o CLI distingue).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbStatusOut {
    pub own: bool,
    /// Grupo efetivo (default = nome do projeto).
    pub grupo: Option<String>,
    /// Container dedicado do grupo (`flux-db-{grupo}`).
    pub container: Option<String>,
    /// running | exited | paused | ausente | desconhecido.
    pub estado: Option<String>,
    /// health.status do engine quando existe.
    pub saude: Option<String>,
    pub ttl_days: Option<u64>,
    pub databases: Vec<DbRow>,
}

/// Database visível no container do grupo. `expira_em_secs` só existe para
/// database GERENCIADO de dev com relógio (prod e sem-relógio nunca expiram).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbRow {
    pub nome: String,
    pub gerenciado: bool,
    pub ultimo_contato: Option<i64>,
    pub expira_em_secs: Option<i64>,
}

/// POST /projects/{name}/db/query — leitura SQL no banco próprio (MCP, 22/10).
/// Só leitura: o fluxd valida com `sql_e_leitura` e recusa o resto com 422 —
/// a regra tem UM dono (o servidor); cliente manda verbatim.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbQueryIn {
    pub sql: String,
    /// Database de contexto (`USE` antes da consulta); ausente = sem USE
    /// (a SQL qualifica com `db.tabela`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database: Option<String>,
}

/// Corpo 200 do db/query: a saída crua do cliente `mysql -N` (TSV, uma linha
/// por registro, sem header) — formatação é do consumidor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbQueryOut {
    pub saida: String,
}

/// Envelope padrão de erro da API de controle.
#[derive(Debug, Serialize)]
pub struct ApiErrorBody {
    pub error: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_parse_e_display() {
        assert_eq!("dev".parse::<Env>().unwrap(), Env::Dev);
        assert_eq!(Env::Prod.to_string(), "prod");
        assert!("homolog".parse::<Env>().is_err());
    }

    #[test]
    fn estados_circulam_pelo_rotulo_do_sqlite() {
        for s in [
            ContainerState::Awake,
            ContainerState::Paused,
            ContainerState::Sleeping,
            ContainerState::Starting,
            ContainerState::Failed,
        ] {
            assert_eq!(ContainerState::from_str_label(s.as_str()), s);
        }
        assert_eq!(
            ContainerState::from_str_label("qualquer-coisa"),
            ContainerState::Unknown
        );
    }
}
