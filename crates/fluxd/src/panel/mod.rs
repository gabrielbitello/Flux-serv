//! Painel web do fluxd (08): HTML gerado server-side — SVG desenhado aqui no
//! servidor, CSS próprio embutido como `const`. JS MÍNIMO e autorizado
//! (26/10, Gabriel: "pode ter js sim"): só a página /events refetcha a si
//! mesma a cada 5 s e troca o corpo da tabela sem reload (pausável, aba
//! oculta não refetch, falha visível no rótulo); o resto do painel continua
//! zero JS com `meta refresh`. Sem rust-embed e sem build de frontend —
//! `cargo build` gera tudo, como a spec manda.
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
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};

use crate::panel_auth;

pub(crate) mod acoes;
pub(crate) mod api_publica;
mod assets;

/// Shells estáticas — HTML sem dado, servido por include_str!.
const SHELL_EVENTOS: &str = include_str!("shells/eventos.html");
const SHELL_DASHBOARD: &str = include_str!("shells/dashboard.html");
const SHELL_PROJETO: &str = include_str!("shells/projeto.html");
const SHELL_OBSERVE: &str = include_str!("shells/observe.html");
// `State` aqui é o extractor do axum; o estado do daemon é sempre acessado
// pelo campo `app.state` — o tipo fica qualificado onde precisar (testes).
use crate::ui::{NOINDEX, x_robots_tag};

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
        // Rescan da sessão de sync (flux watch executa): POST → redirect.
        .route("/projects/{name}/rescan", post(rescan_projeto))
        .route("/events", get(pagina_eventos))
        // Monitor de recursos (13): curvas/picos do observe — o handler extrai
        // `Arc<AppState>` via `FromRef<PanelState>`; auth herdada do guard.
        .route("/observe", get(observe_shell))
        .fallback(rota_desconhecida)
        .route_layer(middleware::from_fn_with_state(st.clone(), exige_sessao));
    Router::new()
        .route("/login", get(login_form).post(login_submit))
        .route("/logout", post(logout))
        // Assets estáticos (03): fora do guard — CSS/fonte não têm segredo e
        // a página de login também carrega o estilo.
        .route("/assets/estilo.css", get(assets::estilo))
        .route("/assets/alpine.min.js", get(assets::alpine))
        .route("/assets/painel.js", get(assets::painel_js))
        .route("/assets/chart.umd.min.js", get(assets::chartjs))
        .route("/assets/fontes/{arquivo}", get(assets::fonte))
        // API pública de leitura (escopo 31/10): auth própria (token LAN /
        // sessão+origem), fora do guard de sessão.
        .merge(api_publica::router(&st))
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
    /// "manter conectado": presente = sessão de 30 dias.
    #[serde(default)]
    manter: Option<String>,
    /// Token do widget Turnstile (quando o captcha está configurado).
    #[serde(rename = "cf-turnstile-response", default)]
    turnstile: Option<String>,
}

/// Textos do login por flag na query — o redirect não leva NENHUM dado do
/// formulário (regra anti-echo da 08, mantida).
fn texto_do_erro(flag: Option<&str>) -> Option<&'static str> {
    match flag {
        Some("1") => Some("Senha incorreta."),
        Some("captcha") => Some("Captcha inválido — tenta de novo."),
        Some("captcha_indisponivel") => {
            Some("Captcha indisponível — problema no servidor; tenta de novo em instantes.")
        }
        Some("lock") => Some("Muitas tentativas — o login reabre em até 5 minutos."),
        _ => None,
    }
}

async fn login_form(State(st): State<PanelState>, RawQuery(q): RawQuery) -> Response {
    let flag = q
        .as_deref()
        .and_then(|s| s.split('&').find_map(|p| p.strip_prefix("erro=")));
    let site = crate::gate::turnstile_config(&st.app.state).site;
    resposta(
        StatusCode::OK,
        render_login(texto_do_erro(flag), site.as_deref()),
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
    headers: axum::http::HeaderMap,
    corpo: Result<Form<CredenciaisLogin>, axum::extract::rejection::FormRejection>,
) -> Response {
    let creds = match corpo {
        Ok(Form(c)) => c,
        Err(e) => {
            return resposta(
                StatusCode::BAD_REQUEST,
                render_login(Some(&format!("Corpo do login inválido: {e}")), None),
            );
        }
    };
    let lan = !crate::waf::fonte_externa(&headers, Some(ip));
    if st.auth.lockout.bloqueado(lan) {
        crate::events::event(
            &st.app.state,
            "warn",
            "painel",
            &format!("login recusado: lockout global ativo ip={ip}"),
        );
        return redirecao_erro_login("lock");
    }
    if !st.auth.limiter.permitir(ip) {
        crate::events::event(
            &st.app.state,
            "warn",
            "painel",
            &format!("login recusado: rate limit excedido ip={ip}"),
        );
        return redirecao_erro_login("lock");
    }
    // Turnstile: SEMPRE que configurado (rodada 12 do escopo) — fail-closed:
    // erro de rede = Invalido = recusa. Errar o captcha CONTA como tentativa.
    if let Some(secret) = crate::gate::turnstile_config(&st.app.state).secret {
        let token = creds.turnstile.as_deref().unwrap_or("");
        match crate::gate::turnstile_verificar(&st.app.http, &secret, token, &ip.to_string()).await
        {
            crate::gate::VereditoTurnstile::Valido => {}
            crate::gate::VereditoTurnstile::Invalido(motivo) => {
                st.auth.lockout.registrar_falha(lan);
                crate::events::event(
                    &st.app.state,
                    "warn",
                    "painel",
                    &format!("login recusado: turnstile ({motivo}) ip={ip}"),
                );
                // Indisponibilidade não é culpa do captcha do usuário — flag
                // própria pra página dizer a verdade em vez de "inválido".
                let flag: &'static str = if motivo.starts_with("captcha indisponível") {
                    "captcha_indisponivel"
                } else {
                    "captcha"
                };
                return redirecao_erro_login(flag);
            }
            crate::gate::VereditoTurnstile::Desativado => {}
        }
    }
    match st.auth.verificar(&creds.password) {
        Err(msg) => {
            // Problema de PLATAFORMA: não é tentativa de senha, não conta
            // pro lockout (e mostra a instrução real — fail-closed).
            tracing::error!("painel: hash indisponível: {msg}");
            resposta(
                StatusCode::SERVICE_UNAVAILABLE,
                render_login(Some(&msg), None),
            )
        }
        Ok(true) => {
            st.auth.lockout.limpar();
            let (token, ttl) = if creds.manter.is_some() {
                (
                    st.auth.sessions.create_longa(),
                    panel_auth::SESSION_TTL_LONGA,
                )
            } else {
                (st.auth.sessions.create(), panel_auth::SESSION_TTL)
            };
            crate::events::event(
                &st.app.state,
                "info",
                "painel",
                &format!("login ok manter={}", creds.manter.is_some()),
            );
            let mut resp = Redirect::to("/").into_response();
            if let Ok(v) = panel_auth::set_cookie_sessao_com(&token, ttl).parse() {
                resp.headers_mut().insert(header::SET_COOKIE, v);
            }
            resp
        }
        Ok(false) => {
            st.auth.lockout.registrar_falha(lan);
            crate::events::event(
                &st.app.state,
                "warn",
                "painel",
                &format!("login recusado: senha incorreta ip={ip}"),
            );
            redirecao_erro_login("1")
        }
    }
}

/// Redirect de erro do login: flag na query, NENHUM dado do form na URL.
fn redirecao_erro_login(flag: &'static str) -> Response {
    let mut resp = Redirect::to(&format!("/login?erro={flag}")).into_response();
    resp.headers_mut()
        .insert(x_robots_tag(), HeaderValue::from_static(NOINDEX));
    resp
}

async fn logout(State(st): State<PanelState>, req: Request) -> Response {
    // "sair de todos os dispositivos" (fatia 8): corpo com todos=1 revoga
    // TODAS as sessões — essencial com sessões de 30 dias. Leitura CRUA do
    // corpo (o extractor Form exige campos; aqui é um checkbox opcional) e
    // token capturado ANTES de consumir o corpo.
    let token = panel_auth::token_do_header(req.headers());
    let global = match axum::body::to_bytes(req.into_body(), 1024).await {
        Ok(corpo) => corpo.windows(7).any(|w| w == b"todos=1"),
        Err(_) => false,
    };
    if global {
        st.auth.sessions.revogar_todas();
        crate::events::event(
            &st.app.state,
            "info",
            "painel",
            "logout GLOBAL (todas as sessões)",
        );
    } else if let Some(t) = token {
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
/// header E no HTML (R22 — vale para 404 do painel também).
/// Referrer `strict-origin-when-cross-origin` (default do navegador): o
/// painel FORA da LAN prova origem pelo Referer da própria página — o
/// `no-referrer` antigo quebrava a regra "Here Maps" da API pública
/// (navegador não mandava Referer nenhum → 403 em tudo, achado ao vivo no
/// 4G do Gabriel, 03/11). Cross-origin continua sem URL completa.
fn resposta(status: StatusCode, page: String) -> Response {
    (
        status,
        [
            (header::CACHE_CONTROL, "no-store"),
            (x_robots_tag(), NOINDEX),
            (header::REFERRER_POLICY, "strict-origin-when-cross-origin"),
        ],
        Html(page),
    )
        .into_response()
}

/// GET / — SHELL estática do dashboard (fatia 5): dados por /api/dashboard,
/// ao vivo por /api/dashboard/follow (push só quando muda), saúde por
/// /api/saude. A flag ?rescan=1 vira aviso no cliente (o nome de projeto
/// nunca volta na URL — regra do post/redirect/get mantida).
async fn dashboard() -> Response {
    resposta(StatusCode::OK, SHELL_DASHBOARD.to_owned())
}

/// GET /observe — SHELL estática (fatia 7): séries por /api/observe
/// (mesmas consultas do render SVG), gráficos em Chart.js vendido.
async fn observe_shell() -> Response {
    resposta(StatusCode::OK, SHELL_OBSERVE.to_owned())
}

/// GET /projects/{name} — SHELL estática (fatia 6a): snapshot por
/// /api/projects/{name} (SSE), logs por /api/projects/{name}/logs. O 404
/// honesto de não-registrado acontece na API (o cliente mostra).
async fn detalhe_projeto() -> Response {
    resposta(StatusCode::OK, SHELL_PROJETO.to_owned())
}

/// GET /events — SHELL estático (fatia 4, style guide 02/11): o HTML não
/// carrega dado; a lista/paginação vem de /api/events e o ao vivo do SSE
/// /api/events/follow. O Rust não renderiza mais HTML desta página — os
/// filtros continuam sendo verdade do SQL (leitura::filtro_de).
async fn pagina_eventos() -> Response {
    resposta(StatusCode::OK, SHELL_EVENTOS.to_owned())
}

/// POST /projects/{name}/rescan: pede re-scan da sessão de sync do projeto
/// (quem executa é o `flux watch`). Padrão post/redirect/get: sucesso volta
/// ao dashboard com a confirmação na query; erro NÃO redireciona — a página
/// renderiza com a mensagem real (503, o painel não finge que deu certo).
/// Todo resultado vira evento — nada silencioso.
async fn rescan_projeto(State(app): State<Arc<AppState>>, params: RawPathParams) -> Response {
    // RawPathParams não tem `get` nesta versão do axum: varre os pares.
    let name = params
        .iter()
        .find(|(k, _)| *k == "name")
        .map(|(_, v)| v)
        .unwrap_or_default();
    match crate::sync::request_rescan(&app, name) {
        Ok(()) => {
            crate::events::event(
                &app.state,
                "info",
                "painel",
                &format!("rescan solicitado pelo painel: {name}"),
            );
            Redirect::to("/?rescan=1").into_response()
        }
        Err(msg) => {
            // Shell estática: a causa mora no evento (o painel de eventos
            // mostra); a URL leva só a flag — nome/causa não vazam pra
            // histórico de navegação.
            crate::events::event(
                &app.state,
                "warn",
                "painel",
                &format!("rescan recusado para {name}: {msg}"),
            );
            Redirect::to("/?rescan=falha").into_response()
        }
    }
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

mod formato;
mod leitura;
mod paginas;
#[cfg(test)]
mod tests;

use paginas::{layout, render_login};

pub(super) use crate::AppState;
