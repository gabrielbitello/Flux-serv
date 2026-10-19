//! Sweep do sono + expiração de temporários (L5.1) — a política do tick de
//! 60 s que morava inline no composition root (lib.rs). Cada tick:
//! (1) re-sync + sono (`SleepManager`, só com engine);
//! (2) expiração de temporários (14 — SEM engine o teardown ainda limpa
//!     dirs + SQLite, com aviso no evento).
//! O PRIMEIRO tick dispara já e é PULADO (o interval do tokio dispara
//! imediato; nada dorme 0 s depois do boot).

use std::sync::Arc;

use bollard::Docker;
use flux_common::config::GlobalConfig;

use crate::AppState;

/// Um tick: sono (com engine) + expiração (sempre). Separado do loop para
/// testar sem engessar `tokio::time`.
pub(crate) async fn tick(shared: &AppState, d: Option<&Docker>, global: &GlobalConfig) {
    if let Some(d) = d {
        let acoes = crate::sleep::SleepManager::new(d.clone(), &shared.state, global.clone())
            .sweep()
            .await;
        for a in acoes {
            tracing::info!("{a}");
        }
    }
    let agora = crate::sleep::now_epoch();
    crate::temporary::expire_check(&shared.state, d.cloned(), agora).await;
}

/// Loop do sweep: 60 s por tick, primeiro pulado.
pub async fn iniciar(shared: Arc<AppState>, global: GlobalConfig) {
    let mut timer = tokio::time::interval(std::time::Duration::from_secs(60));
    timer.tick().await; // primeiro tick dispara já — pula
    loop {
        timer.tick().await;
        tick(&shared, shared.docker.as_ref(), &global).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::State;

    const TEMPORARIO: &str = "name = \"demo\"\n[temporary]\nenabled = true\nttl_hours = 6\n";

    /// Caracterização do tick SEM engine (L5.1): a expiração de temporários
    /// RODA (o projeto some do estado) e o ciclo de sono não roda (nada de
    /// pânico sem Docker).
    #[tokio::test]
    async fn tick_sem_engine_expira_temporario() {
        let st = State::in_memory().unwrap();
        st.register_project("demo", TEMPORARIO, "h").unwrap();
        // created_at no passado: 7 h atrás, TTL 6 h → expirado.
        st.with(|c| {
            c.execute(
                "UPDATE projects SET created_at = datetime('now', '-7 hours') WHERE name = ?1",
                rusqlite::params!["demo"],
            )
        })
        .unwrap();
        let app = Arc::new(AppState::new(st, &GlobalConfig::default(), None));

        tick(&app, None, &GlobalConfig::default()).await;

        assert!(
            app.state.list_projects().unwrap().is_empty(),
            "expiração de temporário roda no tick sem engine"
        );
    }
}
