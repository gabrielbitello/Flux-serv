//! ciclo (L7.2): extraído verbatim do main.rs — movimento sem "melhoria".

use crate::contexto::{LEASE_UP_SECS, contexto_atual, falhar, parse_env};

use crate::api::{ApiClient, ApiError};

/// `flux status [env]` — o que a API de controle expõe hoje. Projeto do
/// diretório não registrado → mostra a lista do fluxd como hint e sai com
/// exit ≠ 0 (o caminho de conserto é `flux up` no repo).
pub(crate) async fn status(env: &str) {
    let _ = parse_env(env); // valida cedo, com exit 2 em valor ruim
    let ctx = contexto_atual();
    let cliente = ApiClient::from_env();
    let st = match cliente.status(&ctx.name).await {
        Ok(s) => s,
        Err(ApiError::NaoEncontrado { msg }) => {
            eprintln!("erro: {msg}");
            match cliente.list_projects().await {
                Ok(lista) if lista.is_empty() => {
                    eprintln!("o fluxd não tem nenhum projeto registrado ainda");
                }
                Ok(lista) => {
                    let nomes: Vec<&str> = lista.iter().map(|p| p.name.as_str()).collect();
                    eprintln!("registrados no fluxd: {}", nomes.join(", "));
                }
                Err(e) => eprintln!("aviso: não consegui listar os projetos do fluxd: {e}"),
            }
            std::process::exit(1);
        }
        Err(e) => falhar(e),
    };
    imprime_status(&st);
}

/// Colunas da 06 suportadas AGORA (sono 04, migrations 05): sync/health/build
/// em tempo real dependem de fases do fluxd — aparecem como "—", nunca
/// inventados.
pub(crate) fn imprime_status(st: &crate::api::ProjectStatusOut) {
    println!(
        "{} · {} · prod https://{} · dev https://{}",
        st.name, st.framework, st.prod_host, st.dev_host
    );
    println!("  sync: — (fase F2 do fluxd) · health: — (fase F7 do fluxd)");
    for (rotulo, linha) in [("dev", &st.sleep.dev), ("prod", &st.sleep.prod)] {
        let mode = linha.mode.as_deref().unwrap_or("sem sinal");
        let lease = match linha.lease_until {
            Some(t) => format!(" · lease até epoch {t}"),
            None => String::new(),
        };
        println!("  sono {rotulo}: {mode}{lease}");
    }
    println!(
        "  migrations aplicadas — dev: {} · prod: {} (pendências: calculadas no `flux up` contra o disco do projeto)",
        st.migrations.dev.len(),
        st.migrations.prod.len()
    );
}

/// `flux ps [env]` — espelho do `docker compose ps` (06) com a regra do
/// `docker ps`: FORA de projeto (pasta sem flux.toml e não registrada)
/// lista TODOS os gerenciados; dentro, filtra o projeto (env afina
/// dev/prod). Estado cru do engine via GET /containers.
pub(crate) async fn ps(env: &str) {
    let cliente = ApiClient::from_env();
    let rows = match cliente.containers().await {
        Ok(r) => r,
        Err(e) => falhar(e),
    };
    if rows.is_empty() {
        println!("nenhum container gerenciado (nenhum `flux up` ainda)");
        return;
    }
    let cwd = std::env::current_dir().unwrap_or_default();
    let pasta = cwd
        .file_name()
        .and_then(|n| n.to_str())
        .map(str::to_owned)
        .unwrap_or_default();
    // Contexto: flux.toml vence (o `name` DELE, não o da pasta — djm mora
    // em DJ-is-magic); sem toml, o nome da pasta se estiver registrado.
    let projeto_ctx: Option<String> = if cwd.join("flux.toml").is_file() {
        std::fs::read_to_string(cwd.join("flux.toml"))
            .ok()
            .and_then(|src| flux_common::config::ProjectToml::parse_str(&src).ok())
            .map(|t| t.effective_name(&pasta).to_owned())
    } else {
        let registrados: Vec<String> = cliente
            .list_projects()
            .await
            .map(|l| l.into_iter().map(|p| p.name).collect())
            .unwrap_or_default();
        registrados.into_iter().find(|r| r == &pasta)
    };
    let env_filtro = if projeto_ctx.is_some() {
        parse_env(env);
        Some(env.to_owned())
    } else {
        None
    };

    let alvo = |r: &crate::api::ContainerRowOut| {
        // compose project: flux-{name}-{env} (worktrees: flux-{name}-@{slug}-{env})
        r.compose_project
            .strip_prefix("flux-")
            .unwrap_or(&r.compose_project)
            .to_owned()
    };
    let nome_ctx: Option<String> = match (&projeto_ctx, env_filtro.as_ref()) {
        (Some(p), Some(e)) => Some(format!("flux-{p}-{e}")),
        (Some(p), None) => Some(format!("flux-{p}-")),
        _ => None,
    };

    let mut visiveis: Vec<&crate::api::ContainerRowOut> = rows
        .iter()
        .filter(|r| match &nome_ctx {
            Some(prefixo) => r.compose_project.starts_with(prefixo.as_str()),
            None => true,
        })
        .collect();
    if visiveis.is_empty() {
        if let Some(p) = projeto_ctx {
            eprintln!(
                "nenhum container de '{p}' — rode `flux up` (ou `flux ps` fora do projeto para ver todos)"
            );
            std::process::exit(1);
        }
        return;
    }
    // Projeto do contexto primeiro; resto depois (fora de projeto = tudo).
    visiveis.sort_by_key(|r| {
        let do_ctx = projeto_ctx
            .as_ref()
            .is_some_and(|p| alvo(r).starts_with(p.as_str()));
        (!do_ctx, r.compose_project.clone(), r.service.clone())
    });

    // Agrupado por projeto ("pastas", pedido do Gabriel 14/10) — estado
    // agregado no header: DORMINDO = nada rodando; PARCIAL = misto (não
    // deveria existir: sono de dev para o projeto INTEIRO); ACORDADO = tudo up.
    let mut grupos: Vec<(String, Vec<&crate::api::ContainerRowOut>)> = Vec::new();
    for r in &visiveis {
        match grupos.last_mut() {
            Some((p, v)) if *p == r.compose_project => v.push(r),
            _ => grupos.push((r.compose_project.clone(), vec![r])),
        }
    }
    for (projeto, containers) in &grupos {
        let rodando = containers.iter().filter(|c| c.state == "running").count();
        let estado_projeto = if rodando == 0 {
            "DORMINDO"
        } else if rodando == containers.len() {
            "ACORDADO"
        } else {
            "PARCIAL"
        };
        // flux-{name}-{env} → o nome que o Gabriel conhece (a pasta).
        let nome = projeto
            .strip_prefix("flux-")
            .unwrap_or(projeto)
            .trim_end_matches("-dev")
            .trim_end_matches("-prod");
        println!("{nome}  [{estado_projeto}]  ({projeto})");
        for c in containers {
            println!("    {:<12} {:<9} {}", c.service, c.state, c.status);
        }
        println!();
    }
    let total = rows.len();
    if projeto_ctx.is_some() && visiveis.len() < total {
        println!(
            "+ {} container(ns) de outros projetos (flux ps fora do projeto lista todos)",
            total - visiveis.len()
        );
    }
}

/// `flux down [env]` (06 + contrato CLI ↔ fluxd): POST /projects/{name}/down —
/// o fluxd roda `docker compose -p flux-{name}-{env} stop` (containers param;
/// banco único segue de pé) e responde `{"stopped":true,"project":…}`. Erro
/// sai com a causa e exit ≠ 0 (06).
pub(crate) async fn down(env: &str) {
    let env = parse_env(env);
    let ctx = contexto_atual();
    let cliente = ApiClient::from_env();
    match cliente.parar(&ctx.name, &env.to_string()).await {
        Ok(r) => {
            if r.stopped {
                println!(
                    "down {}: containers parados (banco único segue de pé)",
                    r.project
                );
            } else {
                // O contrato só devolve stopped=false se algo estranho ocorrer
                // com 200 — nunca tratar como sucesso silencioso.
                eprintln!("erro: fluxd respondeu stopped=false para {}", r.project);
                std::process::exit(1);
            }
        }
        Err(e) => falhar(e),
    }
}

/// restart/ps — o CLI já fala o protocolo da 07: chama o endpoint do
/// projeto, mostra a resposta e sai com exit ≠ 0 enquanto o backend do fluxd
/// ainda é 501. Quando a fase do backend chegar, o comando funciona sem
/// mudança aqui. (down saiu daqui: virou contrato real, `down()` acima;
/// build também — virou o `flux build` real da 02, `build_assets()` acima.)
pub(crate) async fn ciclo_api(acao: &str, env: &str) {
    let env = parse_env(env);
    let ctx = contexto_atual();
    let cliente = ApiClient::from_env();
    let (method, caminho) = match acao {
        "restart" => ("POST", format!("/projects/{}/restart?env={env}", ctx.name)),
        "ps" => ("GET", format!("/projects/{}/ps?env={env}", ctx.name)),
        _ => unreachable!("ação não mapeada: {acao}"),
    };
    match cliente.call_raw(method, &caminho, None).await {
        Ok(corpo) => {
            if corpo.trim().is_empty() {
                println!("{acao} {}: ok", ctx.name);
            } else {
                println!("{acao} {}: {corpo}", ctx.name);
            }
        }
        Err(e) => falhar(e),
    }
}

/// `flux logs [-f] [--tail N] [env]` (06 + contrato CLI ↔ fluxd). Lease ANTES
/// da primeira chamada (04: `flux logs` segura o ambiente acordado; a guarda
/// renova até o comando acabar e o lease morre sozinho no TTL depois). Sem
/// -f: GET devolve text/plain cru. Com -f: consome o stream SSE do fluxd
/// frame a frame até término limpo (fluxd corta; Ctrl+C mata o processo).
/// `flux db fresh [env] --yes [--seed/--no-seed]` — reconstrução forçada de
/// dev (pedido do Gabriel): POST /projects/{name}/fresh; prod é recusado no
/// fluxd (05) e o env argumento nem chega lá com outro valor (parse_env).
pub(crate) async fn db_fresh(env: &str, seed: Option<bool>) {
    let _ = parse_env(env);
    let ctx = contexto_atual();
    let cliente = ApiClient::from_env();
    let corpo = serde_json::to_string(&crate::api::FreshIn {
        env: env.to_owned(),
        seed,
    })
    .map_err(|e| crate::api::ApiError::Protocolo {
        msg: format!("serializando fresh: {e}"),
    })
    .unwrap_or_default();
    match cliente
        .call_raw(
            "POST",
            &format!("/projects/{}/fresh", ctx.name),
            Some(corpo),
        )
        .await
    {
        Ok(saida) => {
            println!("fresh {}/dev: reconstruído (dados apagados)", ctx.name);
            let cauda: Vec<&str> = saida.lines().rev().take(6).collect();
            for linha in cauda.iter().rev() {
                println!("  {linha}");
            }
        }
        Err(e) => falhar(e),
    }
}

/// `flux exec [env] -- <comando…>` (06 + contrato CLI ↔ fluxd): corpo
/// `{"cmd":[…]}`; o fluxd roda `docker exec` no container app e responde
/// text/plain com a ÚLTIMA linha `exit={code}` — o CLI repassa o código como
/// exit próprio. Lease idem logs (04).
pub(crate) async fn exec(command: Vec<String>, env: &str) {
    if command.is_empty() {
        eprintln!(
            "erro: uso: flux exec [env] -- <comando…> (ex.: flux exec dev -- php artisan migrate --force)"
        );
        std::process::exit(2);
    }
    let env = parse_env(env);
    let ctx = contexto_atual();
    let cliente = ApiClient::from_env();

    match cliente
        .lease(&ctx.name, &env.to_string(), LEASE_UP_SECS)
        .await
    {
        Ok(_) => {}
        Err(e) => falhar(e),
    }
    let _guarda = crate::api::manter_lease(
        cliente.clone(),
        ctx.name.clone(),
        env.to_string(),
        LEASE_UP_SECS,
    );

    match cliente
        .executar(&ctx.name, &env.to_string(), &command)
        .await
    {
        Ok((saida, code)) => {
            print!("{saida}");
            if !saida.ends_with('\n') {
                println!();
            }
            std::process::exit(code);
        }
        Err(e) => falhar(e),
    }
}
