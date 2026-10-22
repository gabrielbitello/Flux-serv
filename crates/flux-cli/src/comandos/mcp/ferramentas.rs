//! Despachante do transporte stdio (22/10): as tools falam com a API de
//! controle do fluxd via `ApiClient` (túnel SSH automático) — leitura em
//! `consultas.rs`, ação em `acoes.rs`. O CATÁLOGO e o ENVELOPE do protocolo
//! são os do `flux-common::mcp` (fonte única, compartilhados com o listener
//! HTTP do fluxd) — nada de schema de tool duplicado aqui.

use serde_json::Value;

use super::{acoes, consultas};

/// Implementação do `flux_common::mcp::Despachante` para o CLI.
pub(crate) struct DespachanteCli;

impl flux_common::mcp::Despachante for DespachanteCli {
    async fn chamar(&self, nome: &str, args: &Value) -> Result<Value, String> {
        match nome {
            "flux_projetos" => consultas::projetos().await,
            "flux_status" => consultas::status(args).await,
            "flux_ps" => consultas::ps(args).await,
            "flux_sync_status" => consultas::sync_status(args).await,
            "flux_db_status" => consultas::db_status(args).await,
            "flux_logs" => consultas::logs(args).await,
            "flux_acordar" => acoes::acordar(args).await,
            "flux_db_query" => acoes::db_query(args).await,
            "flux_tool" => acoes::ferramenta_exec(args).await,
            outra => unreachable!("catálogo do flux-common e roteador divergiram: {outra}"),
        }
    }
}
