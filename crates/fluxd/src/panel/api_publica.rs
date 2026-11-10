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
use crate::AppState;
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
        .route("/api/projects/{name}", get(detalhe))
        .route("/api/projects/{name}/follow", get(detalhe_follow))
        // Tail de logs: MESMO handler da :7420 (text/plain ou SSE follow) —
        // read-only, agora também com auth de sessão/token da API pública.
        .route("/api/projects/{name}/logs", get(crate::ciclo::logs))
        .route("/api/projects/{name}/acordar", post(acordar))
        .route(
            "/api/projects/{name}/acoes",
            post(super::acoes::disparar_acao),
        )
        .route(
            "/api/projects/{name}/acoes/atual",
            get(super::acoes::acao_atual),
        )
        .route(
            "/api/projects/{name}/acoes/stream",
            get(super::acoes::acao_stream),
        )
        .route("/api/observe", get(observe))
        .route("/api/observe/follow", get(observe_follow))
        .route(
            "/api/vigia/limiares",
            get(vigia_limiares).post(vigia_set_limiar),
        )
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
    // `Sec-Fetch-Site: same-origin` — header de fetch-metadata que o
    // NAVEGADOR escreve e o JS/curl não pode forjar: prova de origem mais
    // forte que Referer (que política de referrer pode esconder).
    if headers
        .get("sec-fetch-site")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v == "same-origin")
    {
        return true;
    }
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

/// IP real do cliente como STRING — MESMA régua `waf::ip_real` do access
/// (cf-connecting-ip > XFF > ConnectInfo). Vai no JSON como `meu_ip`: o
/// painel marca os eventos "é você" (toggle ocultar meus eventos, 03/11) sem
/// parsear a mensagem e sem eu decidir IP por UA.
fn meu_ip_de(headers: &HeaderMap, req: &Request) -> String {
    let connect = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(a)| a.ip());
    waf::ip_real(headers, connect)
        .map(|i| i.to_string())
        .unwrap_or_default()
}

/// Janela da curva em minutos (`?minutos=`): 15|30|60|360|720|1440 e o
/// realtime (10) do cliente — clamp 1..=1440, default 24 h. Valor inválido
/// não é erro: cai no default (a curva é leve, a página é a fonte).
fn minutos_de(q: Option<&str>) -> usize {
    q.and_then(|s| s.split('&').find_map(|kv| kv.strip_prefix("minutos=")))
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(leitura::CURVA_TETO_MINUTOS)
        .clamp(1, leitura::CURVA_TETO_MINUTOS)
}

// ------------------------------------------------------------------ rotas

/// Evento no JSON da lista: o `EventoRow` do leitura DIRETO — mesmo struct
/// do frame SSE (v7 ip/host, v8 projeto/env/st), sem espelho de campos.
type EventoJson = leitura::EventoRow;

#[derive(serde::Serialize)]
struct ListaEventos {
    pagina: i64,
    paginas: i64,
    total: i64,
    /// IP real do cliente da request (mesma régua do access) — o toggle
    /// "ocultar meus eventos" casa evento.ip == meu_ip (03/11).
    meu_ip: String,
    /// Escopos distintos — os chips do filtro nunca inventam opção (mesma
    /// leitura da página server-side antiga).
    escopos: Vec<String>,
    /// Hosts distintos (access, schema v7) com a classificação dev/prod —
    /// filtro de "projeto de origem".
    hosts: Vec<HostJson>,
    eventos: Vec<EventoJson>,
}

#[derive(serde::Serialize)]
pub(super) struct HostJson {
    pub(super) host: String,
    pub(super) ambiente: String,
}

/// `GET /api/events?escopo=&nivel=&q=&page=` — MESMA leitura (e MESMOS
/// filtros) da página /events, em JSON. O `id` é a chave que o cliente novo
/// usa pra append/scroll compensado sem perder a posição.
async fn eventos(
    State(st): State<PanelState>,
    headers: HeaderMap,
    RawQuery(q): RawQuery,
    req: Request,
) -> Response {
    let filtro = leitura::filtro_de(q.as_deref());
    let meu_ip = meu_ip_de(&headers, &req);
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
                meu_ip,
                escopos: leitura::escopos_distintos(&st.app).unwrap_or_default(),
                hosts: leitura::hosts_distintos(&st.app)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|(host, ambiente)| HostJson { host, ambiente })
                    .collect(),
                eventos: rows.into_iter().collect(),
            })
            .into_response()
        }
        Err(e) => erro_json(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("falha lendo eventos: {e}"),
        ),
    }
}

pub(super) fn erro_json(status: StatusCode, msg: &str) -> Response {
    (status, Json(serde_json::json!({ "error": msg }))).into_response()
}

// ------------------------------------------------------------------ sse

/// `: ping` a cada 15 s — CF e navegador não cortam conexão SSE ociosa.
const PING_CADA: Duration = Duration::from_secs(15);

/// Frame SSE de um evento: JSON na linha `data` (o cliente faz parse e
/// appenda — o `id` é a chave de dedupe/posição). Genérico sobre o tipo da
/// linha (EventoNovo do broadcast, EventoRow do backlog) — MESMOS campos do
/// JSON da lista (v7 ip/host, v8 projeto/env/st): uma definição, zero
/// espelho de campos.
fn frame_de<T: serde::Serialize>(ev: &T) -> String {
    format!(
        "data: {}\n\n",
        serde_json::to_string(ev).expect("serialização de evento não falha (campos planos)")
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
    if let Some(p) = &f.projeto {
        // Espelho do clausula_filtro: coluna nos access v8+; linha antiga/
        // interna (coluna vazia) casa pelo nome citado na mensagem.
        let casa = if ev.projeto.is_empty() {
            ev.message.to_lowercase().contains(&p.to_lowercase())
        } else {
            &ev.projeto == p
        };
        if !casa {
            return false;
        }
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
    let fila: VecDeque<Result<String, Infallible>> =
        backlog.iter().map(|ev| Ok(frame_de(ev))).collect();

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
                            let frame = Ok(frame_de(&ev));
                            return Some((frame, (rx, tick, ultimo, filtro, app, fila)));
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!("cliente de /api/events/follow ficou atrás ({n}) — re-sync do banco");
                            match leitura::eventos_desde(&app, ultimo, &filtro) {
                                Ok(ausentes) => {
                                    if let Some(m) = ausentes.last() {
                                        ultimo = m.id;
                                    }
                                    fila.extend(
                                        ausentes.iter().map(|ev| Ok(frame_de(ev))),
                                    );
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
pub(super) fn resposta_sse(
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

/// GET /api/dashboard?minutos= — snapshot completo (MESMAS consultas do
/// render HTML antigo; a saúde pesada tem endpoint próprio, /api/saude).
/// `minutos` é a janela da curva (clamp 1..=1440, default 24 h).
async fn dashboard(
    State(st): State<PanelState>,
    headers: HeaderMap,
    RawQuery(q): RawQuery,
    req: Request,
) -> Response {
    Json(leitura::dashboard(
        &st.app,
        minutos_de(q.as_deref()),
        meu_ip_de(&headers, &req),
    ))
    .into_response()
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

/// Janela mínima ENTRE dois pushes reagindo a eventos (03/11, pedido do
/// Gabriel para o realtime da curva): rajada de eventos coalesce num push só
/// a cada 300 ms — tempo real sem virar fio de snapshot.
const PUSH_CADA: Duration = Duration::from_millis(300);

/// Recomputa o snapshot (janela + meu_ip do cliente) e devolve o frame SE o
/// hash mudou desde o último push; atualiza o hash de referência.
async fn push_se_mudou(
    app: &AppState,
    ultimo: &mut Option<String>,
    meu_ip: &str,
    minutos: usize,
) -> Option<String> {
    let json = serde_json::to_string(&leitura::dashboard(app, minutos, meu_ip.to_owned()))
        .unwrap_or_default();
    let hash = sha256_hex(json.as_bytes());
    if ultimo.as_deref() == Some(hash.as_str()) {
        return None;
    }
    *ultimo = Some(hash);
    Some(format!("data: {json}\n\n"))
}

/// GET /api/dashboard/follow?minutos= — SSE do snapshot, push SÓ QUANDO
/// MUDA. Três fontes de acordar: evento novo no broadcast (o realtime da
/// curva — coalescido na janela PUSH_CADA de 300 ms), re-scan periódico de
/// 5 s (mudanças que NÃO são evento: sono, sync, migrations) e `: ping` de
/// 15 s entre mudanças. Lagged do broadcast não é buraco: o snapshot é
/// recompute stateless — o tick de 5 s cura sozinho.
async fn dashboard_follow(
    State(st): State<PanelState>,
    headers: HeaderMap,
    RawQuery(q): RawQuery,
    req: Request,
) -> Response {
    let minutos = minutos_de(q.as_deref());
    let meu_ip = meu_ip_de(&headers, &req);
    let app = Arc::clone(&st.app);
    let rx = crate::events::assinar();
    let frames = futures_util::stream::unfold(
        (
            tokio::time::interval(DASH_CADA),
            tokio::time::interval(PING_CADA),
            rx,
            None::<String>,               // hash do último push
            None::<tokio::time::Instant>, // quando foi o último push
            false,                        // evento novo desde o último push
            app,
            meu_ip,
            minutos,
        ),
        |(
            mut snap_t,
            mut ping_t,
            mut rx,
            mut ultimo,
            mut ultimo_push,
            mut sujou,
            app,
            meu_ip,
            minutos,
        )| async move {
            loop {
                tokio::select! {
                    _ = ping_t.tick() => {
                        return Some((
                            Ok(": ping\n\n".to_owned()),
                            (snap_t, ping_t, rx, ultimo, ultimo_push, sujou, app, meu_ip, minutos),
                        ));
                    }
                    // Re-scan periódico — mudanças sem evento novo também
                    // chegam (dot do sono, sync, migrations aplicadas).
                    _ = snap_t.tick() => {
                        if let Some(frame) =
                            push_se_mudou(&app, &mut ultimo, &meu_ip, minutos).await
                        {
                            ultimo_push = Some(tokio::time::Instant::now());
                            return Some((
                                Ok(frame),
                                (snap_t, ping_t, rx, ultimo, ultimo_push, sujou, app, meu_ip, minutos),
                            ));
                        }
                    }
                    ev = rx.recv() => match ev {
                        Ok(_) => sujou = true,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            sujou = true; // o snapshot é recompute — sem buraco possível
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
                    },
                    // Realtime reagindo a evento novo: espera a janela de 300 ms
                    // desde o último push (rajada = UM push), depois recomputa —
                    // o hash pode nem mudar (evento fora do filtro de exibição).
                    _ = async {
                        if !sujou {
                            futures_util::future::pending::<()>().await;
                        }
                        let agora = tokio::time::Instant::now();
                        let liberado = ultimo_push.map(|t| t + PUSH_CADA).unwrap_or(agora);
                        if liberado > agora {
                            tokio::time::sleep(liberado - agora).await;
                        }
                    } => {
                        sujou = false;
                        if let Some(frame) =
                            push_se_mudou(&app, &mut ultimo, &meu_ip, minutos).await
                        {
                            ultimo_push = Some(tokio::time::Instant::now());
                            return Some((
                                Ok(frame),
                                (snap_t, ping_t, rx, ultimo, ultimo_push, sujou, app, meu_ip, minutos),
                            ));
                        }
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

// -------------------------------------------------- detalhe (fatia 6a)

/// GET /api/projects/{name} — snapshot do detalhe (mesmas consultas do
/// render HTML antigo). 404 honesto para não-registrado.
async fn detalhe(State(st): State<PanelState>, Path(name): Path<String>) -> Response {
    let live = crate::compose::live_status(&st.app, st.app.docker.as_ref(), &name).await;
    match leitura::detalhe(&st.app, &name, live) {
        Ok(Some(snap)) => Json(snap).into_response(),
        Ok(None) => erro_json(
            StatusCode::NOT_FOUND,
            &format!("{name}: não registrado no fluxd"),
        ),
        Err(e) => erro_json(StatusCode::SERVICE_UNAVAILABLE, &e),
    }
}

/// GET /api/projects/{name}/follow — SSE do snapshot do detalhe, MESMO
/// padrão do dashboard: recompute a cada 5 s, push só quando muda.
async fn detalhe_follow(State(st): State<PanelState>, Path(name): Path<String>) -> Response {
    let app = Arc::clone(&st.app);
    let frames = futures_util::stream::unfold(
        (
            tokio::time::interval(DASH_CADA),
            tokio::time::interval(PING_CADA),
            None::<String>,
            app,
            name,
        ),
        |(mut snap_t, mut ping_t, mut ultimo, app, name)| async move {
            loop {
                tokio::select! {
                    _ = ping_t.tick() => {
                        return Some((
                            Ok(": ping\n\n".to_owned()),
                            (snap_t, ping_t, ultimo, app, name),
                        ));
                    }
                    _ = snap_t.tick() => {
                        let live = crate::compose::live_status(&app, app.docker.as_ref(), &name)
                            .await;
                        let json = match leitura::detalhe(&app, &name, live) {
                            Ok(Some(snap)) => serde_json::to_string(&snap).unwrap_or_default(),
                            // Não-registrado: frame de erro no stream, nunca
                            // buraco silencioso.
                            Ok(None) => serde_json::json!({"erro": "não registrado"}).to_string(),
                            Err(e) => serde_json::json!({"erro": e}).to_string(),
                        };
                        let hash = sha256_hex(json.to_string().as_bytes());
                        if ultimo.as_deref() == Some(hash.as_str()) {
                            continue;
                        }
                        ultimo = Some(hash);
                        return Some((
                            Ok(format!("data: {json}\n\n")),
                            (snap_t, ping_t, ultimo, app, name),
                        ));
                    }
                }
            }
        },
    );
    resposta_sse(frames)
}

// -------------------------------------------------- observe (fatia 7)

/// Ponto da série em formato compacto pro Chart.js: [ts_epoch, valor].
fn ponto(t: i64, v: f64) -> serde_json::Value {
    serde_json::json!([t, v])
}

/// `GET /api/observe?projeto=&container=&metrica=cpu|mem|net_rx|net_tx&minutos=60|360|1440`
/// — MESMAS consultas do render SVG antigo (`observe::serie`,
/// `series_por_container`, `peaks`), em JSON. Zoom = minutos whitelist
/// (60/360/1440 — decisão de escopo: mesmos dados, janelas na tela).
async fn observe(State(st): State<PanelState>, RawQuery(q): RawQuery) -> Response {
    Json(observe_json(&st.app, q.as_deref())).into_response()
}

/// Corpo do /api/observe — compartilhado pelo GET e pelo follow (mesma
/// leitura, o hash decide o push).
fn observe_json(app: &AppState, query: Option<&str>) -> serde_json::Value {
    let param = |chave: &str| {
        query
            .and_then(|s| {
                s.split('&')
                    .find_map(|kv| kv.strip_prefix(chave)?.split_once('=').map(|(_, v)| v))
            })
            .map(|v| v.to_owned())
            .filter(|v| !v.is_empty())
    };
    let metrica = match param("metrica").as_deref() {
        Some("mem") => crate::observe::Metrica::Mem,
        Some("net_rx") => crate::observe::Metrica::NetRx,
        Some("net_tx") => crate::observe::Metrica::NetTx,
        _ => crate::observe::Metrica::Cpu,
    };
    let minutos = match param("minutos").as_deref() {
        Some("60") => 60,
        Some("360") => 360,
        _ => 1440,
    };
    let projeto = param("projeto");
    let container = param("container");

    let serie = crate::observe::serie(
        &app.state,
        projeto.as_deref(),
        container.as_deref(),
        metrica,
        minutos,
    );
    // Drill por container: mesma regra do render antigo (sem filtro de
    // container E com projeto escolhido).
    let por_container = match (&container, &projeto) {
        (None, Some(p)) => crate::observe::series_por_container(&app.state, p, metrica, minutos)
            .into_iter()
            .map(|(c, serie)| {
                serde_json::json!({
                    "container": c,
                    "serie": serie.iter().map(|(t, v)| ponto(*t, *v)).collect::<Vec<_>>(),
                })
            })
            .collect::<Vec<_>>(),
        _ => Vec::new(),
    };
    let picos = crate::observe::peaks(&app.state, minutos, projeto.as_deref())
        .into_iter()
        .map(|p| {
            serde_json::json!({
                "id": p.id, "ts": p.ts, "container": p.container,
                "projeto": p.compose_project, "service": p.service,
                "image": p.image, "metric": p.metric,
                "valor": p.valor, "threshold": p.threshold,
            })
        })
        .collect::<Vec<_>>();
    let projetos = app.state.list_projects().unwrap_or_default();

    serde_json::json!({
        "metrica": param("metrica").unwrap_or_else(|| "cpu".into()),
        "minutos": minutos,
        "projeto": projeto,
        "container": container,
        "projetos": projetos,
        "serie": serie.iter().map(|(t, v)| ponto(*t, *v)).collect::<Vec<_>>(),
        "por_container": por_container,
        "picos": picos,
    })
}

/// GET /api/observe/follow — SSE do observe: push só quando o hash das
/// séries muda (novo sample do ciclo de 30 s = mudança por definição).
async fn observe_follow(State(st): State<PanelState>, RawQuery(q): RawQuery) -> Response {
    let app = Arc::clone(&st.app);
    let query = q.unwrap_or_default();
    let frames = futures_util::stream::unfold(
        (
            tokio::time::interval(DASH_CADA),
            tokio::time::interval(PING_CADA),
            None::<String>,
            app,
            query,
        ),
        |(mut snap_t, mut ping_t, mut ultimo, app, query): (
            _,
            _,
            Option<String>,
            Arc<AppState>,
            String,
        )| async move {
            loop {
                tokio::select! {
                    _ = ping_t.tick() => {
                        return Some((
                            Ok(": ping\n\n".to_owned()),
                            (snap_t, ping_t, ultimo, app, query),
                        ));
                    }
                    _ = snap_t.tick() => {
                        // Mesma leitura do handler GET: refetch com os
                        // filtros da própria query.
                        let json = observe_json(&app, Some(&query));
                        let hash = sha256_hex(json.to_string().as_bytes());
                        if ultimo.as_deref() == Some(hash.as_str()) {
                            continue;
                        }
                        ultimo = Some(hash);
                        return Some((
                            Ok(format!("data: {json}\n\n")),
                            (snap_t, ping_t, ultimo, app, query),
                        ));
                    }
                }
            }
        },
    );
    resposta_sse(frames)
}

// -------------------------------------------------- vigia limiares (fatia 9b)

/// GET /api/vigia/limiares — default global + overrides por projeto com os
/// derivados efetivos (martelada/ws derivam do base, MESMA fórmula do
/// vigia.rs). Leitura: token ok.
async fn vigia_limiares(State(st): State<PanelState>) -> Response {
    let base_default = crate::vigia::BASE_RECARGA_POR_MIN;
    let derivados = |base: i64| {
        serde_json::json!({
            "recarga_por_min": base,
            "martelada_3min": (base * 7).max(crate::vigia::PISO_MARTELADA_3MIN),
            "ws_por_min": (base / 4).max(crate::vigia::PISO_WS_POR_MIN),
        })
    };
    let overrides = crate::vigia::bases(&st.app.state)
        .unwrap_or_default()
        .into_iter()
        .map(|(projeto, base)| {
            serde_json::json!({ "projeto": projeto, "base": base, "efetivo": derivados(base) })
        })
        .collect::<Vec<_>>();
    let ultimo = crate::panel::leitura::vigia_ultimo_ciclo(&st.app).unwrap_or(None);
    Json(serde_json::json!({
        "default": { "base": base_default, "efetivo": derivados(base_default) },
        "overrides": overrides,
        "ultimo_ciclo": ultimo,
        "cooldown_s": crate::vigia::COOLDOWN_SECS,
    }))
    .into_response()
}

#[derive(serde::Deserialize)]
pub(super) struct CorpoLimiar {
    pub(super) projeto: String,
    /// 1..=600 recargas/min (validação do vigia::definir_base).
    pub(super) base: i64,
    #[serde(default)]
    pub(super) remover: bool,
}

/// POST /api/vigia/limiares — AÇÃO (sessão): grava/remove o override do
/// projeto; o ciclo lê a cada passada — vale em ≤60 s sem restart (mesma
/// mecânica do `flux vigia set`).
pub(super) async fn vigia_set_limiar(
    State(st): State<PanelState>,
    Json(corpo): Json<CorpoLimiar>,
) -> Response {
    let projeto = corpo.projeto.trim();
    if !flux_common::nomes::projeto_valido(projeto) {
        return erro_json(StatusCode::NOT_FOUND, "nome de projeto inválido");
    }
    let resultado: Result<String, String> = if corpo.remover {
        crate::vigia::remover_base(&st.app.state, projeto).map(|existe| {
            if existe {
                "override removido"
            } else {
                "não havia override"
            }
            .to_owned()
        })
    } else {
        crate::vigia::definir_base(&st.app.state, projeto, corpo.base)
            .map(|_| format!("base {} para {}", corpo.base, projeto))
    };
    match resultado {
        Ok(msg) => {
            events::event(
                &st.app.state,
                "info",
                "painel",
                &format!("limiar do vigia pelo painel: {projeto} — {msg}"),
            );
            Json(serde_json::json!({ "projeto": projeto, "resultado": msg })).into_response()
        }
        Err(e) => erro_json(StatusCode::UNPROCESSABLE_ENTITY, &e),
    }
}
