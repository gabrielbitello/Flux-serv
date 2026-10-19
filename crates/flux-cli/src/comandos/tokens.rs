//! tokens (L7.2): extraído verbatim do main.rs — movimento sem "melhoria".

use crate::contexto::sha256_hex;

/// 64 chars hex de 32 bytes do CSPRNG do SO — token da API de controle.
pub(crate) fn token_novo() -> String {
    let mut buf = [0u8; 32];
    getrandom::fill(&mut buf).expect("CSPRNG do SO indisponível — não há token seguro sem isso");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// `flux token generate` (09, estilo key:generate): token novo → sha256
/// gravado no SQLite do fluxd via SSH (`settings.api_token_hash`) + valor
/// claro no config do usuário. O config SÓ é escrito DEPOIS do hash na VM
/// `flux vm ssh` — shell interativo na VM com a chave certa: a entrada
/// `192.168.140.2` do `~/.ssh/config` carrega user `flux` + IdentityFile
/// (09 — a chave do Flux nunca vai para outra máquina). Herda o console e
/// repassa o exit code do ssh.
pub(crate) fn vm_ssh() -> ! {
    let status = std::process::Command::new("ssh")
        .arg(crate::tunel::VM_HOST)
        .status()
        .unwrap_or_else(|e| {
            eprintln!("erro: não consegui lançar o ssh (OpenSSH no PATH?): {e}");
            std::process::exit(127);
        });
    std::process::exit(status.code().unwrap_or(1));
}

/// dar certo — falha no meio nunca deixa CLI com token que o fluxd não
/// conhece. Token antigo morre na hora (revogar = gerar outro).
pub(crate) fn token_generate() {
    let token = token_novo();
    let hash = sha256_hex(token.as_bytes());
    let ssh_alvo = format!("{}@{}", crate::tunel::VM_USER, crate::tunel::VM_HOST);
    let remoto = format!(
        "sudo sqlite3 /srv/flux/state/flux.sqlite3 \"INSERT OR REPLACE INTO \
         settings(k, v) VALUES ('api_token_hash', '{hash}');\""
    );
    let saida = match std::process::Command::new("ssh")
        .args([
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=8",
            &ssh_alvo,
            &remoto,
        ])
        .output()
    {
        Ok(s) => s,
        Err(e) => {
            eprintln!("erro: ssh não lançado (OpenSSH no PATH?): {e}");
            std::process::exit(1);
        }
    };
    if !saida.status.success() {
        eprintln!(
            "erro: gravar api_token_hash na VM falhou (ssh {}): {}",
            ssh_alvo,
            String::from_utf8_lossy(&saida.stderr).trim()
        );
        std::process::exit(1);
    }
    let caminho = match crate::tunel::salvar_token_no_config(&token) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("erro: {e}");
            std::process::exit(1);
        }
    };
    println!(
        "hash gravado na VM ({ssh_alvo}) · config atualizado: {}",
        caminho.display()
    );
    println!();
    println!("token (MOSTRA UMA VEZ — guarde no gerenciador de senhas):");
    println!("  {token}");
    println!();
    println!("próximo comando `flux` já usa o token novo (config.toml [api] token).");
}

/// Mesmo contrato de nome do registro (api.rs, POST /projects): [a-z0-9-],
/// não vazio. Validar ANTES do ssh — o nome entra no comando remoto, nome
/// com aspas/vírgula viraria injeção no shell da VM.
pub(crate) fn nome_de_projeto_ok(nome: &str) -> bool {
    !nome.is_empty()
        && nome
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// `flux project token <nome>` (10, F4): token de deploy POR PROJETO —
/// token novo (CSPRNG do SO, mesmo gerador do token da API), sha256
/// gravado na tabela `deploy_tokens` do SQLite do fluxd via SSH (o
/// handler público de deploy compara esse hash) e valor claro impresso
/// UMA vez (vai para o GitHub Secrets). Regenerar = INSERT OR REPLACE =
/// hash anterior revogado, sem tocar os tokens dos outros projetos.
/// Falha de ssh sai com stderr real e exit ≠ 0 — o segredo nunca é
/// impresso sem a gravação ter acontecido.
pub(crate) fn project_token(name: &str) {
    let nome = name.trim();
    if !nome_de_projeto_ok(nome) {
        eprintln!(
            "erro: nome de projeto inválido: use minúsculas, dígitos e '-' (vira host {nome}.bitello.xyz)"
        );
        std::process::exit(2);
    }
    let token = token_novo();
    let hash = sha256_hex(token.as_bytes());
    let ssh_alvo = format!("{}@{}", crate::tunel::VM_USER, crate::tunel::VM_HOST);
    // `nome` validado contra [a-z0-9-] acima e `hash` é hex fixo de 64
    // chars gerado aqui — nenhum dos dois injeta no shell remoto.
    let remoto = format!(
        "sudo sqlite3 /srv/flux/state/flux.sqlite3 \"INSERT OR REPLACE INTO \
         deploy_tokens(project, token_hash) VALUES ('{nome}', '{hash}');\""
    );
    let saida = match std::process::Command::new("ssh")
        .args([
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=8",
            &ssh_alvo,
            &remoto,
        ])
        .output()
    {
        Ok(s) => s,
        Err(e) => {
            eprintln!("erro: ssh não lançado (OpenSSH no PATH?): {e}");
            std::process::exit(1);
        }
    };
    if !saida.status.success() {
        eprintln!(
            "erro: gravar deploy_tokens[{}] na VM falhou (ssh {}): {}",
            nome,
            ssh_alvo,
            String::from_utf8_lossy(&saida.stderr).trim()
        );
        std::process::exit(1);
    }
    println!(
        "hash de deploy gravado na VM ({ssh_alvo}) · projeto: {nome} · token anterior revogado"
    );
    println!();
    println!("token de deploy (MOSTRA UMA VEZ — guarde no GitHub Secrets):");
    println!("  {token}");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Geração e hash do token de deploy (F4): token = 64 chars hex do
    /// CSPRNG; hash = sha256 lowercase de 64 chars — formato que os DOIS
    /// lados (CLI grava, fluxd compara) têm de concordar, senão vira 401
    /// eterno. Vetor NIST fixa o algoritmo contra regressão.
    #[test]
    fn token_novo_e_sha256_hex_tem_formato_do_contrato() {
        let hex_ok = |s: &str| {
            s.len() == 64
                && s.chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        };
        let token = token_novo();
        assert!(hex_ok(&token), "token é 64 chars hex lowercase: {token}");
        let token2 = token_novo();
        assert_ne!(token, token2, "CSPRNG: dois tokens nunca coincidem");

        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            "vetor NIST do sha256"
        );
        assert!(hex_ok(&sha256_hex(token.as_bytes())));
    }

    /// Nome válido = o mesmo contrato do registro ([a-z0-9-]): nome
    /// malicioso com aspas/vírgula seria injeção no sqlite3 remoto.
    #[test]
    fn nome_de_projeto_ok_cobre_o_contrato_do_registro() {
        assert!(nome_de_projeto_ok("vipers"));
        assert!(nome_de_projeto_ok("dj-is-magic"));
        assert!(nome_de_projeto_ok("abc123"));
        assert!(!nome_de_projeto_ok(""), "vazio recusado");
        assert!(!nome_de_projeto_ok("DJ-is-magic"), "maiúscula recusada");
        assert!(!nome_de_projeto_ok("vipers_dev"), "underscore recusado");
        assert!(
            !nome_de_projeto_ok("x'); DROP TABLE projects;--"),
            "injeção no sqlite3 remoto recusada"
        );
    }
}
