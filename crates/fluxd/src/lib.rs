//! Biblioteca do `fluxd` — o dono de toda a inteligência na VM (07). O binário
//! (`main.rs`) só parseia argumentos; tudo testável vive aqui.

pub mod api;
pub mod banco;
pub mod beat;
pub mod bluegreen;
pub mod ciclo;
pub mod compose;
pub mod decision;
pub mod deploy;
pub mod docker;
pub mod edge;
pub mod events;
pub mod guard;
pub mod mcp;
pub mod migrations;
pub mod observe;
pub mod observe_page;
pub mod pages;
pub mod panel;
pub mod panel_auth;
pub mod paths;
pub mod proxy;
pub mod redroom;
pub mod router;
pub mod sleep;
pub mod sqlite_source;
pub mod state;
pub mod sweep;
pub mod sync;
pub mod temporary;
mod ui;
pub mod waf;

use bollard::Docker;
use flux_common::config::{DomainConfig, GlobalConfig};

use crate::proxy::ProxyClient;
use crate::state::State;

/// Estado compartilhado entre router e API de controle.
pub struct AppState {
    pub state: State,
    pub domain: DomainConfig,
    pub http: ProxyClient,
    /// Cliente Docker quando disponível (na VM sempre; em dev/testes via
    /// `FLUX_DOCKER_URL`). `None` = degrada para estado do SQLite.
    pub docker: Option<Docker>,
    /// Diretório de estado (/srv/flux/state na VM) — onde o fluxd escreve
    /// os overrides de compose (06). `None` = testes/dev sem disco.
    pub state_dir: Option<std::path::PathBuf>,
    /// Flag da rota pública de deploy (`[deploy] public_route`, 10/F4).
    /// `false` = o path não existe para o router (404 brandiado) — desligar
    /// a flag é o rollback completo da fase.
    pub deploy_public: bool,
    /// Rate limit PRÓPRIO da rota de deploy, por projeto (10/F4): bucket
    /// dedicado com limites do deploy.rs — independente da borda global R30
    /// (router.rs), que segue intocada.
    pub deploy_limiter: std::sync::Arc<edge::EdgeState>,
}

impl AppState {
    pub fn new(state: State, config: &GlobalConfig, docker: Option<Docker>) -> Self {
        Self {
            state,
            domain: config.domain.clone(),
            http: proxy::new_client(),
            docker,
            state_dir: None,
            deploy_public: config.deploy_public(),
            deploy_limiter: std::sync::Arc::new(edge::EdgeState::new(edge::EdgeLimits {
                rate_per_min: deploy::DEPLOY_RATE_PER_MIN,
                burst: deploy::DEPLOY_BURST,
                ..edge::EdgeLimits::default()
            })),
        }
    }
}

/// Porta do painel web (08). Com auth real (Argon2id + sessão) o painel
/// também entra no router público atrás do host do painel (03); a porta
/// própria segue de pé como atalho de depuração — SEMPRE 127.0.0.1 (09:
/// só 22/80/443 respondem na VM de fora).
pub const PANEL_PORT: u16 = 7421;

/// Ciclo do `fluxd serve`: router catch-all :8080 (Caddy aponta pra cá, 03 —
/// bind 127.0.0.1: só o Caddy da própria VM alcança, 09) + API de controle
/// :7420 (127.0.0.1, NUNCA sai do localhost, 07) + painel web :7421
/// (127.0.0.1, com auth, debug local). ÚNICA exceção pública: a rota de
/// deploy `POST /api/projects/{name}/deploy` no host do painel (10/F4) —
/// servida pelo router :8080, atrás da flag `[deploy] public_route` e de
/// rate limit + auth por projeto (deploy.rs). A API de CONTROLE da :7420
/// não muda: segue 127.0.0.1 e sem essa rota (build_api intocado).
pub async fn serve(
    db: std::path::PathBuf,
    config: Option<GlobalConfig>,
    port: u16,
    api_port: u16,
) -> Result<(), std::io::Error> {
    let global = match config {
        Some(c) => c,
        None => {
            tracing::warn!(
                "config global ausente (--config) — usando defaults de fábrica (zona bitello.xyz)"
            );
            GlobalConfig::default()
        }
    };

    let st = state::State::open(&db).map_err(std::io::Error::other)?;
    tracing::info!(db = %db.display(), "estado aberto");

    let docker = docker::DockerSource::connect();
    if docker.is_some() {
        tracing::info!("Docker conectado — estado dos containers vem do engine");
    }

    let mut app_state = AppState::new(st, &global, docker);
    // Overrides de compose (06) vivem ao lado do SQLite: /srv/flux/state.
    app_state.state_dir = db.parent().map(std::path::Path::to_path_buf);
    let shared = std::sync::Arc::new(app_state);
    // Auth do painel (08/09): hash em /srv/flux/secrets/panel.argon2.
    // Fail-closed: sem hash, login impossível e a tela ensina a gerar.
    let auth = std::sync::Arc::new(panel_auth::PanelAuth::novo());
    // O config global (mesmo default) alimenta a borda R30 no router; o
    // painel COM auth entra no router público (host do painel, 03).
    let painel = panel::build_panel(std::sync::Arc::clone(&shared), std::sync::Arc::clone(&auth));
    let app = router::build_router(std::sync::Arc::clone(&shared), &global, painel.clone());
    let api_app = api::build_api(std::sync::Arc::clone(&shared));
    let mcp_app = mcp::build_mcp(std::sync::Arc::clone(&shared));

    // Sweep do sleep manager (04): a cada 60 s, re-sincroniza com o Docker e
    // dorme quem estourou ociosidade (com lease/cooldown/CPU como guardas).
    // No MESMO tick roda a expiração dos dev temporários (14) — docker
    // opcional: sem engine o teardown ainda limpa dirs + SQLite (compose
    // é pulado com aviso no evento).
    tokio::spawn(crate::sweep::iniciar(
        std::sync::Arc::clone(&shared),
        global.clone(),
    ));

    // Sampler de recursos (13/16): coleta métricas de 30 s em `metrics_1min`,
    // retenção de 24 h e perícia de pico. O `iniciar` garante o schema,
    // avisa de forma explícita quando não há engine (não inicia) e faz o
    // spawn do próprio loop — aqui é só o disparo único ao lado do sweep.
    {
        let shared_for_observe = std::sync::Arc::clone(&shared);
        tokio::spawn(observe::iniciar(shared_for_observe));
    }

    // 127.0.0.1 nos TRÊS binds (09, critério 2: de fora da VM só 22/80/443
    // respondem — o Caddy é quem fala com o router). ConnectInfo alimenta o
    // rate limit de login do painel por IP real.
    let router_listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    let api_listener = tokio::net::TcpListener::bind(("127.0.0.1", api_port)).await?;
    let panel_listener = tokio::net::TcpListener::bind(("127.0.0.1", PANEL_PORT)).await?;
    // MCP HTTP (22/10 noite): LAN-only no host-only da VirtualBox — a IA do
    // Windows fala com o fluxd por URL, sem processo do flux no host. MESMO
    // gate de token da API de controle; NUNCA 0.0.0.0 nem Cloudflare.
    let mcp_listener = tokio::net::TcpListener::bind((mcp::MCP_BIND_IP, mcp::MCP_PORT)).await?;
    tracing::info!(
        port,
        api_port,
        panel_port = PANEL_PORT,
        mcp = format!("{}:{}", mcp::MCP_BIND_IP, mcp::MCP_PORT),
        "fluxd no ar"
    );

    let router_svc = app.into_make_service_with_connect_info::<std::net::SocketAddr>();
    let api_svc = api_app.into_make_service_with_connect_info::<std::net::SocketAddr>();
    let painel_svc = painel.into_make_service_with_connect_info::<std::net::SocketAddr>();
    let mcp_svc = mcp_app.into_make_service_with_connect_info::<std::net::SocketAddr>();

    // WAF (16) — dependência NUNCA silenciosa: os binds são 127.0.0.1, então
    // o ConnectInfo é sempre loopback (allowlist) e a identificação do IP
    // real de fora depende do X-Forwarded-For que o Caddy seta. Caddy sem
    // XFF desligaria o bloqueio externo sem nenhum erro — o aviso fica no
    // arranque e o warn por-request fica no waf.rs (fonte_ip connectinfo).
    tracing::warn!(
        "waf: bloqueio de request externo depende do X-Forwarded-For do Caddy \
         (binds 127.0.0.1 — sem XFF, todo request entra como loopback/allowlist)"
    );

    tokio::try_join!(
        async { axum::serve(router_listener, router_svc).await },
        async { axum::serve(api_listener, api_svc).await },
        async { axum::serve(panel_listener, painel_svc).await },
        async { axum::serve(mcp_listener, mcp_svc).await }
    )?;
    Ok(())
}
