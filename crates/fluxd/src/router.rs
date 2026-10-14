//! Router catch-all :8080 (03). O Caddy é estático e manda TUDO para cá; quem
//! decide por `Host` é o fluxd. Em dev, TODA resposta — inclusive proxy e
//! páginas — leva `X-Robots-Tag: noindex, nofollow, noarchive` (R22).

use std::sync::Arc;

use axum::Router;
use axum::extract::{Request, State};
use axum::http::{HeaderName, HeaderValue, header};
use axum::response::Response;
use axum::routing::post;
use flux_common::config::GlobalConfig;
use tower::ServiceExt;

use crate::AppState;
use crate::beat;
use crate::decision::{self, Action, Decision};
use crate::docker::DockerSource;
use crate::edge::{EdgeLayer, EdgeLimits};
use crate::events;
use crate::pages;
use crate::proxy;
use crate::sqlite_source::SqliteSource;
use crate::waf::WafLayer;

/// `global` alimenta a borda R30 (teto de body vem do config, 09; o resto é
/// default da 07). Config ausente JÁ chega aqui como `GlobalConfig::default()`
/// — o warn de "usando defaults" é do `serve`, não se repete por request.
/// `panel` é o Router do painel COM auth (08) — o `Action::Panel` manda a
/// request inteira pra ele (host do painel, 03); sem auth no painel a
/// montagem aqui é proibida.
pub fn build_router(app: Arc<AppState>, global: &GlobalConfig, panel: Router) -> Router {
    Router::new()
        // Batida do overlay JS do sono (04): rota REAL no host gerenciado —
        // rota vence o fallback, então `POST /__flux/beat` NUNCA passa pelo
        // dispatch normal (decisão Host/proxy/panel); o handler zera o timer
        // e responde 204 vazio. Adaptador só desembrulha a tupla de estado:
        // axum-core não tem `FromRef` de tupla e beat.rs fica intocado.
        .route("/__flux/beat", post(beat_roteado))
        .fallback(handle)
        // A borda envolve o router inteiro: `layer` aplicado DEPOIS das rotas
        // roda ANTES do fallback — 413/429 morrem aqui, na admissão, antes da
        // decisão (03: camadas de borda como layers tower; 07/R30).
        // WAF (16) entra DEPOIS da EdgeLayer no fluxo do request: em tower a
        // ÚLTIMA `.layer` é a mais externa (roda primeiro), então EdgeLayer
        // por último = admissão (413/429) antes da inspeção do WAF; o que a
        // borda deixa passar é o que o WAF inspeciona.
        .layer(WafLayer::new())
        .layer(EdgeLayer::new(EdgeLimits::from_global(global)))
        .with_state((app, panel))
}

fn x_robots_tag() -> HeaderName {
    HeaderName::from_static("x-robots-tag")
}

/// Adaptador de estado do `POST /__flux/beat` (04): o router carrega
/// `(Arc<AppState>, Router)` e o handler de beat.rs extrai `State<Arc<AppState>>`
/// — axum-core só tem `FromRef` identidade, então a tupla é desembrulhada aqui.
async fn beat_roteado(
    State((app, _panel)): State<(Arc<AppState>, Router)>,
    headers: axum::http::HeaderMap,
) -> Response {
    beat::beat(State(app), headers).await
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

async fn handle(State((app, panel)): State<(Arc<AppState>, Router)>, req: Request) -> Response {
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
        // Painel real com auth (08): a request inteira vai pro Router do
        // painel — sessão/guard/login são coisa dele. A resposta volta para o
        // rastro comum abaixo (log de acesso + noindex quando dev — painel
        // já é noindex pelas próprias respostas, R22).
        Action::Panel => {
            let req = Request::from_parts(parts, body);
            panel
                .clone()
                .oneshot(req)
                .await
                .expect("Router do painel é infalível (Infallible)")
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::StatusCode;
    use flux_common::protocol::Env;

    use crate::panel;
    use crate::panel_auth;
    use crate::state::State;

    /// Router completo (mesma montagem do `serve`: painel + edge + waf + beat)
    /// sobre estado em memória, sem Docker.
    fn router_com(app: &Arc<AppState>) -> Router {
        let auth = std::sync::Arc::new(panel_auth::PanelAuth::novo());
        let painel = panel::build_panel(std::sync::Arc::clone(app), auth);
        build_router(std::sync::Arc::clone(app), &GlobalConfig::default(), painel)
    }

    fn app() -> Arc<AppState> {
        Arc::new(AppState::new(
            State::in_memory().unwrap(),
            &GlobalConfig::default(),
            None,
        ))
    }

    fn req(host: &str, path: &str) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(format!("http://{host}{path}"))
            .header(header::HOST, host)
            .header(header::ACCEPT, "application/json")
            .body(Body::empty())
            .unwrap()
    }

    /// `POST /__flux/beat` é rota REAL: vence o fallback e o host gerenciado
    /// dev mantém o projeto acordado (04) — 204 e `last_interaction` tocada.
    #[tokio::test]
    async fn beat_e_rota_real_e_toca_o_timer_do_dev() {
        let app = app();
        app.state
            .register_project("vipers", "name = \"vipers\"", "h")
            .unwrap();
        let router = router_com(&app);

        let resp = router
            .oneshot(req("vipers-d.bitello.xyz", "/__flux/beat"))
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::NO_CONTENT,
            "beat de host gerenciado: 204 vazio"
        );
        let (last, _, _, _) = app.state.sleep_row("vipers", Env::Dev).unwrap();
        assert!(last.is_some(), "beat zera o timer (touch_interaction, 04)");
    }

    /// Host NÃO gerenciado no beat → 404 SEM tocar nada (rota responde antes
    /// do dispatch, e o handler do beat filtra painel/fantasma).
    #[tokio::test]
    async fn beat_de_host_desconhecido_e_404_sem_tocar_nada() {
        let app = app();
        let router = router_com(&app);

        let resp = router
            .oneshot(req("zzz.bitello.xyz", "/__flux/beat"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    /// WAF montado DEPOIS da EdgeLayer: request de tamanho normal passa pela
    /// borda e é BLOQUEADO na inspeção — padrão SQLI na query, sem IP na
    /// allowlist (oneshot sem ConnectInfo/XFF = fail-closed, 16) → 403 JSON
    /// antes de chegar ao fallback (que responderia 404 brandiado).
    #[tokio::test]
    async fn waf_bloqueia_injecao_antes_do_dispatch_normal() {
        let app = app();
        let router = router_com(&app);

        let resp = router
            .oneshot(req("zzz.bitello.xyz", "/?q=union%20select"))
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::FORBIDDEN,
            "padrão SQLI externo leva 403 do WAF"
        );
        let bytes = http_body_util::BodyExt::collect(resp.into_body())
            .await
            .unwrap()
            .to_bytes();
        let corpo = String::from_utf8_lossy(&bytes);
        assert!(corpo.contains("blocked"), "corpo JSON do bloqueio: {corpo}");
    }
}
