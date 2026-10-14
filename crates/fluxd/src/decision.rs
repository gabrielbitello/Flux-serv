//! Máquina de decisão do router (matriz da spec 03): `Host` + path + estado
//! do container → Ação. Pura: consulta `HostSource`, não toca em rede nem
//! Docker — testável sem VM. Camadas futuras (rate limit R30, WAF R32,
//! headers R32) entram como layers tower ANTES deste dispatch.

use axum::http::HeaderValue;
use flux_common::config::DomainConfig;
use flux_common::host::ManagedHost;
use flux_common::protocol::{ContainerState, Env};

/// Fase da página de espera (03): `docker start` emitido, health ainda não OK;
/// ou provision hook rodando.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitPhase {
    Waking,
    Provisioning,
}

/// O que o router faz com a requisição. Cada fase futura adiciona variantes
/// aqui ou layers na frente — nunca um `if` novo espalhado.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Proxy streaming para o container (prod pausado entra aqui: unpause é
    /// invisível, < 1 s — 11; na fundação o unpause é do módulo docker, F10).
    Proxy {
        name: String,
        env: Env,
        target: String,
    },
    /// 200 página de espera com auto-refresh (dev dormindo/subindo).
    Wait { name: String, phase: WaitPhase },
    /// 200 página de estado "falhou" (crash loop/health) — honesta, nunca 500 cru.
    Failed { name: String },
    /// 200 brandiada: branch de worktree não existe localmente (03).
    BranchMissing { name: String, branch: String },
    /// 503 JSON + Retry-After — API parada/acordando (12): nunca HTML em rota
    /// de API, nunca 200 falso.
    ApiWaking { retry_after: u64 },
    /// 503 JSON sem retry — API e container falhando de verdade.
    ApiFailed,
    /// 404 brandiado "host desconhecido" — único 404 da matriz (03).
    NotFound,
    /// Painel em `flux.bitello.xyz` (auth chega na F8).
    Panel,
    /// `POST /api/projects/{name}/deploy` NO HOST DO PAINEL (10/F4): rota
    /// pública de deploy, gated pela flag `[deploy] public_route` (o gate é
    /// verificado no handler — a decisão não vê config). Só o host do painel
    /// tem a rota; em Prod/Dev o mesmo path segue pro proxy do container.
    Deploy { name: String },
    /// Intercept de `/robots.txt` ANTES do proxy (R22/R23).
    ServeRobots(Robots),
    /// Intercept de `/sitemap.xml` ANTES do proxy (R23).
    ServeSitemap(Sitemap),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Robots {
    /// dev + painel: `Disallow: /` SEMPRE — robots do projeto é IGNORADO em
    /// dev, política não é negociável (R22).
    DisallowAll,
    /// prod sem `public/robots.txt` no release ativo (R23): preexistente
    /// vence SEMPRE — o gerado só existe na ausência.
    ProdAllow { name: String },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Sitemap {
    /// dev + painel: urlset vazio.
    Empty,
    /// prod sem `public/sitemap.xml` no release ativo (R23).
    Prod { name: String },
}

/// Fonte de verdade consultada pela decisão. A implementação de produção lê o
/// SQLite; testes injetam mock.
pub trait HostSource {
    fn project_exists(&self, name: &str) -> bool;
    fn env_state(&self, name: &str, env: Env) -> ContainerState;
    fn branch_exists(&self, name: &str, branch: &str) -> bool;
    fn proxy_target(&self, name: &str, env: Env) -> Option<String>;
    /// Flags do release ATIVO (10): `(has_robots, has_sitemap)` — detecção é
    /// no DEPLOY, nunca por request (03).
    fn release_flags(&self, name: &str) -> (bool, bool);
}

/// `Retry-After` do 503 de API acordando (12).
pub const API_RETRY_AFTER: u64 = 30;

pub struct Decision {
    pub host: ManagedHost,
    pub action: Action,
}

/// Decide a ação para a requisição.
pub fn decide(
    host_raw: &str,
    path: &str,
    wants_json: bool,
    domain: &DomainConfig,
    src: &dyn HostSource,
) -> Decision {
    let host = ManagedHost::parse(host_raw, domain, |n| src.project_exists(n));
    let action = match &host {
        ManagedHost::Panel => match rota_deploy(path) {
            Some(name) => Action::Deploy { name },
            None => match path {
                "/robots.txt" => Action::ServeRobots(Robots::DisallowAll),
                "/sitemap.xml" => Action::ServeSitemap(Sitemap::Empty),
                _ => Action::Panel,
            },
        },
        ManagedHost::Unknown(_) => Action::NotFound,
        ManagedHost::Prod { name } => prod_action(name, path, wants_json, src),
        ManagedHost::Dev { name, branch } => {
            dev_action(name, branch.as_deref(), path, wants_json, src)
        }
    };
    Decision { host, action }
}

/// Reconhece o path EXATO da rota pública de deploy (10/F4):
/// `/api/projects/{name}/deploy` com `name` em `[a-z0-9-]`. Qualquer outro
/// formato (nome vazio, nome inválido, sufixo/ prefixo divergente) devolve
/// `None` — o path cai no fluxo normal do host (painel → 404 do painel;
/// Prod/Dev → proxy ao container, comportamento atual intacto). O MÉTODO não
/// participa da decisão: recusar ≠ POST é do handler (deploy.rs), antes do
/// rate limit.
fn rota_deploy(path: &str) -> Option<String> {
    let rest = path.strip_prefix("/api/projects/")?;
    let name = rest.strip_suffix("/deploy")?;
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return None;
    }
    Some(name.to_owned())
}

fn prod_action(name: &str, path: &str, wants_json: bool, src: &dyn HostSource) -> Action {
    if path == "/robots.txt" && !src.release_flags(name).0 {
        return Action::ServeRobots(Robots::ProdAllow {
            name: name.to_owned(),
        });
    }
    if path == "/sitemap.xml" && !src.release_flags(name).1 {
        return Action::ServeSitemap(Sitemap::Prod {
            name: name.to_owned(),
        });
    }
    from_state(name, Env::Prod, wants_json, src)
}

fn dev_action(
    name: &str,
    branch: Option<&str>,
    path: &str,
    wants_json: bool,
    src: &dyn HostSource,
) -> Action {
    // Em dev a política é SEMPRE Disallow — o robots do projeto é ignorado (R22).
    if path == "/robots.txt" {
        return Action::ServeRobots(Robots::DisallowAll);
    }
    if path == "/sitemap.xml" {
        return Action::ServeSitemap(Sitemap::Empty);
    }
    if let Some(b) = branch
        && !src.branch_exists(name, b)
    {
        return Action::BranchMissing {
            name: name.to_owned(),
            branch: b.to_owned(),
        };
    }
    from_state(name, Env::Dev, wants_json, src)
}

fn from_state(name: &str, env: Env, wants_json: bool, src: &dyn HostSource) -> Action {
    let st = match src.env_state(name, env) {
        ContainerState::Unknown => {
            // A verdade vem do Docker (events + sweep, 07): estado ausente =
            // ainda re-sincronizando — tratar como subindo, nunca como morto.
            ContainerState::Starting
        }
        s => s,
    };
    match st {
        ContainerState::Awake | ContainerState::Paused => match src.proxy_target(name, env) {
            Some(target) => Action::Proxy {
                name: name.to_owned(),
                env,
                target,
            },
            None => {
                tracing::warn!(name, env = %env, "container acordado sem proxy_target no estado — tratando como falho");
                failed(name, wants_json)
            }
        },
        ContainerState::Sleeping | ContainerState::Starting => match (env, wants_json) {
            // API parada/acordando: 503 JSON + Retry-After, nunca HTML (12).
            (_, true) => Action::ApiWaking {
                retry_after: API_RETRY_AFTER,
            },
            // BROWSER: página de espera 200 em dev E em prod — a tabela da
            // 12 é explícita ("Parado: Navegador = página de espera 200,
            // Cliente de API = 503 JSON") e a 03 completa ("Container
            // subindo → 200") sem distinguir ambiente. 503 é SÓ pra quem
            // fala JSON; página de espera nunca é "200 falso" porque ela
            // diz "acordando" e se auto-atualiza (03).
            (_, false) => Action::Wait {
                name: name.to_owned(),
                phase: WaitPhase::Waking,
            },
        },
        ContainerState::Failed => failed(name, wants_json),
        ContainerState::Unknown => unreachable!("Unknown foi normalizado para Starting"),
    }
}

fn failed(name: &str, wants_json: bool) -> Action {
    if wants_json {
        Action::ApiFailed
    } else {
        Action::Failed {
            name: name.to_owned(),
        }
    }
}

/// Rota de API? `Accept: application/json` (sem preferência por HTML) ou path
/// `/api` — base do "nunca HTML em rota de API" (12).
pub fn wants_json(accept: Option<&HeaderValue>, path: &str) -> bool {
    if path == "/api" || path.starts_with("/api/") {
        return true;
    }
    match accept.and_then(|v| v.to_str().ok()) {
        Some(a) => a.contains("application/json") && !a.contains("text/html"),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    struct Mock {
        projects: Vec<String>,
        states: HashMap<(String, Env), ContainerState>,
        targets: HashMap<(String, Env), String>,
        flags: HashMap<String, (bool, bool)>,
        branches: Vec<(String, String)>,
    }

    impl Default for Mock {
        fn default() -> Self {
            Self {
                projects: vec!["vipers".into(), "my-app".into()],
                states: HashMap::new(),
                targets: HashMap::new(),
                flags: HashMap::new(),
                branches: Vec::new(),
            }
        }
    }

    impl Mock {
        fn st(mut self, name: &str, env: Env, s: ContainerState) -> Self {
            self.states.insert((name.into(), env), s);
            self
        }

        fn target(mut self, name: &str, env: Env, t: &str) -> Self {
            self.targets.insert((name.into(), env), t.into());
            self
        }

        fn flags(mut self, name: &str, robots: bool, sitemap: bool) -> Self {
            self.flags.insert(name.into(), (robots, sitemap));
            self
        }

        fn branch(mut self, name: &str, b: &str) -> Self {
            self.branches.push((name.into(), b.into()));
            self
        }

        fn decide(&self, host: &str, path: &str, json: bool) -> Action {
            decide(host, path, json, &DomainConfig::default(), self).action
        }
    }

    impl HostSource for Mock {
        fn project_exists(&self, name: &str) -> bool {
            self.projects.iter().any(|p| p == name)
        }
        fn env_state(&self, name: &str, env: Env) -> ContainerState {
            self.states
                .get(&(name.to_owned(), env))
                .copied()
                .unwrap_or(ContainerState::Unknown)
        }
        fn branch_exists(&self, name: &str, branch: &str) -> bool {
            self.branches.iter().any(|(n, b)| n == name && b == branch)
        }
        fn proxy_target(&self, name: &str, env: Env) -> Option<String> {
            self.targets.get(&(name.to_owned(), env)).cloned()
        }
        fn release_flags(&self, name: &str) -> (bool, bool) {
            self.flags.get(name).copied().unwrap_or((false, false))
        }
    }

    #[test]
    fn matriz_dev_dormindo_browser_recebe_pagina_de_espera_200() {
        let m = Mock::default()
            .st("vipers", Env::Dev, ContainerState::Sleeping)
            .target("vipers", Env::Dev, "127.0.0.1:9001");
        assert_eq!(
            m.decide("vipers-d.bitello.xyz", "/", false),
            Action::Wait {
                name: "vipers".into(),
                phase: WaitPhase::Waking
            }
        );
    }

    #[test]
    fn matriz_api_parada_recebe_503_json_mesmo_em_dev() {
        let m = Mock::default().st("vipers", Env::Dev, ContainerState::Sleeping);
        assert_eq!(
            m.decide("vipers-d.bitello.xyz", "/api/queue", true),
            Action::ApiWaking {
                retry_after: API_RETRY_AFTER
            }
        );
    }

    #[test]
    fn matriz_prod_pausado_proxyla_direto_sem_pagina() {
        let m = Mock::default()
            .st("vipers", Env::Prod, ContainerState::Paused)
            .target("vipers", Env::Prod, "10.0.0.5:80");
        assert_eq!(
            m.decide("vipers.bitello.xyz", "/", false),
            Action::Proxy {
                name: "vipers".into(),
                env: Env::Prod,
                target: "10.0.0.5:80".into()
            }
        );
    }

    #[test]
    fn matriz_prod_parado_browser_espera_api_503() {
        let m = Mock::default().st("vipers", Env::Prod, ContainerState::Sleeping);
        // Browser em prod parado = página de espera 200 (tabela da 12:
        // "Navegador = página de espera 200; Cliente de API = 503 JSON").
        assert_eq!(
            m.decide("vipers.bitello.xyz", "/", false),
            Action::Wait {
                name: "vipers".into(),
                phase: WaitPhase::Waking,
            }
        );
        // Cliente de API = 503 JSON + Retry-After (nunca HTML, nunca 200 falso).
        assert_eq!(
            m.decide("vipers.bitello.xyz", "/", true),
            Action::ApiWaking {
                retry_after: API_RETRY_AFTER
            }
        );
    }

    #[test]
    fn matriz_crash_recebe_pagina_falhou_ou_json() {
        let m = Mock::default().st("vipers", Env::Dev, ContainerState::Failed);
        assert_eq!(
            m.decide("vipers-d.bitello.xyz", "/", false),
            Action::Failed {
                name: "vipers".into()
            }
        );
        assert_eq!(
            m.decide("vipers-d.bitello.xyz", "/api/x", true),
            Action::ApiFailed
        );
    }

    #[test]
    fn acordado_proxila_para_o_alvo() {
        let m = Mock::default()
            .st("vipers", Env::Dev, ContainerState::Awake)
            .target("vipers", Env::Dev, "10.0.0.9:8080");
        assert_eq!(
            m.decide("vipers-d.bitello.xyz", "/", false),
            Action::Proxy {
                name: "vipers".into(),
                env: Env::Dev,
                target: "10.0.0.9:8080".into()
            }
        );
    }

    #[test]
    fn acordado_sem_alvo_falha_visivel_nao_500_cru() {
        let m = Mock::default().st("vipers", Env::Dev, ContainerState::Awake);
        assert_eq!(
            m.decide("vipers-d.bitello.xyz", "/", false),
            Action::Failed {
                name: "vipers".into()
            }
        );
    }

    #[test]
    fn estado_desconhecido_e_tratado_como_subindo_nunca_morto() {
        let m = Mock::default();
        assert_eq!(
            m.decide("vipers-d.bitello.xyz", "/", false),
            Action::Wait {
                name: "vipers".into(),
                phase: WaitPhase::Waking
            }
        );
    }

    #[test]
    fn host_desconhecido_e_unico_404() {
        let m = Mock::default();
        assert_eq!(m.decide("zzz.bitello.xyz", "/", false), Action::NotFound);
    }

    #[test]
    fn branch_inexistente_recebe_pagina_brandiada() {
        let m = Mock::default();
        assert_eq!(
            m.decide("feat-x-vipers-d.bitello.xyz", "/", false),
            Action::BranchMissing {
                name: "vipers".into(),
                branch: "feat-x".into()
            }
        );
        let m2 = m.branch("vipers", "feat-x");
        let a = m2.decide("feat-x-vipers-d.bitello.xyz", "/", false);
        assert!(matches!(a, Action::Wait { .. }), "ação: {a:?}");
    }

    #[test]
    fn robots_em_dev_e_sempre_disallow_r22() {
        let m = Mock::default().flags("vipers", true, true);
        assert_eq!(
            m.decide("vipers-d.bitello.xyz", "/robots.txt", false),
            Action::ServeRobots(Robots::DisallowAll)
        );
    }

    #[test]
    fn robots_em_prod_depende_do_flag_do_release_r23() {
        let sem = Mock::default();
        assert_eq!(
            sem.decide("vipers.bitello.xyz", "/robots.txt", false),
            Action::ServeRobots(Robots::ProdAllow {
                name: "vipers".into()
            })
        );
        let com = Mock::default().flags("vipers", true, true);
        let a = com.decide("vipers.bitello.xyz", "/robots.txt", false);
        assert!(
            matches!(
                a,
                Action::Proxy { .. }
                    | Action::ApiWaking { .. }
                    | Action::NotFound
                    | Action::Wait { .. }
            ),
            "sem flag, robots passa pelo fluxo normal: {a:?}"
        );
    }

    #[test]
    fn sitemap_gerado_somente_na_ausencia_r23() {
        let m = Mock::default();
        assert_eq!(
            m.decide("vipers.bitello.xyz", "/sitemap.xml", false),
            Action::ServeSitemap(Sitemap::Prod {
                name: "vipers".into()
            })
        );
        assert_eq!(
            m.decide("vipers-d.bitello.xyz", "/sitemap.xml", false),
            Action::ServeSitemap(Sitemap::Empty)
        );
        let m2 = m.flags("vipers", true, true);
        let a = m2.decide("vipers.bitello.xyz", "/sitemap.xml", false);
        assert!(
            !matches!(a, Action::ServeSitemap(_)),
            "sitemap preexistente vence: {a:?}"
        );
    }

    #[test]
    fn painel_e_robots_do_painel() {
        let m = Mock::default();
        assert_eq!(m.decide("flux.bitello.xyz", "/", false), Action::Panel);
        assert_eq!(
            m.decide("flux.bitello.xyz", "/robots.txt", false),
            Action::ServeRobots(Robots::DisallowAll)
        );
    }

    /// Rota pública de deploy (10/F4): reconhecida SÓ no host do painel, com
    /// nome válido; a decisão não vê método nem config — handler resolve.
    #[test]
    fn rota_de_deploy_so_no_host_do_painel_com_nome_valido() {
        let m = Mock::default();
        assert_eq!(
            m.decide("flux.bitello.xyz", "/api/projects/vipers/deploy", true),
            Action::Deploy {
                name: "vipers".into()
            },
            "host do painel + path exato → Deploy"
        );
        assert_eq!(
            m.decide("flux.bitello.xyz", "/api/projects/my-app-2/deploy", true),
            Action::Deploy {
                name: "my-app-2".into()
            },
            "nome com dígito e '-' passa"
        );
        // Formatos que NÃO são a rota: caem no fluxo normal do painel.
        for ruim in [
            "/api/projects//deploy",
            "/api/projects/Vipers/deploy",
            "/api/projects/vipers/deploy/", // sufixo divergente
            "/api/projects/vipers/deploi",
            "/api/project/vipers/deploy",
            "/api/deploy",
        ] {
            assert_eq!(
                m.decide("flux.bitello.xyz", ruim, true),
                Action::Panel,
                "path {ruim:?} não é a rota de deploy"
            );
        }
        // robots/sitemap do painel intocados pela novidade.
        assert_eq!(
            m.decide("flux.bitello.xyz", "/robots.txt", false),
            Action::ServeRobots(Robots::DisallowAll)
        );
        assert_eq!(
            m.decide("flux.bitello.xyz", "/sitemap.xml", false),
            Action::ServeSitemap(Sitemap::Empty)
        );
    }

    /// Fora do host do painel o MESMO path mantém o comportamento ATUAL:
    /// Prod/Dev proxyam o container (o path é do app do cliente), host
    /// desconhecido continua no único 404 da matriz (03). Nada de rota nova
    /// vazando para hosts de projeto.
    #[test]
    fn path_de_deploy_fora_do_painel_segue_comportamento_atual() {
        let acordado = Mock::default()
            .st("vipers", Env::Prod, ContainerState::Awake)
            .target("vipers", Env::Prod, "10.0.0.5:80");
        assert_eq!(
            acordado.decide("vipers.bitello.xyz", "/api/projects/vipers/deploy", true),
            Action::Proxy {
                name: "vipers".into(),
                env: Env::Prod,
                target: "10.0.0.5:80".into()
            },
            "prod: path é do app do cliente, não do fluxd"
        );
        let dev = Mock::default()
            .st("vipers", Env::Dev, ContainerState::Awake)
            .target("vipers", Env::Dev, "10.0.0.9:8080");
        assert!(matches!(
            dev.decide("vipers-d.bitello.xyz", "/api/projects/vipers/deploy", true),
            Action::Proxy { .. }
        ));
        let fantasma = Mock::default();
        assert_eq!(
            fantasma.decide("zzz.bitello.xyz", "/api/projects/vipers/deploy", true),
            Action::NotFound
        );
    }

    #[test]
    fn deteccao_de_rota_api() {
        let json = HeaderValue::from_static("application/json");
        assert!(wants_json(Some(&json), "/qualquer/coisa"));
        let html = HeaderValue::from_static("text/html,application/xhtml+xml");
        assert!(!wants_json(Some(&html), "/"));
        assert!(wants_json(Some(&html), "/api/login"), "path /api manda");
        assert!(!wants_json(None, "/"));
    }
}
