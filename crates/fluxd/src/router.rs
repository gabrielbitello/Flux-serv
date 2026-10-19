//! Router catch-all :8080 (03). O Caddy é estático e manda TUDO para cá; quem
//! decide por `Host` é o fluxd. Em dev, TODA resposta — inclusive proxy e
//! páginas — leva `X-Robots-Tag: noindex, nofollow, noarchive` (R22).

use std::sync::Arc;

use axum::Router;
use axum::extract::{Request, State};
use axum::http::{HeaderValue, header};
use axum::response::Response;
use axum::routing::post;
use flux_common::config::{GlobalConfig, ProjectToml};
use tower::ServiceExt;

use crate::AppState;
use crate::beat;
use crate::decision::{self, Action, Decision};
use crate::deploy;
use crate::docker::DockerSource;
use crate::edge::{EdgeLayer, EdgeLimits};
use crate::events;
use crate::pages;
use crate::proxy;
use crate::sqlite_source::SqliteSource;
use crate::ui::{NOINDEX, x_robots_tag};
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

/// Adaptador de estado do `POST /__flux/beat` (04): o router carrega
/// `(Arc<AppState>, Router)` e o handler de beat.rs extrai `State<Arc<AppState>>`
/// — axum-core só tem `FromRef` identidade, então a tupla é desembrulhada aqui.
/// Cooldown anti-flapping (04) quando o projeto ACORDOU — a parte da
/// política que estava escrita 2× nos braços Proxy e Wait (L5.6). Os verbos
/// de acordar ficam em cada braço de propósito: `ensure_awake` (só o app,
/// erro tratado local) no Proxy; `wake` (projeto inteiro, `Result`) no Wait.
fn cooldown_se_acordou(app: &AppState, nome: &str, env: flux_common::protocol::Env, acordou: bool) {
    if acordou {
        let _ = app
            .state
            .set_cooldown(nome, env, crate::sleep::WAKE_COOLDOWN_SECS);
    }
}

async fn beat_roteado(
    State((app, _panel)): State<(Arc<AppState>, Router)>,
    headers: axum::http::HeaderMap,
) -> Response {
    beat::beat(State(app), headers).await
}

/// Teto do hold de API (`[sleep] api_hold`): tempo máximo que a request fica
/// segurada acordando o container antes de cair no fluxo normal (página de
/// espera/503 honestos). Bun acorda em segundos; 90 s cobre boot lento sem
/// pendurar cliente eternamente. (O `NOINDEX` que o master tinha aqui virou
/// `crate::ui::NOINDEX` no L4.4 — fonte única das constantes de UI.)
const WAKE_HOLD_TIMEOUT_SECS: u64 = 90;
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

/// Dev server de pé para o projeto? O `public/hot` é o contrato (06/live):
/// o Vite o cria ao subir e o wrapper do compose o apaga na morte — a
/// existência do arquivo É o estado, sem flag paralela para dessincronizar.
/// Raiz derivada de `state_dir` (`/srv/flux/state` → irmão `projects`) para
/// não duplicar caminho no AppState; sem `state_dir` (testes/dev host) o
/// live fica desligado.
fn live_ativo(app: &AppState, name: &str) -> bool {
    app.state_dir
        .as_ref()
        .and_then(|p| p.parent())
        .is_some_and(|p| p.join("projects").join(name).join("public/hot").is_file())
}

async fn handle(State((app, panel)): State<(Arc<AppState>, Router)>, req: Request) -> Response {
    let started = std::time::Instant::now();
    let (mut parts, body) = req.into_parts();

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
    let Decision { host, mut action } =
        decision::decide(&host_raw, &path, wants_json, &app.domain, &src);

    // Hold de API (`[sleep] api_hold`, 18/10): projeto dormindo/subindo com a
    // flag ligada NÃO devolve página de espera/503 — a request é SEGURA aqui
    // enquanto o container acorda e a porta responde, e a decisão é refeita
    // com o estado novo. IDEs quebram com recusa, toleram espera (pedido do
    // Gabriel p/ zcode-api). Default OFF: sem a flag, nenhum projeto muda.
    if matches!(action, Action::Wait { .. } | Action::ApiWaking { .. })
        && let Some(nome) = projeto_do_host(&host)
        && api_hold_ativo(&app.state, &nome)
        && let Some(d) = app.docker.clone()
    {
        let env = host_env(&host);
        use crate::decision::HostSource as _;
        let _ = app.state.touch_interaction(&nome, env);
        let acordador = DockerSource::new(d, &app.state);
        if let Err(e) = acordador.wake(&nome, env).await {
            tracing::warn!("hold: wake de {nome} falhou: {e}");
        }
        let prazo =
            std::time::Instant::now() + std::time::Duration::from_secs(WAKE_HOLD_TIMEOUT_SECS);
        let mut acordou = false;
        while std::time::Instant::now() < prazo {
            if matches!(
                acordador.env_state(&nome, env),
                flux_common::protocol::ContainerState::Awake
                    | flux_common::protocol::ContainerState::Paused
            ) {
                acordou = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
        if acordou {
            let _ = app
                .state
                .set_cooldown(&nome, env, crate::sleep::WAKE_COOLDOWN_SECS);
            let Decision { action: nova, .. } =
                decision::decide(&host_raw, &path, wants_json, &app.domain, &src);
            // Container RUNNING ainda não é app pronto: com Proxy em mãos,
            // espera a porta TCP viva (mesmo probe do live, 06) antes de
            // assumir a ação — boot do processo ainda em curso devolve o
            // fluxo original, nunca conexão recusada no cliente.
            if let Action::Proxy { target, .. } = &nova {
                let alvo = target.clone();
                let (ip, porta) = match alvo.rsplit_once(':') {
                    Some((i, p)) => (i.to_owned(), p.parse::<u16>().unwrap_or(80)),
                    None => (alvo.clone(), 80),
                };
                while std::time::Instant::now() < prazo {
                    if crate::compose::porta_tcp_viva(&ip, porta).await {
                        action = nova;
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
                if !matches!(action, Action::Proxy { .. }) {
                    tracing::warn!(
                        "hold: porta de {nome} ({alvo}) não abriu em tempo — segue fluxo normal"
                    );
                }
            } else {
                action = nova;
            }
        } else {
            tracing::warn!(
                "hold: {nome} não acordou em {WAKE_HOLD_TIMEOUT_SECS}s — segue fluxo normal"
            );
        }
    }

    let mut resp = match action {
        Action::Proxy { name, target, .. } => {
            let env = host_env(&host);
            // Request proxyado É sinal de interação (04): zera o timer.
            let _ = app.state.touch_interaction(&name, env);
            // Paused/parado acorda ANTES do forward (prod pausado é invisível,
            // < 1 s — 11; erros são logados e o forward reavalia).
            if let Some(d) = &app.docker {
                let src = DockerSource::new(d.clone(), &app.state);
                let acordou = src.ensure_awake(&name, env).await;
                // Acabou de acordar: cooldown anti-flapping (04).
                cooldown_se_acordou(&app, &name, env, acordou);
            }
            // X-Forwarded (03): o forward reescreve o HOST para o IP do
            // container — sem o original em X-Forwarded-Host, o Laravel
            // monta url()/rotas com o IP interno (http://172.18.0.4/signin)
            // que o browser do dev não alcança. O projeto confia no proxy
            // (trustProxies) e usa o host público. Proto é sempre https:
            // na frente do fluxd só existe o TLS do Caddy.
            if let Ok(hv) = HeaderValue::from_str(&host_raw) {
                parts.headers.insert(
                    axum::http::header::HeaderName::from_static("x-forwarded-host"),
                    hv,
                );
                parts.headers.insert(
                    axum::http::header::HeaderName::from_static("x-forwarded-proto"),
                    HeaderValue::from_static("https"),
                );
            }
            // Live same-origin (06/live, método do devserver velho): com o
            // dev server de pé (`public/hot` é o contrato do Vite — nasce
            // com ele, o wrapper do compose apaga na morte), os prefixos do
            // vite no host `-d` vão para a :5173 do MESMO container. Assets
            // e HMR saem pelo próprio domínio, sem cross-origin/CORS; tudo
            // que não é path do vite (Laravel, Livewire, `public/`) segue
            // pro frankenphp intacto. Pedido com `Upgrade` usa o túnel ws.
            // Hot órfão EM TEMPO REAL (npm morreu com SIGKILL — o wrapper
            // não rodou o rm): :5173 não responde → limpa o hot e CAI PRO
            // FRANKENPHP (build) em vez de devolver página failed para um
            // path de módulo — MIME text/html derruba CSS e Alpine inteiros.
            let mut target = target;
            let mut upgrade = None;
            if host.is_dev() && decision::eh_path_vite(&path) && live_ativo(&app, &name) {
                let ip = target
                    .rsplit_once(':')
                    .map(|(ip, _)| ip.to_owned())
                    .unwrap_or_else(|| target.clone());
                if crate::compose::porta_tcp_viva(&ip, decision::VITE_PORT).await {
                    target = format!("{ip}:{}", decision::VITE_PORT);
                    // OnUpgrade mora nas extensions do request: retirar ANTES
                    // do forward — o `into_parts` já aconteceu no topo.
                    upgrade = parts.extensions.remove::<hyper::upgrade::OnUpgrade>();
                    if upgrade.is_some() {
                        tracing::debug!(host = %host_raw, path, "ws do HMR → {target}");
                    }
                } else if let Some(r) = crate::compose::raiz_arvore(&app, &name) {
                    match std::fs::remove_file(r.join("public/hot")) {
                        Ok(()) => {
                            tracing::warn!(host = %host_raw, "dev server morto com hot órfão — hot removido, build assumindo")
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                        Err(e) => tracing::warn!("falha removendo hot órfão: {e}"),
                    }
                }
            }
            let encaminhado = if upgrade.is_some() {
                proxy::forward_ws(&app.http, parts, body, &target, upgrade).await
            } else {
                proxy::forward(&app.http, parts, body, &target).await
            };
            match encaminhado {
                Ok(r) => r,
                Err(e) => {
                    // Upstream falhou → reavalia estado: página "falhou" ou 503
                    // JSON; NUNCA 502 cru (matriz 03).
                    tracing::warn!(host = %host_raw, target = %target, "upstream falhou: {e}");
                    if wants_json {
                        pages::api_failed()
                    } else {
                        pages::failed(
                            &name,
                            host.is_dev(),
                            logo_do_projeto(&app.state, &name).as_deref(),
                        )
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
                    let src = DockerSource::new(d.clone(), &st.state);
                    match src.wake(&nome, env).await {
                        Ok(acordou) => {
                            cooldown_se_acordou(&st, &nome, env, acordou);
                            // Feature 19/10: acordou = religa o live de dev
                            // (o stop do sono matou o vite junto; antes só o
                            // `flux up` religava e o site voltava sem HMR).
                            // Idempotente (decidir_live) e NUNCA afeta o
                            // request: erro é evento warn lá dentro. Só dev:
                            // live é contrato de `[dev] live`.
                            if env == flux_common::protocol::Env::Dev {
                                crate::compose::religar_live_pos_acordar(
                                    &st.state, &st, &d, &nome, env,
                                )
                                .await;
                            }
                        }
                        Err(e) => tracing::warn!("wake de {nome} falhou: {e}"),
                    }
                });
            }
            pages::wait(
                &name,
                phase,
                host.is_dev(),
                logo_do_projeto(&app.state, &name).as_deref(),
            )
        }
        Action::Failed { name } => pages::failed(
            &name,
            host.is_dev(),
            logo_do_projeto(&app.state, &name).as_deref(),
        ),
        Action::BranchMissing { name, branch } => pages::branch_missing(
            &name,
            &branch,
            host.is_dev(),
            logo_do_projeto(&app.state, &name).as_deref(),
        ),
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
        // Rota pública de deploy (10/F4), SÓ no host do painel. Gate da flag
        // PRIMEIRO: `[deploy] public_route` ausente/false = o path NÃO existe
        // (404 brandiado do host) — desligar a flag é o rollback da fase.
        // Ligada, o handler cuida de método/rate/auth/corpo (deploy.rs) e a
        // resposta segue para o rastro comum abaixo.
        Action::Deploy { name } => {
            if !app.deploy_public {
                pages::not_found(&host_raw)
            } else {
                deploy::deploy(Arc::clone(&app), name, Request::from_parts(parts, body)).await
            }
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
/// `[branding] logo_url` do TOML gravado (15) para as páginas de espera/falha.
/// TOML ausente, quebrado ou sem logo = None — a página segue sem marca, o
/// erro real de parse já vive no registro/painel (nunca 500 por causa de
/// decoração).
fn logo_do_projeto(state: &crate::state::State, name: &str) -> Option<String> {
    let src = state.project_toml(name).ok().flatten()?;
    ProjectToml::parse_str(&src)
        .ok()?
        .branding?
        .logo_url()
        .map(str::to_owned)
}

fn host_env(host: &flux_common::host::ManagedHost) -> flux_common::protocol::Env {
    match host {
        flux_common::host::ManagedHost::Dev { .. } => flux_common::protocol::Env::Dev,
        // Host custom de `[domains]` (09) carrega o próprio ambiente.
        flux_common::host::ManagedHost::Custom { dev: true, .. } => flux_common::protocol::Env::Dev,
        _ => flux_common::protocol::Env::Prod,
    }
}

/// Projeto de um host gerenciado — painel e desconhecido não têm (só eles
/// interessam ao hold, que roda depois de uma decisão Wait/ApiWaking).
fn projeto_do_host(host: &flux_common::host::ManagedHost) -> Option<String> {
    match host {
        flux_common::host::ManagedHost::Prod { name }
        | flux_common::host::ManagedHost::Dev { name, .. }
        | flux_common::host::ManagedHost::Custom { name, .. }
        | flux_common::host::ManagedHost::Vite { name, .. } => Some(name.clone()),
        _ => None,
    }
}

/// Flag por projeto (`[sleep] api_hold`): TOML ausente/quebrado/sem a chave =
/// `false` — igual ao logo (15), decoração nunca vira 500.
fn api_hold_ativo(state: &crate::state::State, name: &str) -> bool {
    state
        .project_toml(name)
        .ok()
        .flatten()
        .and_then(|src| ProjectToml::parse_str(&src).ok())
        .and_then(|t| t.sleep)
        .and_then(|s| s.api_hold)
        .unwrap_or(false)
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

    /// Hold de API (18/10): a flag `[sleep].api_hold` é lida do TOML gravado;
    /// projeto sem a chave (ou inexistente) fica OFF — comportamento atual.
    #[test]
    fn api_hold_ativo_le_flag_do_toml_gravado() {
        let app = app();
        app.state
            .register_project(
                "zcode-api",
                "name = \"zcode-api\"\n[sleep]\napi_hold = true\n",
                "h",
            )
            .unwrap();
        app.state
            .register_project("vipers", "name = \"vipers\"", "h")
            .unwrap();
        assert!(api_hold_ativo(&app.state, "zcode-api"), "flag ligada");
        assert!(
            !api_hold_ativo(&app.state, "vipers"),
            "sem a chave = OFF (default)"
        );
        assert!(!api_hold_ativo(&app.state, "fantasma"));
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

    /// Rota pública de deploy (10/F4) com flag LIGADA: no host do painel o
    /// path vive — sem Bearer o handler responde 401 (a rota existe e fecha).
    #[tokio::test]
    async fn deploy_com_flag_ligada_responde_no_host_do_painel() {
        let cfg = GlobalConfig::parse_str("[deploy]\npublic_route = true\n").unwrap();
        let app = Arc::new(AppState::new(State::in_memory().unwrap(), &cfg, None));
        app.state
            .register_project("vipers", "name = \"vipers\"", "h")
            .unwrap();
        let router = router_com(&app);

        let resp = router
            .oneshot(req("flux.bitello.xyz", "/api/projects/vipers/deploy"))
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "flag ligada: rota existe e fecha sem token"
        );
    }

    /// Flag DESLIGADA (default): o mesmo path no host do painel é 404
    /// brandiado — a rota NÃO existe para o router (rollback = desligar flag).
    /// Em host de projeto o path NUNCA foi da rota: segue proxyando o
    /// container (comportamento atual, prova contra vazamento da rota).
    #[tokio::test]
    async fn deploy_sem_flag_some_em_404_brandiado() {
        let app = app(); // GlobalConfig::default() → deploy_public = false
        app.state
            .register_project("vipers", "name = \"vipers\"", "h")
            .unwrap();
        let router = router_com(&app);

        let resp = router
            .oneshot(req("flux.bitello.xyz", "/api/projects/vipers/deploy"))
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "flag desligada: rota some"
        );
        let bytes = http_body_util::BodyExt::collect(resp.into_body())
            .await
            .unwrap()
            .to_bytes();
        let corpo = String::from_utf8_lossy(&bytes);
        assert!(
            corpo.contains("flux.bitello.xyz"),
            "404 brandiado do host do painel: {corpo}"
        );

        // API de controle :7420 intocada (corte 11): sem token, rota de
        // controle existe e fecha 503/401 — nunca 404 de rota pública.
        let api = crate::api::build_api(Arc::clone(&app));
        let resp = api
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("http://127.0.0.1:7420/projects")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status();
        assert!(
            status == StatusCode::SERVICE_UNAVAILABLE || status == StatusCode::UNAUTHORIZED,
            "API de controle segue fechada SEM token: {status}"
        );
    }
}
