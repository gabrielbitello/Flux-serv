//! F5 gated: `ensure_schema` contra engine real — caminho `migrate_cmd`
//! (node/rust) executa DENTRO do container via `docker exec`; caminho laravel
//! com migration mutada em prod bloqueia SEM executar nada.

use flux_common::protocol::Env;
use fluxd::db::{self, SchemaOutcome};
use fluxd::docker::{self, DockerSource};
use fluxd::state::State;

#[tokio::test(flavor = "multi_thread")]
async fn migrate_cmd_roda_dentro_do_container() {
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
        .remove_container("flux-schema-dev-app-1", Some(rm.clone()))
        .await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    let st = State::in_memory().unwrap();
    st.register_project("schema", "name = \"schema\"", "h")
        .unwrap();
    let src = DockerSource::new(engine.clone(), &st);

    let id = docker::create_dummy_container(&engine, "schema", Env::Dev)
        .await
        .expect("dummy");
    src.wake("schema", Env::Dev).await.expect("acordar");

    let toml =
        "framework = \"custom\"\n[db]\nmigrate_cmd = \"sh -c 'echo migrado > /tmp/flux-ok'\"\n";
    let out = db::ensure_schema(&engine, &st, "schema", Env::Dev, toml, &[])
        .await
        .expect("ensure_schema");
    let SchemaOutcome::MigrateCmd { output } = out else {
        panic!("esperava MigrateCmd — {out:?}");
    };
    // o comando redireciona para arquivo: stdout vazio é o comportamento
    // correto — a prova de execução é o arquivo dentro do container (abaixo)
    assert!(output.is_empty(), "saída: {output}");

    // prova de execução DENTRO do container: o arquivo existe lá
    let (_, code) = dbg_probe(&engine).await;
    assert_eq!(code, 0, "arquivo /tmp/flux-ok deveria existir no container");

    // laravel + mutação em prod = bloqueio SEM executar (05)
    st.register_project("bloq", "name = \"bloq\"\nframework = \"laravel\"", "h")
        .unwrap();
    let files = vec![("2024_x.php".to_owned(), "hash_novo".to_owned())];
    st.record_migration("bloq", Env::Prod, "2024_x.php", "hash_velho")
        .unwrap();
    let err = db::ensure_schema(
        &engine,
        &st,
        "bloq",
        Env::Prod,
        "framework = \"laravel\"",
        &files,
    )
    .await
    .expect_err("prod mutado deveria bloquear");
    assert!(err.contains("deploy bloqueado"), "erro: {err}");

    engine
        .remove_container(&id, Some(rm))
        .await
        .expect("limpar");
}

/// exec rápido `cat /tmp/flux-ok` via db:: — valida que o marker ficou no FS
/// do container (reusa o caminho interno pelo caminho público ensure_schema?).
/// Simples: exec via DockerSource não expõe exec — faz via bollard direto.
async fn dbg_probe(engine: &bollard::Docker) -> (String, i64) {
    use bollard::exec::CreateExecOptions;
    use futures_util::StreamExt;
    let c = fluxd::docker::find_app_container(
        engine,
        &fluxd::docker::compose_project("schema", Env::Dev),
    )
    .await
    .expect("container");
    let id = c.id.clone().unwrap();
    let exec = engine
        .create_exec(
            &id,
            CreateExecOptions {
                cmd: Some(vec!["cat".to_owned(), "/tmp/flux-ok".to_owned()]),
                attach_stdout: Some(true),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let mut out = String::new();
    if let Ok(bollard::exec::StartExecResults::Attached { output, .. }) =
        engine.start_exec(&exec.id, None).await
    {
        let mut o = output;
        while let Some(chunk) = o.next().await {
            if let Ok(bollard::container::LogOutput::StdOut { message }) = chunk {
                out.push_str(&String::from_utf8_lossy(&message));
            }
        }
    }
    let inspected = engine.inspect_exec(&exec.id).await.unwrap();
    (out, inspected.exit_code.unwrap_or(-1))
}
