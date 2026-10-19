//! Blue-green de PROD (10/R12): troca de cor com drain e health — o domínio
//! inteiro num módulo próprio (L6.1; antes morava em compose.rs sem menção no
//! doc dele). Consumidores: API (consulta F9 usa `saude_ok`; rota `/switch`
//! usa `trocar_cor`) e o contrato documentado em deploy.rs. `lista_do_projeto`
//! é a delegação da fonte única de listagem (L3.1).

use crate::events;

/// Teto do drain da cor velha (10: HTTP 60 s; worker termina o job atual —
/// a semântica `queue:restart` entra com o deploy de Laravel, F9).
pub const DRAIN_PADRAO_SECS: u64 = 30;

/// Teto do poll de saúde ([health] path, 06): 30 tentativas × 1 s, cada uma
/// com timeout de 2 s — o app tem ~30 s para responder depois de RUNNING.
pub const HEALTH_TENTATIVAS: u32 = 30;
pub const HEALTH_INTERVALO_SECS: u64 = 1;
pub const HEALTH_TIMEOUT_SECS: u64 = 2;

/// Um probe de saúde: `GET http://{target}{path}` no MESMO cliente do proxy
/// (rede docker do host é alcançável). 2xx-3xx = saudável; conexão recusada,
/// 4xx/5xx, timeout ou URI impossível = não saudável — path errado tem de
/// aparecer como falha, nunca virar "RUNNING e pronto".
pub async fn saude_ok(http: &crate::proxy::ProxyClient, target: &str, path: &str) -> bool {
    let caminho = if path.starts_with('/') {
        path.to_owned()
    } else {
        format!("/{path}")
    };
    let Ok(uri) = format!("http://{target}{caminho}").parse::<axum::http::Uri>() else {
        return false;
    };
    match tokio::time::timeout(
        std::time::Duration::from_secs(HEALTH_TIMEOUT_SECS),
        http.get(uri),
    )
    .await
    {
        Ok(Ok(resp)) => resp.status().is_success() || resp.status().is_redirection(),
        _ => false,
    }
}

/// Espera o container `app` RESPONDER no `[health] path` antes de confiar na
/// cor (06 — fecha o F9): RUNNING não garante HTTP. Falha depois do teto é
/// ERRO — o chamador aborta a operação (troca de cor, up).
pub async fn esperar_saude(
    docker: &bollard::Docker,
    http: &crate::proxy::ProxyClient,
    name: &str,
    projeto: &str,
    health_path: &str,
) -> Result<String, String> {
    let target = crate::docker::alvo_app(docker, projeto)
        .await?
        .ok_or_else(|| {
            format!("{projeto}: container app ausente ou sem rede — sem probe de saúde")
        })?;
    for _ in 0..HEALTH_TENTATIVAS {
        if saude_ok(http, &target, health_path).await {
            return Ok(target);
        }
        tokio::time::sleep(std::time::Duration::from_secs(HEALTH_INTERVALO_SECS)).await;
    }
    Err(format!(
        "{name}: app não respondeu GET {health_path} em \
         {}×{HEALTH_TIMEOUT_SECS}s (alvo {target}) — verifique `[health] path` no flux.toml \
         e os logs do container; operação abortada",
        HEALTH_TENTATIVAS
    ))
}

/// Troca de versão invisível (10/R12): aponta o ponteiro para a cor nova
/// (requests novos já nascem nela) e drena a velha com `docker stop -t`.
/// A cor nova PRECISA já estar no ar (o deploy sobe a inativa; aqui é só
/// a troca). Cor velha igual à nova = no-op honesto.
///
/// Retorna (cor_velha, drenada) para o evento/handler.
pub async fn trocar_cor(
    app: std::sync::Arc<crate::AppState>,
    name: &str,
    color: &str,
    drain_secs: u64,
) -> Result<(Option<String>, bool), String> {
    if color != "blue" && color != "green" {
        return Err(format!("cor '{color}' inválida (use blue|green)"));
    }
    let st = &app.state;
    let velha = st.active_color(name).map_err(|e| e.to_string())?;
    if velha.as_deref() == Some(color) {
        return Ok((velha, false));
    }

    // A cor nova tem que estar de pé (o deploy sobe a INATIVA antes da troca).
    let docker = app.docker.clone().ok_or("fluxd sem Docker conectado")?;
    let projeto_novo = format!("flux-{name}-prod-{color}");
    let containers = lista_do_projeto(&docker, &projeto_novo).await?;
    if containers.is_empty() {
        return Err(format!(
            "cor {color} de {name} não está no ar (nenhum container em {projeto_novo}) —              o deploy sobe a cor inativa ANTES da troca (10)"
        ));
    }

    // Health mínimo v1: app RUNNING...
    for c in &containers {
        let svc = c
            .labels
            .as_ref()
            .and_then(|l| l.get("com.docker.compose.service"))
            .cloned()
            .unwrap_or_default();
        let rodando = c
            .state
            .as_ref()
            .is_some_and(|st| matches!(st, bollard::models::ContainerSummaryStateEnum::RUNNING));
        if svc == "app" && !rodando {
            return Err(format!(
                "app da cor {color} não está RUNNING — troca abortada (sem downtime assumido)"
            ));
        }
    }

    // Health REAL ([health] path, 06 — fecha o F9): RUNNING não garante HTTP.
    // A troca só acontece com o path de saúde RESPONDENDO na cor nova; path
    // errado ou app quebrado aborta a troca ANTES do ponteiro se mover.
    let toml_src = st
        .project_toml(name)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("{name}: projeto sem flux.toml gravado no estado"))?;
    let toml = flux_common::config::ProjectToml::parse_str(&toml_src)
        .map_err(|e| format!("flux.toml de {name} inválido: {e}"))?;
    let health_path = toml.health_path().to_owned();
    esperar_saude(&docker, &app.http, name, &projeto_novo, &health_path).await?;
    events::event(
        st,
        "info",
        "deploy",
        &format!("{name}: cor {color} respondeu GET {health_path} — saudável"),
    );

    // TROCA ATÔMICA: ponteiro novo (requests novos nascem na cor nova). O
    // proxy_target do DockerSource resolve SEMPRE pela cor ativa do SQLite
    // (compose_project_cor) — o ponteiro É a troca.
    st.set_active_color(name, color)
        .map_err(|e| e.to_string())?;
    events::event(
        st,
        "info",
        "deploy",
        &format!("{name}: cor ativa → {color} (requests novos nascem nela)"),
    );

    // DRAIN da velha: stop -t (SIGTERM, conexões/job atual terminam no grace).
    let mut drenada = false;
    if let Some(cor_velha) = velha.clone() {
        let projeto_velho = format!("flux-{name}-prod-{cor_velha}");
        let velhos = lista_do_projeto(&docker, &projeto_velho).await?;
        for c in velhos {
            let Some(id) = c.id.clone() else {
                continue;
            };
            let rodando = c.state.as_ref().is_some_and(|st| {
                matches!(st, bollard::models::ContainerSummaryStateEnum::RUNNING)
            });
            if !rodando {
                continue;
            }
            let stop = tokio::process::Command::new("docker")
                .args(["stop", "-t", &drain_secs.to_string(), &id])
                .output()
                .await;
            match stop {
                Ok(s) if s.status.success() => drenada = true,
                Ok(s) => events::event(
                    st,
                    "error",
                    "deploy",
                    &format!(
                        "{name}: drain da cor {cor_velha} falhou em {}: {}",
                        id,
                        String::from_utf8_lossy(&s.stderr).trim()
                    ),
                ),
                Err(e) => events::event(st, "error", "deploy", &format!("{name}: drain {id}: {e}")),
            }
        }
        events::event(
            st,
            "info",
            "deploy",
            &format!(
                "{name}: cor velha {cor_velha} drenada (grace {drain_secs} s, jobs em voo terminaram nela)"
            ),
        );
    }
    Ok((velha, drenada))
}

/// Containers do compose project (resumo cru) — DELEGAÇÃO (L3.1: fonte
/// única de listagem por label vive em docker.rs).
async fn lista_do_projeto(
    docker: &bollard::Docker,
    projeto: &str,
) -> Result<Vec<bollard::models::ContainerSummary>, String> {
    crate::docker::listar_label(docker, Some(projeto)).await
}

#[cfg(test)]
mod troca {
    use super::*;

    #[test]
    fn cor_invalida_recusa_sem_tocar_nada() {
        // trocar_cor valida ANTES de qualquer efeito: cor ruim = Err seco.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let st = crate::state::State::in_memory().unwrap();
            st.register_project("x", "name = \"x\"", "h").unwrap();
            let app = std::sync::Arc::new(crate::AppState::new(
                st,
                &flux_common::config::GlobalConfig::default(),
                None,
            ));
            let r = trocar_cor(app, "x", "roxa", 30).await;
            assert!(r.is_err(), "cor inválida não passa");
        });
    }
}
