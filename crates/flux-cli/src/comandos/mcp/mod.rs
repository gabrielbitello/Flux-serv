//! Servidor MCP do flux (22/10): a IA conversa com a plataforma por tools
//! estruturadas em vez de rodar comando no terminal. Transporte stdio com
//! JSON-RPC 2.0 delimitado por linha (protocolo MCP) — sem dependência nova:
//! envelope e handshake em `protocolo.rs`, catálogo/dispatch em
//! `ferramentas.rs`, leituras em `consultas.rs`, ações em `acoes.rs`.
//!
//! Regras: stdout é SÓ o protocolo (diagnóstico vai pra stderr — o
//! `ApiClient::from_env` já avisa lá); nenhuma tool morre com panic — erro
//! vira `isError: true` com a causa classificada da API; tudo que precisa de
//! projeto aceita `projeto` explícito (o MCP é processo long-lived, o cwd do
//! host não é contexto confiável).

mod acoes;
mod consultas;
mod ferramentas;
mod protocolo;

/// Entrada do `flux mcp`: serve até o stdin fechar (EOF = despedida limpa).
pub(crate) async fn servir() {
    protocolo::servir().await;
}
