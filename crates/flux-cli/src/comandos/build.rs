//! build (L7.2): extraído verbatim do main.rs — movimento sem "melhoria".

use crate::contexto::{LEASE_UP_SECS, contexto_atual, falhar};

use crate::api::ApiClient;
use crate::comandos::live::container_do_servico;

/// Teto próprio do `flux build` (02): build de assets legítimo roda minutos
/// — o cap de 120 s do `flux exec` NÃO vale aqui.
pub(crate) const BUILD_TIMEOUT_SECS: u64 = 1800;

/// Default de [build].command por framework (02): só `laravel` tem default;
/// as demais stacks precisam declarar o comando (nada de chute silencioso).
pub(crate) fn default_build_command(f: flux_common::framework::Framework) -> Option<String> {
    match f {
        flux_common::framework::Framework::Laravel => Some("npm run build".to_owned()),
        _ => None,
    }
}

/// Aspas simples POSIX: fecha a string, escapa a aspa (`'` → `'\''`) e
/// reabre. O comando vem do flux.toml do próprio dev, mas entra num shell
/// remoto — aspas corretas são higiene, não confiança.
pub(crate) fn shell_aspas_simples(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// `flux build` (02): sync garantida → janela de silêncio no Windows →
/// comando no container de dev sob o lock global `/srv/flux/build.lock` →
/// exit code real do comando. Cada passo anunciado; erro em qualquer um
/// sai com exit ≠ 0 e a causa.
pub(crate) async fn build_assets() {
    let ctx = contexto_atual();
    let cfg = flux_common::config::ProjectToml::parse_str(&ctx.toml).unwrap_or_else(|e| {
        eprintln!("erro: flux.toml inválido: {e}");
        std::process::exit(1);
    });
    let build = cfg.build.clone().unwrap_or_default();
    let servico = build.service().to_owned();
    let janela = cfg.build_silence_window_secs();
    let comando = match build
        .command
        .clone()
        .or_else(|| default_build_command(cfg.framework()))
    {
        Some(c) if !c.trim().is_empty() => c,
        Some(_) => {
            eprintln!(
                "erro: [build] command vazio no flux.toml — declare o comando ou remova a chave"
            );
            std::process::exit(2);
        }
        None => {
            eprintln!(
                "erro: framework `{}` não tem default de [build] command — declare no flux.toml (ex.: [build] command = \"npm run build\")",
                cfg.framework()
            );
            std::process::exit(2);
        }
    };

    // [1/4] Contexto + sessão de sync garantida (create é idempotente por
    // nome — sessão viva é reuso, nunca erro).
    println!(
        "[1/4] projeto {} ({}) — garantindo sessão de sync…",
        ctx.name,
        cfg.framework()
    );
    let spec =
        crate::sync::SyncSpec::for_project(&ctx.dir, crate::sync::vm_beta(&ctx.name, None), None)
            .unwrap_or_else(|e| {
                eprintln!("erro: montando sync: {e}");
                std::process::exit(2);
            });
    if let Err(e) = crate::sync::create(&spec) {
        eprintln!("erro: sync: {e}");
        std::process::exit(1);
    }
    println!(
        "[1/4] sync `{}` ativa — a VM recebe os fontes antes do build",
        spec.session()
    );

    // [2/4] Janela de silêncio no lado Windows ([build].silence_window_secs,
    // default 180 s): build sobre árvore em mudança replica pela metade.
    println!(
        "[2/4] janela de silêncio da árvore no Windows ({janela} s; teto {} rodadas)…",
        crate::sync::TETO_RODADAS_ESTABILIDADE
    );
    let extra: Vec<String> = crate::sync::ignores_do_projeto(&ctx.dir, cfg.sync.as_ref())
        .unwrap_or_else(|e| {
            eprintln!("erro: {e}");
            std::process::exit(1);
        });
    let dir = ctx.dir.clone();
    if let Err(e) = crate::sync::esperar_estabilidade(
        &dir,
        &extra,
        None,
        std::time::Duration::from_secs(janela),
        |msg| println!("      {msg}"),
    ) {
        eprintln!("erro: {e}");
        std::process::exit(1);
    }

    // [3/4] Build na VM: container resolvido por LABEL (mesmo mecanismo de
    // resolução do exec no fluxd), comando sob o lock global via flock — um
    // build por vez no fluxd inteiro. Lease antes (04): o container não dorme
    // no meio do build.
    println!(
        "[3/4] build na VM: lock /srv/flux/build.lock + docker exec no serviço `{servico}` (teto {BUILD_TIMEOUT_SECS} s)…"
    );
    let cliente = ApiClient::from_env();
    match cliente.lease(&ctx.name, "dev", LEASE_UP_SECS).await {
        Ok(_) => {}
        Err(e) => falhar(e),
    }
    let _guarda = crate::api::manter_lease(
        cliente.clone(),
        ctx.name.clone(),
        "dev".to_owned(),
        LEASE_UP_SECS,
    );
    let Some(container) = container_do_servico(&ctx.name, "dev", &servico) else {
        eprintln!(
            "erro: container do serviço `{servico}` não encontrado (compose flux-{}-dev) — o projeto subiu? rode `flux up`",
            ctx.name
        );
        std::process::exit(1);
    };
    let code = rodar_build_na_vm(&container, &comando);

    // [4/4] Resultado com exit code real (o CLI repassa o do comando).
    if code == 0 {
        println!("[4/4] build concluído (exit 0)");
    } else {
        eprintln!("[4/4] build falhou (exit {code}) — saída acima tem a causa");
    }
    std::process::exit(code);
}

/// Roda o comando sob o flock na VM, dentro do container, com stdout/stderr
/// HERDADOS (progresso ao vivo do build) e teto próprio de
/// [`BUILD_TIMEOUT_SECS`] — estourou, o ssh é morto e o exit 124 (convenção
/// do GNU timeout) sinaliza o teto; o flock libera o lock na morte do dono.
pub(crate) fn rodar_build_na_vm(container: &str, comando: &str) -> i32 {
    // `cd /var/www` primeiro: é o cwd padrão da plataforma dentro do
    // container `app` (mesmo mecanismo do live server, 06).
    let remoto = format!(
        "flock /srv/flux/build.lock docker exec {container} sh -lc {}",
        shell_aspas_simples(&format!("cd /var/www && {comando}"))
    );
    let mut filho = match crate::ssh_util::ssh_vm(&[&remoto])
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .spawn()
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!("erro: não consegui lançar o ssh para o build: {e}");
            return 127;
        }
    };
    let inicio = std::time::Instant::now();
    loop {
        match filho.try_wait() {
            Ok(Some(st)) => return st.code().unwrap_or(1),
            Ok(None) => {}
            Err(e) => {
                eprintln!("erro: esperando o build (ssh): {e}");
                let _ = filho.kill();
                let _ = filho.wait();
                return 1;
            }
        }
        if inicio.elapsed() >= std::time::Duration::from_secs(BUILD_TIMEOUT_SECS) {
            let _ = filho.kill();
            let _ = filho.wait();
            eprintln!(
                "erro: build estourou o teto de {BUILD_TIMEOUT_SECS} s — processo morto (lock /srv/flux/build.lock liberado pelo flock)"
            );
            return 124;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Default de [build].command (02): só laravel tem — as outras stacks
    /// exigem declaração explícita (recusa com exit 2 no comando).
    #[test]
    fn default_de_build_so_laravel_tem() {
        use flux_common::framework::Framework;
        assert_eq!(
            default_build_command(Framework::Laravel).as_deref(),
            Some("npm run build"),
            "default da 02 para laravel"
        );
        for f in [
            Framework::Node,
            Framework::Rust,
            Framework::Static,
            Framework::Custom,
        ] {
            assert!(
                default_build_command(f).is_none(),
                "{f} não tem default — exige [build] command"
            );
        }
    }

    /// Aspas simples POSIX: aspa interna vira a tríade `'\''` — o comando do
    /// flux.toml entra no shell da VM sem quebrar nem injetar.
    #[test]
    fn shell_aspas_simples_escapa_a_aspa_interna() {
        assert_eq!(shell_aspas_simples("npm run build"), "'npm run build'");
        assert_eq!(
            shell_aspas_simples("npm run build -- --mode='prod'"),
            "'npm run build -- --mode='\\''prod'\\'''"
        );
        assert_eq!(shell_aspas_simples(""), "''");
    }
}
