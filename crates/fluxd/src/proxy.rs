//! Proxy L7 streaming (07): request e response SEM buffering — SSE, upload
//! grande e upgrade passam desde o primeiro byte (o porteiro bufferizava;
//! erro que o Flux não repete). Websocket upgrade completo é da fase F3.
//!
//! Única exceção ao streaming: a injeção do overlay de sono (04). Respostas
//! ELEGÍVEIS (`elegivel`: 200 + text/html + sem content-encoding +
//! content-length presente e < 256 KB) são bufferizadas — teto já garantido
//! pelo content-length — para costurar `beat::SCRIPT` antes de `</body>` e
//! regravar o content-length. Qualquer OUTRA resposta segue intacta, byte a
//! byte (spec 04, "o que NUNCA fazer": jamais corromper corpo, jamais injetar
//! em comprimido; upstream morto no meio de corpo elegível vira `Err` para o
//! router reavaliar — nunca HTML trincado).

use axum::body::Body;
use axum::http::request::Parts;
use axum::http::{
    HeaderMap, HeaderValue, Request, Response as HttpResponse, StatusCode, Uri, header,
};
use axum::response::Response;
use http_body_util::BodyExt;
use hyper_util::client::legacy::{Client, connect::HttpConnector};
use hyper_util::rt::{TokioExecutor, TokioIo};

/// Injeção do overlay de sono (04): ativa por default. Virar `false` aqui
/// desliga a injeção em TODOS os forwards — a rota `/__flux/beat` continua
/// de pé, só o overlay para de ser servido.
pub const INJECT_BEAT: bool = true;

/// Teto do corpo elegível (04): `content-length` PRESENTE e MENOR que isso.
/// Igual ou acima — ou sem length (chunked/streaming) — passa intacto.
pub const MAX_INJECT_BYTES: usize = 256 * 1024;

/// Cliente HTTP do proxy — um só, reutilizado (pool de conexões do hyper).
/// O client legacy do hyper-util conecta HTTP/1.1 com `with_upgrades()` —
/// o túnel do websocket do HMR (06/live) funciona sem flag extra; para
/// request sem upgrade a conexão segue idêntica.
pub type ProxyClient = Client<HttpConnector, Body>;

pub fn new_client() -> ProxyClient {
    Client::builder(TokioExecutor::new()).build_http()
}

/// Headers hop-by-hop (RFC 9110) — não cruzam o proxy.
const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailers",
    "transfer-encoding",
    "upgrade",
];

fn strip_hop_by_hop(headers: &mut HeaderMap) {
    for h in HOP_BY_HOP {
        headers.remove(*h);
    }
}

/// Elegibilidade da injeção (04) direto dos headers da resposta. Header
/// mentiroso (content-length não numérico) = não elegível — passa intacto.
fn resposta_elegivel(status: StatusCode, headers: &HeaderMap) -> bool {
    elegivel(
        status.as_u16(),
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        headers
            .get(header::CONTENT_ENCODING)
            .and_then(|v| v.to_str().ok()),
        headers
            .get(header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok()),
    )
}

/// Elegibilidade pura (04): status 200 + content-type `text/html` + SEM
/// `content-encoding` + `content-length` presente e < `MAX_INJECT_BYTES`.
fn elegivel(
    status: u16,
    content_type: Option<&str>,
    content_encoding: Option<&str>,
    content_length: Option<u64>,
) -> bool {
    if status != 200 {
        return false;
    }
    let Some(ct) = content_type else {
        return false;
    };
    let essencia = ct.split(';').next().unwrap_or_default().trim();
    if !essencia.eq_ignore_ascii_case("text/html") {
        return false;
    }
    if content_encoding.is_some() {
        return false;
    }
    content_length.is_some_and(|l| l < MAX_INJECT_BYTES as u64)
}

/// Costura `<script>SCRIPT</script>` antes do ÚLTIMO `</body` (caixa alta ou
/// baixa — HTML é case-insensitive); sem `</body`, concatena no fim (04).
fn injeta_script(html: &[u8]) -> Vec<u8> {
    let script = format!("<script>{}</script>", crate::beat::SCRIPT);
    let baixo = html.to_ascii_lowercase();
    match baixo.windows(6).rposition(|w| w == b"</body") {
        Some(i) => {
            let mut out = Vec::with_capacity(html.len() + script.len());
            out.extend_from_slice(&html[..i]);
            out.extend_from_slice(script.as_bytes());
            out.extend_from_slice(&html[i..]);
            out
        }
        None => {
            let mut out = html.to_vec();
            out.extend_from_slice(script.as_bytes());
            out
        }
    }
}

/// Encaminha a requisição para `target` (`host:porta` do container) mantendo
/// path/query. Erro volta para o router reavaliar estado — nunca 502 cru
/// (matriz 03).
pub async fn forward(
    client: &ProxyClient,
    mut parts: Parts,
    body: Body,
    target: &str,
) -> Result<Response, String> {
    let pq = parts
        .uri
        .path_and_query()
        .map(|p| p.as_str().to_owned())
        .unwrap_or_else(|| "/".to_owned());
    let uri = Uri::builder()
        .scheme("http")
        .authority(target)
        .path_and_query(pq)
        .build()
        .map_err(|e| format!("alvo de proxy inválido {target:?}: {e}"))?;

    strip_hop_by_hop(&mut parts.headers);
    parts.headers.insert(
        header::HOST,
        HeaderValue::from_str(target)
            .map_err(|e| format!("alvo de proxy inválido {target:?}: {e}"))?,
    );

    let mut builder = Request::builder().method(parts.method.clone()).uri(uri);
    if let Some(h) = builder.headers_mut() {
        *h = std::mem::take(&mut parts.headers);
    }
    let req = builder
        .body(body)
        .map_err(|e| format!("montagem do request para {target:?}: {e}"))?;

    let resp: HttpResponse<hyper::body::Incoming> = client
        .request(req)
        .await
        .map_err(|e| format!("upstream {target}: {e}"))?;
    let (mut p2, b2) = resp.into_parts();
    strip_hop_by_hop(&mut p2.headers);

    // Injeção do overlay de sono (04): SÓ na resposta elegível — bufferiza
    // (teto garantido pelo content-length já checado), costura o script e
    // regrava o content-length com o tamanho real. Falha no MEIO do corpo
    // elegível vira Err (o router reavalia) — nunca devolve HTML trincado.
    if INJECT_BEAT && resposta_elegivel(p2.status, &p2.headers) {
        return match b2.collect().await {
            Ok(coletado) => {
                let injetado = injeta_script(&coletado.to_bytes());
                p2.headers.insert(
                    header::CONTENT_LENGTH,
                    HeaderValue::from(injetado.len() as u64),
                );
                Ok(Response::from_parts(p2, Body::from(injetado)))
            }
            Err(e) => Err(format!(
                "upstream {target}: resposta elegível à injeção falhou no meio do corpo: {e}"
            )),
        };
    }

    Ok(Response::from_parts(p2, Body::new(b2)))
}

/// Encaminha um pedido de UPGRADE (websocket do HMR, 06/live) para `target`.
/// Diferença do `forward`: `strip_hop_by_hop` NÃO passa — `Connection` e
/// `Upgrade` SÃO o pedido. Chegando 101 do upstream, devolve o 101 para o
/// servidor do fluxd e spawna o túnel bruto (`copy_bidirectional`) entre as
/// duas pontas: depois do upgrade não existe HTTP, é byte a byte nos dois
/// sentidos (o cliente precisa ter sido construído com `upgrade(true)`).
/// `down` é o `OnUpgrade` extraído das extensions do request do browser —
/// retirado ANTES do `into_parts`, porque hyper registra nele.
pub async fn forward_ws(
    client: &ProxyClient,
    mut parts: Parts,
    body: Body,
    target: &str,
    down: Option<hyper::upgrade::OnUpgrade>,
) -> Result<Response, String> {
    let pq = parts
        .uri
        .path_and_query()
        .map(|p| p.as_str().to_owned())
        .unwrap_or_else(|| "/".to_owned());
    let uri = Uri::builder()
        .scheme("http")
        .authority(target)
        .path_and_query(pq)
        .build()
        .map_err(|e| format!("alvo de ws inválido {target:?}: {e}"))?;

    parts.headers.insert(
        header::HOST,
        HeaderValue::from_str(target)
            .map_err(|e| format!("alvo de ws inválido {target:?}: {e}"))?,
    );

    let mut builder = Request::builder().method(parts.method.clone()).uri(uri);
    if let Some(h) = builder.headers_mut() {
        *h = std::mem::take(&mut parts.headers);
    }
    let req = builder
        .body(body)
        .map_err(|e| format!("montagem do ws para {target:?}: {e}"))?;

    let resp: HttpResponse<hyper::body::Incoming> = client
        .request(req)
        .await
        .map_err(|e| format!("upstream ws {target}: {e}"))?;
    let (mut p2, b2) = resp.into_parts();

    // Sem 101 o dev server recusou o upgrade (caiu no HTTP normal): devolve
    // a resposta como qualquer outra — o router nunca finge que tunelou.
    if p2.status != StatusCode::SWITCHING_PROTOCOLS {
        strip_hop_by_hop(&mut p2.headers);
        return Ok(Response::from_parts(p2, Body::new(b2)));
    }

    let up = p2.extensions.remove::<hyper::upgrade::OnUpgrade>();
    let resp = Response::from_parts(p2, Body::empty());
    match (down, up) {
        (Some(down), Some(up)) => {
            tokio::spawn(async move {
                match tokio::try_join!(down, up) {
                    Ok((browser, upstream)) => {
                        // hyper 1.x divide os traits de IO: TokioIo adapta
                        // `Upgraded` para o copy_bidirectional do tokio.
                        let (mut browser, mut upstream) =
                            (TokioIo::new(browser), TokioIo::new(upstream));
                        if let Err(e) =
                            tokio::io::copy_bidirectional(&mut browser, &mut upstream).await
                        {
                            tracing::debug!("túnel ws encerrado: {e}");
                        }
                    }
                    Err(e) => tracing::warn!("upgrade ws falhou nas pontas: {e}"),
                }
            });
        }
        (down, up) => tracing::warn!(
            target,
            "101 sem ponta de upgrade: browser={} upstream={}",
            down.is_some(),
            up.is_some()
        ),
    }
    Ok(resp)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::Bytes;
    use axum::response::Html;
    use axum::routing::get;
    use flux_common::config::GlobalConfig;
    use std::sync::Arc;

    use crate::AppState;
    use crate::state::State;

    // ------------------------------------------------ elegibilidade (04)

    #[test]
    fn matriz_de_elegibilidade_status_tipo_encoding_tamanho() {
        // Elegível: 200 + text/html (com charset) + sem encoding + length < teto.
        assert!(elegivel(
            200,
            Some("text/html; charset=utf-8"),
            None,
            Some(1000)
        ));
        // Status: só 200.
        assert!(!elegivel(204, Some("text/html"), None, Some(1000)));
        assert!(!elegivel(301, Some("text/html"), None, Some(1000)));
        assert!(!elegivel(304, Some("text/html"), None, Some(1000)));
        assert!(!elegivel(404, Some("text/html"), None, Some(1000)));
        assert!(!elegivel(500, Some("text/html"), None, Some(1000)));
        // Content-type errado (ou ausente): fora — xhtml não é text/html.
        assert!(!elegivel(200, Some("application/json"), None, Some(1000)));
        assert!(!elegivel(
            200,
            Some("application/xhtml+xml"),
            None,
            Some(1000)
        ));
        assert!(!elegivel(200, Some("text/plain"), None, Some(1000)));
        assert!(!elegivel(200, None, None, Some(1000)));
        // Comprimida NUNCA (04: jamais corromper corpo).
        assert!(!elegivel(200, Some("text/html"), Some("gzip"), Some(1000)));
        assert!(!elegivel(200, Some("text/html"), Some("br"), Some(1000)));
        // Sem content-length (chunked/streaming) NUNCA — streaming preservado.
        assert!(!elegivel(200, Some("text/html"), None, None));
        // Tamanho: < 256 KB; exatamente 256 KB passa FORA ("< 256 KB").
        assert!(elegivel(
            200,
            Some("text/html"),
            None,
            Some(MAX_INJECT_BYTES as u64 - 1)
        ));
        assert!(!elegivel(
            200,
            Some("text/html"),
            None,
            Some(MAX_INJECT_BYTES as u64)
        ));
        // Length zero é elegível (sem </body> = cola no fim).
        assert!(elegivel(200, Some("text/html"), None, Some(0)));
    }

    #[test]
    fn header_mentiroso_de_content_length_nao_e_elegivel() {
        let mut h = HeaderMap::new();
        h.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html"));
        h.insert(header::CONTENT_LENGTH, HeaderValue::from_static("muito"));
        assert!(
            !resposta_elegivel(StatusCode::OK, &h),
            "content-length não numérico = fora, corpo passa intacto"
        );
    }

    // ------------------------------------------------ injeção no corpo

    #[test]
    fn injecao_antes_do_ultimo_body() {
        let out = injeta_script(b"<html><body><p>x</p></body></html>");
        let out = String::from_utf8(out).unwrap();
        let script = format!("<script>{}</script>", crate::beat::SCRIPT);
        assert_eq!(
            out,
            format!("<html><body><p>x</p>{script}</body></html>"),
            "script tem que entrar ANTES de </body>"
        );
    }

    #[test]
    fn injecao_acha_body_em_caixa_alta() {
        let out = String::from_utf8(injeta_script(b"<HTML><BODY>ok</BODY></HTML>")).unwrap();
        let script = format!("<script>{}</script>", crate::beat::SCRIPT);
        assert!(
            out.contains(&script) && out.find("<script>").unwrap() < out.rfind("</BODY>").unwrap(),
            "</BODY> maiúsculo também recebe antes: {out}"
        );
    }

    #[test]
    fn injecao_sem_body_concatena_no_fim() {
        let out = String::from_utf8(injeta_script(b"<html>sem fechamento</html>")).unwrap();
        let script = format!("<script>{}</script>", crate::beat::SCRIPT);
        assert!(
            out.ends_with(&script),
            "sem </body> o script cola no fim do corpo: {out}"
        );
    }

    #[test]
    fn injecao_em_corpo_vazio_coloca_s_o_script() {
        let out = String::from_utf8(injeta_script(b"")).unwrap();
        assert_eq!(
            out,
            format!("<script>{}</script>", crate::beat::SCRIPT),
            "corpo vazio elegível vira só o script"
        );
    }

    // ------------------------------------------ forward de verdade (e2e)

    /// Servidor efêmero no loopback com as três superfícies da matriz:
    /// `/` = HTML com content-length (elegível), `/stream` = HTML chunked sem
    /// content-length (streaming intocado), `/json` = content-type errado.
    async fn servidor() -> String {
        async fn stream_html() -> axum::http::Response<Body> {
            let s = futures_util::stream::iter(vec![
                Ok::<Bytes, std::convert::Infallible>(Bytes::from("<html><body>")),
                Ok(Bytes::from("sem length")),
            ]);
            axum::http::Response::builder()
                .header(header::CONTENT_TYPE, "text/html")
                .body(Body::from_stream(s))
                .unwrap()
        }
        let router = Router::new()
            .route(
                "/",
                get(|| async { Html("<html><body><p>oi</p></body></html>") }),
            )
            .route("/stream", get(|| async { stream_html().await }))
            .route(
                "/json",
                get(|| async { axum::Json(serde_json::json!({"ok": true})) }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        addr.to_string()
    }

    async fn pedir(
        app: &Arc<AppState>,
        target: &str,
        path: &str,
    ) -> (StatusCode, HeaderMap, Vec<u8>) {
        let (parts, _) = Request::builder()
            .uri(format!("http://upstream{path}"))
            .body(Body::empty())
            .unwrap()
            .into_parts();
        let resp = forward(&app.http, parts, Body::empty(), target)
            .await
            .unwrap();
        let status = resp.status();
        let headers = resp.headers().clone();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (status, headers, bytes.to_vec())
    }

    #[tokio::test]
    async fn forward_ws_sem_101_no_upstream_passa_http_normal_sem_injecao() {
        // O dev server recusou o upgrade e respondeu HTTP comum: a resposta
        // volta pelo caminho normal (o router nunca finge que tunelou) — e
        // o ws NUNCA injeta o overlay de sono, mesmo em HTML elegível.
        let app = Arc::new(AppState::new(
            State::in_memory().unwrap(),
            &GlobalConfig::default(),
            None,
        ));
        let target = servidor().await;
        let (parts, _) = Request::builder()
            .uri("http://upstream/")
            .header(header::CONNECTION, "Upgrade")
            .header(header::UPGRADE, "websocket")
            .body(Body::empty())
            .unwrap()
            .into_parts();
        let resp = forward_ws(&app.http, parts, Body::empty(), &target, None)
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let out = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(out.contains("oi"), "corpo do upstream passa: {out}");
        assert!(
            !out.contains(&crate::beat::SCRIPT),
            "forward_ws não injeta overlay: {out}"
        );
    }

    #[tokio::test]
    async fn forward_injeta_em_elegivel_e_ajusta_content_length() {
        let app = Arc::new(AppState::new(
            State::in_memory().unwrap(),
            &GlobalConfig::default(),
            None,
        ));
        let target = servidor().await;
        let (status, headers, corpo) = pedir(&app, &target, "/").await;
        assert_eq!(status, StatusCode::OK);
        let out = String::from_utf8(corpo).unwrap();
        let script = format!("<script>{}</script>", crate::beat::SCRIPT);
        assert_eq!(
            out,
            format!("<html><body><p>oi</p>{script}</body></html>"),
            "forward elegível tem que receber o overlay antes de </body>"
        );
        let cl: usize = headers
            .get(header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        assert_eq!(
            cl,
            out.len(),
            "content-length tem que conferir com o corpo injetado ({cl} vs {})",
            out.len()
        );
    }

    #[tokio::test]
    async fn forward_streaming_e_content_type_errado_passam_intactos() {
        let app = Arc::new(AppState::new(
            State::in_memory().unwrap(),
            &GlobalConfig::default(),
            None,
        ));
        let target = servidor().await;

        // Chunked sem content-length: streaming preservado, SEM injeção.
        let (status, headers, corpo) = pedir(&app, &target, "/stream").await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            !String::from_utf8_lossy(&corpo).contains("<script>"),
            "sem content-length NUNCA injeta: {}",
            String::from_utf8_lossy(&corpo)
        );
        assert!(
            !headers.contains_key(header::CONTENT_LENGTH),
            "chunked não pode ganhar content-length inventado"
        );

        // Content-type errado: corpo intocado, byte a byte.
        let (_, _, corpo) = pedir(&app, &target, "/json").await;
        let out = String::from_utf8(corpo).unwrap();
        assert!(
            !out.contains("<script>") && out.contains("\"ok\":true"),
            "JSON não recebe overlay: {out}"
        );
    }
}
