//! estabilidade (L7.4): árvore git parada antes do up — rastreáveis,
//! matching de ignore, hash e espera. Extraído verbatim do sync.rs. Sem
//! `crate::` (ponte #[path]); GLOBAL_IGNORES vem de super::spec.

use std::path::Path;
use std::process::Command;

use super::spec::GLOBAL_IGNORES;
use sha2::{Digest, Sha256};

/// Teto de espera da estabilidade, em rodadas da janela (02: "ex.: 3x a
/// janela"). Três hashes consecutivos separados pela janela sem mudança
/// nenhuma — estourou, algo está escrevendo sem parar e o erro é gritado.
pub const TETO_RODADAS_ESTABILIDADE: u32 = 3;

/// Lista os arquivos rastreáveis do projeto: `git ls-files --cached --others
/// --exclude-standard` no diretório (rastreados + não-ignorados, a MESMA
/// visão do `.gitignore` que a sync replica), recortada pelo `subcaminho`
/// quando dado (ex.: `database/migrations`), com os GLOBAL_IGNORES + os
/// extras do projeto filtrados fora da lista. Saída com `-z` (paths crus,
/// sem o escape de aspas do git). Erro com origem — repo ausente NUNCA vira
/// lista vazia silenciosa.
pub fn listar_arquivos_rastreaveis(
    dir: &Path,
    extra_ignore: &[String],
    subcaminho: Option<&Path>,
) -> Result<Vec<String>, String> {
    let mut args: Vec<String> = vec![
        "-C".into(),
        dir.display().to_string(),
        "ls-files".into(),
        "-z".into(),
        "--cached".into(),
        "--others".into(),
        "--exclude-standard".into(),
    ];
    if let Some(sub) = subcaminho {
        args.push("--".into());
        args.push(sub.display().to_string().replace('\\', "/"));
    }
    let saida = Command::new("git")
        .args(&args)
        .output()
        .map_err(|e| format!("lançando git ls-files em {}: {e}", dir.display()))?;
    if !saida.status.success() {
        return Err(format!(
            "git ls-files falhou em {}: {}",
            dir.display(),
            String::from_utf8_lossy(&saida.stderr).trim()
        ));
    }
    let mut ignora: Vec<String> = GLOBAL_IGNORES.iter().map(|s| s.to_string()).collect();
    ignora.extend(
        extra_ignore
            .iter()
            .filter(|i| !i.trim().is_empty())
            .cloned(),
    );
    Ok(String::from_utf8_lossy(&saida.stdout)
        .split('\0')
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .filter(|l| !caminho_ignorado(l, &ignora))
        .map(ToOwned::to_owned)
        .collect())
}

/// Um caminho relativo é ignorado por ALGUM padrão? Semântica gitignore no
/// subconjunto que a plataforma usa (02): padrão com `/` é ancorado na raiz
/// (`public/hot` só casa na raiz), padrão solto casa qualquer segmento
/// (`.env`, `*.log`) e padrão terminado em `/` casa o diretório em qualquer
/// nível (`vendor/`).
pub(crate) fn caminho_ignorado(rel: &str, padroes: &[String]) -> bool {
    let rel = rel.replace('\\', "/");
    padroes.iter().any(|p| padrao_casa(&rel, p))
}

fn padrao_casa(rel: &str, padrao: &str) -> bool {
    // Âncora (gitignore): '/' INICIAL = casa SÓ na raiz; sem âncora, o
    // segmento casa em qualquer nível. A âncora vem ANTES do sufixo de dir —
    // "/vendor/" não pode virar segmento literal "/vendor" (nunca casa).
    let (ancorado, padrao) = match padrao.strip_prefix('/') {
        Some(p) => (true, p),
        None => (false, padrao),
    };
    if let Some(dir) = padrao.strip_suffix('/') {
        return if ancorado {
            rel == dir || rel.starts_with(&format!("{dir}/"))
        } else {
            rel.split('/').any(|seg| seg == dir)
        };
    }
    if padrao.contains('/') {
        return glob_casa(rel, padrao);
    }
    rel.split('/').any(|seg| glob_casa(seg, padrao))
}

/// Glob mínimo: só `*` (o suficiente para os ignores da plataforma). Sem `*`
/// é igualdade exata; com, o prefixo e o sufixo ancoram e o meio casa em
/// ordem.
fn glob_casa(texto: &str, padrao: &str) -> bool {
    let partes: Vec<&str> = padrao.split('*').collect();
    if partes.len() == 1 {
        return texto == padrao;
    }
    let mut resto = texto;
    for (i, parte) in partes.iter().enumerate() {
        if i == 0 {
            if !resto.starts_with(parte) {
                return false;
            }
            resto = &resto[parte.len()..];
        } else if i == partes.len() - 1 {
            if !resto.ends_with(parte) {
                return false;
            }
        } else {
            match resto.find(parte) {
                Some(p) => resto = &resto[p + parte.len()..],
                None => return false,
            }
        }
    }
    true
}

/// Hash de estabilidade da árvore: sha256 sobre (caminho relativo + tamanho +
/// mtime) de cada arquivo, ordenado — quietude SEM ler conteúdo (vendor com
/// GB seria desperdício; tamanho+mtime detectam escrita). `None` = nenhum
/// arquivo rastreável (subárvore vazia/ausente é um estado legítimo e estável).
pub fn hash_estabilidade(
    dir: &Path,
    extra_ignore: &[String],
    subcaminho: Option<&Path>,
) -> Result<Option<String>, String> {
    let arquivos = listar_arquivos_rastreaveis(dir, extra_ignore, subcaminho)?;
    if arquivos.is_empty() {
        return Ok(None);
    }
    let mut h = Sha256::new();
    for rel in &arquivos {
        let meta = std::fs::metadata(dir.join(rel))
            .map_err(|e| format!("stat de {rel} (árvore de {}): {e}", dir.display()))?;
        let desde_epoca = meta
            .modified()
            .map_err(|e| format!("mtime de {rel}: {e}"))?
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| format!("mtime de {rel} antes da época Unix: {e}"))?;
        h.update(rel.as_bytes());
        h.update([0]);
        h.update(meta.len().to_le_bytes());
        h.update(desde_epoca.as_secs().to_le_bytes());
        h.update(desde_epoca.subsec_nanos().to_le_bytes());
    }
    Ok(Some(hex_de(&h.finalize())))
}

fn hex_de(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Espera a árvore estabilizar (02, janela de silêncio): hash → espera a
/// janela → hash de novo; iguais = estável. Teto de
/// [`TETO_RODADAS_ESTABILIDADE`] rodadas — estourou, `Err` com o motivo
/// (build de asset ou plano de migrations NUNCA rodam sobre árvore em
/// mudança sem o operador saber). `progresso` recebe as mensagens de cada
/// etapa (o CLI imprime; os testes coletam) — a função em si não fala com
/// rede nem com o mutagen.
pub fn esperar_estabilidade(
    dir: &Path,
    extra_ignore: &[String],
    subcaminho: Option<&Path>,
    janela: std::time::Duration,
    mut progresso: impl FnMut(&str),
) -> Result<(), String> {
    let Some(mut anterior) = hash_estabilidade(dir, extra_ignore, subcaminho)? else {
        progresso("nenhum arquivo rastreável na subárvore — considerada estável");
        return Ok(());
    };
    for rodada in 1..=TETO_RODADAS_ESTABILIDADE {
        progresso(&format!(
            "janela de silêncio: {} s (rodada {rodada}/{TETO_RODADAS_ESTABILIDADE})…",
            janela.as_secs()
        ));
        std::thread::sleep(janela);
        let Some(atual) = hash_estabilidade(dir, extra_ignore, subcaminho)? else {
            // A árvore ESVZIOU no meio da espera: sem arquivo, nada muda —
            // estado estável por definição (a sync não tem o que replicar).
            progresso("subárvore ficou vazia durante a espera — considerada estável");
            return Ok(());
        };
        if atual == anterior {
            progresso("árvore estável — pode seguir");
            return Ok(());
        }
        progresso("árvore mudou durante a janela — reiniciando a espera");
        anterior = atual;
    }
    Err(format!(
        "árvore de {} não estabilizou em {TETO_RODADAS_ESTABILIDADE} rodadas de {} s — algo está escrevendo sem parar (watcher local? build em curso?)",
        dir.display(),
        janela.as_secs()
    ))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn padrao_de_ignore_cobre_o_subconjunto_da_plataforma() {
        let p: Vec<String> = [
            ".env",
            ".env.*",
            "public/hot",
            "vendor/",
            "node_modules/",
            "*.log",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert!(caminho_ignorado(".env", &p));
        assert!(caminho_ignorado(".env.local", &p), ".env.* casa o sufixo");
        assert!(
            caminho_ignorado("app/.env.local", &p),
            "padrão solto casa qualquer segmento"
        );
        assert!(
            caminho_ignorado("public/hot", &p),
            "com / é ancorado na raiz"
        );
        assert!(
            !caminho_ignorado("a/public/hot", &p),
            "ancorado NÃO casa em subnível"
        );
        assert!(
            caminho_ignorado("vendor/autoload.php", &p),
            "diretório com / final casa em qualquer nível"
        );
        assert!(caminho_ignorado("x/vendor/y.php", &p));
        assert!(caminho_ignorado("debug.log", &p), "glob *.log");
        assert!(
            !caminho_ignorado("debug.log.bak", &p),
            "sufixo não é sufixo aqui"
        );
        assert!(!caminho_ignorado("log.txt", &p));
        assert!(
            !caminho_ignorado("app/Http/Controllers/Controller.php", &p),
            "código do projeto nunca é ignorado por esses padrões"
        );
        // Windows: barra invertida normalizada antes de casar.
        assert!(caminho_ignorado("vendor\\autoload.php", &p));
    }

    #[test]
    fn dir_sem_git_e_erro_com_origem() {
        if !git_disponivel() {
            eprintln!("git indisponível — teste pulado");
            return;
        }
        let base = std::env::temp_dir().join(format!("flux-est-nogit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(base.join("a.txt"), b"x").unwrap();
        let err = hash_estabilidade(&base, &[], None).unwrap_err();
        assert!(
            err.contains("git ls-files"),
            "origem do erro tem de citar o git: {err}"
        );
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn hash_da_arvore_ignora_globais_e_muda_com_conteudo() {
        if !git_disponivel() {
            eprintln!("git indisponível — teste pulado");
            return;
        }
        let base = repo_temp("ignora");
        std::fs::write(base.join("rotas.php"), b"um").unwrap();
        std::fs::write(base.join(".env"), b"segredo").unwrap();
        std::fs::create_dir_all(base.join("vendor/x")).unwrap();
        std::fs::write(base.join("vendor/x/autoload.php"), b"v").unwrap();

        let h0 = hash_estabilidade(&base, &[], None).unwrap().unwrap();

        // GLOBAL_IGNORES fora do hash: mudança em .env e vendor/ NÃO move o
        // hash — é exatamente o que impede o build de disparar por causa deles.
        std::fs::write(base.join(".env"), b"outro-segredo").unwrap();
        std::fs::write(base.join("vendor/x/autoload.php"), b"v2").unwrap();
        let h1 = hash_estabilidade(&base, &[], None).unwrap().unwrap();
        assert_eq!(h0, h1, "vendor/ e .env ficam fora do hash");

        // Conteúdo de arquivo de verdade muda → hash muda.
        std::fs::write(base.join("rotas.php"), b"dois").unwrap();
        let h2 = hash_estabilidade(&base, &[], None).unwrap().unwrap();
        assert_ne!(h0, h2, "mudança rastreada tem de aparecer no hash");

        // extra_ignore do projeto entra no filtro: com tudo ignorado, a
        // subárvore vira None (estado legítimo e estável).
        let vazio = hash_estabilidade(&base, &["rotas.php".to_owned()], None).unwrap();
        assert_eq!(vazio, None, "sem arquivo rastreável: None, não erro");

        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn subcaminho_recorta_o_escopo_do_hash() {
        if !git_disponivel() {
            eprintln!("git indisponível — teste pulado");
            return;
        }
        let base = repo_temp("sub");
        std::fs::create_dir_all(base.join("database/migrations")).unwrap();
        std::fs::create_dir_all(base.join("app")).unwrap();
        std::fs::write(base.join("database/migrations/0001_cria.php"), b"m").unwrap();
        std::fs::write(base.join("app/outro.php"), b"o").unwrap();
        let sub = Path::new("database/migrations");

        let h0 = hash_estabilidade(&base, &[], Some(sub)).unwrap().unwrap();
        // Mudança FORA do escopo não move o hash da subárvore.
        std::fs::write(base.join("app/outro.php"), b"o2").unwrap();
        let h1 = hash_estabilidade(&base, &[], Some(sub)).unwrap().unwrap();
        assert_eq!(h0, h1, "fora do subcaminho não conta");
        // Mudança DENTRO move.
        std::fs::write(base.join("database/migrations/0001_cria.php"), b"m2").unwrap();
        let h2 = hash_estabilidade(&base, &[], Some(sub)).unwrap().unwrap();
        assert_ne!(h0, h2, "migration mutada tem de mover o hash");

        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn espera_estabilidade_passa_quando_nada_muda() {
        if !git_disponivel() {
            eprintln!("git indisponível — teste pulado");
            return;
        }
        let base = repo_temp("estavel");
        std::fs::write(base.join("a.php"), b"a").unwrap();
        let mut msgs: Vec<String> = Vec::new();
        esperar_estabilidade(&base, &[], None, Duration::from_millis(30), |m| {
            msgs.push(m.to_owned())
        })
        .expect("árvore parada estabiliza na primeira rodada");
        assert!(
            msgs.iter().any(|m| m.contains("estável")),
            "progresso anunciou a estabilidade: {msgs:?}"
        );
        std::fs::remove_dir_all(&base).ok();
    }

    /// Escritor em loop + janela de 100 ms: entre DOIS hashes consecutivos
    /// sempre cai ao menos uma escrita (25 ms de período), então o teto de
    /// 3 rodadas estoura com motivo — o caminho de erro da 02.
    #[test]
    fn espera_estabilidade_erra_quando_escrita_nao_para() {
        if !git_disponivel() {
            eprintln!("git indisponível — teste pulado");
            return;
        }
        let base = repo_temp("instavel");
        let alvo = base.join("a.php");
        std::fs::write(&alvo, b"00000000000000000000").unwrap();
        let escritor = {
            let alvo = alvo.clone();
            std::thread::spawn(move || {
                for i in 0..60u32 {
                    std::thread::sleep(Duration::from_millis(25));
                    let corpo = format!("{i:020}");
                    if std::fs::write(&alvo, corpo.as_bytes()).is_err() {
                        break;
                    }
                }
            })
        };
        let err = esperar_estabilidade(&base, &[], None, Duration::from_millis(100), |_| {})
            .expect_err("escrita contínua NÃO estabiliza");
        escritor.join().unwrap();
        assert!(
            err.contains("não estabilizou"),
            "motivo explícito no erro: {err}"
        );
        std::fs::remove_dir_all(&base).ok();
    }

    /// Git no PATH? Os testes de árvore precisam de `git ls-files`; sem git,
    /// passam vazios (mesma convenção dos testes gated por `MUTAGEN_BIN`).
    /// Compartilhados com os testes de spec (ignores/dockerignore criam repo).
    pub(crate) fn git_disponivel() -> bool {
        Command::new("git")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Repo git vazio em tempdir (estilo dos testes do main.rs: pasta única
    /// por processo, removida no fim).
    pub(crate) fn repo_temp(nome: &str) -> std::path::PathBuf {
        let base = std::env::temp_dir().join(format!("flux-est-{nome}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let st = Command::new("git")
            .args(["-C", &base.display().to_string(), "init", "-q"])
            .status()
            .expect("git deveria ter sido lançado (checado por git_disponivel)");
        assert!(st.success(), "git init falhou em {}", base.display());
        base
    }
}
