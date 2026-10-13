//! Testes de integração do router: sobe um upstream dummy na loopback, aponta
//! o `proxy_target` no SQLite e verifica a cadeia completa decisão → proxy →
//! página gerada — sem VM, sem Docker (fundação testável no Windows).

use std::sync::Arc;

use axum::Router;
use axum::http::{Request, StatusCode};
use axum::routing::get as axum_get;
use flux_common::config::GlobalConfig;
use flux_common::protocol::{ContainerState, Env};
use fluxd::AppState;
use fluxd::router::build_router;
use fluxd::state::State;
use hyper_util::client::legacy::{Client, connect::HttpConnector};
use hyper_util::rt::TokioExecutor;

type TestClient = Client<HttpConnector, http_body_util::Empty<axum::body::Bytes>>;

fn test_client() -> TestClient {
    Client::builder(TokioExecutor::new()).build_http()
}

async fn spawn_upstream() -> u16 {
    let app = Router::new().route("/", axum_get(|| async { "olá do upstream" }));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(l, app).await.unwrap();
    });
    port
}

async fn spawn_router(st: State) -> u16 {
    let app = Arc::new(AppState::new(st, &GlobalConfig::default(), None));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(l, build_router(app, &GlobalConfig::default()))
            .await
            .unwrap();
    });
    port
}

async fn get(
    client: &TestClient,
    port: u16,
    host: &str,
    path: &str,
) -> (StatusCode, String, Vec<(String, String)>) {
    let uri = format!("http://127.0.0.1:{port}{path}");
    let req = Request::builder()
        .uri(&uri)
        .header("host", host)
        .body(http_body_util::Empty::<axum::body::Bytes>::new())
        .unwrap();
    let resp = client.request(req).await.unwrap();
    let status = resp.status();
    let headers: Vec<(String, String)> = resp
        .headers()
        .iter()
        .map(|(k, v)| (k.as_str().to_owned(), v.to_str().unwrap().to_owned()))
        .collect();
    let body = http_body_util::BodyExt::collect(resp.into_body())
        .await
        .unwrap()
        .to_bytes();
    (status, String::from_utf8(body.to_vec()).unwrap(), headers)
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}

#[tokio::test]
async fn proxy_streaming_para_container_acordado_com_noindex_em_dev() {
    let upstream_port = spawn_upstream().await;
    let st = State::in_memory().unwrap();
    st.register_project("vipers", "name = \"vipers\"", "h")
        .unwrap();
    st.set_env_state("vipers", Env::Dev, ContainerState::Awake)
        .unwrap();
    st.set_proxy_target("vipers", Env::Dev, &format!("127.0.0.1:{upstream_port}"))
        .unwrap();

    let port = spawn_router(st).await;
    let client = test_client();
    let (status, body, headers) = get(&client, port, "vipers-d.bitello.xyz", "/").await;

    assert_eq!(status, StatusCode::OK, "corpo: {body}");
    assert_eq!(body, "olá do upstream");
    assert_eq!(
        header(&headers, "x-robots-tag"),
        Some("noindex, nofollow, noarchive"),
        "toda resposta de dev leva noindex (R22)"
    );
}

#[tokio::test]
async fn host_desconhecido_recebe_404_brandiado() {
    let st = State::in_memory().unwrap();
    let port = spawn_router(st).await;
    let client = test_client();
    let (status, body, _) = get(&client, port, "zzz.bitello.xyz", "/").await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body.contains("Host desconhecido"), "corpo: {body}");
}

#[tokio::test]
async fn dev_dormindo_recebe_pagina_de_espera_200() {
    let st = State::in_memory().unwrap();
    st.register_project("pedi", "name = \"pedi\"", "h").unwrap();
    st.set_env_state("pedi", Env::Dev, ContainerState::Sleeping)
        .unwrap();

    let port = spawn_router(st).await;
    let client = test_client();
    let (status, body, headers) = get(&client, port, "pedi-d.bitello.xyz", "/").await;

    assert_eq!(status, StatusCode::OK, "nunca 5xx cru para dev dormindo");
    assert!(body.contains("http-equiv=\"refresh\""), "corpo: {body}");
    assert!(body.contains("Acordando pedi"), "corpo: {body}");
    assert_eq!(header(&headers, "cache-control"), Some("no-store"));
}

#[tokio::test]
async fn api_dormindo_recebe_503_json_com_retry_after() {
    let st = State::in_memory().unwrap();
    st.register_project("pedi", "name = \"pedi\"", "h").unwrap();
    st.set_env_state("pedi", Env::Dev, ContainerState::Sleeping)
        .unwrap();

    let port = spawn_router(st).await;
    let client = test_client();
    let (status, body, headers) = get(&client, port, "pedi-d.bitello.xyz", "/api/fila").await;

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "corpo: {body}");
    assert_eq!(
        body, r#"{"error":"service_waking","retry_after":30}"#,
        "contrato do corpo JSON (12)"
    );
    assert_eq!(header(&headers, "retry-after"), Some("30"));
}

#[tokio::test]
async fn robots_em_dev_e_gerado_disallow_r22() {
    let st = State::in_memory().unwrap();
    st.register_project("pedi", "name = \"pedi\"", "h").unwrap();

    let port = spawn_router(st).await;
    let client = test_client();
    let (status, body, _) = get(&client, port, "pedi-d.bitello.xyz", "/robots.txt").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "User-agent: *\nDisallow: /\n");
}

#[tokio::test]
async fn proxy_em_prod_passa_sem_noindex() {
    let upstream_port = spawn_upstream().await;
    let st = State::in_memory().unwrap();
    st.register_project("vipers", "name = \"vipers\"", "h")
        .unwrap();
    st.set_env_state("vipers", Env::Prod, ContainerState::Awake)
        .unwrap();
    st.set_proxy_target("vipers", Env::Prod, &format!("127.0.0.1:{upstream_port}"))
        .unwrap();

    let port = spawn_router(st).await;
    let client = test_client();
    let (status, body, headers) = get(&client, port, "vipers.bitello.xyz", "/").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "olá do upstream");
    assert!(
        header(&headers, "x-robots-tag").is_none(),
        "prod NÃO leva noindex — resposta do app passa intocada"
    );
}

#[tokio::test]
async fn healthz_da_api_de_controle_responde() {
    let st = State::in_memory().unwrap();
    let app = Arc::new(AppState::new(st, &GlobalConfig::default(), None));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(l, fluxd::api::build_api(app)).await.unwrap();
    });

    let client = test_client();
    let req = Request::builder()
        .uri(format!("http://127.0.0.1:{port}/healthz"))
        .body(http_body_util::Empty::<axum::body::Bytes>::new())
        .unwrap();
    let resp = client.request(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = http_body_util::BodyExt::collect(resp.into_body())
        .await
        .unwrap()
        .to_bytes();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(text.contains("\"status\":\"ok\""), "corpo: {text}");
}
