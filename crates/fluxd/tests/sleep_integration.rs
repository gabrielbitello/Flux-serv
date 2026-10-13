//! F4 gated: o sweep dorme o dev ocioso (`idle_minutes = 0`), respeita o
//! cooldown pós-wake e re-sincroniza estado externo. GATED por
//! `FLUX_DOCKER_URL` (engine real — docker do WSL em dev).

use flux_common::config::GlobalConfig;
use flux_common::protocol::{ContainerState, Env};
use fluxd::decision::HostSource;
use fluxd::docker::{self, DockerSource};
use fluxd::sleep::{SleepManager, WAKE_COOLDOWN_SECS};
use fluxd::state::State;

#[tokio::test(flavor = "multi_thread")]
async fn sweep_dorme_ocioso_e_cooldown_segura() {
    if std::env::var("FLUX_DOCKER_URL").is_err() {
        eprintln!("pulado: FLUX_DOCKER_URL não definido — sem engine de Docker");
        return;
    }
    let Some(engine) = DockerSource::connect() else {
        eprintln!("pulado: Docker não conectou");
        return;
    };
    use bollard::query_parameters::RemoveContainerOptionsBuilder;
    let rm = RemoveContainerOptionsBuilder::default().force(true).build();
    let _ = engine
        .remove_container("flux-sono-dev-app-1", Some(rm.clone()))
        .await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    let st = State::in_memory().unwrap();
    st.register_project("sono", "[sleep]\nidle_minutes = 0\n", "h")
        .unwrap();
    let src = DockerSource::new(engine.clone(), &st);

    let id = docker::create_dummy_container(&engine, "sono", Env::Dev)
        .await
        .expect("criar dummy");
    src.wake("sono", Env::Dev).await.expect("acordar dummy");
    for _ in 0..20 {
        if src.env_state("sono", Env::Dev) == ContainerState::Awake {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    }
    assert_eq!(src.env_state("sono", Env::Dev), ContainerState::Awake);

    // sinal recente + cooldown do wake: sweep NÃO dorme
    let mgr = SleepManager::new(engine.clone(), &st, GlobalConfig::default());
    let _ = st.set_cooldown("sono", Env::Dev, WAKE_COOLDOWN_SECS);
    let acoes = mgr.sweep().await;
    assert!(
        !acoes.iter().any(|a| a.contains("dormiu")),
        "cooldown deveria segurar: {acoes:?}"
    );

    // cooldown expirado + sinal antigo + idle_minutes=0: sweep dorme (stop)
    st.set_cooldown("sono", Env::Dev, -1).unwrap();
    st.touch_interaction("sono", Env::Dev).unwrap();
    st.with(|c| {
        c.execute(
            "UPDATE sleep_state SET last_interaction = datetime('now', '-1 minute')
             WHERE project = 'sono' AND env = 'dev'",
            [],
        )
    })
    .unwrap();
    let acoes = mgr.sweep().await;
    assert!(
        acoes.iter().any(|a| a.contains("sono/dev dormiu (stop)")),
        "esperava sono/dev dormindo — {acoes:?}"
    );
    let (_, _, _, mode) = st.sleep_row("sono", Env::Dev).unwrap();
    assert_eq!(mode.as_deref(), Some("sleeping"));

    // verdade do Docker: parado
    assert_eq!(src.env_state("sono", Env::Dev), ContainerState::Sleeping);

    // limpeza
    engine
        .remove_container(&id, Some(rm))
        .await
        .expect("remover dummy");
}
