//! sync_cmd (L7.2): extraído verbatim do main.rs — movimento sem "melhoria".

use crate::contexto::contexto_atual;

/// Teto de espera do full rescan do `flux sync reset` (02: "termina sozinho,
/// mostra progresso"; 10 min acima disso é anomalia visível).
pub(crate) const TETO_RESET_SECS: u64 = 600;

/// Arquivos no status de sessão em formato de linha (`None` = ainda sem
/// scan — nunca vira "0", que mentiria).
pub(crate) fn fmt_files(f: Option<u64>) -> String {
    f.map(|n| n.to_string()).unwrap_or_else(|| "—".into())
}

/// Sessão de sync do projeto no diretório atual — base de todo `flux sync *`
/// (02: uma sessão por projeto, `flux-{name}`, ou `flux-{name}-{slug}` na
/// worktree). Erro de config sai com exit 2 (recusa, não falha de rede).
pub(crate) fn sync_spec_do_contexto() -> crate::sync::SyncSpec {
    let ctx = contexto_atual();
    crate::sync::SyncSpec::for_project(&ctx.dir, crate::sync::vm_beta(&ctx.name, None), None)
        .unwrap_or_else(|e| {
            eprintln!("erro: montando sync do projeto: {e}");
            std::process::exit(2);
        })
}

/// `flux sync status` (02): estado da sessão do projeto. Saudável imprime o
/// marcador do aceite 5 ("ok · sem pendências"); sessão inexistente é erro
/// com instrução (o caminho de conserto é `flux up`), nunca silêncio.
pub(crate) fn sync_status_cmd() {
    let nome = sync_spec_do_contexto().session();
    let st = match crate::sync::status_of(&nome) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("erro: apurando status da sync: {e}");
            std::process::exit(1);
        }
    };
    let Some(s) = st else {
        eprintln!("erro: sessão `{nome}` não existe — `flux up` cria a sync do projeto (02)");
        std::process::exit(1);
    };
    if s.saudavel() {
        println!("{nome}: ok · sem pendências");
        return;
    }
    println!(
        "{nome}: {} · pausada: {}",
        s.status.rotulo(),
        if s.paused { "sim" } else { "não" }
    );
    println!(
        "  arquivos: alpha {} · beta {}",
        fmt_files(s.alpha.files),
        fmt_files(s.beta.files)
    );
    println!(
        "  pendências: alpha scan {} · alpha transição {} · beta scan {} · beta transição {} · conflitos {}",
        s.alpha.scan_problems,
        s.alpha.transition_problems,
        s.beta.scan_problems,
        s.beta.transition_problems,
        s.conflicts
    );
    match &s.last_error {
        Some(e) => println!("  last_error: {e}"),
        None => println!("  last_error: —"),
    }
}

pub(crate) fn sync_pause_cmd() {
    let nome = sync_spec_do_contexto().session();
    if let Err(e) = crate::sync::pause(&nome) {
        eprintln!("erro: pausando a sync: {e}");
        std::process::exit(1);
    }
    println!("sync `{nome}` pausada (operador pode trabalhar na VM sem réplica chegando)");
}

pub(crate) fn sync_resume_cmd() {
    let nome = sync_spec_do_contexto().session();
    if let Err(e) = crate::sync::resume(&nome) {
        eprintln!("erro: retomando a sync: {e}");
        std::process::exit(1);
    }
    println!("sync `{nome}` retomada — o que acumulou converge sozinho");
}

/// `flux sync reset` (02: "mata sessão e recria com full rescan; termina
/// sozinho, mostra progresso") — este é o comando de "sync instantânea em
/// todos os arquivos respeitando as regras": terminate + create força o scan
/// completo dos dois lados, e o progresso é apurado com `status_of` a cada
/// 2 s até a sessão voltar a Ociosa. Timeout = erro GRITADO com exit ≠ 0.
pub(crate) fn sync_reset_cmd() {
    let spec = sync_spec_do_contexto();
    let nome = spec.session();
    println!("reset: terminate + create de `{nome}` (full rescan nos dois lados)…");
    if let Err(e) = crate::sync::reset(&spec) {
        eprintln!("erro: {e}");
        std::process::exit(1);
    }
    println!("sessão recriada — esperando o rescan convergir (teto {TETO_RESET_SECS} s)…");
    let mut esperado = 0u64;
    let mut ultimo: Option<String> = None;
    loop {
        match crate::sync::status_of(&nome) {
            Ok(Some(s)) => {
                if s.status == crate::sync::StatusState::Ociosa {
                    if s.saudavel() {
                        println!("ok · sem pendências");
                    } else {
                        println!(
                            "ociosa com pendências (pausada: {}) — `flux sync status` detalha",
                            if s.paused { "sim" } else { "não" }
                        );
                    }
                    return;
                }
                let rot = s.status.rotulo();
                if ultimo.as_deref() != Some(rot.as_str()) {
                    println!("  … {rot}");
                    ultimo = Some(rot);
                }
            }
            Ok(None) => {
                if ultimo.as_deref() != Some("ausente") {
                    println!("  … sessão ainda não aparece na listagem do mutagen");
                    ultimo = Some("ausente".to_owned());
                }
            }
            Err(e) => {
                eprintln!("erro: apurando progresso do reset: {e}");
                std::process::exit(1);
            }
        }
        if esperado >= TETO_RESET_SECS {
            eprintln!(
                "erro: full rescan de `{nome}` não convergiu em {TETO_RESET_SECS} s — `flux sync status` mostra onde travou (erro NUNCA fica silencioso, 02)"
            );
            std::process::exit(1);
        }
        std::thread::sleep(std::time::Duration::from_secs(2));
        esperado += 2;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_da_sessao_nunca_vira_zero_sem_scan() {
        assert_eq!(fmt_files(Some(123)), "123");
        assert_eq!(fmt_files(None), "—", "None = sem scan; 0 mentiria");
    }
}
