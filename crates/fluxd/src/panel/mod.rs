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
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};

use crate::observe_page;
use crate::panel_auth;
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

async fn dashboard(State(app): State<Arc<AppState>>, RawQuery(q): RawQuery) -> Response {
    // Confirmação do rescan (post/redirect/get): flag na query, texto na
    // página — nome de projeto nunca volta na URL.
    let aviso = if q
        .as_deref()
        .is_some_and(|s| s.split('&').any(|p| p == "rescan=1"))
    {
        Some("rescan queued — o flux watch executa")
    } else {
        None
    };
    resposta(StatusCode::OK, render_dashboard(&app, aviso))
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
            crate::events::event(
                &app.state,
                "warn",
                "painel",
                &format!("rescan recusado para {name}: {msg}"),
            );
            resposta(
                StatusCode::SERVICE_UNAVAILABLE,
                render_dashboard(
                    &app,
                    Some(&format!("Falha ao pedir rescan de {name}: {msg}")),
                ),
            )
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

use formato::page_de;
use paginas::{
    detalhe_status, layout, render_dashboard, render_detalhe, render_eventos, render_login,
};

pub(super) use crate::AppState;
