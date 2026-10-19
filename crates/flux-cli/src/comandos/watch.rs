//! watch (L7.2): extraído verbatim do main.rs — movimento sem "melhoria".

use crate::api::ApiClient;

/// `flux watch` (02): serviço de longa vida que fecha o circuito CLI ↔ painel
/// — reporta as sessões `flux-*` do mutagen ao fluxd a cada 5 s
/// (POST /sync/report), aplica o rescan pedido pelo painel (GET /sync/status
/// → `sync::reset` → report com rescan_applied=true) e marca "absent" os
/// projetos do fluxd sem sessão local viva. Erro NUNCA derruba o serviço nem
/// passa em silêncio: loga com a origem e o próximo tick tenta de novo.
/// Ctrl-C encerra limpo (nenhuma sessão mutagen é tocada).
pub(crate) async fn watch() {
    let cliente = ApiClient::from_env();
    println!(
        "watch: reportando sessões flux-* ao fluxd ({}) a cada 5 s — Ctrl-C encerra",
        cliente.base_url()
    );
    loop {
        tick_watch(&cliente).await;
        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => {}
            _ = tokio::signal::ctrl_c() => {
                println!("watch: encerrado (Ctrl-C) — sessões mutagen intocadas");
                return;
            }
        }
    }
}

/// Uma rodada do watch (ver [`watch`]). Retorna sempre — erros viram log com
/// origem, o loop é do chamador.
pub(crate) async fn tick_watch(cliente: &ApiClient) {
    // 1) Sessões locais → report individual ("watching").
    let sessoes = match crate::sync::status_all() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("watch: status_all do mutagen falhou: {e}");
            return;
        }
    };
    let vivas: Vec<&crate::sync::SessionStatus> = sessoes
        .iter()
        .filter(|s| s.name.starts_with("flux-"))
        .collect();
    for s in &vivas {
        let report = crate::api::SyncReportIn {
            // Projeto derivado da sessão (`flux-{name}` → {name}; a sessão de
            // worktree `flux-{name}-{slug}` deriva `{name}-{slug}`, que NÃO é
            // projeto registrado — o fluxd RECUSA com 404 e loga a
            // divergência: linha fantasma nunca nasce (o painel casa pelo
            // projeto registrado e o purge apaga pelo nome exato). O erro
            // sai no log do watch a cada tick — nada silencioso. O rescan
            // pedido para o projeto principal ALCANÇA a sessão de worktree:
            // o tick 2 casa sessão por prefixo `flux-{project}-`.
            project: s.name.strip_prefix("flux-").unwrap_or(&s.name).to_owned(),
            session: s.name.clone(),
            status: "watching".to_owned(),
            paused: s.paused,
            files: s.beta.files.or(s.alpha.files),
            problems: s.alpha.scan_problems
                + s.alpha.transition_problems
                + s.beta.scan_problems
                + s.beta.transition_problems
                + s.conflicts,
            last_error: s.last_error.clone(),
            rescan_applied: false,
        };
        if let Err(e) = cliente.sync_report(&report).await {
            eprintln!("watch: report de `{}`: {e}", s.name);
        }
    }
    // 2) Estado do fluxd: rescan pedido → reset + re-report com
    // rescan_applied=true; projeto sem sessão local → "absent".
    let st = match cliente.sync_status().await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("watch: GET /sync/status: {e}");
            return;
        }
    };
    for p in st.projects {
        let principal = format!("flux-{}", p.project);
        let prefixo = format!("flux-{}-", p.project);
        let locais: Vec<&&crate::sync::SessionStatus> = vivas
            .iter()
            .filter(|s| s.name == principal || s.name.starts_with(&prefixo))
            .collect();
        if locais.is_empty() {
            let report = crate::api::SyncReportIn {
                project: p.project.clone(),
                session: principal,
                status: "absent".to_owned(),
                paused: false,
                files: None,
                problems: 0,
                last_error: None,
                // NÃO limpa a flag: absent não aplicou rescan nenhum.
                rescan_applied: false,
            };
            if let Err(e) = cliente.sync_report(&report).await {
                eprintln!("watch: report absent de `{}`: {e}", p.project);
            }
            if p.rescan_requested {
                eprintln!(
                    "watch: rescan pedido para `{}` sem sessão local viva — a flag segue no fluxd até a sessão voltar",
                    p.project
                );
            }
            continue;
        }
        if p.rescan_requested
            && let Err(e) = rescan_sessao(cliente, locais[0]).await
        {
            eprintln!("watch: rescan de `{}`: {e}", p.project);
        }
    }
}

/// Rescan pedido pelo painel (02): reconstrói a spec a partir da sessão viva
/// (o mutagen grava alpha e beta na sessão), faz terminate + create (full
/// rescan) e re-reporta com `rescan_applied=true` — é o que limpa a flag no
/// fluxd. Erro em qualquer ponto sobe com a causa (o tick loga).
pub(crate) async fn rescan_sessao(
    cliente: &ApiClient,
    s: &crate::sync::SessionStatus,
) -> Result<(), String> {
    let spec = spec_da_sessao(s)?;
    crate::sync::reset(&spec).map_err(|e| e.to_string())?;
    println!(
        "watch: rescan aplicado em `{}` (terminate + create)",
        spec.session()
    );
    // Estado fresco pós-recriação — vai estar trabalhando; é o que o painel
    // vê até o rescan terminar sozinho.
    let fresco = crate::sync::status_of(&spec.session())
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("sessão `{}` sumiu logo após o reset", spec.session()))?;
    let report = crate::api::SyncReportIn {
        project: spec.project.clone(),
        session: fresco.name.clone(),
        status: "watching".to_owned(),
        paused: fresco.paused,
        files: fresco.beta.files.or(fresco.alpha.files),
        problems: fresco.alpha.scan_problems
            + fresco.alpha.transition_problems
            + fresco.beta.scan_problems
            + fresco.beta.transition_problems
            + fresco.conflicts,
        last_error: fresco.last_error.clone(),
        rescan_applied: true,
    };
    cliente
        .sync_report(&report)
        .await
        .map_err(|e| e.to_string())
}

/// Reconstrói a [`crate::sync::SyncSpec`] de uma sessão viva a partir do que
/// o mutagen gravou: alpha = path local do endpoint; beta = `user@host:path`
/// do endpoint remoto; projeto lido do flux.toml da pasta alpha (senão nome
/// da pasta). O nome da sessão recriada TEM de bater com o original — nome
/// divergente viraria "outra" sessão para o painel, então reconstrução que
/// não fecha é ERRO, nunca chute.
pub(crate) fn spec_da_sessao(
    s: &crate::sync::SessionStatus,
) -> Result<crate::sync::SyncSpec, String> {
    if s.alpha.path.trim().is_empty() {
        return Err(format!(
            "sessão `{}` sem path de alpha — impossível reconstruir a spec",
            s.name
        ));
    }
    let alpha = std::path::PathBuf::from(s.alpha.path.trim());
    let beta = if s.beta.remote {
        let (user, host) = s.beta.host.split_once('@').ok_or_else(|| {
            format!(
                "beta remoto da sessão `{}` sem `user@host`: `{}`",
                s.name, s.beta.host
            )
        })?;
        if s.beta.path.trim().is_empty() {
            return Err(format!(
                "beta remoto da sessão `{}` sem path — impossível reconstruir",
                s.name
            ));
        }
        crate::sync::Beta::Ssh {
            user: user.to_owned(),
            host: host.to_owned(),
            path: s.beta.path.trim().to_owned(),
        }
    } else {
        crate::sync::Beta::Local(std::path::PathBuf::from(s.beta.path.trim()))
    };
    let base = crate::sync::SyncSpec::for_project(&alpha, beta, None).map_err(|e| e.to_string())?;
    if s.name == base.session() {
        return Ok(base);
    }
    // Worktree: o slug vive no sufixo do nome da sessão (spec 02) — a pasta
    // alpha da worktree tem o MESMO project no flux.toml, então o sufixo é
    // recuperável com determinismo.
    let prefixo = format!("flux-{}-", base.project);
    if let Some(slug) = s.name.strip_prefix(&prefixo) {
        let mut spec = base.clone();
        spec.branch_slug = Some(slug.to_owned());
        if spec.session() == s.name {
            return Ok(spec);
        }
    }
    Err(format!(
        "sessão `{}` não reconstrói a partir de {} (o alpha da sessão aponta outra pasta?)",
        s.name,
        alpha.display()
    ))
}
