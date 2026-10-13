//! Proxy L7 streaming (07): request e response SEM buffering — SSE, upload
//! grande e upgrade passam desde o primeiro byte (o porteiro bufferizava;
//! erro que o Flux não repete). Websocket upgrade completo é da fase F3.

use axum::body::Body;
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, Request, Response as HttpResponse, Uri, header};
use axum::response::Response;
use hyper_util::client::legacy::{Client, connect::HttpConnector};
use hyper_util::rt::TokioExecutor;

/// Cliente HTTP do proxy — um só, reutilizado (pool de conexões do hyper).
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
    Ok(Response::from_parts(p2, Body::new(b2)))
}
