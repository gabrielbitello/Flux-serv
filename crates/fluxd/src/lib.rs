//! Biblioteca do `fluxd` — o dono de toda a inteligência na VM (07). O binário
//! (`main.rs`) só parseia argumentos; tudo testável vive aqui.

pub mod api;
pub mod db;
pub mod decision;
pub mod docker;
pub mod edge;
pub mod events;
pub mod guard;
pub mod pages;
pub mod panel;
pub mod proxy;
pub mod router;
pub mod sleep;
pub mod sqlite_source;
pub mod state;

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
}

impl AppState {
    pub fn new(state: State, config: &GlobalConfig, docker: Option<Docker>) -> Self {
        Self {
            state,
            domain: config.domain.clone(),
            http: proxy::new_client(),
            docker,
        }
    }
}

/// Porta do painel web (08). Porta PRÓPRIA de propósito: o painel desta fase
/// não tem auth — o aviso de "SEM AUTENTICAÇÃO" fica em toda página e o
/// isolamento de porta é o cercado (nunca entra no wildcard público :8080,
/// 03). O painel real com auth (Argon2id + sessão) substitui a montagem.
pub const PANEL_PORT: u16 = 7421;

/// Ciclo do `fluxd serve`: router catch-all :8080 (Caddy aponta pra cá, 03) +
/// API de controle :7420 (bind 127.0.0.1, NUNCA sai do localhost, 07) +
/// painel web :7421 (porta própria, sem auth — ver `PANEL_PORT`).
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

    let shared = std::sync::Arc::new(AppState::new(st, &global, docker));
    // O config global (mesmo default) alimenta a borda R30 no router.
    let app = router::build_router(std::sync::Arc::clone(&shared), &global);
    let api_app = api::build_api(std::sync::Arc::clone(&shared));
    // Painel (08) montado em porta própria — sem auth, então NUNCA no :8080.
    let painel = panel::build_panel(std::sync::Arc::clone(&shared));

    // Sweep do sleep manager (04): a cada 60 s, re-sincroniza com o Docker e
    // dorme quem estourou ociosidade (com lease/cooldown/CPU como guardas).
    let shared_for_sweep = std::sync::Arc::clone(&shared);
    if let Some(d) = &shared.docker {
        let d = d.clone();
        let global = global.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(60));
            tick.tick().await; // primeiro tick dispara já — pula
            loop {
                tick.tick().await;
                let acoes =
                    sleep::SleepManager::new(d.clone(), &shared_for_sweep.state, global.clone())
                        .sweep()
                        .await;
                for a in acoes {
                    tracing::info!("{a}");
                }
            }
        });
    }

    let router_listener = tokio::net::TcpListener::bind(("0.0.0.0", port)).await?;
    let api_listener = tokio::net::TcpListener::bind(("127.0.0.1", api_port)).await?;
    let panel_listener = tokio::net::TcpListener::bind(("0.0.0.0", PANEL_PORT)).await?;
    tracing::info!(port, api_port, panel_port = PANEL_PORT, "fluxd no ar");

    tokio::try_join!(
        async { axum::serve(router_listener, app).await },
        async { axum::serve(api_listener, api_app).await },
        async { axum::serve(panel_listener, painel).await }
    )?;
    Ok(())
}
