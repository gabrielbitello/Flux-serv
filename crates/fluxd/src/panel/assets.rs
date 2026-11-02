//! Assets estáticos do painel (03): `estilo.css` (style guide aprovado,
//! 02/11) e as fontes IBM Plex VENDIDAS no repo (`fontes/`, OFL.txt) —
//! nada de CDN: o painel abre em LAN/offline e não vaza request pra
//! terceiros. Tudo embedded (include_str!/include_bytes!): sem acesso a
//! disco, sem path traversal — o nome do arquivo casa por igualdade exata.
//!
//! SEM auth de propósito: é CSS e fonte, zero segredo, e a página de login
//! (única sem sessão) também carrega o estilo.

use axum::extract::Path;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};

pub(super) const ESTILO: &str = include_str!("estilo.css");

/// Alpine.js 3.17.5 VENDIDO (MIT, ALPINE-LICENSE-MIT) — build padrão (usa
/// eval; o painel hoje não emite CSP — se um dia emitir, migrar pro build
/// CSP e manter tudo em Alpine.data, que já é como painel.js se escreve).
const ALPINE: &str = include_str!("assets/alpine.min.js");
/// Cliente do painel novo (componentes Alpine.data; zero innerHTML com dado
/// do servidor — invariant testado).
const PAINEL_JS: &str = include_str!("assets/painel.js");

/// Chart.js 4.4.7 VENDIDO (MIT, CHARTJS-LICENSE-MIT) — gráficos do /observe.
const CHARTJS: &str = include_str!("assets/chart.umd.min.js");

const PLEX_SANS_400: &[u8] = include_bytes!("fontes/IBMPlexSans-Regular.woff2");
const PLEX_SANS_500: &[u8] = include_bytes!("fontes/IBMPlexSans-Medium.woff2");
const PLEX_SANS_600: &[u8] = include_bytes!("fontes/IBMPlexSans-SemiBold.woff2");
const PLEX_MONO_400: &[u8] = include_bytes!("fontes/IBMPlexMono-Regular.woff2");
const PLEX_MONO_500: &[u8] = include_bytes!("fontes/IBMPlexMono-Medium.woff2");

/// `GET /assets/estilo.css` — a folha única do painel novo.
pub(super) async fn estilo() -> Response {
    resposta(ESTILO.as_bytes(), "text/css; charset=utf-8")
}

/// `GET /assets/alpine.min.js` — framework vendido, sem CDN.
pub(super) async fn alpine() -> Response {
    resposta(ALPINE.as_bytes(), "application/javascript; charset=utf-8")
}

/// `GET /assets/painel.js` — o cliente do painel.
pub(super) async fn painel_js() -> Response {
    resposta(
        PAINEL_JS.as_bytes(),
        "application/javascript; charset=utf-8",
    )
}

/// `GET /assets/chart.umd.min.js` — gráficos do /observe, sem CDN.
pub(super) async fn chartjs() -> Response {
    resposta(CHARTJS.as_bytes(), "application/javascript; charset=utf-8")
}

/// `GET /assets/fontes/{arquivo}` — woff2 casando por nome exato; nome
/// desconhecido = 404 (não vira listagem nem leitura de disco).
pub(super) async fn fonte(Path(arquivo): Path<String>) -> Response {
    let corpo = match arquivo.as_str() {
        "IBMPlexSans-Regular.woff2" => PLEX_SANS_400,
        "IBMPlexSans-Medium.woff2" => PLEX_SANS_500,
        "IBMPlexSans-SemiBold.woff2" => PLEX_SANS_600,
        "IBMPlexMono-Regular.woff2" => PLEX_MONO_400,
        "IBMPlexMono-Medium.woff2" => PLEX_MONO_500,
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    resposta(corpo, "font/woff2")
}

/// `'static` nos dois: todo corpo aqui é const embedded (include_*), e o
/// `IntoResponse` de `&[u8]` no axum só existe para `'static`.
fn resposta(corpo: &'static [u8], tipo: &'static str) -> Response {
    (
        [
            (header::CONTENT_TYPE, tipo),
            // Imutável na prática (fonte/versionado); 1 dia basta pra
            // atualização simples sem bust de cache.
            (header::CACHE_CONTROL, "public, max-age=86400"),
        ],
        corpo,
    )
        .into_response()
}
