//! Router catch-all :8080 (03). O Caddy é estático e manda TUDO para cá; quem
//! decide por `Host` é o fluxd. Em dev, TODA resposta — inclusive proxy e
//! páginas — leva `X-Robots-Tag: noindex, nofollow, noarchive` (R22).

use std::sync::Arc;

use axum::Router;
use axum::extract::{Request, State};
use axum::http::{HeaderName, HeaderValue, header};
use axum::response::Response;

use crate::AppState;
use crate::decision::{self, Action, Decision};
use crate::events;
use crate::pages;
use crate::proxy;
use crate::sqlite_source::SqliteSource;

pub fn build_router(app: Arc<AppState>) -> Router {
    Router::new().fallback(handle).with_state(app)
}

fn x_robots_tag() -> HeaderName {
    HeaderName::from_static("x-robots-tag")
}

const NOINDEX: &str = "noindex, nofollow, noarchive";

async fn handle(State(app): State<Arc<AppState>>, req: Request) -> Response {
    let started = std::time::Instant::now();
    let (parts, body) = req.into_parts();

    let host_raw = parts
        .headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_owned())
        .or_else(|| parts.uri.host().map(|h| h.to_owned()))
        .unwrap_or_default();
    let path = parts.uri.path().to_owned();
    let wants_json = decision::wants_json(parts.headers.get(header::ACCEPT), &path);
    let method = parts.method.clone();

    let src = SqliteSource::new(&app.state);
    let Decision { host, action } =
        decision::decide(&host_raw, &path, wants_json, &app.domain, &src);

    let mut resp = match action {
        Action::Proxy { name, target, .. } => {
            match proxy::forward(&app.http, parts, body, &target).await {
                Ok(r) => r,
                Err(e) => {
                    // Upstream falhou → reavalia estado: página "falhou" ou 503
                    // JSON; NUNCA 502 cru (matriz 03).
                    tracing::warn!(host = %host_raw, target = %target, "upstream falhou: {e}");
                    if wants_json {
                        pages::api_failed()
                    } else {
                        pages::failed(&name, host.is_dev())
                    }
                }
            }
        }
        Action::Wait { name, phase } => pages::wait(&name, phase, host.is_dev()),
        Action::Failed { name } => pages::failed(&name, host.is_dev()),
        Action::BranchMissing { name, branch } => {
            pages::branch_missing(&name, &branch, host.is_dev())
        }
        Action::ApiWaking { retry_after } => pages::api_waking(retry_after),
        Action::ApiFailed => pages::api_failed(),
        Action::NotFound => pages::not_found(&host_raw),
        Action::Panel => pages::panel(),
        Action::ServeRobots(r) => pages::robots(&r, &app.domain),
        Action::ServeSitemap(s) => pages::sitemap(&s, &app.domain),
    };

    if host.is_dev() {
        resp.headers_mut()
            .insert(x_robots_tag(), HeaderValue::from_static(NOINDEX));
    }

    events::access(
        &app.state,
        &host_raw,
        method.as_str(),
        &path,
        resp.status().as_u16(),
        started.elapsed().as_millis(),
    );
    resp
}
