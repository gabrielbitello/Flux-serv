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

#[tokio::test]
async fn eventos_sao_paginados_e_navegaveis() {
    let app = app_sem_docker();
    for i in 0..120 {
        crate::events::event(&app.state, "info", "teste", &format!("evento-{i:03}"));
    }
    let (_, _, p1) = get(&app, "/events").await;
    assert!(p1.contains("página 1 de 3"), "120/50 = 3 páginas: {p1}");
    assert!(p1.contains("evento-119"), "mais novo primeiro: {p1}");
    assert!(!p1.contains("evento-000"), "offset correto: {p1}");
    assert!(p1.contains("próxima →"), "nav: {p1}");

    let (_, _, p3) = get(&app, "/events?page=3").await;
    assert!(p3.contains("página 3 de 3"), "body: {p3}");
    assert!(
        p3.contains("evento-000"),
        "última página tem o mais antigo: {p3}"
    );
    assert!(p3.contains("← anterior"), "nav: {p3}");

    let (_, _, fora) = get(&app, "/events?page=99").await;
    assert!(
        fora.contains("página 3 de 3"),
        "página além do fim é clampada, não 500: {fora}"
    );
}

#[tokio::test]
async fn html_de_evento_e_escapado() {
    let app = app_sem_docker();
    crate::events::event(&app.state, "warn", "xss", "<script>alert(1)</script> & <b>");
    let (_, _, body) = get(&app, "/events").await;
    assert!(
        !body.contains("<script>alert"),
        "mensagem nunca vira HTML: {body}"
    );
    assert!(
        body.contains("&lt;script&gt;alert(1)&lt;/script&gt;"),
        "escapado mas presente: {body}"
    );
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
