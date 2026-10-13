//! Teste de integração com Docker REAL (F3): cria container dummy com as
//! labels da convenção, valida estado/alto via `DockerSource` e as ações
//! wake/pause/unpause/sleep. GATED: só roda com `FLUX_DOCKER_URL` apontando
//! para um engine (ex.: docker do WSL em tcp://127.0.0.1:2375). Sem a env,
//! o teste passa vazio — CI pode injetar o engine quando existir.

use flux_common::protocol::{ContainerState, Env};
use fluxd::decision::HostSource;
use fluxd::docker::{self, DockerSource};
use fluxd::state::State;

#[tokio::test(flavor = "multi_thread")]
async fn docker_source_ciclo_completo_de_estado() {
    if std::env::var("FLUX_DOCKER_URL").is_err() {
        eprintln!("pulado: FLUX_DOCKER_URL não definido — sem engine de Docker");
        return;
    }
    let Some(engine) = DockerSource::connect() else {
        eprintln!("pulado: Docker não conectou mesmo com FLUX_DOCKER_URL");
        return;
    };
    let st = State::in_memory().unwrap();
    st.register_project("teste", "name = \"teste\"", "h")
        .unwrap();
    let src = DockerSource::new(engine.clone(), &st);

    use bollard::query_parameters::RemoveContainerOptionsBuilder;
    let rm = RemoveContainerOptionsBuilder::default().force(true).build();

    // limpeza defensiva: rodada anterior pode ter deixado o dummy (CREATED
    // conta como Starting e suja o assert de ausência abaixo)
    let _ = engine
        .remove_container("flux-teste-dev-app-1", Some(rm.clone()))
        .await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    // container ausente → Unknown (→ decisão trata como Starting)
    assert_eq!(src.env_state("teste", Env::Dev), ContainerState::Unknown);

    let id = docker::create_dummy_container(&engine, "teste", Env::Dev)
        .await
        .expect("criar container dummy");

    // criado e nunca rodado → Starting; sem IP → sem alvo de proxy
    assert_eq!(src.env_state("teste", Env::Dev), ContainerState::Starting);
    assert_eq!(src.proxy_target("teste", Env::Dev), None);

    // wake → Awake + alvo de proxy com IP
    src.wake("teste", Env::Dev).await.expect("wake");
    for _ in 0..10 {
        if src.env_state("teste", Env::Dev) == ContainerState::Awake {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    }
    assert_eq!(src.env_state("teste", Env::Dev), ContainerState::Awake);
    let target = src
        .proxy_target("teste", Env::Dev)
        .expect("ip do container");
    assert!(target.contains(':'), "alvo precisa ser ip:porta — {target}");

    // pause/unpause (hibernação de prod, 11)
    src.pause("teste", Env::Dev).await.expect("pause");
    assert_eq!(src.env_state("teste", Env::Dev), ContainerState::Paused);
    src.unpause("teste", Env::Dev).await.expect("unpause");
    assert_eq!(src.env_state("teste", Env::Dev), ContainerState::Awake);

    // sleep → parado
    src.sleep("teste", Env::Dev).await.expect("sleep");
    assert_eq!(src.env_state("teste", Env::Dev), ContainerState::Sleeping);

    // limpeza — container dummy sai do engine
    engine
        .remove_container(&id, Some(rm))
        .await
        .expect("remover container dummy");
}
