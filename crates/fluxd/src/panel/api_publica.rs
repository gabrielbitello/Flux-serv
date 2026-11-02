//! API pública de LEITURA no host do painel (escopo fechado com o Gabriel,
//! 31/10–02/11): JSON para agentes (token read-only PRÓPRIO, que só vale
//! dentro da LAN) e para o próprio painel novo (sessão do Argon2; fora da
//! LAN exige Origin/Referer do painel — regra "Here Maps", pedido dele).
//! Toda rota daqui é GET e não muta nada: escrita continua SÓ na :7420
//! (token forte, 127.0.0.1). Recusa NUNCA é silenciosa (evento `auth` com
//! IP real, mesma postura do gate da API de controle).
//!
//! IP real: MESMA régua do WAF (`waf::fonte_externa` — cf-connecting-ip >
//! XFF > ConnectInfo; `None` = externo, fail-closed). Via Cloudflare TODO
//! IP chega externo — máquina da LAN usa hosts file apontando direto pra
//! VM (padrão zcode-api-d), e é esse o desenho, não um bug.

use std::net::SocketAddr;

use axum::extract::{ConnectInfo, RawQuery, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use rusqlite::params;

use super::PanelState;
use super::formato::page_de;
use super::leitura;
use crate::api::auth::{bearer_token, hash_confere, sha256_hex};
use crate::events;
use crate::state::State as DbState;
use crate::waf;

/// Chave do token de LEITURA — distinto do `api_token_hash` da :7420 de
/// propósito: vazar este token NÃO dá up/down/db de escrita.
const TOKEN_KEY: &str = "eventos_token_hash";

/// Sub-router da API pública — montado no host do painel, FORA do guard de
/// sessão (tem auth própria). Rotas novas de leitura entram aqui e herdam a
/// camada de uma vez. SEM `with_state`: o `with_state(st)` único do
/// `build_panel` fecha o tipo, igual ao sub-router `protegido`.
pub(super) fn router(st: &PanelState) -> Router<PanelState> {
    Router::new()
        .route("/api/events", get(eventos))
        .route_layer(middleware::from_fn_with_state(st.clone(), auth_publica))
}

// ------------------------------------------------------------------ auth

/// A camada de auth da API pública, em ordem:
/// 1. `Authorization: Bearer <token de leitura>` — precisa casar com o
///    `eventos_token_hash` E virar da LAN. De fora, MESMO com token certo,
///    é 403 (o token existe pros agentes da rede do Gabriel, não pra
///    internet).
/// 2. Sessão do painel (cookie do login Argon2). De fora da LAN exige
///    Origin/Referer apontando pro próprio painel — GET direto de curl com
///    cookie roubado, sem origem, não passa (Here Maps).
/// 3. Nada disso = 401.
pub(super) async fn auth_publica(
    State(st): State<PanelState>,
    req: Request,
    next: Next,
) -> Response {
    let headers = req.headers().clone();
    let ip = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(a)| a.ip());
    let externo = waf::fonte_externa(&headers, ip);

    if let Some(token) = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(bearer_token)
    {
        let Some(hash) = token_hash(&st.app.state) else {
            return erro_json(
                StatusCode::SERVICE_UNAVAILABLE,
                "api pública de leitura não configurada: não há eventos_token_hash no estado — `flux token eventos` gera",
            );
        };
        if !hash_confere(&sha256_hex(token.as_bytes()), &hash) {
            recusa(
                &st,
                ip,
                "token de leitura recusado (ausente no header, malformado ou hash divergente)",
            );
            return erro_json(StatusCode::UNAUTHORIZED, "token de leitura recusado");
        }
        if externo {
            recusa(
                &st,
                ip,
                "token de leitura usado de FORA da LAN — ele só vale na rede do Gabriel",
            );
            return erro_json(
                StatusCode::FORBIDDEN,
                "token de leitura só vale dentro da LAN",
            );
        }
        return next.run(req).await;
    }

    if let Some(t) = super::panel_auth::token_do_header(&headers)
        && st.auth.sessions.validate(&t)
    {
        if externo && !origem_do_painel(&headers, &st.app.domain.panel_host) {
            recusa(
                &st,
                ip,
                "sessão válida de FORA da LAN sem Origin/Referer do painel (GET direto na API)",
            );
            return erro_json(
                StatusCode::FORBIDDEN,
                "fora da LAN a API só responde para a própria página do painel",
            );
        }
        return next.run(req).await;
    }

    recusa(&st, ip, "api pública sem token e sem sessão");
    erro_json(
        StatusCode::UNAUTHORIZED,
        "sem credencial: token de leitura (na LAN) ou sessão do painel",
    )
}

/// Registro da recusa: warn scope `auth` com o IP real (mesmo escopo do
/// gate da :7420 — o painel de eventos mostra a tentativa).
fn recusa(st: &PanelState, ip: Option<std::net::IpAddr>, causa: &str) {
    events::event_com_ip(
        &st.app.state,
        "warn",
        "auth",
        &format!("api pública de leitura: {causa}"),
        &ip.map(|i| i.to_string()).unwrap_or_else(|| "sem-ip".into()),
    );
}

/// Lê o hash do token de leitura. Fail-closed igual ao da :7420: erro de
/// banco não vira porta aberta.
fn token_hash(st: &DbState) -> Option<String> {
    match st.with(|c| {
        c.query_row(
            "SELECT v FROM settings WHERE k = ?1",
            params![TOKEN_KEY],
            |r| r.get::<_, String>(0),
        )
    }) {
        Ok(h) => Some(h),
        Err(crate::state::StateError::Sql(rusqlite::Error::QueryReturnedNoRows)) => None,
        Err(e) => {
            tracing::error!("não consegui ler {TOKEN_KEY} do estado — recusando: {e}");
            None
        }
    }
}

/// Regra Here Maps: fora da LAN, a chamada precisa vir da página do painel
/// (Origin do fetch/SSE ou Referer do navegador apontando pro painel).
fn origem_do_painel(headers: &HeaderMap, painel_host: &str) -> bool {
    ["origin", "referer"].iter().any(|k| {
        headers
            .get(*k)
            .and_then(|v| v.to_str().ok())
            .and_then(autoridade)
            .is_some_and(|h| h.eq_ignore_ascii_case(painel_host))
    })
}

/// `https://flux.bitello.xyz/events?q=x` → `flux.bitello.xyz` (sem porta).
fn autoridade(url: &str) -> Option<String> {
    let resto = url.split_once("://")?.1;
    let fim = resto.find(['/', '?']).unwrap_or(resto.len());
    let autoridade = &resto[..fim];
    let sem_porta = autoridade
        .rsplit_once(':')
        .filter(|(host, porta)| !host.is_empty() && porta.bytes().all(|c| c.is_ascii_digit()))
        .map(|(host, _)| host)
        .unwrap_or(autoridade);
    Some(sem_porta.to_owned())
}

// ------------------------------------------------------------------ rotas

#[derive(serde::Serialize)]
struct EventoJson {
    id: i64,
    created_at: String,
    level: String,
    scope: String,
    message: String,
}

#[derive(serde::Serialize)]
struct ListaEventos {
    pagina: i64,
    paginas: i64,
    total: i64,
    eventos: Vec<EventoJson>,
}

/// `GET /api/events?escopo=&nivel=&q=&page=` — MESMA leitura (e MESMOS
/// filtros) da página /events, em JSON. O `id` é a chave que o cliente novo
/// usa pra append/scroll compensado sem perder a posição.
async fn eventos(State(st): State<PanelState>, RawQuery(q): RawQuery) -> Response {
    let filtro = leitura::filtro_de(q.as_deref());
    match leitura::eventos_filtrados(
        &st.app,
        page_de(q.as_deref()),
        leitura::EVENTOS_POR_PAGINA,
        &filtro,
    ) {
        Ok((page, rows, total)) => {
            let paginas =
                ((total + leitura::EVENTOS_POR_PAGINA - 1) / leitura::EVENTOS_POR_PAGINA).max(1);
            Json(ListaEventos {
                pagina: page,
                paginas,
                total,
                eventos: rows
                    .into_iter()
                    .map(|ev| EventoJson {
                        id: ev.id,
                        created_at: ev.created_at,
                        level: ev.level,
                        scope: ev.scope,
                        message: ev.message,
                    })
                    .collect(),
            })
            .into_response()
        }
        Err(e) => erro_json(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("falha lendo eventos: {e}"),
        ),
    }
}

fn erro_json(status: StatusCode, msg: &str) -> Response {
    (status, Json(serde_json::json!({ "error": msg }))).into_response()
}
