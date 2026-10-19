//! env_cmd (L7.2): extraído verbatim do main.rs — movimento sem "melhoria".

/// `flux env` (02): o `.env` vive SÓ na VM (`/srv/flux/projects/{name}/.env`)
/// — o repo do Windows NUNCA ganha uma cópia (repo intocado, 02). `push`
/// envia o `.env` da pasta local (rascunho gitignored) pra VM; `pull` traz o
/// da VM pra pasta local; `edit` puxa pra um temp FORA do repo, abre o
/// $EDITOR e devolve. Conteúdo nunca é exibido nem logado; destino sempre
/// chmod 600. Tudo por SSH com a chave da entrada do ~/.ssh/config.
pub(crate) fn env_ssh_alvo() -> String {
    format!("{}@{}", crate::tunel::VM_USER, crate::tunel::VM_HOST)
}

pub(crate) fn env_remoto(name: &str) -> String {
    format!("/srv/flux/projects/{name}/.env")
}

pub(crate) fn env_pull(name: &str) {
    let saida = std::process::Command::new("ssh")
        .args([
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=8",
            &env_ssh_alvo(),
            &format!("cat {}", env_remoto(name)),
        ])
        .output()
        .unwrap_or_else(|e| {
            eprintln!("erro: não consegui lançar o ssh: {e}");
            std::process::exit(127);
        });
    if !saida.status.success() {
        let cauda = String::from_utf8_lossy(&saida.stderr).trim().to_owned();
        eprintln!("erro: pull do .env falhou (o projeto já subiu na VM?): {cauda}");
        std::process::exit(1);
    }
    std::fs::write(".env", &saida.stdout).unwrap_or_else(|e| {
        eprintln!("erro: escrevendo .env local: {e}");
        std::process::exit(1);
    });
    if !gitignored_env() {
        eprintln!("aviso: `.env` não está no .gitignore do repo — NUNCA comita esse arquivo (02)");
    }
    println!(".env da VM → .env local ({} bytes)", saida.stdout.len());
}

pub(crate) fn env_push(name: &str) {
    let corpo = std::fs::read(".env").unwrap_or_else(|e| {
        eprintln!("erro: sem .env na pasta local para enviar: {e} (use `flux env pull` primeiro)");
        std::process::exit(1);
    });
    let saida = std::process::Command::new("ssh")
        .args([
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=8",
            &env_ssh_alvo(),
            &format!(
                "cat > {} && chmod 600 {}",
                env_remoto(name),
                env_remoto(name)
            ),
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut filho| {
            use std::io::Write;
            filho
                .stdin
                .as_mut()
                .expect("stdin pipado")
                .write_all(&corpo)?;
            filho.wait_with_output()
        })
        .unwrap_or_else(|e| {
            eprintln!("erro: não consegui enviar o .env por ssh: {e}");
            std::process::exit(127);
        });
    if !saida.status.success() {
        let cauda = String::from_utf8_lossy(&saida.stderr).trim().to_owned();
        eprintln!("erro: push do .env falhou: {cauda}");
        std::process::exit(1);
    }
    println!(
        ".env local → VM ({} bytes) — vale no próximo request; restart não precisa",
        corpo.len()
    );
}

pub(crate) fn env_edit(name: &str) {
    let temp = std::env::temp_dir().join(format!("flux-env-{name}.env"));
    let saida = std::process::Command::new("ssh")
        .args([
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=8",
            &env_ssh_alvo(),
            &format!("cat {}", env_remoto(name)),
        ])
        .output()
        .unwrap_or_else(|e| {
            eprintln!("erro: não consegui lançar o ssh: {e}");
            std::process::exit(127);
        });
    if !saida.status.success() {
        let cauda = String::from_utf8_lossy(&saida.stderr).trim().to_owned();
        eprintln!("erro: edit precisa do .env na VM (subiu o projeto?): {cauda}");
        std::process::exit(1);
    }
    std::fs::write(&temp, &saida.stdout).unwrap_or_else(|e| {
        eprintln!("erro: escrevendo rascunho em {}: {e}", temp.display());
        std::process::exit(1);
    });
    let editor = std::env::var("EDITOR")
        .or_else(|_| std::env::var("VISUAL"))
        .unwrap_or_else(|_| "notepad".to_owned());
    let status = std::process::Command::new(&editor)
        .arg(&temp)
        .status()
        .unwrap_or_else(|e| {
            eprintln!("erro: abrindo editor \"{editor}\": {e}");
            std::process::exit(127);
        });
    if !status.success() {
        eprintln!(
            "erro: editor saiu com falha — nada foi enviado; rascunho em {}",
            temp.display()
        );
        std::process::exit(1);
    }
    let corpo = std::fs::read(&temp).unwrap_or_else(|e| {
        eprintln!("erro: relendo rascunho: {e}");
        std::process::exit(1);
    });
    std::fs::remove_file(&temp).ok();
    std::fs::write(".env", &corpo).ok();
    env_push(name);
}

/// `.env` coberto pelo .gitignore do repo? Barra barra defensiva: avisa, não
/// bloqueia — o arquivo vive na VM; a cópia local é rascunho.
pub(crate) fn gitignored_env() -> bool {
    std::fs::read_to_string(".gitignore")
        .map(|g| g.lines().any(|l| l.trim() == ".env"))
        .unwrap_or(false)
}
