//! Wizard `flux up -m` (06): série de perguntas no terminal, respostas viram
//! um `flux.toml` versionável na raiz do repo. Existe porque o manifesto
//! explícito é a regra (deny_unknown_fields) e digitar TOML na mão é atrito.
//!
//! Regras: `flux.toml` existente = RECUSA (nunca sobrescreve silencioso);
//! Ctrl+C/EOF no meio = nada é escrito; tudo tem default no Enter. Zero
//! dependência nova — stdin com `read_line`, igual ao resto do CLI.
//! L2.6: a MONTAGEM é função pura (`montar_manifesto`) — valida entrada
//! hostil (aspa/barra invertida em `health` gerava TOML inválido que
//! explodia longe da causa, no parse do próximo `up`) e faz ROUND-TRIP
//! (`ProjectToml::parse_str` tem que aceitar o que o wizard escreve).

use std::io::{self, BufRead, Write};
use std::path::Path;

use flux_common::config::ProjectToml;
use flux_common::nomes::projeto_valido;

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

    // 1. Nome: default = pasta em kebab-case (contrato único: nomes.rs, L2.1).
    let nome_default = pasta.to_lowercase();
    let nome = pergunta("nome do projeto (domínios derivam dele)", &nome_default)?;
    let nome = if nome.is_empty() { nome_default } else { nome };
    if !projeto_valido(&nome) {
        eprintln!(
            "erro: nome '{nome}' inválido — só minúscula, dígito e traço \
             (ele vira domínio e sufixo de container)"
        );
        return Ok(None);
    }

    // 2. Framework: auto-detecta e confirma (L2.4: lista executável única =
    //    o FromStr do flux-common).
    let sugerido = detectar_framework(raiz);
    let fw = pergunta("framework (laravel|node|rust|static|custom)", sugerido)?;
    let fw = if fw.is_empty() {
        sugerido.to_owned()
    } else {
        fw
    };

    // 3. Health path (06: node/rust DEVEM declarar).
    let health_default = if fw == "laravel" { "/up" } else { "" };
    let health = pergunta(
        "health path (caminho que responde OK no container)",
        health_default,
    )?;

    // 4. Temporário? (14) — o TTL é parseado na borda de I/O; a REGRA
    //    (0 h = recusa) vive na montagem pura.
    let temporario = pergunta(
        "dev TEMPORÁRIO? apaga da VM após TTL sem interação (s/N)",
        "n",
    )?;
    let temporario = matches!(
        temporario.trim().to_lowercase().as_str(),
        "s" | "sim" | "y" | "yes"
    );
    let mut ttl: Option<u64> = None;
    if temporario {
        let resposta = pergunta("TTL em horas sem interação nenhuma", "6")?;
        let Ok(horas) = resposta.parse::<u64>() else {
            eprintln!("erro: TTL '{resposta}' não é um número de horas");
            return Ok(None);
        };
        ttl = Some(horas);
    }

    let toml = match montar_manifesto(&nome, &fw, &health, temporario, ttl) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("erro: {e}");
            return Ok(None);
        }
    };
    // Round-trip (L2.6): o wizard NUNCA escreve um TOML que o parse da
    // plataforma recuse — a falha antiga aparecia longe da causa.
    if let Err(e) = ProjectToml::parse_str(&toml) {
        eprintln!("erro: o TOML gerado não parseia ({e}) — nada foi escrito");
        return Ok(None);
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

/// Monta o `flux.toml` — função PURA, sem I/O: toda validação acontece aqui
/// e nada é escrito pelo chamador enquanto ela devolver `Err` (L2.6).
fn montar_manifesto(
    nome: &str,
    fw: &str,
    health: &str,
    temporario: bool,
    ttl: Option<u64>,
) -> Result<String, String> {
    if !projeto_valido(nome) {
        return Err(format!(
            "nome '{nome}' inválido — só minúscula, dígito e traço \
             (ele vira domínio e sufixo de container)"
        ));
    }
    fw.parse::<flux_common::framework::Framework>()
        .map_err(|e| format!("framework '{fw}' — {e}"))?;
    if health.is_empty() && fw != "laravel" && fw != "static" {
        return Err(
            "node/rust exigem health path explícito (a plataforma confirma \
             'acordado e saudável' por ele)"
                .to_owned(),
        );
    }
    // Aspa e barra invertida no `health` interpolam mal no TOML
    // (`path = "{health}"`) — antes gravava manifesto INVÁLIDO.
    if health.contains('"') || health.contains('\\') {
        return Err(format!(
            "health path '{health}' tem aspa ou barra invertida — \
             não é um caminho HTTP válido e quebraria o TOML"
        ));
    }
    if temporario {
        let horas = ttl.ok_or("TTL ausente para projeto temporário")?;
        if horas == 0 {
            return Err("TTL 0 h apagaria na primeira varredura — mínimo sensível é 1".to_owned());
        }
    }

    let mut toml = format!("name = \"{nome}\"\nframework = \"{fw}\"\n");
    if !health.is_empty() {
        toml.push_str(&format!("\n[health]\npath = \"{health}\"\n"));
    }
    if temporario {
        toml.push_str(&format!(
            "\n[temporary]\nenabled = true\nttl_hours = {}\n",
            ttl.unwrap_or_default()
        ));
    }
    Ok(toml)
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

    /// L2.6: caracterização da montagem pura — caso de ouro do TOML.
    #[test]
    fn montar_manifesto_caso_de_ouro() {
        let toml = montar_manifesto("pedi", "laravel", "/up", true, Some(6)).unwrap();
        assert_eq!(
            toml,
            "name = \"pedi\"\nframework = \"laravel\"\n\n[health]\npath = \"/up\"\n\n[temporary]\nenabled = true\nttl_hours = 6\n"
        );
    }

    /// L2.6: aspa e barra invertida no health são recusadas ANTES de gerar.
    #[test]
    fn montar_manifesto_recusa_health_hostil() {
        for health in ["/up?x=\"", "/up\\\\ok", "\""] {
            let e = montar_manifesto("pedi", "node", health, false, None)
                .expect_err(&format!("health {health:?} deveria ser recusado"));
            assert!(
                e.contains("aspa") || e.contains("barra"),
                "erro fala do problema real: {e}"
            );
        }
    }

    /// L2.6: TTL 0 e node sem health continuam recusados (regras pré-existentes).
    #[test]
    fn montar_manifesto_recusa_ttl_zero_e_node_sem_health() {
        let e =
            montar_manifesto("pedi", "laravel", "/up", true, Some(0)).expect_err("TTL 0 recusa");
        assert!(e.contains("TTL 0"), "erro: {e}");
        let e =
            montar_manifesto("pedi", "node", "", false, None).expect_err("node sem health recusa");
        assert!(e.contains("health path"), "erro: {e}");
        // E nome inválido também é da montagem (contrato único do L2.1).
        assert!(montar_manifesto("Pedi", "laravel", "/up", false, None).is_err());
    }
}
