//! Biblioteca do `fluxd` — o dono de toda a inteligência na VM (07). O binário
//! (`main.rs`) só parseia argumentos; tudo testável vive aqui.

pub mod api;
pub mod decision;
pub mod events;
pub mod pages;
pub mod proxy;
pub mod router;
pub mod sqlite_source;
pub mod state;

use flux_common::config::{DomainConfig, GlobalConfig};

use crate::proxy::ProxyClient;
use crate::state::State;

/// Estado compartilhado entre router e API de controle.
pub struct AppState {
    pub state: State,
    pub domain: DomainConfig,
    pub http: ProxyClient,
}

impl AppState {
    pub fn new(state: State, config: &GlobalConfig) -> Self {
        Self {
            state,
            domain: config.domain.clone(),
            http: proxy::new_client(),
        }
    }
}

/// Ciclo do `fluxd serve`: router catch-all :8080 (Caddy aponta pra cá, 03) +
/// API de controle :7420 (bind 127.0.0.1, NUNCA sai do localhost, 07).
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

    let shared = std::sync::Arc::new(AppState::new(st, &global));
    let app = router::build_router(std::sync::Arc::clone(&shared));
    let api_app = api::build_api(shared);

    let router_listener = tokio::net::TcpListener::bind(("0.0.0.0", port)).await?;
    let api_listener = tokio::net::TcpListener::bind(("127.0.0.1", api_port)).await?;
    tracing::info!(port, api_port, "fluxd no ar");

    tokio::try_join!(async { axum::serve(router_listener, app).await }, async {
        axum::serve(api_listener, api_app).await
    })?;
    Ok(())
}
