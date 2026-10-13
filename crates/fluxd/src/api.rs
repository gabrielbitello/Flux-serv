//! API de controle :7420 (07) — bind 127.0.0.1, NUNCA exposta à LAN. A
//! fundação só responde `/healthz`; todo o resto recusa com causa clara. O
//! token obrigatório entra junto do primeiro endpoint real (F6) — endpoint
//! inexistente não vaza nada, mas endpoint novo nasce já com token.

use std::sync::Arc;

use axum::Router;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::get;
use flux_common::protocol::{ApiErrorBody, HealthBody};

use crate::AppState;

pub fn build_api(app: Arc<AppState>) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .fallback(not_implemented)
        .with_state(app)
}

async fn healthz() -> Json<HealthBody> {
    Json(HealthBody {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
    })
}

async fn not_implemented() -> Response {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(ApiErrorBody {
            error:
                "endpoint da API de controle ainda não implementado — fases F5–F17 (docs/specs/)"
                    .into(),
        }),
    )
        .into_response()
}
