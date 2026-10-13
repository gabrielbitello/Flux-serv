//! Sleep manager (F4, spec 04): dev dorme (`docker stop`) após `idle_minutes`
//! sem sinal; prod hiberna (`docker pause`, 11) após `prod_idle_minutes`.
//! Sinais que mantêm acordado: request proxyado, lease de CLI/painel, CPU
//! acima do teto. Cooldown anti-flapping de 5 min após acordar. A VERDADE do
//! estado é re-sincronizada do Docker a cada sweep — estado interno nunca é
//! autoridade (07).

use std::time::{SystemTime, UNIX_EPOCH};

use bollard::Docker;
use flux_common::config::GlobalConfig;
use flux_common::config::ProjectToml;
use flux_common::protocol::{ContainerState, Env};

use crate::docker::{self, DockerSource};
use crate::events;
use crate::state::State;

/// Cooldown anti-flapping pós-acordar (04): 5 min imune a dormir.
pub const WAKE_COOLDOWN_SECS: i64 = 300;

pub fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Decisão pura de dormir — todos os inputs explícitos, testável sem Docker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SleepDecision {
    /// Continua acordado: faltam N segundos para o limite.
    KeepAwake { remaining_secs: i64 },
    /// Sinal ativo: lease ou cooldown ainda válidos.
    Held,
    /// CPU acima do teto — zera o timer (teste/build rodando, 04).
    Busy,
    /// Dormir agora (dev: stop; prod: pause conforme `[sleep] prod_mode`).
    Sleep,
}

pub fn should_sleep(
    docker_state: ContainerState,
    now: i64,
    last_interaction: Option<i64>,
    lease_until: Option<i64>,
    cooldown_until: Option<i64>,
    idle_limit_secs: i64,
) -> SleepDecision {
    if docker_state != ContainerState::Awake {
        return SleepDecision::KeepAwake { remaining_secs: -1 };
    }
    if lease_until.is_some_and(|t| t > now) || cooldown_until.is_some_and(|t| t > now) {
        return SleepDecision::Held;
    }
    let Some(last) = last_interaction else {
        // Nunca houve sinal: não há ociosidade medível — não dorme (dado
        // ausente não vira "está ocioso há infinito").
        return SleepDecision::KeepAwake {
            remaining_secs: idle_limit_secs,
        };
    };
    let idle = now - last;
    if idle >= idle_limit_secs {
        SleepDecision::Sleep
    } else {
        SleepDecision::KeepAwake {
            remaining_secs: idle_limit_secs - idle,
        }
    }
}

/// Limites de sono de um projeto, com precedência: `[sleep]` do `flux.toml`
/// vence; default vem do config global (15 min dev / 30 min prod).
#[derive(Debug, Clone, Copy)]
pub struct SleepLimits {
    pub dev_enabled: bool,
    pub dev_idle_secs: i64,
    pub prod_mode: flux_common::config::ProdSleepMode,
    pub prod_idle_secs: i64,
    pub cpu_percent_teto: f64,
}

pub fn limits_for(toml_src: &str, global: &GlobalConfig) -> SleepLimits {
    let t = ProjectToml::parse_str(toml_src).ok();
    let sleep = t.as_ref().and_then(|p| p.sleep.as_ref());
    SleepLimits {
        dev_enabled: sleep.and_then(|s| s.dev_enabled).unwrap_or(true),
        dev_idle_secs: (sleep
            .and_then(|s| s.idle_minutes)
            .unwrap_or_else(|| global.idle_minutes()) as i64)
            * 60,
        prod_mode: sleep
            .and_then(|s| s.prod_mode)
            .unwrap_or(flux_common::config::ProdSleepMode::Pause),
        prod_idle_secs: (sleep.and_then(|s| s.prod_idle_minutes).unwrap_or(30) as i64) * 60,
        cpu_percent_teto: sleep.and_then(|s| s.cpu_percent_teto).unwrap_or(5.0),
    }
}

pub struct SleepManager<'a> {
    docker: Docker,
    st: &'a State,
    global: GlobalConfig,
}

/// CPU do container em % (0–100×núcleos), de um snapshot de stats.
pub fn cpu_percent(stats: &bollard::models::ContainerStatsResponse) -> f64 {
    let (Some(cpu_delta), Some(system_delta)) = (
        stats
            .cpu_stats
            .as_ref()
            .and_then(|s| s.cpu_usage.as_ref().and_then(|u| u.total_usage))
            .map(|v| v as f64),
        stats
            .cpu_stats
            .as_ref()
            .and_then(|s| s.system_cpu_usage)
            .map(|v| v as f64),
    ) else {
        return 0.0;
    };
    if system_delta <= 0.0 || cpu_delta <= 0.0 {
        return 0.0;
    }
    let online = stats
        .cpu_stats
        .as_ref()
        .and_then(|s| s.online_cpus)
        .unwrap_or(1) as f64;
    (cpu_delta / system_delta) * online * 100.0
}

impl<'a> SleepManager<'a> {
    pub fn new(docker: Docker, st: &'a State, global: GlobalConfig) -> Self {
        Self { docker, st, global }
    }

    /// CPU % atual do container (um snapshot de stats, não stream).
    async fn container_cpu(&self, id: &str) -> f64 {
        use futures_util::StreamExt;
        let mut stream = self.docker.stats(id, None);
        match stream.next().await {
            Some(Ok(stats)) => cpu_percent(&stats),
            Some(Err(e)) => {
                tracing::warn!("stats de {id} falhou: {e}");
                0.0
            }
            None => 0.0,
        }
    }

    /// Um ciclo do sweep (60 s): re-sincroniza estado do Docker e dorme quem
    /// estourou ociosidade. Retorna o que fez (para o painel/F15).
    pub async fn sweep(&self) -> Vec<String> {
        let mut acoes = Vec::new();
        let Ok(projetos) = self.st.list_projects() else {
            return acoes;
        };
        let now = now_epoch();
        for name in projetos {
            for env in [Env::Dev, Env::Prod] {
                if let Some(acao) = self.sweep_one(&name, env, now).await {
                    acoes.push(acao);
                }
            }
        }
        acoes
    }

    async fn sweep_one(&self, name: &str, env: Env, now: i64) -> Option<String> {
        let Ok(toml_src) = self.st.with(|c| {
            c.query_row("SELECT toml FROM projects WHERE name = ?1", [name], |r| {
                r.get::<_, String>(0)
            })
        }) else {
            return None;
        };
        let limits = limits_for(&toml_src, &self.global);

        use crate::decision::HostSource as _;
        let src = DockerSource::new(self.docker.clone(), self.st);
        let docker_state = src.env_state(name, env);

        // Re-sync: a verdade é o Docker. Divergência interna é corrigida aqui.
        let (_, _, _, mode_interno) = self.st.sleep_row(name, env).ok()?;
        let mode_esperado = docker_state.as_str().to_owned();
        if mode_interno.as_deref() != Some(mode_esperado.as_str()) {
            let _ = self.st.record_mode(name, env, docker_state);
            events::event(
                self.st,
                "info",
                "sleep",
                &format!(
                    "re-sync {name}/{env}: {mode_interno:?} → {mode_esperado} (verdade do Docker)"
                ),
            );
        }

        // Prod em pause NÃO dorme de novo (já hibernado); Awake/Paused do
        // Docker são os únicos estados elegíveis.
        if !matches!(docker_state, ContainerState::Awake) {
            return None;
        }

        let Ok((last, lease, cooldown, _)) = self.st.sleep_row(name, env) else {
            return None;
        };

        let (idle_limit, acao) = match env {
            Env::Dev => {
                if !limits.dev_enabled {
                    return None;
                }
                (limits.dev_idle_secs, "stop")
            }
            Env::Prod => match limits.prod_mode {
                flux_common::config::ProdSleepMode::Off => return None,
                flux_common::config::ProdSleepMode::Pause => (limits.prod_idle_secs, "pause"),
                flux_common::config::ProdSleepMode::Stop => (limits.prod_idle_secs, "stop"),
            },
        };

        match should_sleep(docker_state, now, last, lease, cooldown, idle_limit) {
            SleepDecision::Sleep => {
                // Guarda de CPU (04): teste/build rodando zera o timer.
                let cpu = self.container_cpu_ativa(name, env).await;
                if cpu > limits.cpu_percent_teto {
                    let _ = self.st.touch_interaction(name, env);
                    return Some(format!("{name}/{env}: CPU {cpu:.1}% > teto — timer zerado"));
                }
                let resultado = match acao {
                    "pause" => src.pause(name, env).await,
                    _ => src.sleep(name, env).await,
                };
                match resultado {
                    Ok(()) => {
                        let novo = if acao == "pause" {
                            ContainerState::Paused
                        } else {
                            ContainerState::Sleeping
                        };
                        let _ = self.st.record_mode(name, env, novo);
                        let msg = format!("{name}/{env} dormiu ({acao}) após ociosidade");
                        events::event(self.st, "info", "sleep", &msg);
                        Some(msg)
                    }
                    Err(e) => {
                        let msg = format!("{name}/{env}: falha ao dormir ({acao}): {e}");
                        events::event(self.st, "error", "sleep", &msg);
                        Some(msg)
                    }
                }
            }
            SleepDecision::Busy => Some(format!("{name}/{env}: CPU alta — acordado")),
            SleepDecision::Held => None,
            SleepDecision::KeepAwake { .. } => None,
        }
    }

    async fn container_cpu_ativa(&self, name: &str, env: Env) -> f64 {
        let Some(c) =
            docker::find_app_container(&self.docker, &docker::compose_project(name, env)).await
        else {
            return 0.0;
        };
        match c.id.as_deref() {
            Some(id) => self.container_cpu(id).await,
            None => 0.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_common::protocol::ContainerState;

    #[test]
    fn so_dorme_acordado_e_ocioso_sem_guardas() {
        let now = 1_000_000;
        assert_eq!(
            should_sleep(
                ContainerState::Sleeping,
                now,
                Some(now - 999),
                None,
                None,
                900
            ),
            SleepDecision::KeepAwake { remaining_secs: -1 }
        );
        assert_eq!(
            should_sleep(
                ContainerState::Awake,
                now,
                Some(now - 999),
                Some(now + 60),
                None,
                900
            ),
            SleepDecision::Held
        );
        assert_eq!(
            should_sleep(
                ContainerState::Awake,
                now,
                Some(now - 999),
                None,
                Some(now + 60),
                900
            ),
            SleepDecision::Held
        );
        assert_eq!(
            should_sleep(ContainerState::Awake, now, None, None, None, 900),
            SleepDecision::KeepAwake {
                remaining_secs: 900
            }
        );
        assert_eq!(
            should_sleep(ContainerState::Awake, now, Some(now - 600), None, None, 900),
            SleepDecision::KeepAwake {
                remaining_secs: 300
            }
        );
        assert_eq!(
            should_sleep(ContainerState::Awake, now, Some(now - 901), None, None, 900),
            SleepDecision::Sleep
        );
    }

    #[test]
    fn limites_do_toml_vencem_o_global() {
        let global = GlobalConfig::default();
        let padrao = limits_for("", &global);
        assert_eq!(padrao.dev_idle_secs, 15 * 60);
        assert_eq!(padrao.prod_idle_secs, 30 * 60);
        assert!(padrao.dev_enabled);
        assert_eq!(padrao.cpu_percent_teto, 5.0);

        let custom = limits_for(
            "[sleep]\nidle_minutes = 5\nprod_idle_minutes = 10\nprod_mode = \"stop\"\ndev_enabled = false\ncpu_percent_teto = 2.5\n",
            &global,
        );
        assert_eq!(custom.dev_idle_secs, 5 * 60);
        assert_eq!(custom.prod_idle_secs, 10 * 60);
        assert!(!custom.dev_enabled);
        assert_eq!(custom.prod_mode, flux_common::config::ProdSleepMode::Stop);
        assert_eq!(custom.cpu_percent_teto, 2.5);
    }
}
