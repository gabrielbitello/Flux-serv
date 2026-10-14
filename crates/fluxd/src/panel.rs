//! Painel web do fluxd (08): HTML gerado server-side — SVG desenhado aqui no
//! servidor, CSS próprio embutido como `const`, ZERO JavaScript (nem htmx
//! nesta fase: `meta refresh` resolve a atualização e tudo continua legível
//! com JS desligado, critério 4 da 08). Sem rust-embed e sem build de
//! frontend — `cargo build` gera tudo, como a spec manda.
//!
//! AUTH REAL (08/09): login único com senha em Argon2id
//! (`/srv/flux/secrets/panel.argon2`), sessão por cookie httpOnly +
//! `SameSite=Lax` (12 h), rate limit de 5 logins/min por IP e guard de sessão
//! em TODA rota exceto `/login`. `build_panel` devolve o Router pronto para
//! entrar no wildcard público atrás do host do painel (03) — e também roda
//! sozinho na `:7421` para depuração local (sempre 127.0.0.1, 09). Toda
//! resposta leva `X-Robots-Tag: noindex, nofollow, noarchive` + `meta robots`
//! (R22 — o painel é como dev para bots).
//!
//! Honestidade dos dados (regra da casa): o que o painel não sabe, ele diz
//! que não sabe — "sem dados ainda", "sem verdade de disco nesta fase" —
//! nunca dado inventado. Estado de sono: verdade do Docker quando existe,
//! senão a linha do sweep (60 s, 04) — com a FONTE escrita na tela. Erro de
//! leitura do estado aparece na página com a mensagem real, nunca silencioso.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::extract::{ConnectInfo, Form, FromRef, RawPathParams, RawQuery, Request, State};
use axum::http::{HeaderName, HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use flux_common::config::ProjectToml;
use flux_common::protocol::{ContainerState, Env};
use rusqlite::params;

use crate::AppState;
use crate::decision::HostSource;
use crate::docker;
use crate::observe_page;
use crate::panel_auth;
use crate::sqlite_source;
// `State` aqui é o extractor do axum; o estado do daemon é sempre acessado
// pelo campo `app.state` — o tipo fica qualificado onde precisar (testes).
use crate::state::SleepRow;

/// Header anti-índice em TODA resposta do painel (R22 — painel é como dev).
const NOINDEX: &str = "noindex, nofollow, noarchive";

fn x_robots_tag() -> HeaderName {
    HeaderName::from_static("x-robots-tag")
}

/// Últimos eventos no dashboard e tamanho de página de `/events`.
const EVENTOS_DASHBOARD: i64 = 15;
const EVENTOS_POR_PAGINA: i64 = 50;
/// Janela da curva SVG: 24 h de eventos por minuto (08).
const CURVA_MINUTOS: usize = 24 * 60;
const CURVA_W: u32 = 720;
const CURVA_H: u32 = 120;

/// Design tokens — CADA cor do light tem par no dark (regra de UI do Gabriel,
/// herdada do padrão Filament; nenhum token some na troca de tema).
const CSS: &str = r#"
:root{
--bg:#f1f5f9;--card:#ffffff;--text:#0f172a;--muted:#64748b;--line:#e2e8f0;
--accent:#2563eb;--accentbg:#dbeafe;--ok:#15803d;--okbg:#dcfce7;
--warn:#b45309;--warnbg:#fef3c7;--err:#b91c1c;--errbg:#fee2e2;
--info:#0e7490;--infobg:#cffafe;--idle:#475569;--idlebg:#e2e8f0}
@media (prefers-color-scheme:dark){:root{
--bg:#0b1220;--card:#121b2f;--text:#e2e8f0;--muted:#94a3b8;--line:#26344f;
--accent:#93c5fd;--accentbg:#1e3a8a;--ok:#4ade80;--okbg:#14532d;
--warn:#fbbf24;--warnbg:#451a03;--err:#fca5a5;--errbg:#450a0a;
--info:#67e8f9;--infobg:#164e63;--idle:#94a3b8;--idlebg:#1e293b}}
*{box-sizing:border-box}
body{margin:0;background:var(--bg);color:var(--text);
font:15px/1.55 system-ui,-apple-system,"Segoe UI",sans-serif;padding-bottom:3rem}
.wrap{max-width:1060px;margin:0 auto;padding:1.2rem}
header.top{display:flex;gap:1.2rem;align-items:baseline;flex-wrap:wrap;
border-bottom:1px solid var(--line);padding-bottom:.6rem;margin-bottom:1rem}
header.top h1{font-size:1.15rem;margin:0}
nav a{margin-right:.9rem;color:var(--accent);text-decoration:none}
nav a:hover{text-decoration:underline}
.aviso{background:var(--warnbg);color:var(--warn);border:1px solid var(--warn);
border-radius:8px;padding:.55rem .8rem;margin:0 0 1rem;font-size:.88rem}
.cards{display:grid;grid-template-columns:repeat(auto-fill,minmax(330px,1fr));gap:1rem}
.card{background:var(--card);border:1px solid var(--line);border-radius:10px;padding:.9rem 1rem}
.card h3{margin:0 0 .5rem;font-size:1rem;display:flex;gap:.5rem;align-items:center;flex-wrap:wrap}
.card a{color:var(--accent);text-decoration:none}
.card a:hover{text-decoration:underline}
a{color:var(--accent)}
.badge{background:var(--accentbg);color:var(--accent);border-radius:999px;
padding:.05rem .55rem;font-size:.72rem;font-weight:600}
.dot{display:inline-block;width:.62rem;height:.62rem;border-radius:50%;margin-right:.35rem}
.dot.ok{background:var(--ok)}.dot.warn{background:var(--warn)}.dot.err{background:var(--err)}
.dot.info{background:var(--info)}.dot.accent{background:var(--accent)}.dot.idle{background:var(--idle)}
table{width:100%;border-collapse:collapse;font-size:.86rem}
th,td{text-align:left;padding:.3rem .5rem;border-bottom:1px solid var(--line);vertical-align:top}
th{color:var(--muted);font-weight:600}
.urls{margin:.6rem 0 0;font-size:.82rem;color:var(--muted);word-break:break-all}
section{margin-top:1.6rem}
section h2{font-size:1rem;margin:0 0 .6rem}
.muted{color:var(--muted)}
.ok-t{color:var(--ok)}.warn-t{color:var(--warn)}.err-t{color:var(--err)}
.empty{background:var(--card);border:1px dashed var(--line);border-radius:10px;
padding:1.4rem;text-align:center;color:var(--muted)}
.curve{background:var(--card);border:1px solid var(--line);border-radius:10px;padding:.8rem}
.curve svg{display:block;width:100%;height:auto}
.curve polyline{stroke:var(--accent);fill:none;stroke-width:1.5}
.curve .eixo{stroke:var(--line)}
.curve text{fill:var(--muted);font-size:10px}
.pager{display:flex;gap:1rem;margin-top:.8rem}
code{background:var(--idlebg);color:var(--text);border-radius:4px;padding:.05rem .3rem;font-size:.85em}
footer{margin-top:2rem;color:var(--muted);font-size:.78rem}
@media(max-width:640px){.wrap{padding:.7rem}table{font-size:.8rem}}
"#;

// ------------------------------------------------------------------ router

/// Estado do painel: o daemon (`AppState`) + o auth (sessões/limiter/hash).
/// Router do axum aceita UM tipo de estado — os handlers extraem o que
/// precisam via `FromRef`.
#[derive(Clone)]
pub struct PanelState {
    pub app: Arc<AppState>,
    pub auth: Arc<panel_auth::PanelAuth>,
}

impl FromRef<PanelState> for Arc<AppState> {
    fn from_ref(s: &PanelState) -> Self {
        s.app.clone()
    }
}

impl FromRef<PanelState> for Arc<panel_auth::PanelAuth> {
    fn from_ref(s: &PanelState) -> Self {
        s.auth.clone()
    }
}

/// Monta o Router do painel (08) COM auth: só `/login` é aberto; todo o resto
/// passa pelo guard de sessão (`exige_sessao`) e cai no redirect para o login
/// sem cookie válido. Autorizado a entrar no wildcard público atrás do host
/// do painel (03) — sem este auth, a montagem é proibida.
pub fn build_panel(app: Arc<AppState>, auth: Arc<panel_auth::PanelAuth>) -> Router {
    let st = PanelState { app, auth };
    // Protegido SEM `with_state` próprio: o `route_layer` do guard vale só
    // para as rotas deste sub-router — /login e /logout ficam de fora.
    let protegido = Router::new()
        .route("/", get(dashboard))
        .route("/projects/{name}", get(detalhe_projeto))
        .route("/events", get(pagina_eventos))
        // Monitor de recursos (13): curvas/picos do observe — o handler extrai
        // `Arc<AppState>` via `FromRef<PanelState>`; auth herdada do guard.
        .route("/observe", get(observe_page::pagina))
        .fallback(rota_desconhecida)
        .route_layer(middleware::from_fn_with_state(st.clone(), exige_sessao));
    Router::new()
        .route("/login", get(login_form).post(login_submit))
        .route("/logout", post(logout))
        .merge(protegido)
        .with_state(st)
}

/// Guard de sessão: cookie válido → segue; qualquer outra coisa → login.
/// Redirect (303) também para deep links — após logar, o dashboard é a
/// entrada (v1: sem `?next=`, zero superfície de open-redirect).
async fn exige_sessao(State(st): State<PanelState>, req: Request, next: Next) -> Response {
    let autenticado =
        panel_auth::token_do_header(req.headers()).is_some_and(|t| st.auth.sessions.validate(&t));
    if autenticado {
        next.run(req).await
    } else {
        Redirect::to("/login").into_response()
    }
}

#[derive(serde::Deserialize)]
struct CredenciaisLogin {
    password: String,
}

async fn login_form(RawQuery(q): RawQuery) -> Response {
    let com_erro = q
        .as_deref()
        .is_some_and(|s| s.split('&').any(|p| p == "erro=1"));
    resposta(
        StatusCode::OK,
        render_login(if com_erro {
            Some("Senha incorreta.")
        } else {
            None
        }),
    )
}

/// IP real do cliente para o rate limit de login (08). `ConnectInfo` só
/// existe quando o `serve` usa `into_make_service_with_connect_info` — sem a
/// extension (chamados de teste diretos), cai para 127.0.0.1 (nunca pânico).
struct IpDoCliente(std::net::IpAddr);

impl<S> axum::extract::FromRequestParts<S> for IpDoCliente
where
    S: Send + Sync,
{
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _state: &S,
    ) -> Result<Self, Self::Rejection> {
        let ip = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|ConnectInfo(a)| a.ip())
            .unwrap_or_else(|| std::net::IpAddr::from([127, 0, 0, 1]));
        Ok(Self(ip))
    }
}

/// Login (08): rate limit 5/min por IP ANTES de qualquer leitura de hash;
/// hash ausente/inválido = 503 honesto com a instrução de geração (fail-closed,
/// mesma postura da API sem token); senha certa = sessão nova + cookie.
/// Todo resultado (ok/recusa/limite) vira evento no estado — nada silencioso.
async fn login_submit(
    State(st): State<PanelState>,
    IpDoCliente(ip): IpDoCliente,
    corpo: Result<Form<CredenciaisLogin>, axum::extract::rejection::FormRejection>,
) -> Response {
    let creds = match corpo {
        Ok(Form(c)) => c,
        Err(e) => {
            return resposta(
                StatusCode::BAD_REQUEST,
                render_login(Some(&format!("Corpo do login inválido: {e}"))),
            );
        }
    };
    if !st.auth.limiter.permitir(ip) {
        crate::events::event(
            &st.app.state,
            "warn",
            "painel",
            &format!("login recusado: rate limit excedido ip={ip}"),
        );
        return resposta(
            StatusCode::TOO_MANY_REQUESTS,
            render_login(Some("Muitas tentativas — aguarde 1 minuto.")),
        );
    }
    match st.auth.verificar(&creds.password) {
        Err(msg) => {
            tracing::error!("painel: hash indisponível: {msg}");
            resposta(StatusCode::SERVICE_UNAVAILABLE, render_login(Some(&msg)))
        }
        Ok(true) => {
            let token = st.auth.sessions.create();
            crate::events::event(&st.app.state, "info", "painel", "login ok");
            let mut resp = Redirect::to("/").into_response();
            if let Ok(v) = panel_auth::set_cookie_sessao(&token).parse() {
                resp.headers_mut().insert(header::SET_COOKIE, v);
            }
            resp
        }
        Ok(false) => {
            crate::events::event(
                &st.app.state,
                "warn",
                "painel",
                &format!("login recusado: senha incorreta ip={ip}"),
            );
            let mut resp = Redirect::to("/login?erro=1").into_response();
            resp.headers_mut()
                .insert(x_robots_tag(), HeaderValue::from_static(NOINDEX));
            resp
        }
    }
}

async fn logout(State(st): State<PanelState>, req: Request) -> Response {
    if let Some(t) = panel_auth::token_do_header(req.headers()) {
        st.auth.sessions.revoke(&t);
        crate::events::event(&st.app.state, "info", "painel", "logout");
    }
    let mut resp = Redirect::to("/login").into_response();
    if let Ok(v) = panel_auth::set_cookie_limpo().parse() {
        resp.headers_mut().insert(header::SET_COOKIE, v);
    }
    resp
}

/// Toda resposta do painel passa por aqui: no-store (estado vivo), noindex no
/// header E no HTML (R22 — vale para 404 do painel também) + no-referrer.
fn resposta(status: StatusCode, page: String) -> Response {
    (
        status,
        [
            (header::CACHE_CONTROL, "no-store"),
            (x_robots_tag(), NOINDEX),
            (header::REFERRER_POLICY, "no-referrer"),
        ],
        Html(page),
    )
        .into_response()
}

async fn dashboard(State(app): State<Arc<AppState>>) -> Response {
    resposta(StatusCode::OK, render_dashboard(&app))
}

async fn detalhe_projeto(State(app): State<Arc<AppState>>, params: RawPathParams) -> Response {
    // RawPathParams não tem `get` nesta versão do axum: varre os pares.
    let name = params
        .iter()
        .find(|(k, _)| *k == "name")
        .map(|(_, v)| v)
        .unwrap_or_default();
    resposta(detalhe_status(&app, name), render_detalhe(&app, name))
}

async fn pagina_eventos(State(app): State<Arc<AppState>>, RawQuery(q): RawQuery) -> Response {
    resposta(StatusCode::OK, render_eventos(&app, page_de(q.as_deref())))
}

async fn rota_desconhecida() -> Response {
    resposta(
        StatusCode::NOT_FOUND,
        layout(
            "Flux — painel",
            "<h1>Rota desconhecida</h1>\
             <p class=\"muted\">O painel expõe <code>/</code>, <code>/projects/{name}</code> \
             e <code>/events</code>.</p><p><a href=\"/\">← dashboard</a></p>",
            false,
        ),
    )
}

// ------------------------------------------------------------------ leitura

/// Verdade do estado de um ambiente, com a FONTE declarada para o leitor:
/// Docker quando existe (07 — estado interno nunca é verdade absoluta),
/// senão a linha gravada pelo sweep do sleep manager (04, ciclo de 60 s).
fn verdade_ambiente(app: &AppState, name: &str, env: Env) -> (ContainerState, &'static str) {
    match &app.docker {
        Some(d) => (
            docker::DockerSource::new(d.clone(), &app.state).env_state(name, env),
            "docker (verdade)",
        ),
        None => (
            sqlite_source::SqliteSource::new(&app.state).env_state(name, env),
            "estado (sweep 60 s)",
        ),
    }
}

/// Um ambiente de um projeto pronto para renderizar. Erro de leitura vira
/// `Err(texto)` — o card mostra a mensagem real, nunca silêncio.
struct PainelEnv {
    env: Env,
    estado: ContainerState,
    fonte: &'static str,
    row: Result<SleepRow, String>,
    aplicadas: Vec<(String, String)>,
}

fn ambiente(app: &AppState, name: &str, env: Env) -> PainelEnv {
    let (estado, fonte) = verdade_ambiente(app, name, env);
    PainelEnv {
        env,
        estado,
        fonte,
        row: app
            .state
            .sleep_row(name, env)
            .map_err(|e| format!("falha lendo sleep_state: {e}")),
        aplicadas: app.state.migration_rows(name, env).unwrap_or_else(|e| {
            tracing::warn!("painel: migrations de {name}/{env} ilegíveis: {e}");
            Vec::new()
        }),
    }
}

/// `(name, flux.toml bruto)` de todos os projetos registrados.
fn projetos(app: &AppState) -> Result<Vec<(String, String)>, String> {
    app.state
        .with(|c| {
            let mut stmt = c.prepare("SELECT name, toml FROM projects ORDER BY name")?;
            let rows =
                stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
            rows.collect()
        })
        .map_err(|e| e.to_string())
}

struct EventoRow {
    level: String,
    scope: String,
    message: String,
    created_at: String,
}

const COLUNAS_EVENTO: &str = "level, scope, message, created_at";

fn row_evento(r: &rusqlite::Row<'_>) -> rusqlite::Result<EventoRow> {
    Ok(EventoRow {
        level: r.get(0)?,
        scope: r.get(1)?,
        message: r.get(2)?,
        created_at: r.get(3)?,
    })
}

/// Últimos N eventos, mais novo primeiro (dashboard).
fn eventos_recentes(app: &AppState, n: i64) -> Result<Vec<EventoRow>, String> {
    app.state
        .with(|c| {
            let mut stmt = c.prepare(&format!(
                "SELECT {COLUNAS_EVENTO} FROM events ORDER BY id DESC LIMIT ?1"
            ))?;
            let rows = stmt.query_map(params![n], row_evento)?;
            rows.collect()
        })
        .map_err(|e| e.to_string())
}

/// Página de eventos — a página é CLAMPADA ao total ANTES do fetch (pedir
/// página 99 sem clampa devolveria "página 3" com zero linhas: desconexo).
fn eventos_pagina(
    app: &AppState,
    page: i64,
    size: i64,
) -> Result<(i64, Vec<EventoRow>, i64), String> {
    let total: i64 = app
        .state
        .with(|c| c.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0)))
        .map_err(|e| e.to_string())?;
    let paginas = ((total + size - 1) / size).max(1);
    let page = page.min(paginas);
    let offset = (page - 1) * size;
    let rows = app
        .state
        .with(|c| {
            let mut stmt = c.prepare(&format!(
                "SELECT {COLUNAS_EVENTO} FROM events ORDER BY id DESC LIMIT ?1 OFFSET ?2"
            ))?;
            let rows = stmt.query_map(params![size, offset], row_evento)?;
            rows.collect()
        })
        .map_err(|e| e.to_string())?;
    Ok((page, rows, total))
}

/// Época atual em segundos, pela MESMA fonte dos timestamps gravados
/// (`datetime('now')` é UTC no SQLite) — relógio único, sem drift de host.
fn agora_epoch(app: &AppState) -> Result<i64, String> {
    app.state
        .with(|c| {
            c.query_row("SELECT CAST(strftime('%s','now') AS INTEGER)", [], |r| {
                r.get(0)
            })
        })
        .map_err(|e| e.to_string())
}

/// Curva SVG das últimas 24 h de eventos por minuto (08). `None` = nenhum
/// evento na janela — o chamador mostra "sem dados ainda", honesto.
fn curva_eventos(app: &AppState) -> Result<Option<String>, String> {
    let agora = agora_epoch(app)?;
    let buckets: Vec<(i64, i64)> = app
        .state
        .with(|c| {
            let mut stmt = c.prepare(
                "SELECT CAST(strftime('%s', created_at) AS INTEGER) / 60 AS minuto, COUNT(*)
                 FROM events
                 WHERE created_at >= datetime('now', '-24 hours')
                 GROUP BY minuto",
            )?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?;
            rows.collect()
        })
        .map_err(|e| e.to_string())?;

    // 1440 buckets (um por minuto). Timestamps no futuro (relógio adiantado)
    // não entram — dado inventado não.
    let minuto_atual = agora / 60;
    let mut conts = vec![0u64; CURVA_MINUTOS];
    for (minuto, n) in buckets {
        if minuto > minuto_atual {
            continue;
        }
        let base = minuto_atual - (CURVA_MINUTOS as i64 - 1);
        if let Ok(idx) = usize::try_from(minuto - base)
            && idx < CURVA_MINUTOS
        {
            conts[idx] += n as u64;
        }
    }
    let total: u64 = conts.iter().sum();
    if total == 0 {
        return Ok(None);
    }
    let max = *conts.iter().max().unwrap_or(&1);

    // Desenho server-side: 1440 pontos (~17 kB) cabe folgado no orçamento de
    // 100 kB da spec. y=112 é o chão; pico vai a y=14.
    let mut pts = String::with_capacity(conts.len() * 12);
    for (i, c) in conts.iter().enumerate() {
        let x = i as f64 * (CURVA_W as f64 - 1.0) / (CURVA_MINUTOS as f64 - 1.0);
        let y = 112.0 - (*c as f64 / max as f64) * 98.0;
        pts.push_str(&format!("{x:.1},{y:.1} "));
    }
    Ok(Some(format!(
        "<svg viewBox=\"0 0 {CURVA_W} {CURVA_H}\" role=\"img\" \
         aria-label=\"eventos por minuto nas últimas 24 horas\">\
         <line class=\"eixo\" x1=\"0\" y1=\"112\" x2=\"{CURVA_W}\" y2=\"112\"/>\
         <polyline points=\"{pts}\"/>\
         <text x=\"4\" y=\"10\">pico {max}/min · {total} eventos</text>\
         <text x=\"4\" y=\"117\">-24 h</text>\
         <text x=\"{}\" y=\"117\">agora</text>\
         </svg>",
        CURVA_W - 34
    )))
}

// ---------------------------------------------------------------- formato

/// Escape HTML para TODA interpolação de dado (nome de projeto, mensagem de
/// evento, host...). Mesmo conjunto de replaces de `pages::html_escape`.
fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Escape do valor usado em `LIKE '%nome%'` (detalhe de projeto).
fn like_esc(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// `?page=N` da query string — parse manual (extrator com serde não é
/// dependência desta fase). Página inválida/ausente/0 → 1 (clamp honesto).
fn page_de(query: Option<&str>) -> i64 {
    query
        .and_then(|q| q.split('&').find_map(|kv| kv.strip_prefix("page=")))
        .and_then(|v| v.parse::<i64>().ok())
        .filter(|p| *p >= 1)
        .unwrap_or(1)
}

/// Época → "YYYY-MM-DD HH:MM:SS" UTC (algoritmo civil de Howard Hinnant —
/// std Rust não tem tz database; painel exibe tudo em UTC e DIZ isso).
fn fmt_utc(secs: i64) -> String {
    let dias = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    let (h, mi, s) = (sod / 3600, (sod % 3600) / 60, sod % 60);
    let z = dias + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02} {h:02}:{mi:02}:{s:02}")
}

/// Epoch opcional → "há 5 min" / "em 12 min" com o absoluto UTC no `title`.
/// `None` → "—" (nunca hora inventada). Passado conta com "há", futuro com
/// "em"; abaixo de 1 min, segundos.
fn quando(now: i64, v: Option<i64>) -> String {
    let Some(v) = v else {
        return "—".into();
    };
    let diff = v - now;
    let rotulo = diff.abs();
    let rel = if rotulo < 60 {
        format!("{}{rotulo} s", if diff < 0 { "há " } else { "em " })
    } else if rotulo < 3600 {
        format!(
            "{}{} min",
            if diff < 0 { "há " } else { "em " },
            rotulo / 60
        )
    } else if rotulo < 86_400 {
        format!(
            "{}{} h",
            if diff < 0 { "há " } else { "em " },
            rotulo / 3600
        )
    } else {
        format!(
            "{}{} d",
            if diff < 0 { "há " } else { "em " },
            rotulo / 86_400
        )
    };
    format!("<span title=\"{} UTC\">{rel}</span>", fmt_utc(v))
}

/// Estado → (classe do dot, rótulo pt-BR). Cores espelham a matriz da 03.
fn dot(estado: ContainerState) -> (&'static str, &'static str) {
    match estado {
        ContainerState::Awake => ("ok", "acordado"),
        ContainerState::Paused => ("info", "pausado"),
        ContainerState::Sleeping => ("warn", "dormindo"),
        ContainerState::Starting => ("accent", "subindo"),
        ContainerState::Failed => ("err", "falhou"),
        ContainerState::Unknown => ("idle", "desconhecido"),
    }
}

fn badge_level(level: &str) -> String {
    let classe = match level {
        "info" => "muted",
        "warn" => "warn-t",
        "error" | "err" => "err-t",
        _ => "muted",
    };
    format!("<span class=\"{classe}\">{}</span>", esc(level))
}

// ------------------------------------------------------------------ layout

/// Esqueleto de página: tokens CSS, botão Sair, `meta robots` (R22) e
/// `meta refresh` opcional (só dashboard — eventos/detalhe são leitura sob
/// demanda; zero JS: o refresh é mecânico do browser).
fn layout(title: &str, body: &str, auto_refresh: bool) -> String {
    let refresh = if auto_refresh {
        "<meta http-equiv=\"refresh\" content=\"5\">"
    } else {
        ""
    };
    format!(
        r#"<!doctype html>
<html lang="pt-br">
<head>
<meta charset="utf-8">
{refresh}
<meta name="robots" content="noindex, nofollow">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title}</title>
<style>{CSS}</style>
</head>
<body>
<div class="wrap">
<header class="top">
<h1>Flux · painel</h1>
<nav><a href="/">Dashboard</a><a href="/events">Eventos</a><a href="/observe">Monitor</a><a href="http://192.168.140.2:8081/" title="Gerenciador de banco — LAN-only (05)">Adminer</a></nav>
<form method="post" action="/logout" style="margin-left:auto">
<button type="submit" style="background:var(--idlebg);color:var(--text);
border:1px solid var(--line);border-radius:6px;padding:.2rem .7rem;cursor:pointer">Sair</button>
</form>
</header>
{body}
<footer>fluxd — painel server-side, zero JS (spec 08) · horários em UTC</footer>
</div>
</body>
</html>"#
    )
}

/// Tela de login (08): campo de senha só — usuário único (Gabriel), o nome
/// não participa da autenticação. Mensagem opcional em cima (senha errada,
/// rate limit, hash ausente). Sem nav — quem não logou não navega.
fn render_login(mensagem: Option<&str>) -> String {
    let msg = mensagem
        .map(|m| format!("<div class=\"aviso\">{}</div>", esc(m)))
        .unwrap_or_default();
    let mut body = String::from("<h1>Entrar no painel</h1>");
    body.push_str(&msg);
    body.push_str(
        r#"<form method="post" action="/login" class="card" style="max-width:380px;
display:flex;flex-direction:column;gap:.7rem;margin-top:.8rem">
<label for="password">Senha</label>
<input type="password" id="password" name="password" required autofocus
style="background:var(--bg);color:var(--text);border:1px solid var(--line);
border-radius:6px;padding:.45rem .6rem">
<button type="submit" style="background:var(--accentbg);color:var(--accent);
border:1px solid var(--accent);border-radius:6px;padding:.45rem .6rem;
font-weight:600;cursor:pointer">Entrar</button>
</form>"#,
    );
    layout("Flux — entrar", &body, false)
}

fn linha_estado(e: &PainelEnv) -> String {
    let (classe, rotulo) = dot(e.estado);
    match &e.row {
        Ok(_) => format!(
            "<span class=\"dot {classe}\"></span>{rotulo} <span class=\"muted\">({})</span>",
            e.fonte
        ),
        Err(msg) => format!(
            "<span class=\"dot err\"></span><span class=\"err-t\">{}</span>",
            esc(msg)
        ),
    }
}

// --------------------------------------------------------------- dashboard

fn render_dashboard(app: &AppState) -> String {
    let mut body = String::new();

    match projetos(app) {
        Ok(lista) if lista.is_empty() => {
            body.push_str(
                "<div class=\"empty\">Nenhum projeto registrado ainda.\
                 <br><span class=\"muted\">Projetos aparecem aqui quando o fluxd conhece \
                 o <code>flux.toml</code> (spec 06).</span></div>",
            );
        }
        Ok(lista) => {
            // Sem `unwrap_or(0)`: now=0 faria `quando` renderizar "em 20 mil
            // dias" como se fosse verdade — a falha real aparece na página,
            // como em toda leitura do estado (promessa do módulo).
            let now = match agora_epoch(app) {
                Ok(n) => n,
                Err(e) => {
                    body.push_str(&format!(
                        "<div class=\"empty err-t\">Falha lendo o relógio do estado: {}</div>",
                        esc(&e)
                    ));
                    return layout("Flux — painel", &body, true);
                }
            };
            body.push_str("<div class=\"cards\">");
            for (name, toml_src) in lista {
                let dev = ambiente(app, &name, Env::Dev);
                let prod = ambiente(app, &name, Env::Prod);
                let framework = match ProjectToml::parse_str(&toml_src) {
                    Ok(t) => format!("<span class=\"badge\">{}</span>", t.framework()),
                    Err(e) => format!(
                        "<span class=\"badge err-t\" title=\"{}\">flux.toml inválido</span>",
                        esc(&e.to_string())
                    ),
                };
                let dev_host = app.domain.dev_host(&name);
                let prod_host = app.domain.prod_host(&name);
                body.push_str(&format!(
                    "<article class=\"card\"><h3><a href=\"/projects/{}\">{}</a>{framework}</h3>\
                     <table>\
                     <tr><th>dev</th><td>{}</td><td>migrations aplicadas: {}</td></tr>\
                     <tr><th>prod</th><td>{}</td><td>migrations aplicadas: {}</td></tr>\
                     </table>\
                     <p class=\"urls\"><a href=\"https://{dev_host}\">{dev_host}</a> · \
                     <a href=\"https://{prod_host}\">{prod_host}</a></p>",
                    esc(&name),
                    esc(&name),
                    linha_estado(&dev),
                    dev.aplicadas.len(),
                    linha_estado(&prod),
                    prod.aplicadas.len(),
                ));
                // Sinais de sono (04) quando existem — ausente vira "—".
                if let (Ok(drow), Ok(prow)) = (&dev.row, &prod.row) {
                    body.push_str(&format!(
                        "<table><tr><th></th><th>interação</th><th>lease</th><th>cooldown</th></tr>\
                         <tr><th>dev</th><td>{}</td><td>{}</td><td>{}</td></tr>\
                         <tr><th>prod</th><td>{}</td><td>{}</td><td>{}</td></tr></table>",
                        quando(now, drow.0),
                        quando(now, drow.1),
                        quando(now, drow.2),
                        quando(now, prow.0),
                        quando(now, prow.1),
                        quando(now, prow.2),
                    ));
                }
                body.push_str("</article>");
            }
            body.push_str("</div>");
        }
        Err(e) => body.push_str(&format!(
            "<div class=\"empty err-t\">Falha lendo projetos do estado: {}</div>",
            esc(&e)
        )),
    }

    // Curva 24 h — sem dado, a frase exata, sem gráfico vazio mentindo vida.
    body.push_str("<section><h2>Eventos por minuto — últimas 24 h</h2>");
    match curva_eventos(app) {
        Ok(Some(svg)) => body.push_str(&format!("<div class=\"curve\">{svg}</div>")),
        Ok(None) => body.push_str("<div class=\"empty\">sem dados ainda</div>"),
        Err(e) => body.push_str(&format!(
            "<div class=\"empty err-t\">Falha lendo eventos: {}</div>",
            esc(&e)
        )),
    }
    body.push_str("</section>");

    body.push_str("<section><h2>Últimos eventos</h2>");
    match eventos_recentes(app, EVENTOS_DASHBOARD) {
        Ok(rows) if rows.is_empty() => {
            body.push_str("<div class=\"empty\">sem eventos registrados ainda</div>")
        }
        Ok(rows) => {
            body.push_str("<table><tr><th>quando (UTC)</th><th>nível</th><th>escopo</th><th>mensagem</th></tr>");
            for ev in rows {
                body.push_str(&format!(
                    "<tr><td class=\"muted\">{}</td><td>{}</td><td><code>{}</code></td><td>{}</td></tr>",
                    esc(&ev.created_at),
                    badge_level(&ev.level),
                    esc(&ev.scope),
                    esc(&ev.message)
                ));
            }
            body.push_str("</table><p><a href=\"/events\">todos os eventos →</a></p>");
        }
        Err(e) => body.push_str(&format!(
            "<div class=\"empty err-t\">Falha lendo eventos: {}</div>",
            esc(&e)
        )),
    }
    body.push_str("</section>");

    layout("Flux — painel", &body, true)
}

// ----------------------------------------------------------------- detalhe

fn detalhe_status(app: &AppState, name: &str) -> StatusCode {
    match projetos(app) {
        Ok(lista) if lista.iter().any(|(n, _)| n == name) => StatusCode::OK,
        Ok(_) => StatusCode::NOT_FOUND,
        // Estado ilegível: 503 é a verdade (painel não consegue provar que o
        // projeto existe) — com a mensagem real na página, nunca silêncio.
        Err(_) => StatusCode::SERVICE_UNAVAILABLE,
    }
}

fn render_detalhe(app: &AppState, name: &str) -> String {
    let Ok(lista) = projetos(app) else {
        return layout(
            "Flux — painel",
            "<h1>Estado indisponível</h1><p class=\"err-t\">Não consegui ler a lista de \
             projetos — o erro real está no log do fluxd.</p><p><a href=\"/\">← dashboard</a></p>",
            false,
        );
    };
    let Some((_, toml_src)) = lista.iter().find(|(n, _)| n == name) else {
        return layout(
            "Flux — painel",
            &format!(
                "<h1>Projeto não registrado</h1><p><code>{}</code> não está no estado do \
                 fluxd. Projetos entram pelo <code>flux.toml</code> (spec 06).</p>\
                 <p><a href=\"/\">← dashboard</a></p>",
                esc(name)
            ),
            false,
        );
    };

    let mut body = String::new();
    let framework = match ProjectToml::parse_str(toml_src) {
        Ok(t) => format!("<span class=\"badge\">{}</span>", t.framework()),
        Err(e) => format!(
            "<span class=\"badge err-t\" title=\"{}\">flux.toml inválido</span>",
            esc(&e.to_string())
        ),
    };
    let dev_host = app.domain.dev_host(name);
    let prod_host = app.domain.prod_host(name);
    body.push_str(&format!(
        "<h1>{} {framework}</h1>\
         <p class=\"urls\">dev <a href=\"https://{dev_host}\">{dev_host}</a> · \
         prod <a href=\"https://{prod_host}\">{prod_host}</a></p>",
        esc(name)
    ));

    let dev = ambiente(app, name, Env::Dev);
    let prod = ambiente(app, name, Env::Prod);
    // Sem `unwrap_or(0)` pelo mesmo motivo do dashboard: relógio ilegível não
    // vira "em 20 mil dias" — a causa real sobe na página.
    let now = match agora_epoch(app) {
        Ok(n) => n,
        Err(e) => {
            return layout(
                "Flux — painel",
                &format!(
                    "<h1>Estado indisponível</h1><p class=\"err-t\">Falha lendo o relógio do \
                     estado: {}</p><p><a href=\"/\">← dashboard</a></p>",
                    esc(&e)
                ),
                false,
            );
        }
    };

    body.push_str("<section><h2>Ambientes</h2><table>\
                   <tr><th></th><th>estado</th><th>última interação</th><th>lease até</th><th>cooldown até</th></tr>");
    for e in [&dev, &prod] {
        let (sinais, _) = match &e.row {
            Ok(row) => (
                format!(
                    "<td>{}</td><td>{}</td><td>{}</td>",
                    quando(now, row.0),
                    quando(now, row.1),
                    quando(now, row.2)
                ),
                "",
            ),
            Err(msg) => (
                format!("<td colspan=\"3\" class=\"err-t\">{}</td>", esc(msg)),
                "",
            ),
        };
        let (dclasse, drotulo) = dot(e.estado);
        body.push_str(&format!(
            "<tr><th>{}</th><td><span class=\"dot {dclasse}\"></span>{drotulo} \
             <span class=\"muted\">({})</span></td>{sinais}</tr>",
            e.env, e.fonte
        ));
    }
    body.push_str("</table></section>");

    for e in [&dev, &prod] {
        body.push_str(&format!("<section><h2>Migrations — {}</h2>", e.env));
        if e.aplicadas.is_empty() {
            body.push_str(&format!(
                "<div class=\"empty\">nenhuma migration aplicada em {} ainda</div>",
                e.env
            ));
        } else {
            body.push_str("<ul>");
            for (file, sha) in &e.aplicadas {
                body.push_str(&format!(
                    "<li><code>{}</code> <span class=\"muted\">{}</span></li>",
                    esc(file),
                    esc(sha.get(..8).unwrap_or(sha))
                ));
            }
            body.push_str("</ul>");
        }
        // O painel desta fase lê SÓ o estado: o diff contra o disco é do ciclo
        // de schema (05), que roda no sync/deploy. Dizer "não sei" é a saída
        // honesta — inventar "0 pendentes" não é.
        body.push_str(
            "<p class=\"muted\">pendentes: sem verdade de disco nesta fase \
                       (o diff roda no ciclo de schema, spec 05)</p></section>",
        );
    }

    body.push_str(&format!(
        "<section><h2>Eventos que mencionam \"{}</h2>",
        esc(name)
    ));
    match app.state.with(|c| {
        let mut stmt = c.prepare(
            "SELECT level, scope, message, created_at FROM events
             WHERE message LIKE '%' || ?1 || '%' ESCAPE '\\'
             ORDER BY id DESC LIMIT 20",
        )?;
        let rows = stmt.query_map(params![like_esc(name)], row_evento)?;
        rows.collect::<rusqlite::Result<Vec<EventoRow>>>()
    }) {
        Ok(rows) if rows.is_empty() => {
            body.push_str("<div class=\"empty\">nenhum evento menciona este projeto ainda</div>")
        }
        Ok(rows) => {
            body.push_str("<table><tr><th>quando (UTC)</th><th>nível</th><th>escopo</th><th>mensagem</th></tr>");
            for ev in rows {
                body.push_str(&format!(
                    "<tr><td class=\"muted\">{}</td><td>{}</td><td><code>{}</code></td><td>{}</td></tr>",
                    esc(&ev.created_at),
                    badge_level(&ev.level),
                    esc(&ev.scope),
                    esc(&ev.message)
                ));
            }
            body.push_str("</table>");
        }
        Err(e) => body.push_str(&format!(
            "<div class=\"empty err-t\">Falha na busca de eventos: {}</div>",
            esc(&e.to_string())
        )),
    }
    body.push_str("</section><p><a href=\"/\">← dashboard</a></p>");

    layout(&format!("Flux — {}", esc(name)), &body, false)
}

// ------------------------------------------------------------------ eventos

fn render_eventos(app: &AppState, page_pedida: i64) -> String {
    let mut body = String::new();
    match eventos_pagina(app, page_pedida, EVENTOS_POR_PAGINA) {
        Ok((page, rows, total)) => {
            let paginas = ((total + EVENTOS_POR_PAGINA - 1) / EVENTOS_POR_PAGINA).max(1);
            body.push_str(&format!(
                "<h1>Eventos</h1><p class=\"muted\">página {page} de {paginas} · \
                 {total} eventos no total</p>"
            ));
            if rows.is_empty() {
                body.push_str("<div class=\"empty\">sem eventos registrados ainda</div>");
            } else {
                body.push_str(
                    "<table><tr><th>quando (UTC)</th><th>nível</th><th>escopo</th><th>mensagem</th></tr>",
                );
                for ev in rows {
                    body.push_str(&format!(
                        "<tr><td class=\"muted\">{}</td><td>{}</td><td><code>{}</code></td><td>{}</td></tr>",
                        esc(&ev.created_at),
                        badge_level(&ev.level),
                        esc(&ev.scope),
                        esc(&ev.message)
                    ));
                }
                body.push_str("</table>");
            }
            body.push_str("<div class=\"pager\">");
            if page > 1 {
                body.push_str(&format!(
                    "<a href=\"/events?page={}\">← anterior</a>",
                    page - 1
                ));
            }
            if page < paginas {
                body.push_str(&format!(
                    "<a href=\"/events?page={}\">próxima →</a>",
                    page + 1
                ));
            }
            body.push_str("</div><p><a href=\"/\">← dashboard</a></p>");
        }
        Err(e) => body.push_str(&format!(
            "<h1>Eventos</h1><div class=\"empty err-t\">Falha lendo eventos: {}</div>",
            esc(&e)
        )),
    }
    layout("Flux — eventos", &body, false)
}

// -------------------------------------------------------------------- testes
//
// End-to-end de verdade: sobe o Router do painel em porta efêmera no loopback
// e fala HTTP com o MESMO cliente do proxy (`AppState.http`, hyper-util) —
// mesmo efeito do `oneshot` do tower sem depender de crate que não está
// declarada neste crate. Estado sempre `State::in_memory` (nunca toca disco).

#[cfg(test)]
mod tests {
    use super::*;
    use argon2::password_hash::SaltString;
    use argon2::{Argon2, PasswordHasher};
    use axum::body::Body;
    use flux_common::config::GlobalConfig;
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

    fn noindex_de(headers: &axum::http::HeaderMap) -> String {
        headers
            .get("x-robots-tag")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned()
    }

    // ------------------------------------------------------------- helpers

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
        let Some(d) = docker::DockerSource::connect() else {
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
}
