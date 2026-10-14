//! Biblioteca do `fluxd` — o dono de toda a inteligência na VM (07). O binário
//! (`main.rs`) só parseia argumentos; tudo testável vive aqui.

pub mod api;
pub mod beat;
pub mod ciclo;
pub mod compose;
pub mod db;
pub mod decision;
pub mod docker;
pub mod edge;
pub mod events;
pub mod guard;
pub mod observe;
pub mod observe_page;
pub mod pages;
pub mod panel;
pub mod panel_auth;
pub mod proxy;
pub mod router;
pub mod sleep;
pub mod sqlite_source;
pub mod state;
pub mod temporary;
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
}

impl AppState {
    pub fn new(state: State, config: &GlobalConfig, docker: Option<Docker>) -> Self {
        Self {
            state,
            domain: config.domain.clone(),
            http: proxy::new_client(),
            docker,
            state_dir: None,
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
/// (127.0.0.1, com auth, debug local).
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

    // Sweep do sleep manager (04): a cada 60 s, re-sincroniza com o Docker e
    // dorme quem estourou ociosidade (com lease/cooldown/CPU como guardas).
    // No MESMO tick roda a expiração dos dev temporários (14) — docker
    // opcional: sem engine o teardown ainda limpa dirs + SQLite (compose
    // é pulado com aviso no evento).
    let shared_for_sweep = std::sync::Arc::clone(&shared);
    {
        let d = shared.docker.clone();
        let global = global.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(60));
            tick.tick().await; // primeiro tick dispara já — pula
            loop {
                tick.tick().await;
                if let Some(d) = &d {
                    let acoes = sleep::SleepManager::new(
                        d.clone(),
                        &shared_for_sweep.state,
                        global.clone(),
                    )
                    .sweep()
                    .await;
                    for a in acoes {
                        tracing::info!("{a}");
                    }
                }
                let agora = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|t| t.as_secs() as i64)
                    .unwrap_or(0);
                temporary::expire_check(&shared_for_sweep.state, d.clone(), agora).await;
            }
        });
    }

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
    tracing::info!(port, api_port, panel_port = PANEL_PORT, "fluxd no ar");

    let router_svc = app.into_make_service_with_connect_info::<std::net::SocketAddr>();
    let api_svc = api_app.into_make_service_with_connect_info::<std::net::SocketAddr>();
    let painel_svc = painel.into_make_service_with_connect_info::<std::net::SocketAddr>();

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
        async { axum::serve(panel_listener, painel_svc).await }
    )?;
    Ok(())
}
