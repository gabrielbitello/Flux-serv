//! Testes do painel (L6.3): movidos INTEIROS do panel.rs — são a
//! caracterização (23 testes; corpos intocados).

use std::sync::Arc;

use super::formato::{badge_sync, epoch_de, esc, fmt_utc, page_de, quando};
use super::paginas::CSS_TAIL;
use super::*;
use argon2::Argon2;
use argon2::password_hash::{PasswordHasher, SaltString};
use axum::body::Body;
use axum::http::{StatusCode, header};
use flux_common::config::GlobalConfig;
use flux_common::protocol::{ContainerState, Env};
use http_body_util::BodyExt;

const SENHA_DE_TESTE: &str = "senha-de-teste";

fn app_sem_docker() -> Arc<AppState> {
    let st = crate::state::State::in_memory().unwrap();
    Arc::new(AppState::new(st, &GlobalConfig::default(), None))
}

/// Router do painel COM auth de verdade: hash Argon2id de
/// `SENHA_DE_TESTE` em tempdir (um PanelAuth novo por chamada — sessões
/// e limiter zerados entre testes). O `_dir` tem que sobreviver à chamada.
async fn painel_com_auth(app: &Arc<AppState>) -> (Router, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let salt = SaltString::generate(&mut rand_core::OsRng);
    let hash = Argon2::default()
        .hash_password(SENHA_DE_TESTE.as_bytes(), &salt)
        .unwrap()
        .to_string();
    std::fs::write(dir.path().join("panel.argon2"), &hash).unwrap();
    let auth = Arc::new(panel_auth::PanelAuth::with_hash_path(
        dir.path().join("panel.argon2"),
    ));
    (build_panel(Arc::clone(app), auth), dir)
}

async fn pedir(
    app: &Arc<AppState>,
    router: Router,
    method: &str,
    uri: &str,
    cookie: Option<&str>,
    corpo: Option<String>,
) -> (StatusCode, axum::http::HeaderMap, String) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let url: hyper::Uri = format!("http://{addr}{uri}").parse().unwrap();
    let mut builder = axum::http::Request::builder()
        .method(method)
        .uri(url)
        .header(header::HOST, "flux.bitello.xyz");
    if let Some(c) = cookie {
        builder = builder.header(header::COOKIE, c);
    }
    let req = match corpo {
        Some(b) => builder
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(b))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };
    let resp = app.http.request(req).await.unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let body = String::from_utf8(
        resp.into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap();
    (status, headers, body)
}

/// GET de verdade: server efêmero + cliente hyper do próprio AppState —
/// loga sozinho (POST /login) e leva o cookie da sessão.
async fn get(app: &Arc<AppState>, uri: &str) -> (StatusCode, axum::http::HeaderMap, String) {
    let (router, _dir) = painel_com_auth(app).await;
    let (_, headers, _) = pedir(
        app,
        router.clone(),
        "POST",
        "/login",
        None,
        Some(format!("password={SENHA_DE_TESTE}")),
    )
    .await;
    let cookie = headers
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .expect("login de teste tem que devolver Set-Cookie")
        .to_owned();
    let cookie = cookie.split(';').next().unwrap().to_owned();
    pedir(app, router, "GET", uri, Some(&cookie), None).await
}

/// POST de verdade: server efêmero + cliente hyper, loga sozinho e faz o
/// POST com o cookie da sessão (ações do painel: rescan, logout).
async fn post_logado(
    app: &Arc<AppState>,
    uri: &str,
) -> (StatusCode, axum::http::HeaderMap, String) {
    let (router, _dir) = painel_com_auth(app).await;
    let (_, headers, _) = pedir(
        app,
        router.clone(),
        "POST",
        "/login",
        None,
        Some(format!("password={SENHA_DE_TESTE}")),
    )
    .await;
    let cookie = headers
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .expect("login de teste tem que devolver Set-Cookie")
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    pedir(app, router, "POST", uri, Some(&cookie), None).await
}

fn noindex_de(headers: &axum::http::HeaderMap) -> String {
    headers
        .get("x-robots-tag")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned()
}

// ------------------------------------------------------------- helpers

/// L4.4: a composição base+tail nunca perde tokens do contrato.
#[test]
fn css_admin_mais_tail_tem_tokens_compartilhados_e_exclusivos() {
    let css = format!("{}\n{}", crate::ui::CSS_ADMIN, CSS_TAIL);
    assert!(css.contains("--accent:#2563eb"), "token compartilhado");
    assert!(
        css.contains("--attnbg:#ffedd5"),
        "token exclusivo do painel"
    );
    assert!(css.contains(".sb-ok{"), "badge de sync (tail)");
}

#[test]
fn esc_nao_deixa_html_passar() {
    assert_eq!(
        esc(r#"<script>alert("x")</script>& "#),
        "&lt;script&gt;alert(&quot;x&quot;)&lt;/script&gt;&amp; "
    );
}

#[test]
fn page_de_parseia_clampa_e_ignora_lixo() {
    assert_eq!(page_de(None), 1);
    assert_eq!(page_de(Some("page=3")), 3);
    assert_eq!(page_de(Some("foo=9&page=2")), 2);
    assert_eq!(page_de(Some("page=0")), 1);
    assert_eq!(page_de(Some("page=-5")), 1);
    assert_eq!(page_de(Some("page=abc")), 1);
}

#[test]
fn fmt_utc_bate_com_epocas_conhecidas() {
    assert_eq!(fmt_utc(0), "1970-01-01 00:00:00");
    assert_eq!(fmt_utc(1_000_000_000), "2001-09-09 01:46:40");
    // 29 de fevereiro (ano bissexto) — a parte civil do algoritmo.
    assert_eq!(fmt_utc(951_782_400), "2000-02-29 00:00:00");
}

#[test]
fn quando_relabs_com_none_vira_traco() {
    assert_eq!(quando(1_000, None), "—");
    assert!(quando(1_000, Some(995)).contains("há 5 s"));
    assert!(quando(1_000, Some(940)).contains("há 1 min"));
    assert!(quando(1_000, Some(1_000 - 600)).contains("há 10 min"));
    // Fronteira: exatamente 1 h arredonda para horas.
    assert!(quando(1_000, Some(1_000 - 3_600)).contains("há 1 h"));
    assert!(quando(1_000, Some(1_000 + 1_800)).contains("em 30 min"));
    assert!(quando(1_000, Some(2_000)).contains("em 16 min"));
}

// ---------------------------------------------------------------- sync

#[test]
fn epoch_de_e_a_inversa_de_fmt_utc() {
    for e in [0i64, 1_000_000_000, 951_782_400, 1_760_000_000] {
        assert_eq!(epoch_de(&fmt_utc(e)), Some(e), "roundtrip de {e}");
    }
    // Separador 'T' (RFC 3339-ish) vale o mesmo que o espaço do SQLite.
    assert_eq!(
        epoch_de("2026-10-14T12:00:00"),
        epoch_de("2026-10-14 12:00:00")
    );
    // Formato REAL do `updated_at` do sync.rs: RFC3339 com 'Z' no fim.
    assert_eq!(
        epoch_de("2026-10-14T12:00:00Z"),
        epoch_de("2026-10-14 12:00:00")
    );
    // Sem segundos também parseia (o ':00' é assumido).
    assert_eq!(epoch_de("1970-01-01 00:00"), Some(0));
    // Lixo e relógio civil inválido NUNCA viram hora.
    assert_eq!(epoch_de("lixo"), None);
    assert_eq!(epoch_de("2026-13-01 00:00:00"), None, "mês 13 não existe");
    assert_eq!(epoch_de("2026-10-14 25:00:00"), None, "hora 25 não existe");
}

/// Matriz do badge de sync: watching=verde, absent=cinza (sem sessão),
/// paused=azul, trabalho=amarelo, erro/pendências=laranja (sobrepõe),
/// sem linha=cinza.
#[test]
fn badge_sync_segue_a_regra_do_status_digerido() {
    let row =
        |status: &str, paused: bool, problems: u64, erro: Option<&str>| crate::sync::SyncRow {
            project: "p".into(),
            status: status.into(),
            paused,
            files: None,
            problems,
            last_error: erro.map(str::to_owned),
            updated_at: String::new(),
            rescan_requested: false,
        };
    assert_eq!(badge_sync(None), ("sb-idle", "sem sessão"));
    assert_eq!(
        badge_sync(Some(&row("watching", false, 0, None))),
        ("sb-ok", "saudável")
    );
    // "absent" do contrato (watch: projeto sem sessão local) = cinza de
    // sem sessão — NUNCA o amarelo de trabalho.
    assert_eq!(
        badge_sync(Some(&row("absent", false, 0, None))),
        ("sb-idle", "sem sessão")
    );
    assert_eq!(
        badge_sync(Some(&row("syncing", false, 0, None))),
        ("sb-warn", "trabalhando")
    );
    assert_eq!(
        badge_sync(Some(&row("scanning", false, 0, None))),
        ("sb-warn", "trabalhando")
    );
    // Status cru (não digerido) sem sinais = classe de trabalho.
    assert_eq!(
        badge_sync(Some(&row("algo-estranho", false, 0, None))),
        ("sb-warn", "trabalhando")
    );
    assert_eq!(
        badge_sync(Some(&row("haltered", true, 0, None))),
        ("sb-info", "pausada")
    );
    // Erro ou pendência sobe para atenção — mesmo com watching.
    assert_eq!(
        badge_sync(Some(&row("watching", false, 2, None))),
        ("sb-attn", "atenção")
    );
    assert_eq!(
        badge_sync(Some(&row("watching", false, 0, Some("conflict")))),
        ("sb-attn", "atenção")
    );
}

/// Card sem sessão de sync é honesto ("sem sessão", cinza) e traz o form
/// de rescan apontando para o POST certo.
/// Dashboard (fatia 5): shell estática + snapshot via API — projeto sem
/// linha de sync vira `"sync":null` (o cliente mostra "sem sessão"; o
/// painel não inventa sessão).
#[tokio::test]
async fn dashboard_shell_e_snapshot_sem_sync() {
    let app = app_sem_docker();
    app.state.register_project("vipers", "", "h").unwrap();
    let (status, _, shell) = get(&app, "/").await;
    assert_eq!(status, StatusCode::OK);
    assert!(shell.contains("dashboardPage"), "componente: {shell}");
    let (_, _, js) = get(&app, "/assets/painel.js").await;
    assert!(js.contains("/api/dashboard/follow"), "SSE no cliente: {js}");
    let (router, _dir) = painel_com_auth(&app).await;
    let cookie = cookie_de_sessao(&app, &router).await;
    let (s2, snap) = pedir_api(
        &app,
        router,
        "/api/dashboard",
        Some(&cookie),
        &[("x-forwarded-for", "192.168.140.50".into())],
    )
    .await;
    assert_eq!(s2, StatusCode::OK, "{snap}");
    assert!(snap.contains("\"nome\":\"vipers\""), "{snap}");
    assert!(snap.contains("\"sync\":null"), "sem sync é null: {snap}");
}

/// Aviso do rescan (`/?rescan=1` e `/?rescan=falha`) é TRATADO PELO CLIENTE
/// (shell estática) — o contrato vive no painel.js.
#[tokio::test]
async fn dashboard_aviso_de_rescan_e_do_cliente() {
    let app = app_sem_docker();
    let (_, _, js) = get(&app, "/assets/painel.js").await;
    assert!(
        js.contains("'rescan'"),
        "flag rescan tratada no cliente: {js}"
    );
    assert!(
        js.contains("'falha'"),
        "falha do rescan tem aviso próprio: {js}"
    );
}

/// Rescan é rota protegida como qualquer outra: sem sessão, 303 pro login.
#[tokio::test]
async fn rescan_exige_sessao_como_toda_rota_protegida() {
    let app = app_sem_docker();
    let (router, _dir) = painel_com_auth(&app).await;
    let (status, headers, _) =
        pedir(&app, router, "POST", "/projects/vipers/rescan", None, None).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(
        headers.get(header::LOCATION).and_then(|v| v.to_str().ok()),
        Some("/login"),
        "guard de sessão cobre o POST de rescan"
    );
}

/// Rescan de projeto não registrado (fatia 5): redirect com flag genérica
/// (nome/causa NÃO vão pra URL) e a causa REAL no evento — o painel de
/// eventos mostra, nada fingido.
#[tokio::test]
async fn rescan_de_projeto_desconhecido_flag_e_evento() {
    let app = app_sem_docker();
    let (status, headers, _) = post_logado(&app, "/projects/fantasma/rescan").await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(
        headers.get(header::LOCATION).and_then(|v| v.to_str().ok()),
        Some("/?rescan=falha"),
        "flag genérica na URL"
    );
    let (router, _dir) = painel_com_auth(&app).await;
    let cookie = cookie_de_sessao(&app, &router).await;
    let (_, evs) = pedir_api(
        &app,
        router,
        "/api/events?escopo=painel",
        Some(&cookie),
        &[("x-forwarded-for", "192.168.140.50".into())],
    )
    .await;
    assert!(
        evs.contains("rescan recusado para fantasma"),
        "causa real no evento: {evs}"
    );
    assert!(evs.contains("não registrado"), "{evs}");
}

/// Rescan de projeto registrado: post/redirect/get de volta ao dashboard
/// com a confirmação — e a flag setada aparece como "rescan pendente".
#[tokio::test]
async fn rescan_de_projeto_registrado_confirma_e_marca_pendente() {
    let app = app_sem_docker();
    app.state.register_project("vipers", "", "h").unwrap();
    let (status, headers, _) = post_logado(&app, "/projects/vipers/rescan").await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(
        headers.get(header::LOCATION).and_then(|v| v.to_str().ok()),
        Some("/?rescan=1"),
        "redirect de volta ao dashboard"
    );
    let (_, _, shell) = get(&app, "/?rescan=1").await;
    assert!(shell.contains("dashboardPage"), "shell servida: {shell}");
    let (router, _dir) = painel_com_auth(&app).await;
    let cookie = cookie_de_sessao(&app, &router).await;
    let (_, snap) = pedir_api(
        &app,
        router,
        "/api/dashboard",
        Some(&cookie),
        &[("x-forwarded-for", "192.168.140.50".into())],
    )
    .await;
    assert!(
        snap.contains("\"rescan_requested\":true"),
        "flag do request_rescan no snapshot: {snap}"
    );
}

// ---------------------------------------------------------- integração

#[tokio::test]
async fn dashboard_vazio_e_honesto_por_login_e_noindex() {
    let app = app_sem_docker();
    let (status, headers, body) = get(&app, "/").await;
    assert_eq!(status, StatusCode::OK);
    // O login do próprio harness grava eventos ("login ok" + access log),
    // então a curva TEM dados nesta cena — o vazio aqui é de PROJETOS.
    assert!(
        body.contains("Nenhum projeto registrado"),
        "estado vazio tem que ser dito: {body}"
    );
    assert!(
        !body.contains("AVISO"),
        "auth real não tem aviso de sem-auth: {body}"
    );
    assert_eq!(noindex_de(&headers), NOINDEX);
    assert_eq!(
        headers
            .get(header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default(),
        "no-store"
    );
}

#[tokio::test]
async fn dashboard_mostra_sono_migrations_e_eventos() {
    let app = app_sem_docker();
    let st = &app.state;
    st.register_project("vipers", "name = \"vipers\"", "hash1")
        .unwrap();
    st.set_env_state("vipers", Env::Dev, ContainerState::Sleeping)
        .unwrap();
    st.record_mode("vipers", Env::Prod, ContainerState::Paused)
        .unwrap();
    st.record_migration(
        "vipers",
        Env::Dev,
        "2024_01_cria_tabela.php",
        "aabbccdd11223344",
    )
    .unwrap();
    crate::events::event(
        st,
        "info",
        "access",
        "GET / 200 3ms host=vipers-d.bitello.xyz",
    );

    let (router, _dir) = painel_com_auth(&app).await;
    let cookie = cookie_de_sessao(&app, &router).await;
    let (status, snap) = pedir_api(
        &app,
        router,
        "/api/dashboard",
        Some(&cookie),
        &[("x-forwarded-for", "192.168.140.50".into())],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(snap.contains("\"nome\":\"vipers\""), "{snap}");
    assert!(
        snap.contains("\"estado\":\"sleeping\"") && snap.contains("\"estado\":\"paused\""),
        "estados de ambos ambientes: {snap}"
    );
    assert!(
        snap.contains("sweep"),
        "sem docker, a FONTE é declarada: {snap}"
    );
    assert!(
        snap.contains("\"aplicadas\":1"),
        "contagem de migrations no card: {snap}"
    );
    assert!(
        snap.contains("GET / 200 3ms host=vipers-d.bitello.xyz"),
        "últimos eventos: {snap}"
    );
    assert!(
        snap.contains("\"curva\":{"),
        "evento recente dentro da janela vira curva: {snap}"
    );
}

/// Detalhe (fatia 6a): 404 honesto na API para não-registrado; a SHELL é
/// sempre 200 (o cliente mostra o erro que a API mandou).
#[tokio::test]
async fn detalhe_de_projeto_inexistente_e_404_na_api() {
    let app = app_sem_docker();
    // Shell servida com sessão (o guard de página não mudou).
    let (_, _, shell) = get(&app, "/projects/naoexiste").await;
    assert!(shell.contains("projetoPage"), "shell do detalhe: {shell}");
    // 404 com a causa, na API.
    let (router, _dir) = painel_com_auth(&app).await;
    let cookie = cookie_de_sessao(&app, &router).await;
    let (s, body) = pedir_api(
        &app,
        router,
        "/api/projects/naoexiste",
        Some(&cookie),
        &[("x-forwarded-for", "192.168.140.50".into())],
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert!(body.contains("não registrado"), "{body}");
}

#[tokio::test]
async fn detalhe_mostra_sinais_de_sono_e_migrations() {
    let app = app_sem_docker();
    let st = &app.state;
    st.register_project(
        "pedi",
        "framework = \"node\"\n[health]\npath = \"/healthz\"\n",
        "h",
    )
    .unwrap();
    st.set_lease("pedi", Env::Dev, 3600).unwrap();
    st.set_cooldown("pedi", Env::Dev, 120).unwrap();
    // Interação no passado DETERMINÍSTICO: touch_interaction grava
    // datetime('now') com precisão de segundo — ler logo depois pode cair
    // no MESMO segundo e o relativo viraria "em 0 s". Gravamos -10 min.
    st.with(|c| {
        c.execute(
            "UPDATE sleep_state SET last_interaction = datetime('now', '-10 minutes')
                 WHERE project = 'pedi' AND env = 'dev'",
            [],
        )
    })
    .unwrap();
    st.record_migration("pedi", Env::Dev, "0001_init.sql", "sha_de_dev_0001")
        .unwrap();
    st.record_migration("pedi", Env::Prod, "0001_init.sql", "sha_de_prod_0001")
        .unwrap();

    let (router, _dir) = painel_com_auth(&app).await;
    let cookie = cookie_de_sessao(&app, &router).await;
    let (status, snap) = pedir_api(
        &app,
        router,
        "/api/projects/pedi",
        Some(&cookie),
        &[("x-forwarded-for", "192.168.140.50".into())],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(snap.contains("\"nome\":\"pedi\""), "{snap}");
    assert!(snap.contains("node"), "framework do flux.toml: {snap}");
    assert!(
        snap.contains("\"lease\":") && snap.contains("\"cooldown\":"),
        "sinais do sleep manager no snapshot: {snap}"
    );
    assert!(
        snap.contains("0001_init.sql") && snap.contains("sha_de_dev_0001"),
        "migrations com hash: {snap}"
    );
    assert!(
        snap.contains("pedi-d.bitello.xyz"),
        "hosts derivados: {snap}"
    );
    assert!(
        snap.contains("sweep"),
        "sem docker, a FONTE é declarada: {snap}"
    );
}

/// Paginação (fatia 4): a verdade migrou da página HTML pro /api/events —
/// mesmo contrato (50/página, mais novo primeiro, clamp além do fim).
#[tokio::test]
async fn eventos_paginam_pela_api_e_clampam() {
    let app = app_sem_docker();
    for i in 0..120 {
        crate::events::event(&app.state, "info", "teste", &format!("evento-{i:03}"));
    }
    let (router, _dir) = painel_com_auth(&app).await;
    let cookie = cookie_de_sessao(&app, &router).await;
    // Sessão + LAN (sem header de origem externa a matriz da fatia 1 exige).
    let lan = [("x-forwarded-for", "192.168.140.50".into())];
    let (s1, p1) = pedir_api(
        &app,
        router.clone(),
        "/api/events?escopo=teste&page=1",
        Some(&cookie),
        &lan,
    )
    .await;
    assert_eq!(s1, StatusCode::OK);
    assert!(p1.contains("\"total\":120"), "{p1}");
    assert!(p1.contains("\"paginas\":3"), "120/50 = 3: {p1}");
    assert!(p1.contains("evento-119"), "mais novo primeiro: {p1}");
    assert!(!p1.contains("evento-000"), "offset correto: {p1}");
    let (_, p3) = pedir_api(
        &app,
        router.clone(),
        "/api/events?escopo=teste&page=3",
        Some(&cookie),
        &lan,
    )
    .await;
    assert!(
        p3.contains("evento-000"),
        "última página tem o mais antigo: {p3}"
    );
    let (_, fora) = pedir_api(
        &app,
        router,
        "/api/events?escopo=teste&page=99",
        Some(&cookie),
        &lan,
    )
    .await;
    assert!(
        fora.contains("\"pagina\":3"),
        "página além do fim é clampada, não 500: {fora}"
    );
}

/// XSS (fatia 4): a shell é ESTÁTICA (nunca carrega mensagem) e o cliente
/// NÃO injeta HTML — mensagens entram só por x-text/textContent. O
/// innerHTML que comia o `<details>` aberto não volta em nenhuma forma.
#[tokio::test]
async fn shell_de_eventos_e_cliente_sem_html_injetado() {
    let app = app_sem_docker();
    crate::events::event(&app.state, "warn", "xss", "<script>alert(1)</script> & <b>");
    let (_, _, shell) = get(&app, "/events").await;
    assert!(
        !shell.contains("<script>alert"),
        "shell estática não carrega mensagem: {shell}"
    );
    assert!(
        shell.contains("eventosPage"),
        "componente do cliente presente"
    );
    let (_, _, js) = get(&app, "/assets/painel.js").await;
    assert!(
        !js.contains("innerHTML"),
        "cliente não injeta HTML: innerHTML proibido"
    );
    assert!(!js.contains("document.write"), "document.write proibido");
}

#[tokio::test]
async fn noindex_em_todas_as_rotas_do_painel_r22() {
    let app = app_sem_docker();
    app.state.register_project("vipers", "", "h").unwrap();
    for uri in ["/", "/events", "/projects/vipers", "/rota-inexistente"] {
        let (_, headers, _) = get(&app, uri).await;
        assert_eq!(noindex_de(&headers), NOINDEX, "rota {uri} sem noindex");
    }
}

/// Monitor (13): `/observe` montada no router protegido — sem sessão o
/// guard herda (redirect pro login) e, com sessão, a página do monitor
/// responde no layout com o link novo do nav.
#[tokio::test]
async fn observe_exige_sessao_e_responde_com_link_no_nav() {
    let app = app_sem_docker();
    let (router, _dir) = painel_com_auth(&app).await;

    let (status, headers, _) = pedir(&app, router.clone(), "GET", "/observe", None, None).await;
    assert_eq!(status, StatusCode::SEE_OTHER, "auth herdada do guard");
    assert_eq!(
        headers.get(header::LOCATION).and_then(|v| v.to_str().ok()),
        Some("/login"),
        "sem sessão manda pro login como qualquer rota"
    );

    let (status, _, body) = get(&app, "/observe").await;
    assert_eq!(status, StatusCode::OK, "com sessão o monitor responde");
    assert!(body.contains("Monitor 24 h"), "página do monitor: {body}");
    assert!(
        body.contains("href=\"/observe\""),
        "nav do layout tem o link do Monitor: {body}"
    );
}

// --------------------------------------------------------------- auth

#[tokio::test]
async fn sem_sessao_qualquer_rota_vai_pro_login() {
    let app = app_sem_docker();
    let (router, _dir) = painel_com_auth(&app).await;
    for uri in ["/", "/events", "/projects/x"] {
        let (status, headers, _) = pedir(&app, router.clone(), "GET", uri, None, None).await;
        assert_eq!(status, StatusCode::SEE_OTHER, "rota {uri} sem cookie");
        assert_eq!(
            headers.get(header::LOCATION).and_then(|v| v.to_str().ok()),
            Some("/login"),
            "rota {uri} manda pro login"
        );
    }
}

#[tokio::test]
async fn login_correto_sessao_logout_revoga() {
    let app = app_sem_docker();
    let (router, _dir) = painel_com_auth(&app).await;

    // Formulário aberto e sem mensagem de erro.
    let (status, _, form) = pedir(&app, router.clone(), "GET", "/login", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(form.contains("name=\"password\""), "form: {form}");
    assert!(
        !form.contains("class=\"aviso\""),
        "sem erro ainda (CSS tem .aviso — olha a classe na div): {form}"
    );

    // Senha ERRADA → redirect de volta com erro; sem cookie.
    let (status, headers, _) = pedir(
        &app,
        router.clone(),
        "POST",
        "/login",
        None,
        Some("password=errada".to_owned()),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert!(
        headers
            .get(header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .contains("erro=1"),
        "senha errada volta pro form com erro"
    );
    assert!(
        headers.get(header::SET_COOKIE).is_none(),
        "recusa não cria sessão"
    );

    // Senha CERTA → 303 + Set-Cookie de sessão.
    let (status, headers, _) = pedir(
        &app,
        router.clone(),
        "POST",
        "/login",
        None,
        Some(format!("password={SENHA_DE_TESTE}")),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let cookie_bruto = headers
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .expect("login ok tem Set-Cookie")
        .to_owned();
    assert!(cookie_bruto.contains("HttpOnly") && cookie_bruto.contains("SameSite=Lax"));
    let cookie = cookie_bruto.split(';').next().unwrap().to_owned();

    // Dashboard com a sessão: 200 de verdade.
    let (status, _, body) = pedir(&app, router.clone(), "GET", "/", Some(&cookie), None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Nenhum projeto registrado"), "body: {body}");

    // Logout revoga: o MESMO cookie não serve mais.
    let (status, headers, _) =
        pedir(&app, router.clone(), "POST", "/logout", Some(&cookie), None).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert!(
        headers
            .get(header::SET_COOKIE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .contains("Max-Age=0"),
        "logout limpa o cookie"
    );
    let (status, _, _) = pedir(&app, router, "GET", "/", Some(&cookie), None).await;
    assert_eq!(status, StatusCode::SEE_OTHER, "sessão revogada não entra");
}

#[tokio::test]
async fn sexto_login_no_minuto_come_429() {
    let app = app_sem_docker();
    let (router, _dir) = painel_com_auth(&app).await;
    for i in 0..5 {
        let (status, _, _) = pedir(
            &app,
            router.clone(),
            "POST",
            "/login",
            None,
            Some("password=errada".to_owned()),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::SEE_OTHER,
            "tentativa {i} passa do limite"
        );
    }
    // Limite estourado = redirect com flag (anti-echo) e a mensagem na
    // página de login (o texto do lock cobre rate limit e lockout).
    let (status, headers, _) = pedir(
        &app,
        router,
        "POST",
        "/login",
        None,
        Some(format!("password={SENHA_DE_TESTE}")),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(
        headers.get(header::LOCATION).and_then(|v| v.to_str().ok()),
        Some("/login?erro=lock"),
        "recusa de limite leva à flag lock"
    );
    let (_, _, pagina) = get(&app, "/login?erro=lock").await;
    assert!(
        pagina.contains("Muitas tentativas — o login reabre em até 5 minutos."),
        "mensagem do lock na página: {pagina}"
    );
}

#[tokio::test]
async fn hash_ausente_da_503_com_instrucao() {
    let app = app_sem_docker();
    let dir = tempfile::tempdir().unwrap();
    let auth = Arc::new(panel_auth::PanelAuth::with_hash_path(
        dir.path().join("nao-existe"),
    ));
    let router = build_panel(Arc::clone(&app), auth);
    let (status, _, body) = pedir(
        &app,
        router,
        "POST",
        "/login",
        None,
        Some("password=x".to_owned()),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "fail-closed");
    assert!(
        body.contains("argon2") && body.contains("SUA-SENHA"),
        "503 ensina a gerar o hash: {body}"
    );
}

/// GATED: exige engine de Docker (`FLUX_DOCKER_URL`, convenção do repo —
/// NUNCA definida aqui). Com engine, a fonte declarada tem que virar
/// "docker (verdade)" em vez do sweep.
#[tokio::test(flavor = "multi_thread")]
async fn com_engine_a_fonte_e_docker() {
    if std::env::var("FLUX_DOCKER_URL").is_err() {
        eprintln!("pulado: FLUX_DOCKER_URL ausente — verdade via Docker não testada");
        return;
    }
    let Some(d) = crate::docker::DockerSource::connect() else {
        eprintln!("pulado: conexão com o Docker falhou");
        return;
    };
    let st = crate::state::State::in_memory().unwrap();
    st.register_project("sem-container", "x", "h").unwrap();
    let app = Arc::new(AppState::new(st, &GlobalConfig::default(), Some(d)));
    let (_, _, body) = get(&app, "/").await;
    assert!(
        body.contains("docker (verdade)"),
        "com engine, a fonte é o Docker: {body}"
    );
}

// -------------------------------------------------- eventos com filtro 26/10

#[test]
fn parse_access_digei_a_linha_canonica() {
    let a = super::formato::parse_access(
        "GET /cms/records/rides 200 120ms host=vippers-d.bitello.xyz \
         ip=1.2.3.4 xff=192.168.140.1 ua=\"Chrome\" hdrs[cookie=<redigido:5B>]",
    )
    .expect("access parseia");
    assert_eq!(a.metodo, "GET");
    assert_eq!(a.caminho, "/cms/records/rides");
    assert_eq!(a.status, 200);
    assert_eq!(a.dur_ms, 120);
    assert_eq!(a.host, "vippers-d.bitello.xyz");

    // Caminho com query e POST; duração ilegível vira 0 (nunca inventada).
    let b = super::formato::parse_access("POST /livewire/update?x=1 429 ??ms host=h ip=i")
        .expect("parseia");
    assert_eq!(b.caminho, "/livewire/update?x=1");
    assert_eq!(b.status, 429);
    assert_eq!(b.dur_ms, 0);

    // O que não é access vem cru — nunca disfarçado.
    assert!(super::formato::parse_access("vigia: recarga — caminho `/`").is_none());
}

#[test]
fn classe_status_separa_o_que_importa() {
    assert_eq!(super::formato::classe_status(200), "st-ok");
    assert_eq!(super::formato::classe_status(304), "st-3");
    assert_eq!(super::formato::classe_status(404), "st-warn");
    assert_eq!(super::formato::classe_status(429), "st-err", "429 é sinal");
    assert_eq!(super::formato::classe_status(502), "st-err");
}

#[test]
fn query_encode_decode_roundtrip_com_acento_e_especial() {
    let casos = [
        "tráfego & <caminho>/?#",
        "recarga em loop",
        "50%+de+max",
        "áéíóú çãõ",
    ];
    for c in casos {
        let enc = super::formato::query_encode(c);
        assert!(
            !enc.contains(['&', '<', '>', ' ', '+']),
            "encoded seguro: {enc}"
        );
        assert_eq!(super::formato::query_decode(&enc), c, "roundtrip de {c}");
    }
}

#[tokio::test]
async fn eventos_filtram_por_escopo_nivel_e_busca() {
    let app = app_sem_docker();
    crate::events::event(
        &app.state,
        "info",
        "access",
        "GET /a 200 1ms host=h ip=1.1.1.1",
    );
    crate::events::event(
        &app.state,
        "warn",
        "anomalia",
        "vigia: recarga — caminho `/`",
    );
    crate::events::event(&app.state, "error", "edge", "borda: 429 host=h");

    // Escopo isola.
    let (_, rows, total) = leitura::eventos_filtrados(
        &app,
        1,
        50,
        &leitura::FiltroEventos {
            scope: Some("anomalia".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!((rows.len(), total), (1, 1));
    assert!(rows[0].message.contains("recarga"));

    // Nível isola.
    let (_, rows, total) = leitura::eventos_filtrados(
        &app,
        1,
        50,
        &leitura::FiltroEventos {
            nivel: Some("error".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!((rows.len(), total), (1, 1));
    assert!(rows[0].message.contains("429"));

    // Busca bate na mensagem com aspas e sintaxe de URL limpa.
    let (_, rows, total) = leitura::eventos_filtrados(
        &app,
        1,
        50,
        &leitura::FiltroEventos {
            busca: Some("caminho".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!((rows.len(), total), (1, 1));
}

/// Shell do /events (fatia 4): sessão continua valendo (sem cookie = login)
/// e o markup fala com API+SSE — os filtros são verdade do SQL
/// (leitura::eventos_filtrados, testado à parte).
#[tokio::test]
async fn eventos_shell_tem_sessao_e_fala_com_api() {
    let app = app_sem_docker();
    let (router, _dir) = painel_com_auth(&app).await;
    let (s, headers, _) = pedir(&app, router.clone(), "GET", "/events", None, None).await;
    assert_eq!(s, StatusCode::SEE_OTHER, "sem cookie vai pro login");
    assert_eq!(
        headers.get(header::LOCATION).and_then(|v| v.to_str().ok()),
        Some("/login")
    );
    let cookie = cookie_de_sessao(&app, &router).await;
    let (_, _, shell) = pedir(&app, router, "GET", "/events", Some(&cookie), None).await;
    assert!(
        shell.contains("/api/events/follow"),
        "SSE no cliente: {shell}"
    );
    assert!(shell.contains("/api/events"), "lista via API: {shell}");
    assert!(
        shell.contains("x-data=\"eventosPage\""),
        "componente Alpine: {shell}"
    );
}

/// Assets (03): estilo e fontes são PÚBLICOS (sem sessão — a página de
/// login também carrega o estilo) e embedded; nome desconhecido = 404 sem
/// tocar disco.
#[tokio::test]
async fn assets_estilo_publico_sem_sessao() {
    let app = app_sem_docker();
    let (router, _dir) = painel_com_auth(&app).await;
    let (status, headers, body) =
        pedir(&app, router, "GET", "/assets/estilo.css", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        headers[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("text/css"),
        "content-type css: {:?}",
        headers[header::CONTENT_TYPE]
    );
    assert!(body.contains("IBM Plex Sans"), "fonte vendida declarada");
    assert!(body.contains("--err:"), "tokens semânticos presentes");
}

#[tokio::test]
async fn assets_fonte_publica_e_404_para_nome_desconhecido() {
    let app = app_sem_docker();
    let (router, _dir) = painel_com_auth(&app).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let url: hyper::Uri = format!("http://{addr}/assets/fontes/IBMPlexSans-Regular.woff2")
        .parse()
        .unwrap();
    let req = axum::http::Request::builder()
        .method("GET")
        .uri(url)
        .header(header::HOST, "flux.bitello.xyz")
        .body(Body::empty())
        .unwrap();
    let resp = app.http.request(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()[header::CONTENT_TYPE], "font/woff2");
    let bytes = resp
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    assert_eq!(&bytes[..4], b"wOF2", "woff2 de verdade, não página de erro");
    // Nome fora da tabela = 404 (sem listagem, sem disco).
    let (router2, _dir2) = painel_com_auth(&app).await;
    let (s2, _, _) = pedir(
        &app,
        router2,
        "GET",
        "/assets/fontes/hack.woff2",
        None,
        None,
    )
    .await;
    assert_eq!(s2, StatusCode::NOT_FOUND);
}

// ------------------------------------------------------------------ api pública

const TOKEN_LEITURA: &str = "token-de-leitura-de-teste";

/// Grava o hash do token de leitura nos settings (o que o `flux token
/// eventos` vai fazer de verdade na fatia de geração).
fn gravar_token_leitura(app: &Arc<AppState>) {
    app.state
        .with(|c| {
            c.execute(
                "INSERT INTO settings(k, v) VALUES ('eventos_token_hash', ?1)
                 ON CONFLICT(k) DO UPDATE SET v = excluded.v",
                [crate::api::auth::sha256_hex(TOKEN_LEITURA.as_bytes())],
            )
        })
        .unwrap();
}

/// GET na API pública com headers extras (auth/origin/cf-connecting-ip).
/// Respostas são JSON — utf8 seguro.
async fn pedir_api(
    app: &Arc<AppState>,
    router: Router,
    uri: &str,
    cookie: Option<&str>,
    extra: &[(&str, String)],
) -> (StatusCode, String) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let url: hyper::Uri = format!("http://{addr}{uri}").parse().unwrap();
    let mut builder = axum::http::Request::builder()
        .method("GET")
        .uri(url)
        .header(header::HOST, "flux.bitello.xyz");
    if let Some(c) = cookie {
        builder = builder.header(header::COOKIE, c);
    }
    for (k, v) in extra {
        builder = builder.header(*k, v.clone());
    }
    let resp = app
        .http
        .request(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let body = String::from_utf8(
        resp.into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap();
    (status, body)
}

async fn cookie_de_sessao(app: &Arc<AppState>, router: &Router) -> String {
    let (_, headers, _) = pedir(
        app,
        router.clone(),
        "POST",
        "/login",
        None,
        Some(format!("password={SENHA_DE_TESTE}")),
    )
    .await;
    let cru = headers
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .expect("login de teste tem que devolver Set-Cookie")
        .to_owned();
    cru.split(';').next().unwrap().to_owned()
}

#[tokio::test]
async fn api_eventos_lan_com_token_retorna_json_com_id() {
    let app = app_sem_docker();
    gravar_token_leitura(&app);
    crate::events::event(&app.state, "info", "teste", "evento-x-api-42");
    let (router, _dir) = painel_com_auth(&app).await;
    let (status, body) = pedir_api(
        &app,
        router,
        "/api/events",
        None,
        &[
            ("x-forwarded-for", "192.168.140.50".into()),
            ("authorization", format!("Bearer {TOKEN_LEITURA}")),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("\"total\""), "contagem no JSON: {body}");
    assert!(body.contains("\"paginas\""), "paginação no JSON: {body}");
    assert!(body.contains("\"id\":"), "id estável no JSON: {body}");
    assert!(body.contains("evento-x-api-42"), "{body}");
}

#[tokio::test]
async fn api_eventos_sem_credencial_401() {
    let app = app_sem_docker();
    let (router, _dir) = painel_com_auth(&app).await;
    let (status, body) = pedir_api(&app, router, "/api/events", None, &[]).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(body.contains("error"), "{body}");
}

#[tokio::test]
async fn api_eventos_token_sem_hash_503_e_hash_divergente_401() {
    let app = app_sem_docker();
    // Fail-closed: token enviado mas NADA configurado = 503 (postura da :7420).
    let (router, _dir) = painel_com_auth(&app).await;
    let (s1, _) = pedir_api(
        &app,
        router,
        "/api/events",
        None,
        &[
            ("x-forwarded-for", "192.168.140.50".into()),
            ("authorization", "Bearer qualquer".into()),
        ],
    )
    .await;
    assert_eq!(s1, StatusCode::SERVICE_UNAVAILABLE);
    // Configurado, token errado = 401.
    gravar_token_leitura(&app);
    let (router2, _dir2) = painel_com_auth(&app).await;
    let (s2, _) = pedir_api(
        &app,
        router2,
        "/api/events",
        None,
        &[
            ("x-forwarded-for", "192.168.140.50".into()),
            ("authorization", "Bearer errado".into()),
        ],
    )
    .await;
    assert_eq!(s2, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn api_eventos_token_valido_de_fora_da_lan_403() {
    let app = app_sem_docker();
    gravar_token_leitura(&app);
    let (router, _dir) = painel_com_auth(&app).await;
    let (status, body) = pedir_api(
        &app,
        router,
        "/api/events",
        None,
        &[
            ("cf-connecting-ip", "203.0.113.9".into()),
            ("authorization", format!("Bearer {TOKEN_LEITURA}")),
        ],
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "token NÃO vale fora da LAN: {body}"
    );
}

#[tokio::test]
async fn api_eventos_sessao_de_fora_exige_origem_do_painel() {
    let app = app_sem_docker();
    crate::events::event(&app.state, "info", "teste", "msg-sessao-7");
    let (router, _dir) = painel_com_auth(&app).await;
    let cookie = cookie_de_sessao(&app, &router).await;
    // Curl direto de fora com o cookie, SEM Origin/Referer = 403 (Here Maps).
    let (s1, _) = pedir_api(
        &app,
        router.clone(),
        "/api/events",
        Some(&cookie),
        &[("cf-connecting-ip", "203.0.113.9".into())],
    )
    .await;
    assert_eq!(s1, StatusCode::FORBIDDEN);
    // A página do painel chamando (Origin do próprio host) = 200.
    let (s2, body) = pedir_api(
        &app,
        router.clone(),
        "/api/events",
        Some(&cookie),
        &[
            ("cf-connecting-ip", "203.0.113.9".into()),
            ("origin", "https://flux.bitello.xyz".into()),
        ],
    )
    .await;
    assert_eq!(s2, StatusCode::OK, "{body}");
    assert!(body.contains("msg-sessao-7"), "{body}");
    // Origem de terceiro (Referer) = 403.
    let (s3, _) = pedir_api(
        &app,
        router,
        "/api/events",
        Some(&cookie),
        &[
            ("cf-connecting-ip", "203.0.113.9".into()),
            ("referer", "https://evil.example/x".into()),
        ],
    )
    .await;
    assert_eq!(s3, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn api_eventos_sessao_na_lan_sem_origem_passa() {
    let app = app_sem_docker();
    let (router, _dir) = painel_com_auth(&app).await;
    let cookie = cookie_de_sessao(&app, &router).await;
    let (status, _) = pedir_api(
        &app,
        router,
        "/api/events",
        Some(&cookie),
        &[("x-forwarded-for", "192.168.140.77".into())],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

/// Token de leitura auto-gerado (fatia 3): 1º boot escreve valor claro +
/// hash casando; boot seguinte NÃO regenera (valor estável — quem já
/// copiou o token não é derrubado por restart).
#[tokio::test]
async fn token_leitura_auto_gera_so_uma_vez() {
    let app = app_sem_docker();
    let dir = tempfile::tempdir().unwrap();
    let arquivo = dir.path().join("eventos.token");
    super::api_publica::garantir_token_leitura_em(&app.state, &arquivo);
    assert!(arquivo.exists(), "valor claro escrito no 1º boot");
    let valor = std::fs::read_to_string(&arquivo).unwrap();
    assert_eq!(valor.trim().len(), 64, "token = 64 chars hex: {valor:?}");
    let hash = crate::api::auth::sha256_hex(valor.trim().as_bytes());
    let no_banco = app
        .state
        .with(|c| {
            c.query_row(
                "SELECT v FROM settings WHERE k = 'eventos_token_hash'",
                [],
                |r| r.get::<_, String>(0),
            )
        })
        .unwrap();
    assert_eq!(no_banco, hash, "hash do settings casa com o valor claro");
    super::api_publica::garantir_token_leitura_em(&app.state, &arquivo);
    assert_eq!(
        std::fs::read_to_string(&arquivo).unwrap(),
        valor,
        "boot seguinte não regenera"
    );
}

/// Lê frames do stream SSE até achar `alvo` (ou esgotar tentativas) — o
/// stream é infinito, o teste corta quando viu o que precisa.
async fn ler_ate(body: &mut hyper::body::Incoming, alvo: &str, max_frames: usize) -> String {
    let mut visto = String::new();
    for _ in 0..max_frames {
        if visto.contains(alvo) {
            break;
        }
        match tokio::time::timeout(
            std::time::Duration::from_secs(10),
            http_body_util::BodyExt::frame(body),
        )
        .await
        {
            Ok(Some(Ok(frame))) => {
                if let Some(d) = frame.data_ref() {
                    visto.push_str(&String::from_utf8_lossy(d));
                }
            }
            _ => break,
        }
    }
    visto
}

async fn abrir_sse(
    app: &Arc<AppState>,
    router: Router,
    uri: &str,
) -> (StatusCode, String, hyper::body::Incoming) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let url: hyper::Uri = format!("http://{addr}{uri}").parse().unwrap();
    let req = axum::http::Request::builder()
        .method("GET")
        .uri(url)
        .header(header::HOST, "flux.bitello.xyz")
        .header(header::COOKIE, "irrelevante: auth é por Bearer aqui")
        .header("x-forwarded-for", "192.168.140.50")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN_LEITURA}"))
        .body(Body::empty())
        .unwrap();
    let resp = app.http.request(req).await.unwrap();
    let ctype = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    (resp.status(), ctype, resp.into_body())
}

/// SSE (fatia 2): backlog (id > desde, MESMO filtro, ordem cronológica)
/// chega no arranque; evento gravado DEPOIS da abertura chega ao vivo.
#[tokio::test]
async fn sse_eventos_follow_backlog_e_ao_vivo() {
    let app = app_sem_docker();
    gravar_token_leitura(&app);
    crate::events::event(&app.state, "info", "teste", "sse-backlog-9");
    let (router, _dir) = painel_com_auth(&app).await;
    let (status, ctype, mut body) = abrir_sse(&app, router, "/api/events/follow?desde=0").await;
    assert_eq!(status, StatusCode::OK);
    assert!(ctype.starts_with("text/event-stream"), "{ctype}");
    let backlog = ler_ate(&mut body, "sse-backlog-9", 10).await;
    assert!(backlog.contains("sse-backlog-9"), "backlog: {backlog}");
    assert!(backlog.contains("\"id\":"), "JSON no data: {backlog}");
    // Ao vivo: gravar depois da abertura chega pelo broadcast.
    crate::events::event(&app.state, "warn", "teste", "sse-vivo-77");
    let vivo = ler_ate(&mut body, "sse-vivo-77", 10).await;
    assert!(vivo.contains("sse-vivo-77"), "ao vivo: {vivo}");
}

/// Filtro do SSE: ?nivel=warn deixa o info de fora (backlog), o warn entra.
#[tokio::test]
async fn sse_eventos_follow_respeita_filtro() {
    let app = app_sem_docker();
    gravar_token_leitura(&app);
    crate::events::event(&app.state, "info", "teste", "sse-info-fora-1");
    crate::events::event(&app.state, "warn", "teste", "sse-warn-dentro-2");
    let (router, _dir) = painel_com_auth(&app).await;
    let (status, _, mut body) =
        abrir_sse(&app, router, "/api/events/follow?desde=0&nivel=warn").await;
    assert_eq!(status, StatusCode::OK);
    let frames = ler_ate(&mut body, "sse-warn-dentro-2", 10).await;
    assert!(frames.contains("sse-warn-dentro-2"), "{frames}");
    assert!(
        !frames.contains("sse-info-fora-1"),
        "info NÃO passa no filtro warn: {frames}"
    );
}

// ------------------------------------------------- dashboard: endpoints novos

/// ACORDAR é AÇÃO: token de leitura = 403 (read-only por contrato), sem
/// credencial = 401, com sessão na LAN sem Docker = 503 honesto.
#[tokio::test]
async fn acordar_e_acao_sessao_obrigatoria() {
    let app = app_sem_docker();
    app.state.register_project("vipers", "", "h").unwrap();
    gravar_token_leitura(&app);
    let (router, _dir) = painel_com_auth(&app).await;
    let cookie = cookie_de_sessao(&app, &router).await;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let post = |token: Option<String>, cookie: Option<String>| {
        let url: hyper::Uri = format!("http://{addr}/api/projects/vipers/acordar")
            .parse()
            .unwrap();
        let mut b = axum::http::Request::builder()
            .method("POST")
            .uri(url)
            .header(header::HOST, "flux.bitello.xyz")
            .header("x-forwarded-for", "192.168.140.50");
        if let Some(t) = token {
            b = b.header(header::AUTHORIZATION, format!("Bearer {t}"));
        }
        if let Some(c) = cookie {
            b = b.header(header::COOKIE, c);
        }
        app.http.request(b.body(Body::empty()).unwrap())
    };

    let r = post(Some(TOKEN_LEITURA.into()), None).await.unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN, "token NÃO executa ação");
    let r = post(None, None).await.unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    let r = post(None, Some(cookie)).await.unwrap();
    assert_eq!(
        r.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "sessão ok, mas sem Docker a recusa é honesta"
    );
}

/// Push SÓ QUANDO MUDA: o primeiro frame é o snapshot; snapshot idêntico
/// NÃO gera frame novo (só ping). Mudou (evento novo muda os "eventos"),
/// o frame seguinte carrega o snapshot novo.
#[tokio::test]
async fn sse_dashboard_pusha_so_quando_muda() {
    let app = app_sem_docker();
    app.state
        .register_project("vipers", "name = \"vipers\"", "hash1")
        .unwrap();
    let (router2, _dir2) = painel_com_auth(&app).await;
    let cookie = cookie_de_sessao(&app, &router2).await;
    let (status, ctype, mut body) = {
        // abre o stream com sessão+LAN
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, router2).await.unwrap();
        });
        let url: hyper::Uri = format!("http://{addr}/api/dashboard/follow")
            .parse()
            .unwrap();
        let req = axum::http::Request::builder()
            .method("GET")
            .uri(url)
            .header(header::HOST, "flux.bitello.xyz")
            .header(header::COOKIE, &cookie)
            .header("x-forwarded-for", "192.168.140.50")
            .body(Body::empty())
            .unwrap();
        let resp = app.http.request(req).await.unwrap();
        let ctype = resp
            .headers()
            .get(header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        (resp.status(), ctype, resp.into_body())
    };
    assert_eq!(status, StatusCode::OK);
    assert!(ctype.starts_with("text/event-stream"), "{ctype}");
    let primeiro = ler_ate(&mut body, "\"projetos\"", 10).await;
    assert!(
        primeiro.contains("\"nome\":\"vipers\""),
        "snapshot 1: {primeiro}"
    );
    // Snapshot idêntico: NADA chega em 7 s (o tick de 5 s passa em branco).
    // Janela de silêncio: NENHUM snapshot novo em 7 s — `: ping` pode (o
    // primeiro tick do interval dispara imediato e é comentário, não dado).
    let fim = tokio::time::Instant::now() + std::time::Duration::from_secs(7);
    loop {
        let restante = fim.saturating_duration_since(tokio::time::Instant::now());
        if restante.is_zero() {
            break;
        }
        match tokio::time::timeout(restante, http_body_util::BodyExt::frame(&mut body)).await {
            Ok(Some(Ok(frame))) => {
                if let Some(d) = frame.data_ref() {
                    let txt = String::from_utf8_lossy(d);
                    assert!(
                        !txt.contains("\"projetos\""),
                        "push sem mudança de snapshot: {txt}"
                    );
                }
            }
            _ => break,
        }
    }
    // Mudou: evento novo muda o snapshot → frame novo dentro do tick.
    crate::events::event(&app.state, "info", "teste", "dash-mudou-51");
    let segundo = ler_ate(&mut body, "dash-mudou-51", 10).await;
    assert!(
        segundo.contains("dash-mudou-51"),
        "push na mudança: {segundo}"
    );
}

// -------------------------------------------------- login endurecido (fatia 8)

/// POST /login com headers extras (cf-connecting-ip p/ simular externo).
async fn login_com(
    app: &Arc<AppState>,
    router: Router,
    corpo: String,
    extra: &[(&str, String)],
) -> (StatusCode, axum::http::HeaderMap, String) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let url: hyper::Uri = format!("http://{addr}/login").parse().unwrap();
    let mut b = axum::http::Request::builder()
        .method("POST")
        .uri(url)
        .header(header::HOST, "flux.bitello.xyz")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
    for (k, v) in extra {
        b = b.header(*k, v.clone());
    }
    let resp = app
        .http
        .request(b.body(Body::from(corpo)).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let body = String::from_utf8(
        resp.into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap();
    (status, headers, body)
}

fn flag_de(h: &axum::http::HeaderMap) -> Option<String> {
    h.get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|l| l.strip_prefix("/login?erro="))
        .map(|f| f.to_owned())
}

/// 3 erros de FORA da LAN em <= 90 s = lock GLOBAL: a senha CERTA de fora
/// cai na flag `lock`, MAS a mesma senha da LAN passa (LAN imune, decisão
/// do Gabriel — em casa você nunca fica preso).
#[tokio::test]
async fn lockout_global_trava_fora_e_lan_passa() {
    let app = app_sem_docker();
    let externo = [("cf-connecting-ip", "203.0.113.77".into())];
    // UM auth só: lockout/limiter/sessões vivem no PanelAuth compartilhado —
    // painel_com_auth novo por login zeraria o lockout entre tentativas.
    let (router, _dir) = painel_com_auth(&app).await;
    for i in 0..3 {
        let (s, h, _) = login_com(&app, router.clone(), "password=errada".into(), &externo).await;
        assert_eq!(s, StatusCode::SEE_OTHER, "tentativa {i}");
        assert_eq!(flag_de(&h).as_deref(), Some("1"), "erro de senha é flag 1");
    }
    // Senha CERTA de fora: travada.
    let (s, h, _) = login_com(
        &app,
        router.clone(),
        format!("password={SENHA_DE_TESTE}"),
        &externo,
    )
    .await;
    assert_eq!(s, StatusCode::SEE_OTHER);
    assert_eq!(
        flag_de(&h).as_deref(),
        Some("lock"),
        "fora da LAN pega o lock"
    );
    // A MESMA senha da LAN (sem header externo): passa — login ok volta pra /.
    let (s, h, _) = login_com(&app, router, format!("password={SENHA_DE_TESTE}"), &[]).await;
    assert_eq!(s, StatusCode::SEE_OTHER, "LAN loga");
    assert_eq!(
        h.get(header::LOCATION).and_then(|v| v.to_str().ok()),
        Some("/")
    );
}

/// "manter conectado": checkbox no form → cookie com Max-Age de 30 dias;
/// sem o checkbox → 12 h como sempre.
#[tokio::test]
async fn manter_conectado_estende_o_cookie() {
    let app = app_sem_docker();
    async fn max_age(app: &Arc<AppState>, corpo: String) -> u64 {
        let (router, _dir) = painel_com_auth(app).await;
        let (_, h, _) = login_com(app, router, corpo, &[]).await;
        h.get(header::SET_COOKIE)
            .and_then(|v| v.to_str().ok())
            .and_then(|c| c.split(';').find(|p| p.trim().starts_with("Max-Age=")))
            .and_then(|p| p.trim().strip_prefix("Max-Age="))
            .and_then(|n| n.parse::<u64>().ok())
            .unwrap_or(0)
    };
    assert_eq!(
        max_age(&app, format!("password={SENHA_DE_TESTE}&manter=1")).await,
        30 * 24 * 60 * 60,
        "manter=1 → 30 dias"
    );
    assert_eq!(
        max_age(&app, format!("password={SENHA_DE_TESTE}")).await,
        12 * 60 * 60,
        "sem manter → 12 h"
    );
}

/// "sair de TODOS os dispositivos": logout com todos=1 revoga TODAS as
/// sessões (a sessão B, de outro login, deixa de valer).
#[tokio::test]
async fn logout_global_revoga_todas_as_sessoes() {
    let app = app_sem_docker();
    let (router, _dir) = painel_com_auth(&app).await;
    let cookie_a = cookie_de_sessao(&app, &router).await;
    let cookie_b = cookie_de_sessao(&app, &router).await;
    let (status, _, _) = pedir(
        &app,
        router.clone(),
        "POST",
        "/logout",
        Some(&cookie_a),
        Some("todos=1".into()),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    // As DUAS sessões morrem: GET / com qualquer uma volta pro login.
    for c in [cookie_a, cookie_b] {
        let (s, h, _) = pedir(&app, router.clone(), "GET", "/", Some(&c), None).await;
        assert_eq!(s, StatusCode::SEE_OTHER, "sessão morta");
        assert_eq!(
            h.get(header::LOCATION).and_then(|v| v.to_str().ok()),
            Some("/login")
        );
    }
}

/// Turnstile configurado = login SEM token de captcha é recusado (fail-
/// closed: a ponte nem existe no teste — erro de rede é Invalido) e a
/// página de login rende o widget com a sitekey.
#[tokio::test]
async fn turnstile_configurado_exige_o_token() {
    let app = app_sem_docker();
    crate::gate::definir_turnstile(&app.state, "sitekey-de-teste", "secret-de-teste").unwrap();
    let (router, _dir) = painel_com_auth(&app).await;
    // /login é público: GET CRU (o get() logaria — e o login agora exige captcha)
    let (_, _, pagina) = pedir(&app, router.clone(), "GET", "/login", None, None).await;
    assert!(
        pagina.contains("cf-turnstile") && pagina.contains("sitekey-de-teste"),
        "widget rendido com a sitekey: {pagina}"
    );
    let (s, h, _) = login_com(&app, router, format!("password={SENHA_DE_TESTE}"), &[]).await;
    assert_eq!(s, StatusCode::SEE_OTHER);
    assert_eq!(
        flag_de(&h).as_deref(),
        Some("captcha"),
        "sem token de captcha, nem com senha certa"
    );
}
