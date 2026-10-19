//! live (L7.2): extraído verbatim do main.rs — movimento sem "melhoria".

use std::path::Path;

use crate::contexto::contexto_atual;

/// `flux live [env]` — toggle do live server SEM down (06/live).
///
/// OFF: mata os processos node do container, limpa o `public/hot` (o site
/// volta pro manifest NA HORA — fim do JS "explodido") e guarda o comando
/// em `.flux/live.bak` (gitignored), deixando `live = ""` no flux.toml.
/// ON: restaura o comando do .bak e dispara o dev server no container
/// (docker exec -d com o mesmo wrapper de limpeza do fluxd). O flux.toml da
/// VM é regravado no próximo `flux up` (register). Conteúdo do .env/.bak
/// nunca é exibido.
pub(crate) fn live_toggle(env: &str) {
    let _ = env; // live é dev-only hoje (gatilho env == "dev" no compose.rs)
    let ctx = contexto_atual();
    let toml = flux_common::config::ProjectToml::parse_str(&ctx.toml).unwrap_or_else(|e| {
        eprintln!("erro: flux.toml inválido: {e}");
        std::process::exit(1);
    });
    let Some(dev) = toml.dev else {
        eprintln!("erro: flux.toml sem seção [dev] — nada para alternar");
        std::process::exit(1);
    };
    let ligado = dev
        .live
        .as_deref()
        .map(|l| !l.trim().is_empty())
        .unwrap_or(false);
    let container = container_do_projeto(&ctx.name, "dev");
    println!(
        "live {} — alternando (comando atual: {})",
        if ligado { "ON" } else { "OFF" },
        dev.live.as_deref().unwrap_or("(nenhum)")
    );

    if ligado {
        // ---------- OFF ----------
        let bak = ctx.dir.join(".flux/live.bak");
        let cmd = dev.live.as_deref().unwrap_or_default().to_owned();
        std::fs::create_dir_all(bak.parent().expect(".flux tem pai")).ok();
        std::fs::write(&bak, &cmd).unwrap_or_else(|e| {
            eprintln!("erro: salvando comando em {}: {e}", bak.display());
            std::process::exit(1);
        });
        set_live_no_toml(&ctx.dir, None);
        if let Some(c) = &container {
            let remote = format!(
                "docker exec {c} sh -c 'for d in /proc/[0-9]*; do c=$(tr \"\\0\" \" \" \
                 < $d/cmdline 2>/dev/null); case \"$c\" in *node*) kill ${{d#/proc/}} \
                 2>/dev/null;; esac; done; rm -f public/hot'"
            );
            let _ = crate::ssh_util::ssh_vm(&[&remote]).status();
        }
        println!("live OFF — dev server morto, public/hot limpo; site no manifest (F5)");
        println!("comando guardado em {} — `flux live` religa", bak.display());
    } else {
        // ---------- ON ----------
        let bak = ctx.dir.join(".flux/live.bak");
        let Ok(cmd) = std::fs::read_to_string(&bak) else {
            eprintln!(
                "erro: sem {} para religar — o live nunca foi desligado por aqui",
                bak.display()
            );
            std::process::exit(1);
        };
        let cmd = cmd.trim().to_owned();
        if cmd.is_empty() {
            eprintln!("erro: {} vazio — nada para religar", bak.display());
            std::process::exit(1);
        }
        set_live_no_toml(&ctx.dir, Some(&cmd));
        println!("live ON no flux.toml — `flux up` dispara o dev server");
        println!("comando restaurado: {cmd}");
    }
}

/// Nome do container `app` do projeto (compose), via label — primeiro match.
pub(crate) fn container_do_projeto(name: &str, env: &str) -> Option<String> {
    container_do_servico(name, env, "app")
}

/// Nome do container de UM serviço do projeto (compose), via label — o MESMO
/// mecanismo de resolução do exec no fluxd (labels
/// `com.docker.compose.project`/`...service`, nunca nome de container), aqui
/// por `docker ps` na VM por SSH. Primeiro match. O rótulo segue o contrato
/// `flux-{name}[-{env}|-@{slug}-{env}]` — a MESMA convenção da sessão de sync
/// (sync.rs), da listagem do `ps` (ciclo.rs) e do `watch` (watch.rs); mudar
/// num ponto sem os outros divide o nome em silêncio.
pub(crate) fn container_do_servico(name: &str, env: &str, service: &str) -> Option<String> {
    let projeto = format!("flux-{name}-{env}");
    let saida = crate::ssh_util::ssh_vm(&[&format!(
        "docker ps --filter label=com.docker.compose.project={projeto} \
                 --filter label=com.docker.compose.service={service} --format {{{{.Names}}}}"
    )])
    .output()
    .ok()?;
    String::from_utf8_lossy(&saida.stdout)
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(ToOwned::to_owned)
}

/// Substitui (ou limpa) a linha `live = ...` dentro da seção `[dev]` do
/// flux.toml LOCAL — edição por linhas, sem parser: o arquivo é pequeno e
/// o register valida de verdade no próximo up.
pub(crate) fn set_live_no_toml(dir: &Path, cmd: Option<&str>) {
    let caminho = dir.join("flux.toml");
    let conteudo = std::fs::read_to_string(&caminho).unwrap_or_else(|e| {
        eprintln!("erro: lendo {}: {e}", caminho.display());
        std::process::exit(1);
    });
    let linha_nova = match cmd {
        Some(c) => format!("live = {:?}", c),
        None => "live = \"\"".to_owned(),
    };
    let mut dentro_dev = false;
    let mut dev_idx: Option<usize> = None;
    let mut achou: Option<usize> = None;
    let mut linhas: Vec<String> = conteudo
        .lines()
        .enumerate()
        .map(|(i, l)| {
            let t = l.trim();
            if t.starts_with('[') {
                dentro_dev = t == "[dev]";
                if dentro_dev && dev_idx.is_none() {
                    dev_idx = Some(i);
                }
                return l.to_owned();
            }
            if dentro_dev && (t.starts_with("live =") || t.starts_with("live=")) {
                achou = Some(i);
                return linha_nova.clone();
            }
            l.to_owned()
        })
        .collect();
    match achou {
        Some(_) => {}
        None => match dev_idx {
            Some(i) => linhas.insert(i + 1, linha_nova),
            None => {
                eprintln!("erro: flux.toml sem seção [dev] — live não alternado");
                std::process::exit(1);
            }
        },
    }
    std::fs::write(&caminho, linhas.join("\n") + "\n").unwrap_or_else(|e| {
        eprintln!("erro: escrevendo {}: {e}", caminho.display());
        std::process::exit(1);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggle_de_live_edita_linha_na_secao_dev_sem_duplicar() {
        let base = std::env::temp_dir().join(format!("flux-live-test-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(
            base.join("flux.toml"),
            "name = \"x\"\n\n[dev]\nlive = \"npm run dev\"\n",
        )
        .unwrap();

        set_live_no_toml(&base, None);
        let s = std::fs::read_to_string(base.join("flux.toml")).unwrap();
        assert!(s.contains("live = \"\""), "OFF limpa o live");

        set_live_no_toml(&base, Some("npm run build -- --watch"));
        let s = std::fs::read_to_string(base.join("flux.toml")).unwrap();
        assert!(s.contains("live = \"npm run build -- --watch\""));
        assert_eq!(s.matches("[dev]").count(), 1, "toggle não duplica a seção");

        std::fs::remove_dir_all(&base).ok();
    }
}
