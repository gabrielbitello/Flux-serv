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
use crate::docker::DockerSource;
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

/// Fonte da decisão: Docker quando existe (estado/alto do engine — verdade,
/// 07), SQLite como fallback (dev/testes sem engine).
enum Source<'a> {
    Docker(DockerSource<'a>),
    Sqlite(SqliteSource<'a>),
}

impl decision::HostSource for Source<'_> {
    fn project_exists(&self, name: &str) -> bool {
        match self {
            Self::Docker(s) => s.project_exists(name),
            Self::Sqlite(s) => s.project_exists(name),
        }
    }
    fn env_state(
        &self,
        name: &str,
        env: flux_common::protocol::Env,
    ) -> flux_common::protocol::ContainerState {
        match self {
            Self::Docker(s) => s.env_state(name, env),
            Self::Sqlite(s) => s.env_state(name, env),
        }
    }
    fn branch_exists(&self, name: &str, branch: &str) -> bool {
        match self {
            Self::Docker(s) => s.branch_exists(name, branch),
            Self::Sqlite(s) => s.branch_exists(name, branch),
        }
    }
    fn proxy_target(&self, name: &str, env: flux_common::protocol::Env) -> Option<String> {
        match self {
            Self::Docker(s) => s.proxy_target(name, env),
            Self::Sqlite(s) => s.proxy_target(name, env),
        }
    }
    fn release_flags(&self, name: &str) -> (bool, bool) {
        match self {
            Self::Docker(s) => s.release_flags(name),
            Self::Sqlite(s) => s.release_flags(name),
        }
    }
}

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

    let src = match &app.docker {
        Some(d) => Source::Docker(DockerSource::new(d.clone(), &app.state)),
        None => Source::Sqlite(SqliteSource::new(&app.state)),
    };
    let Decision { host, action } =
        decision::decide(&host_raw, &path, wants_json, &app.domain, &src);

    let mut resp = match action {
        Action::Proxy { name, target, .. } => {
            let env = host_env(&host);
            // Request proxyado É sinal de interação (04): zera o timer.
            let _ = app.state.touch_interaction(&name, env);
            // Paused/parado acorda ANTES do forward (prod pausado é invisível,
            // < 1 s — 11; erros são logados e o forward reavalia).
            if let Some(d) = &app.docker {
                let src = DockerSource::new(d.clone(), &app.state);
                if src.ensure_awake(&name, env).await {
                    // Acabou de acordar: cooldown anti-flapping (04).
                    let _ = app
                        .state
                        .set_cooldown(&name, env, crate::sleep::WAKE_COOLDOWN_SECS);
                }
            }
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
        Action::Wait { name, phase } => {
            let env = host_env(&host);
            let _ = app.state.touch_interaction(&name, env);
            // Página de espera dispara o wake (03: "docker start + página de
            // espera") — em background, o refresh do navegador colhe o resultado.
            if let Some(d) = &app.docker {
                let d = d.clone();
                let st = Arc::clone(&app);
                let env = host_env(&host);
                let nome = name.clone();
                tokio::spawn(async move {
                    let src = DockerSource::new(d, &st.state);
                    match src.wake(&nome, env).await {
                        Ok(acordou) => {
                            if acordou {
                                let _ = st.state.set_cooldown(
                                    &nome,
                                    env,
                                    crate::sleep::WAKE_COOLDOWN_SECS,
                                );
                            }
                        }
                        Err(e) => tracing::warn!("wake de {nome} falhou: {e}"),
                    }
                });
            }
            pages::wait(&name, phase, host.is_dev())
        }
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

/// Ambiente da ação corrente — só chega aqui para hosts gerenciados
/// (Prod/Dev); painel e desconhecido não tomam caminho de proxy/wake.
fn host_env(host: &flux_common::host::ManagedHost) -> flux_common::protocol::Env {
    match host {
        flux_common::host::ManagedHost::Dev { .. } => flux_common::protocol::Env::Dev,
        _ => flux_common::protocol::Env::Prod,
    }
}
