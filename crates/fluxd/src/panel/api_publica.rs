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

use std::collections::VecDeque;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{ConnectInfo, Path, RawQuery, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
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
    use axum::routing::post;
    Router::new()
        .route("/api/events", get(eventos))
        .route("/api/events/follow", get(eventos_follow))
        .route("/api/dashboard", get(dashboard))
        .route("/api/dashboard/follow", get(dashboard_follow))
        .route("/api/saude", get(saude))
        .route("/api/projects/{name}/acordar", post(acordar))
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
        // READ-ONLY por contrato: o token existe pros agentes LEREM a
        // fábrica; ação (POST/acordar) é coisa de sessão do painel.
        if req.method() != axum::http::Method::GET {
            recusa(&st, ip, "token de leitura tentou AÇÃO (só GET é leitura)");
            return erro_json(
                StatusCode::FORBIDDEN,
                "token de leitura não executa ações — entre no painel (sessão)",
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
    /// Escopos distintos — os chips do filtro nunca inventam opção (mesma
    /// leitura da página server-side antiga).
    escopos: Vec<String>,
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
                escopos: leitura::escopos_distintos(&st.app).unwrap_or_default(),
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

// ------------------------------------------------------------------ sse

/// `: ping` a cada 15 s — CF e navegador não cortam conexão SSE ociosa.
const PING_CADA: Duration = Duration::from_secs(15);

/// Frame SSE de um evento: JSON na linha `data` (o cliente faz parse e
/// appenda — o `id` é a chave de dedupe/posição).
fn frame_de(id: i64, created_at: &str, level: &str, scope: &str, message: &str) -> String {
    format!(
        "data: {}\n\n",
        serde_json::json!({
            "id": id,
            "created_at": created_at,
            "level": level,
            "scope": scope,
            "message": message,
        })
    )
}

/// Espelho EM MEMÓRIA do `clausula_filtro` (SQL) para o que chega AO VIVO
/// pelo broadcast. O backlog e o re-sync passam pelo SQL real — aqui é só
/// o espelho: escopo/nível por igualdade, busca por substring insensível a
/// caixa (≈ o LIKE do SQLite, ASCII-insensitive).
fn casa_filtro(f: &leitura::FiltroEventos, ev: &crate::events::EventoNovo) -> bool {
    if let Some(s) = &f.scope
        && ev.scope != *s
    {
        return false;
    }
    if let Some(n) = &f.nivel
        && ev.level != *n
    {
        return false;
    }
    if let Some(q) = &f.busca
        && !ev.message.to_lowercase().contains(&q.to_lowercase())
    {
        return false;
    }
    true
}

/// `GET /api/events/follow?escopo=&nivel=&q=&desde=<id>` — SSE dos eventos
/// novos (mesmos filtros da lista). Ordem de arranque: ASSINA o broadcast
/// e SÓ DEPOIS consulta o backlog (id > `desde`, ordem cronológica) — a
/// janela de corrida vira no máximo uma duplicata, que o cliente mata por
/// `id`. `: ping` a cada 15 s. Cliente lento demais (Lagged do broadcast):
/// re-sync do BANCO pelo último id enviado — buraco no stream é falha
/// silenciosa, e aqui falha silenciosa é proibido.
async fn eventos_follow(State(st): State<PanelState>, RawQuery(q): RawQuery) -> Response {
    let filtro = leitura::filtro_de(q.as_deref());
    let desde = q
        .as_deref()
        .and_then(|s| s.split('&').find_map(|kv| kv.strip_prefix("desde=")))
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0);

    let rx = crate::events::assinar();
    let backlog = match leitura::eventos_desde(&st.app, desde, &filtro) {
        Ok(b) => b,
        Err(e) => {
            return erro_json(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("falha no backlog do stream: {e}"),
            );
        }
    };
    let ultimo = backlog.last().map(|ev| ev.id).unwrap_or(desde);
    let fila: VecDeque<Result<String, Infallible>> = backlog
        .iter()
        .map(|ev| {
            Ok(frame_de(
                ev.id,
                &ev.created_at,
                &ev.level,
                &ev.scope,
                &ev.message,
            ))
        })
        .collect();

    let app = Arc::clone(&st.app);
    let frames = futures_util::stream::unfold(
        (
            rx,
            tokio::time::interval(PING_CADA),
            ultimo,
            filtro,
            app,
            fila,
        ),
        |(mut rx, mut tick, mut ultimo, filtro, app, mut fila)| async move {
            loop {
                if let Some(frame) = fila.pop_front() {
                    return Some((frame, (rx, tick, ultimo, filtro, app, fila)));
                }
                tokio::select! {
                    _ = tick.tick() => {
                        return Some((
                            Ok(": ping\n\n".to_owned()),
                            (rx, tick, ultimo, filtro, app, fila),
                        ));
                    }
                    ev = rx.recv() => match ev {
                        Ok(ev) => {
                            if ev.id <= ultimo {
                                continue; // duplicata backlog×broadcast
                            }
                            if !casa_filtro(&filtro, &ev) {
                                continue;
                            }
                            ultimo = ev.id;
                            let frame =
                                Ok(frame_de(ev.id, &ev.created_at, &ev.level, &ev.scope, &ev.message));
                            return Some((frame, (rx, tick, ultimo, filtro, app, fila)));
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!("cliente de /api/events/follow ficou atrás ({n}) — re-sync do banco");
                            match leitura::eventos_desde(&app, ultimo, &filtro) {
                                Ok(ausentes) => {
                                    if let Some(m) = ausentes.last() {
                                        ultimo = m.id;
                                    }
                                    fila.extend(ausentes.iter().map(|ev| {
                                        Ok(frame_de(
                                            ev.id, &ev.created_at, &ev.level, &ev.scope, &ev.message,
                                        ))
                                    }));
                                    continue;
                                }
                                Err(e) => {
                                    // Visível NO stream (frame de erro) — nunca some.
                                    let frame = Ok(format!(
                                        "event: erro\ndata: {}\n\n",
                                        serde_json::json!({ "error": format!("falha no re-sync: {e}") })
                                    ));
                                    return Some((frame, (rx, tick, ultimo, filtro, app, fila)));
                                }
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
                    }
                }
            }
        },
    );
    resposta_sse(frames)
}

/// Resposta SSE com os headers de praxe (mesmo par do SSE de logs do
/// ciclo.rs).
fn resposta_sse(
    frames: impl futures_util::Stream<Item = Result<String, Infallible>> + Send + 'static,
) -> Response {
    let mut res = Response::new(Body::from_stream(frames));
    *res.status_mut() = StatusCode::OK;
    res.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    res.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    res
}

// ------------------------------------------------------- token de leitura

/// Arquivo com o VALOR CLARO do token de leitura na VM (mesmo diretório do
/// `panel.argon2`). O hash vai pro settings; quem precisa do valor lê o
/// arquivo — o CLI `flux token eventos` faz o cat por SSH. MESMO caminho
/// hardcoded no CLI (comandos/tokens.rs): caminho de VM é contrato dos dois
/// lados, igual ao flux.sqlite3 lá.
pub(crate) const TOKEN_ARQUIVO: &str = "/srv/flux/secrets/eventos.token";

/// 64 chars hex de 32 bytes do CSPRNG do SO — mesmo contrato do token da
/// API de controle (`flux token generate`).
fn token_novo_hex() -> String {
    let mut buf = [0u8; 32];
    getrandom::fill(&mut buf).expect("CSPRNG do SO indisponível — não há token seguro sem isso");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// Garante o token de leitura configurado no boot (`serve`): settings sem
/// hash → gera token, escreve o valor claro em `TOKEN_ARQUIVO` (0600) e
/// grava o hash. FALHA NUNCA derruba o daemon: vira error no log + evento
/// warn e a API segue fail-closed (503) até resolver. Hash presente sem
/// arquivo: NÃO regenera (revogar é `flux token eventos --gerar`) — só
/// avisa. Arquivo antes do hash: se a gravação do hash falhar, o arquivo
/// fica órfão e o PRÓXIMO boot regenera por cima — sem token zumbi.
pub(crate) fn garantir_token_leitura(st: &DbState) {
    garantir_token_leitura_em(st, std::path::Path::new(TOKEN_ARQUIVO));
}

/// Mesmo fluxo com o caminho injetado — os testes usam tempdir em vez do
/// /srv/flux/secrets da VM.
pub(crate) fn garantir_token_leitura_em(st: &DbState, arquivo: &std::path::Path) {
    if token_hash(st).is_some() {
        if !arquivo.exists() {
            tracing::warn!(
                "{TOKEN_KEY} existe mas {} não — `flux token eventos --gerar` recria o arquivo",
                arquivo.display()
            );
        }
        return;
    }
    let token = token_novo_hex();
    if let Err(e) = gravar_arquivo_token(arquivo, &token) {
        let causa = format!(
            "falha escrevendo {}: {e} — token de leitura NÃO gerado",
            arquivo.display()
        );
        tracing::error!("{causa}");
        events::event(st, "warn", "painel", &causa);
        return;
    }
    let hash = sha256_hex(token.as_bytes());
    let gravado = st.with(|c| {
        c.execute(
            "INSERT INTO settings(k, v) VALUES (?1, ?2)
             ON CONFLICT(k) DO UPDATE SET v = excluded.v",
            params![TOKEN_KEY, hash],
        )
    });
    match gravado {
        Ok(_) => {
            tracing::info!(
                "token de leitura da API pública gerado em {}",
                arquivo.display()
            );
            events::event(
                st,
                "info",
                "painel",
                "api pública de leitura: token gerado — valor claro em /srv/flux/secrets/eventos.token (`flux token eventos` mostra)",
            );
        }
        Err(e) => {
            // Arquivo órfão de propósito: o próximo boot regenera o par.
            tracing::error!("falha gravando {TOKEN_KEY}: {e} — regenero no próximo boot");
        }
    }
}

fn gravar_arquivo_token(arquivo: &std::path::Path, token: &str) -> std::io::Result<()> {
    std::fs::write(arquivo, token)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(arquivo, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

// ---------------------------------------------------- dashboard (fatia 5)

/// GET /api/dashboard — snapshot completo (MESMAS consultas do render HTML
/// antigo; a saúde pesada tem endpoint próprio, /api/saude).
async fn dashboard(State(st): State<PanelState>) -> Response {
    Json(leitura::dashboard(&st.app)).into_response()
}

/// GET /api/saude — o doctor (mesma máquina do `flux doctor`), puxado pelo
/// dot do header a cada minuto. Sem Docker a resposta é honesta sobre o
/// que não deu pra checar.
async fn saude(State(st): State<PanelState>) -> Response {
    match crate::doctor::checar_todos(&st.app).await {
        Ok(rel) => Json(rel).into_response(),
        Err(e) => erro_json(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("falha no doctor: {e}"),
        ),
    }
}

/// Intervalo entre snapshots do dashboard — push SÓ QUANDO MUDA (decisão
/// do escopo): hash sha256 do JSON; igual = nada no fio.
const DASH_CADA: Duration = Duration::from_secs(5);

/// GET /api/dashboard/follow — SSE do snapshot: recomputa a cada 5 s e só
/// empurra quando muda; `: ping` a cada 15 s entre mudanças (o custo no
/// servidor é o mesmo do meta refresh de 5 s que o dashboard carregava —
/// o ganho é o cliente sem recarga e sem estado perdido).
async fn dashboard_follow(State(st): State<PanelState>) -> Response {
    let app = Arc::clone(&st.app);
    let frames = futures_util::stream::unfold(
        (
            tokio::time::interval(DASH_CADA),
            tokio::time::interval(PING_CADA),
            None::<String>,
            app,
        ),
        |(mut snap_t, mut ping_t, mut ultimo, app)| async move {
            loop {
                tokio::select! {
                    _ = ping_t.tick() => {
                        return Some((
                            Ok(": ping\n\n".to_owned()),
                            (snap_t, ping_t, ultimo, app),
                        ));
                    }
                    _ = snap_t.tick() => {
                        let json =
                            serde_json::to_string(&leitura::dashboard(&app)).unwrap_or_default();
                        let hash = sha256_hex(json.as_bytes());
                        if ultimo.as_deref() == Some(hash.as_str()) {
                            continue; // nada mudou: silêncio É o comportamento
                        }
                        ultimo = Some(hash);
                        return Some((
                            Ok(format!("data: {json}\n\n")),
                            (snap_t, ping_t, ultimo, app),
                        ));
                    }
                }
            }
        },
    );
    resposta_sse(frames)
}

/// POST /api/projects/{name}/acordar — AÇÃO (sessão; o middleware barra o
/// token de leitura). Mesmo `wake` da página de espera: banco próprio
/// acorda ANTES, containers parados do projeto sobem. 202 com o que
/// aconteceu; falha vira o status certo com a causa, nunca silêncio.
async fn acordar(State(st): State<PanelState>, Path(name): Path<String>) -> Response {
    if !flux_common::nomes::projeto_valido(&name) {
        return erro_json(StatusCode::NOT_FOUND, "nome de projeto inválido");
    }
    match leitura::projetos(&st.app) {
        Ok(lista) if lista.iter().any(|(n, _)| *n == name) => {}
        Ok(_) => {
            return erro_json(
                StatusCode::NOT_FOUND,
                &format!("{name}: não registrado no fluxd"),
            );
        }
        Err(e) => {
            return erro_json(
                StatusCode::SERVICE_UNAVAILABLE,
                &format!("estado ilegível: {e}"),
            );
        }
    }
    let Some(docker) = st.app.docker.clone() else {
        return erro_json(
            StatusCode::SERVICE_UNAVAILABLE,
            "fluxd sem Docker conectado — acordar só na VM",
        );
    };
    // DockerSource embrulha o cliente bollard + estado (mesma montagem do
    // router nas páginas de espera).
    match crate::docker::DockerSource::new(docker, &st.app.state)
        .wake(&name, flux_common::protocol::Env::Dev)
        .await
    {
        Ok(acordou) => {
            events::event(
                &st.app.state,
                "info",
                "painel",
                &format!("acordar {name}/dev pelo painel: containers parados = {acordou}"),
            );
            (
                StatusCode::ACCEPTED,
                Json(serde_json::json!({ "projeto": name, "acordou": acordou })),
            )
                .into_response()
        }
        Err(e) => erro_json(StatusCode::CONFLICT, &format!("wake de {name} falhou: {e}")),
    }
}
