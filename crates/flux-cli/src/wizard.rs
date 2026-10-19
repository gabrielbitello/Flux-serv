//! Wizard `flux up -m` (06): série de perguntas no terminal, respostas viram
//! um `flux.toml` versionável na raiz do repo. Existe porque o manifesto
//! explícito é a regra (deny_unknown_fields) e digitar TOML na mão é atrito.
//!
//! Regras: `flux.toml` existente = RECUSA (nunca sobrescreve silencioso);
//! Ctrl+C/EOF no meio = nada é escrito; tudo tem default no Enter. Zero
//! dependência nova — stdin com `read_line`, igual ao resto do CLI.

use std::io::{self, BufRead, Write};
use std::path::Path;

/// Resultado do wizard — o TOML já foi gravado na raiz; o resumo existe para
/// o chamador exibir/logar.
pub struct ManifestoGerado {
    #[allow(dead_code)] // consumidor futuro: `flux up` imprime o resumo no log
    pub toml: String,
}

/// Roda o wizard. `pasta` = nome do diretório atual (default do nome).
/// `raiz` = onde o `flux.toml` seria escrito (recusa se já existe).
/// Retorna `None` quando o usuário aborta.
pub fn gerar(raiz: &Path, pasta: &str) -> io::Result<Option<ManifestoGerado>> {
    let caminho = raiz.join("flux.toml");
    if caminho.exists() {
        eprintln!(
            "erro: {} já existe — o wizard NUNCA sobrescreve; edite à mão ou apague antes",
            caminho.display()
        );
        return Ok(None);
    }

    println!("flux up -m — gerando o flux.toml (Enter aceita o default; Ctrl+C aborta)");

    // 1. Nome: default = pasta em kebab-case.
    let nome_default = pasta.to_lowercase();
    let nome = pergunta("nome do projeto (domínios derivam dele)", &nome_default)?;
    let nome = if nome.is_empty() { nome_default } else { nome };
    if !nome
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        || nome.is_empty()
    {
        eprintln!(
            "erro: nome '{nome}' inválido — só minúscula, dígito e traço \
             (ele vira domínio e sufixo de container)"
        );
        return Ok(None);
    }

    // 2. Framework: auto-detecta e confirma.
    let sugerido = detectar_framework(raiz);
    let fw = pergunta("framework (laravel|node|rust|static|custom)", sugerido)?;
    let fw = if fw.is_empty() {
        sugerido.to_owned()
    } else {
        fw
    };
    // L2.4: UMA lista executável — o FromStr do flux-common (framework.rs).
    // A cópia aqui driftava da lista ali se uma variante nova nascesse.
    if let Err(e) = fw.parse::<flux_common::framework::Framework>() {
        eprintln!("erro: framework '{fw}' — {e}");
        return Ok(None);
    }

    // 3. Health path (06: node/rust DEVEM declarar).
    let health_default = if fw == "laravel" { "/up" } else { "" };
    let health = pergunta(
        "health path (caminho que responde OK no container)",
        health_default,
    )?;
    if health.is_empty() && fw != "laravel" && fw != "static" {
        eprintln!(
            "erro: node/rust exigem health path explícito (a plataforma confirma \
             'acordado e saudável' por ele)"
        );
        return Ok(None);
    }

    // 4. Temporário? (14)
    let temporario = pergunta(
        "dev TEMPORÁRIO? apaga da VM após TTL sem interação (s/N)",
        "n",
    )?;
    let temporario = matches!(
        temporario.trim().to_lowercase().as_str(),
        "s" | "sim" | "y" | "yes"
    );
    let mut toml = format!("name = \"{nome}\"\nframework = \"{fw}\"\n");
    if !health.is_empty() {
        toml.push_str(&format!("\n[health]\npath = \"{health}\"\n"));
    }
    if temporario {
        let ttl = pergunta("TTL em horas sem interação nenhuma", "6")?;
        let Ok(horas) = ttl.parse::<u64>() else {
            eprintln!("erro: TTL '{ttl}' não é um número de horas");
            return Ok(None);
        };
        if horas == 0 {
            eprintln!("erro: TTL 0 h apagaria na primeira varredura — mínimo sensível é 1");
            return Ok(None);
        }
        toml.push_str(&format!(
            "\n[temporary]\nenabled = true\nttl_hours = {horas}\n"
        ));
    }

    println!();
    println!("--- flux.toml gerado ---");
    print!("{toml}");
    println!("------------------------");
    let confirma = pergunta("escrever na raiz do repo e seguir o up? (S/n)", "s")?;
    if matches!(
        confirma.trim().to_lowercase().as_str(),
        "n" | "não" | "nao" | "no"
    ) {
        println!("abortado — nada foi escrito");
        return Ok(None);
    }
    std::fs::write(&caminho, &toml)?;
    println!("escrito em {}", caminho.display());
    Ok(Some(ManifestoGerado { toml }))
}

fn pergunta(rotulo: &str, default: &str) -> io::Result<String> {
    let mostra_default = if default.is_empty() {
        String::new()
    } else {
        format!(" [{default}]")
    };
    print!("{rotulo}{mostra_default}: ");
    io::stdout().flush()?;
    let mut linha = String::new();
    io::stdin().lock().read_line(&mut linha)?;
    Ok(linha.trim().to_owned())
}

/// Auto-detecção pela raiz do repo (mesmos sinais da 06: artisan → laravel,
/// package.json → node, Cargo.toml → rust; nada bate = laravel é o default
/// da plataforma, o Enter confirma).
fn detectar_framework(raiz: &Path) -> &'static str {
    if raiz.join("artisan").is_file() {
        "laravel"
    } else if raiz.join("Cargo.toml").is_file() {
        "rust"
    } else if raiz.join("package.json").is_file() {
        "node"
    } else {
        "laravel"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detecta_pela_raiz() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(
            detectar_framework(tmp.path()),
            "laravel",
            "default da plataforma"
        );
        std::fs::write(tmp.path().join("artisan"), "").unwrap();
        assert_eq!(detectar_framework(tmp.path()), "laravel");
        std::fs::remove_file(tmp.path().join("artisan")).unwrap();
        std::fs::write(tmp.path().join("Cargo.toml"), "").unwrap();
        assert_eq!(detectar_framework(tmp.path()), "rust");
        std::fs::remove_file(tmp.path().join("Cargo.toml")).unwrap();
        std::fs::write(tmp.path().join("package.json"), "{}").unwrap();
        assert_eq!(detectar_framework(tmp.path()), "node");
    }

    #[test]
    fn toml_existente_recusa_sem_escrever() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("flux.toml"), "name = \"x\"\n").unwrap();
        let antes = std::fs::read_to_string(tmp.path().join("flux.toml")).unwrap();
        // gerar() sem stdin interativo: a recusa acontece ANTES de ler stdin.
        let r = gerar(tmp.path(), "qualquer").unwrap();
        assert!(r.is_none(), "flux.toml existente = aborta");
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("flux.toml")).unwrap(),
            antes,
            "arquivo intocado"
        );
    }
}
