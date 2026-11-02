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
#[tokio::test]
async fn dashboard_mostra_sem_sessao_e_form_de_rescan() {
    let app = app_sem_docker();
    app.state.register_project("vipers", "", "h").unwrap();
    let (status, _, body) = get(&app, "/").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("sem sessão"),
        "card sem sync não inventa sessão: {body}"
    );
    assert!(
        body.contains("action=\"/projects/vipers/rescan\""),
        "botão de rescan aponta para o POST: {body}"
    );
}

/// Confirmação do rescan chega como aviso no dashboard (`/?rescan=1`).
#[tokio::test]
async fn dashboard_com_flag_rescan_mostra_confirmacao() {
    let app = app_sem_docker();
    let (_, _, body) = get(&app, "/?rescan=1").await;
    assert!(
        body.contains("rescan queued — o flux watch executa"),
        "confirmação na página: {body}"
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

/// Rescan de projeto não registrado: recusa com a mensagem REAL na UI
/// (503) — o painel não finge que o pedido entrou na fila.
#[tokio::test]
async fn rescan_de_projeto_desconhecido_mostra_o_erro_na_ui() {
    let app = app_sem_docker();
    let (status, _, body) = post_logado(&app, "/projects/fantasma/rescan").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        body.contains("Falha ao pedir rescan de fantasma"),
        "erro nomeia o projeto na página: {body}"
    );
    assert!(
        body.contains("não registrado"),
        "mensagem real do sync.rs na página: {body}"
    );
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
    let (_, _, body) = get(&app, "/?rescan=1").await;
    assert!(
        body.contains("rescan queued — o flux watch executa"),
        "confirmação na página: {body}"
    );
    assert!(
        body.contains("rescan pendente"),
        "flag do request_rescan vira badge: {body}"
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

    let (status, _, body) = get(&app, "/").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("vipers"), "body: {body}");
    assert!(
        body.contains("dormindo") && body.contains("pausado"),
        "estados de ambos ambientes: {body}"
    );
    assert!(
        body.contains("estado (sweep 60 s)"),
        "sem docker, a FONTE é declarada: {body}"
    );
    assert!(
        body.contains("migrations aplicadas: 1"),
        "contagem de migrations no card: {body}"
    );
    assert!(
        body.contains("GET / 200 3ms host=vipers-d.bitello.xyz"),
        "últimos eventos: {body}"
    );
    assert!(
        body.contains("<svg") && body.contains("<polyline"),
        "evento recente dentro da janela desenha a curva: {body}"
    );
}

#[tokio::test]
async fn detalhe_de_projeto_inexistente_e_404_brandiado() {
    let app = app_sem_docker();
    let (status, headers, body) = get(&app, "/projects/naoexiste").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body.contains("não registrado"), "body: {body}");
    assert_eq!(noindex_de(&headers), NOINDEX, "404 do painel também é R22");
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

    let (status, _, body) = get(&app, "/projects/pedi").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("pedi"), "body: {body}");
    assert!(body.contains("node"), "framework do flux.toml: {body}");
    assert!(
        body.contains("lease") && body.contains("cooldown"),
        "sinais do sleep manager na tabela: {body}"
    );
    assert!(
        body.contains("em "),
        "lease/cooldown futuros são contagem: {body}"
    );
    assert!(
        body.contains("há 10 min"),
        "interação passada é relativo: {body}"
    );
    assert!(
        body.contains("0001_init.sql"),
        "migrations listadas: {body}"
    );
    assert!(
        body.contains("sem verdade de disco nesta fase"),
        "pendentes é honesto: {body}"
    );
    assert!(
        body.contains("pedi-d.bitello.xyz"),
        "URL dev clicável: {body}"
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
    let (status, _, body) = pedir(
        &app,
        router,
        "POST",
        "/login",
        None,
        Some(format!("password={SENHA_DE_TESTE}")),
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert!(
        body.contains("Muitas tentativas"),
        "429 do painel é página brandiada: {body}"
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
            std::time::Duration::from_secs(3),
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
