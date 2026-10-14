//! Testes de integração da rota pública de deploy (10/F4): `POST
//! /api/projects/{name}/deploy` no host do painel, na montagem REAL do
//! `serve` (router catch-all com EdgeLayer R30 + WAF + painel com auth
//! fail-closed; lib.rs:120-121).
//!
//! Cenários cobertos aqui:
//! 1. Auth fail-closed — sem Bearer → 401; Bearer de OUTRO projeto → 403.
//! 2. Rate limit PRÓPRIO da rota (10/min, burst 10) — o 11º request em
//!    rajada leva `429` + `Retry-After` (a borda global R30 é 300/min com
//!    burst 100 e não interfere nessa contagem).
//! 3. Deploy concorrente do mesmo projeto → 409 (guard em memória).
//! 4. Guard de descendência do pipeline — SHA órfão (origin não conhece) é
//!    recusado; SHA empurrado prepara o release-dir imutável (git real em
//!    tempdir).
//!
//! Recortes declarados:
//! - O guard de concorrência (`EM_VOO`, deploy.rs:84) é PRIVADO do módulo —
//!   o cenário 3 exercita o handler público `deploy` com os dois futures no
//!   mesmo `tokio::join!`: corpos `Full` estão Ready no primeiro poll,
//!   nenhum handler cede ao executor antes do claim e o task de fundo do 1º
//!   (que soltaria o guard) só roda depois — 202 e 409, nessa ordem,
//!   determinístico no runtime current-thread do `#[tokio::test]`.
//! - O guard de descendência é a unidade pura de caminhos
//!   `preparar_release_em` (deploy.rs:397) com git real — não a VM
//!   (`/srv/flux/releases` não existe aqui).
//! - `sha256_hex` é `pub(crate)` (api.rs:141): o teste calcula o hash com
//!   `sha2` direto — mesmo algoritmo, hex lowercase de 64 chars.

use std::path::Path;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::Request;
use axum::http::{StatusCode, header};
use flux_common::config::GlobalConfig;
use fluxd::AppState;
use fluxd::router::build_router;
use fluxd::state::State;
use hyper::body::Bytes;
use hyper_util::client::legacy::{Client, connect::HttpConnector};
use hyper_util::rt::TokioExecutor;
use sha2::{Digest, Sha256};

type TestClient = Client<HttpConnector, http_body_util::Full<Bytes>>;

/// SHA fixo (40 hex) dos corpos — o handler só valida o FORMATO no request
/// (deploy.rs:250-256); a descendência de verdade é o guard do pipeline
/// (cenário 4).
const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

fn corpo_sha() -> String {
    format!(r#"{{"ref": "{SHA}", "actor": "teste", "run": "42"}}"#)
}

/// Mesmo formato que o `flux project token` grava e o handler compara:
/// sha256 hex lowercase de 64 chars.
fn sha256_hex(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn test_client() -> TestClient {
    Client::builder(TokioExecutor::new()).build_http()
}

/// Mesma montagem do `serve` (lib.rs): router completo sobre estado em
/// memória, sem Docker — painel COM auth (hash aponta para arquivo que não
/// existe: fail-closed).
async fn spawn_router(st: State, cfg: &GlobalConfig) -> u16 {
    let app = Arc::new(AppState::new(st, cfg, None));
    let auth = Arc::new(fluxd::panel_auth::PanelAuth::novo());
    let painel = fluxd::panel::build_panel(Arc::clone(&app), auth);
    let router = build_router(Arc::clone(&app), cfg, painel);
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(l, router).await.unwrap();
    });
    port
}

async fn post_deploy(
    client: &TestClient,
    port: u16,
    name: &str,
    token: Option<&str>,
    corpo: Option<&str>,
) -> (StatusCode, String, Option<String>) {
    let mut b = hyper::Request::builder()
        .method("POST")
        .uri(format!(
            "http://127.0.0.1:{port}/api/projects/{name}/deploy"
        ))
        .header("host", "flux.bitello.xyz");
    if let Some(t) = token {
        b = b.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    if corpo.is_some() {
        b = b.header(header::CONTENT_TYPE, "application/json");
    }
    let req = b
        .body(http_body_util::Full::new(Bytes::from(
            corpo.unwrap_or("").to_owned(),
        )))
        .unwrap();
    let resp = client.request(req).await.unwrap();
    let status = resp.status();
    let retry_after = resp
        .headers()
        .get(header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_owned());
    let bytes = http_body_util::BodyExt::collect(resp.into_body())
        .await
        .unwrap()
        .to_bytes();
    (
        status,
        String::from_utf8(bytes.to_vec()).unwrap(),
        retry_after,
    )
}

/// Auth fail-closed da rota (10/F4): sem Bearer → 401 com a rota EXISTINDO
/// (nunca 202 de graça); Bearer do projeto B no path do projeto A → 403 com
/// a causa nomeada — o hash conferido é SEMPRE o do projeto do path
/// (deploy.rs:158-229). Nomes ÚNICOS no processo: os testes do crate rodam
/// em paralelo.
#[tokio::test]
async fn auth_fail_closed_sem_token_401_e_token_de_outro_projeto_403() {
    let cfg = GlobalConfig::parse_str("[deploy]\npublic_route = true\n").unwrap();
    let st = State::in_memory().unwrap();
    st.register_project("it-proj-a", "name = \"it-proj-a\"", "h")
        .unwrap();
    st.register_project("it-proj-b", "name = \"it-proj-b\"", "h")
        .unwrap();
    st.set_deploy_token_hash("it-proj-a", &sha256_hex(b"token-do-a"))
        .unwrap();
    st.set_deploy_token_hash("it-proj-b", &sha256_hex(b"token-do-b"))
        .unwrap();
    let port = spawn_router(st, &cfg).await;
    let cli = test_client();

    let (s, corpo, _) = post_deploy(&cli, port, "it-proj-a", None, Some(&corpo_sha())).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "sem Bearer: {corpo}");
    assert!(
        corpo.contains("token de deploy ausente"),
        "401 com a causa: {corpo}"
    );

    let (s, corpo, _) = post_deploy(
        &cli,
        port,
        "it-proj-a",
        Some("token-do-b"),
        Some(&corpo_sha()),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "token do B no path do A: {corpo}");
    assert!(
        corpo.contains("OUTRO projeto"),
        "403 distingue a causa: {corpo}"
    );
}

/// Rate limit PRÓPRIO da rota, por projeto (10/F4, deploy.rs:134-139): o
/// bucket (10/min, burst 10) é consumido ANTES do auth, então os 10
/// primeiros requests sem token passam pelo limiter (e viram 401 no gate) —
/// o 11º em rajada acha o bucket seco: `429` + `Retry-After` ≥ 1 s e corpo
/// JSON `rate_limited` (o path /api é wants_json, decision.rs:234-242).
#[tokio::test]
async fn rate_limit_proprio_seca_no_11o_request_com_retry_after() {
    let cfg = GlobalConfig::parse_str("[deploy]\npublic_route = true\n").unwrap();
    let st = State::in_memory().unwrap();
    st.register_project("it-rl", "name = \"it-rl\"", "h")
        .unwrap();
    let port = spawn_router(st, &cfg).await;
    let cli = test_client();

    for i in 0..10 {
        let (s, corpo, _) = post_deploy(&cli, port, "it-rl", None, Some(&corpo_sha())).await;
        assert_eq!(
            s,
            StatusCode::UNAUTHORIZED,
            "request {i}: bucket com vaga, fecha no auth: {corpo}"
        );
    }
    let (s, corpo, retry_after) = post_deploy(&cli, port, "it-rl", None, Some(&corpo_sha())).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS, "11º request: {corpo}");
    let retry = retry_after.expect("429 da rota leva Retry-After");
    assert!(
        retry.parse::<u64>().unwrap_or(0) >= 1,
        "Retry-After aponta o segundo da recarga: {retry}"
    );
    assert!(corpo.contains("rate_limited"), "corpo JSON do 429: {corpo}");
}

/// 409 para deploy concorrente do MESMO projeto (10/F4, deploy.rs:258-265):
/// o 1º entra em voo (202) e o 2º acha o guard ocupado (409 com causa).
/// Recorte: o guard (`EM_VOO`) é privado — exercitado pelo handler público
/// com os dois futures no mesmo poll (ver doc do módulo). Nome ÚNICO no
/// processo: os testes do crate dividem o `EM_VOO` estático.
#[tokio::test]
async fn deploy_concorrente_do_mesmo_projeto_e_409() {
    let st = State::in_memory().unwrap();
    st.register_project("it-conc", "name = \"it-conc\"", "h")
        .unwrap();
    st.set_deploy_token_hash("it-conc", &sha256_hex(b"segredo"))
        .unwrap();
    let app = Arc::new(AppState::new(st, &GlobalConfig::default(), None));

    let pedido = || {
        Request::builder()
            .method("POST")
            .uri("http://flux.bitello.xyz/api/projects/it-conc/deploy")
            .header(header::HOST, "flux.bitello.xyz")
            .header(header::AUTHORIZATION, "Bearer segredo")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(corpo_sha()))
            .unwrap()
    };

    let (primeiro, segundo) = tokio::join!(
        fluxd::deploy::deploy(Arc::clone(&app), "it-conc".to_owned(), pedido()),
        fluxd::deploy::deploy(Arc::clone(&app), "it-conc".to_owned(), pedido()),
    );
    assert_eq!(primeiro.status(), StatusCode::ACCEPTED, "1º entra em voo");
    assert_eq!(
        segundo.status(),
        StatusCode::CONFLICT,
        "2º do mesmo projeto: 409"
    );
    let bytes = http_body_util::BodyExt::collect(segundo.into_body())
        .await
        .unwrap()
        .to_bytes();
    let corpo = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(corpo.contains("em andamento"), "409 com a causa: {corpo}");
}

// ------------------------------------------------- guard do pipeline (git real)

fn git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .expect("git disponível no PATH do ambiente de teste");
    assert!(
        out.status.success(),
        "git {args:?} falhou: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// Guard de descendência do pipeline (10, cenário B): commit ÓRFÃO (existe
/// só no clone; origin não conhece) é recusado com a causa; commit EMPURRADO
/// prepara o release-dir `{root}/{name}/{sha}` via `git worktree` com o
/// conteúdo do SHA; e o dir é IMUTÁVEL — mesmo SHA de novo não reescreve
/// (deploy.rs:397-451). Git REAL em tempdir: a unidade recebe os caminhos de
/// fora (na VM: repo sincronizado da 02 + `/srv/flux/releases`).
#[tokio::test]
async fn sha_orfao_recusado_e_sha_empurrado_prepara_release_imutavel() {
    let tmp = tempfile::tempdir().unwrap();
    let origin = tmp.path().join("origin.git");
    let clone = tmp.path().join("clone");

    git(tmp.path(), &["init", "--bare", origin.to_str().unwrap()]);
    git(tmp.path(), &["init", clone.to_str().unwrap()]);
    std::fs::write(clone.join("f.txt"), "conteudo").unwrap();
    git(&clone, &["add", "f.txt"]);
    git(&clone, &["commit", "-m", "empurrado"]);
    git(
        &clone,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    git(&clone, &["push", "origin", "HEAD"]);
    let sha_bom = git(&clone, &["rev-parse", "HEAD"]);

    // Órfão: commit vazio SÓ no clone, nunca enviado a origin.
    git(&clone, &["commit", "--allow-empty", "-m", "orfao"]);
    let sha_orfao = git(&clone, &["rev-parse", "HEAD"]);
    assert_ne!(sha_bom, sha_orfao);

    let releases = tmp.path().join("releases");

    let err = fluxd::deploy::preparar_release_em(&clone, &releases, "it-deploy", &sha_orfao)
        .await
        .expect_err("SHA órfão tem que ser recusado");
    assert!(err.contains("órfão"), "erro nomeia a causa: {err}");

    let dir = fluxd::deploy::preparar_release_em(&clone, &releases, "it-deploy", &sha_bom)
        .await
        .expect("SHA empurrado passa pelo guard");
    assert!(dir.is_dir(), "release-dir {} criado", dir.display());
    assert_eq!(
        std::fs::read_to_string(dir.join("f.txt")).unwrap(),
        "conteudo",
        "checkout do SHA certo no release-dir"
    );

    let err2 = fluxd::deploy::preparar_release_em(&clone, &releases, "it-deploy", &sha_bom)
        .await
        .expect_err("release-dir é imutável");
    assert!(err2.contains("IMUTÁVEL"), "erro: {err2}");
}
