//! Tools de LEITURA do MCP (22/10): diagnóstico puro — nada aqui muda estado
//! (nem lease longo). Saída é JSON curado: campos que a IA precisa, com os
//! nomes do protocolo; o que o protocolo só entrega Deserialize, entra como
//! json! mão-livre SEM inventar campo.

use std::path::PathBuf;

use serde_json::{Value, json};

use crate::api::{ApiClient, ApiError};
use crate::contexto::{contexto_atual_ou_erro, contexto_de_dir};

use super::ferramentas::arg_str;

pub(crate) fn cliente() -> ApiClient {
    ApiClient::from_env()
}

/// Projeto da tool: argumento explícito vence; sem ele, contexto do cwd (o
/// processo MCP pode não estar na pasta do projeto — nesse caso a resposta
/// diz o que passar). Erro de contexto NUNCA mata o servidor.
pub(crate) fn projeto_de(args: &Value) -> Result<String, String> {
    if let Some(p) = arg_str(args, "projeto") {
        return Ok(p);
    }
    let ctx = contexto_atual_ou_erro()?;
    Ok(ctx.name)
}

/// Env do argumento (dev é o padrão) — validação com `Env` do protocolo,
/// sem `exit` (parse_env mata o processo, que aqui é long-lived).
pub(crate) fn env_de(args: &Value) -> Result<flux_common::protocol::Env, String> {
    arg_str(args, "env")
        .unwrap_or_else(|| "dev".to_owned())
        .parse()
        .map_err(|m: String| format!("env inválido: {m}"))
}

/// GET /projects — resumo de todos os registrados.
pub(crate) async fn projetos() -> Result<Value, String> {
    let lista = cliente().list_projects().await.map_err(|e| e.to_string())?;
    serde_json::to_value(lista).map_err(|e| format!("serializando projetos: {e}"))
}

/// GET /projects/{name} — estado curado: containers do engine, sono, migrations.
pub(crate) async fn status(args: &Value) -> Result<Value, String> {
    let projeto = projeto_de(args)?;
    let st = cliente().status(&projeto).await.map_err(erro_com_dica)?;
    Ok(json!({
        "name": st.name,
        "framework": st.framework,
        "prod_host": st.prod_host,
        "dev_host": st.dev_host,
        "dev_container_state": st.dev_container_state,
        "sono": {
            "dev": sono_json(&st.sleep.dev.mode, &st.sleep.dev.lease_until),
            "prod": sono_json(&st.sleep.prod.mode, &st.sleep.prod.lease_until),
        },
        "migrations_aplicadas": {
            "dev": st.migrations.dev.len(),
            "prod": st.migrations.prod.len(),
        }
    }))
}

fn sono_json(mode: &Option<String>, lease_until: &Option<i64>) -> Value {
    json!({
        "mode": mode,
        "lease_until": lease_until
    })
}

/// GET /containers — todos os gerenciados, agrupados por compose project com
/// o estado agregado do `flux ps` (DORMINDO/PARCIAL/ACORDADO).
pub(crate) async fn ps(args: &Value) -> Result<Value, String> {
    let filtro = arg_str(args, "projeto");
    let rows = cliente().containers().await.map_err(|e| e.to_string())?;
    let mut grupos: Vec<Value> = Vec::new();
    for r in rows {
        let nome_curto = r
            .compose_project
            .strip_prefix("flux-")
            .unwrap_or(&r.compose_project);
        if let Some(f) = &filtro
            && !nome_curto.starts_with(f.as_str())
        {
            continue;
        }
        let rodando = r.state == "running";
        match grupos
            .last_mut()
            .filter(|g| g["compose_project"] == json!(r.compose_project))
        {
            Some(g) => {
                g["containers"].as_array_mut().unwrap().push(linha(&r));
                let total = g["containers"].as_array().unwrap().len();
                let ativos = g["containers"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|c| c["state"] == json!("running"))
                    .count();
                g["estado"] = json!(estado_agregado(ativos, total));
            }
            None => {
                grupos.push(json!({
                    "compose_project": r.compose_project,
                    "estado": estado_agregado(usize::from(rodando), 1),
                    "containers": [linha(&r)],
                }));
            }
        }
    }
    Ok(json!({ "projetos": grupos }))
}

fn estado_agregado(rodando: usize, total: usize) -> &'static str {
    if rodando == 0 {
        "DORMINDO"
    } else if rodando == total {
        "ACORDADO"
    } else {
        "PARCIAL"
    }
}

fn linha(r: &crate::api::ContainerRowOut) -> Value {
    json!({
        "service": r.service,
        "state": r.state,
        "status": r.status,
    })
}

/// Sync Windows→VM: sessão mutagen LOCAL (a verdade do alpha no Windows) +
/// a visão do fluxd (report do watch). `dir` é a raiz do projeto no Windows.
pub(crate) async fn sync_status(args: &Value) -> Result<Value, String> {
    let dir = match arg_str(args, "dir") {
        Some(d) => PathBuf::from(d),
        None => {
            std::env::current_dir().map_err(|e| format!("sem `dir` e sem cwd utilizável: {e}"))?
        }
    };
    let ctx = contexto_de_dir(dir)?;
    let sessao =
        crate::sync::SyncSpec::for_project(&ctx.dir, crate::sync::vm_beta(&ctx.name, None), None)
            .map_err(|e| format!("montando sync do projeto: {e}"))?
            .session();

    let local = match crate::sync::status_of(&sessao) {
        Ok(Some(s)) => {
            let saudavel = s.saudavel();
            json!({
                "existe": true,
                "saudavel": saudavel,
                "status": s.status.rotulo(),
                "pausada": s.paused,
                "alpha": lado(&s.alpha),
                "beta": lado(&s.beta),
                "conflitos": s.conflicts,
                "last_error": s.last_error,
            })
        }
        Ok(None) => json!({
            "existe": false,
            "dica": "sessão não existe — `flux up` cria a sync do projeto"
        }),
        Err(e) => return Err(format!("apurando status da sync: {e}")),
    };

    let cliente = cliente();
    let fluxd = match cliente.sync_status().await {
        Ok(st) => st
            .projects
            .into_iter()
            .find(|p| p.project == ctx.name)
            .map(|p| {
                json!({
                    "status": p.status,
                    "files": p.files,
                    "problems": p.problems,
                    "last_error": p.last_error,
                    "rescan_requested": p.rescan_requested,
                })
            }),
        // Sem visão do fluxd a resposta segue com a local — o que interessa
        // para "o sync está correto?" é a sessão do mutagen.
        Err(_) => None,
    };
    let veredito = veredito_sync(&local, &fluxd);
    Ok(json!({
        "projeto": ctx.name,
        "sessao": sessao,
        "local": local,
        "fluxd": fluxd,
        "veredito": veredito,
    }))
}

fn lado(e: &crate::sync::EndpointStatus) -> Value {
    json!({
        "path": e.path,
        "conectado": e.connected,
        "arquivos": e.files,
        "scan_problems": e.scan_problems,
        "transition_problems": e.transition_problems,
    })
}

/// Veredito honesto: "ok" SÓ com sessão saudável (sem pendências) e, quando
/// existe visão do fluxd, sem problemas nem rescan pendente. Qualquer
/// dúvida = "atenção" com o porquê — nunca "ok" chutado.
fn veredito_sync(local: &Value, fluxd: &Option<Value>) -> &'static str {
    if local["saudavel"] != json!(true) {
        return "atenção";
    }
    match fluxd {
        Some(f) if f["problems"] != json!(0) || f["rescan_requested"] == json!(true) => "atenção",
        _ => "ok",
    }
}

/// GET /projects/{name}/db — banco próprio: container, databases, TTL.
pub(crate) async fn db_status(args: &Value) -> Result<Value, String> {
    let projeto = projeto_de(args)?;
    let st = cliente().db_status(&projeto).await.map_err(erro_com_dica)?;
    serde_json::to_value(st).map_err(|e| format!("serializando db status: {e}"))
}

/// GET /projects/{name}/logs (sem follow) — cauda crua do container.
pub(crate) async fn logs(args: &Value) -> Result<Value, String> {
    let projeto = projeto_de(args)?;
    let env = env_de(args)?;
    let tail = super::ferramentas::arg_int(args, "tail", 200, 1, 1000);
    let caminho = crate::comandos::logs::caminho_logs(&projeto, &env.to_string(), tail, false);
    let corpo = cliente()
        .call_raw("GET", &caminho, None)
        .await
        .map_err(|e| e.to_string())?;
    Ok(json!({ "projeto": projeto, "env": env.to_string(), "tail": tail, "logs": corpo }))
}

/// 404 do fluxd = projeto não registrado; a dica é a lista do que existe
/// (mesmo caminho do `flux status` do CLI).
pub(crate) fn erro_com_dica(e: ApiError) -> String {
    match &e {
        ApiError::NaoEncontrado { msg } => format!("{msg} — nomes válidos: consulte flux_projetos"),
        _ => e.to_string(),
    }
}
