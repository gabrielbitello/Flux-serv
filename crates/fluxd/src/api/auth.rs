//! Gate 09 + primitivas de token (L5.3): falha é SEMPRE visível e
//! fail-closed — sem hash no estado a API inteira responde 503; hash
//! divergente vira 401 com evento gravado. `bearer_token`/`sha256_hex`/
//! `hash_confere` são `pub(crate)` porque o handler de deploy (deploy.rs)
//! reusa o MESMO parse e a MESMA comparação do token de projeto.

use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::Next;
use axum::response::Response;
use rusqlite::params;
use sha2::{Digest, Sha256};
use std::sync::Arc;

use super::erro;
use crate::AppState;
use crate::events;
use crate::state::State as DbState;

pub(crate) const TOKEN_KEY: &str = "api_token_hash";

pub(crate) async fn auth(
    State(app): State<Arc<AppState>>,
    headers: HeaderMap,
    req: Request,
    next: Next,
) -> Response {
    let Some(hash) = stored_token_hash(&app.state) else {
        return erro(
            StatusCode::SERVICE_UNAVAILABLE,
            "api de controle não configurada: não há api_token_hash no estado — gere o token no `flux setup vm` e defina-o no banco (spec 09)",
        );
    };
    let enviado = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(bearer_token);
    // Recusa quando: sem header OU hash divergente (comparação em tempo
    // constante — ver `hash_confere`).
    if enviado.is_none_or(|t| !hash_confere(&sha256_hex(t.as_bytes()), &hash)) {
        // Recusa vira evento (09 critério 3) — o painel mostra tentativa de
        // acesso com token ruim, o CLI recebe a causa.
        events::event(
            &app.state,
            "warn",
            "auth",
            "token da API de controle recusado (header ausente, malformado ou hash divergente)",
        );
        return erro(
            StatusCode::UNAUTHORIZED,
            "token da API de controle recusado — repasse o Bearer gerado no `flux setup vm` (no CLI, env FLUX_API_TOKEN)",
        );
    }
    next.run(req).await
}
/// Lê o hash do token do estado. Qualquer falha de leitura fecha a API
/// (fail closed): erro de banco não pode virar porta aberta.
fn stored_token_hash(st: &DbState) -> Option<String> {
    match st.with(|c| {
        c.query_row(
            "SELECT v FROM settings WHERE k = ?1",
            params![TOKEN_KEY],
            |r| r.get::<_, String>(0),
        )
    }) {
        Ok(h) => Some(h),
        Err(crate::state::StateError::Sql(rusqlite::Error::QueryReturnedNoRows)) => None,
        Err(e) => {
            tracing::error!("não consegui ler {TOKEN_KEY} do estado — recusando requisições: {e}");
            None
        }
    }
}
/// `Authorization: Bearer <token>` → `<token>`. `pub(crate)` SÓ para o
/// handler de deploy (deploy.rs) reusar o mesmo parse — build_api e as rotas
/// da :7420 seguem intocados (a rota de deploy NUNCA entra aqui).
pub(crate) fn bearer_token(v: &str) -> Option<&str> {
    v.strip_prefix("Bearer ").filter(|t| !t.is_empty())
}
/// sha256 em hex lowercase de 64 chars — formato gravado pelo
/// `flux project token` e esperado pelo handler de deploy nos DOIS lados.
pub(crate) fn sha256_hex(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    let mut s = String::with_capacity(64);
    for b in h.finalize() {
        s.push_str(&format!("{b:02x}"));
    }
    s
}
/// Compara dois hashes hex em TEMPO CONSTANTE (XOR acumulado, sem early-exit):
/// o `!=` de String abandona no primeiro byte divergente e mede timing por
/// byte num processo local. O bind 127.0.0.1 já mitiga (lib.rs), mas o gate
/// de auth usa o padrão — custo trivial. Divergência de tamanho é pública
/// (sha256 hex são sempre 64 chars) e sai cedo sem vazar conteúdo.
/// `pub(crate)` SÓ para o handler de deploy comparar o token do projeto.
pub(crate) fn hash_confere(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}
