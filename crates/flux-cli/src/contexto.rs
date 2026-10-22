//! Contexto do projeto + primitivas compartilhadas (L7.1): o que TODOS os
//! domínios de comando usam — mora aqui para cada extração seguinte (L7.2)
//! ser isolada. `contexto_atual` mantém os `process::exit` no lugar (CLI
//! monocomando: erro de contexto é fatal e printado, sem Result).

use flux_common::config::ProjectToml;
use sha2::{Digest, Sha256};

/// Lease do `flux up` (04): cobre uma sessão de trabalho padrão; renovado a
/// cada ttl/2 enquanto o comando vive e morrendo sozinho depois (o sono é o
/// padrão da plataforma, não exceção).
pub(crate) const LEASE_UP_SECS: i64 = 15 * 60;

/// Projeto do diretório atual (06: "diretório = contexto"). `flux.toml`
/// explícito vence; sem ele, detecção mínima pelo nome da pasta com AVISO —
/// nunca dedução silenciosa.
pub(crate) struct Contexto {
    pub(crate) name: String,
    pub(crate) toml: String,
    pub(crate) toml_hash: String,
    /// Raiz do repo (alpha da sync, 02).
    pub(crate) dir: std::path::PathBuf,
    /// Dedução não-fatal (sem flux.toml = nome da pasta): o CLI imprime e
    /// segue; processos long-lived (MCP) decidem o que fazem com ela.
    pub(crate) aviso: Option<String>,
}

pub(crate) fn contexto_atual() -> Contexto {
    match contexto_atual_ou_erro() {
        Ok(c) => {
            if let Some(a) = &c.aviso {
                eprintln!("aviso: {a}");
            }
            c
        }
        Err(e) => {
            eprintln!("erro: {e}");
            std::process::exit(1);
        }
    }
}

/// Variante sem `exit` para processos long-lived (servidor MCP, 22/10): erro
/// de contexto NÃO pode matar o processo — sobe como `Err` com a causa.
pub(crate) fn contexto_atual_ou_erro() -> Result<Contexto, String> {
    contexto_de_dir(
        std::env::current_dir().map_err(|e| format!("não consegui ler o diretório atual: {e}"))?,
    )
}

/// A MESMA regra de contexto para um diretório explícito (MCP resolve a
/// sessão de sync por `dir` passado pela tool). Fonte única da dedução.
pub(crate) fn contexto_de_dir(dir: std::path::PathBuf) -> Result<Contexto, String> {
    let pasta = dir
        .file_name()
        .and_then(|n| n.to_str())
        .map(str::to_owned)
        .ok_or_else(|| format!("diretório sem nome de pasta utilizável: {}", dir.display()))?;
    let caminho = dir.join("flux.toml");
    let (toml, name, aviso) = if caminho.exists() {
        let src = std::fs::read_to_string(&caminho)
            .map_err(|e| format!("lendo {}: {e}", caminho.display()))?;
        let parsed = ProjectToml::parse_str(&src)
            .map_err(|e| format!("{} inválido: {e}", caminho.display()))?;
        (src, parsed.effective_name(&pasta).to_owned(), None)
    } else {
        let aviso = format!(
            "sem flux.toml em {} — projeto herdando o nome da pasta ({pasta}) e defaults da plataforma",
            dir.display()
        );
        (String::new(), pasta.clone(), Some(aviso))
    };
    Ok(Contexto {
        toml_hash: sha256_hex(toml.as_bytes()),
        name,
        toml,
        dir,
        aviso,
    })
}

pub(crate) fn sha256_hex(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    let mut s = String::with_capacity(64);
    for b in h.finalize() {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

pub(crate) fn parse_env(env: &str) -> Env {
    match env.parse() {
        Ok(e) => e,
        Err(m) => {
            eprintln!("erro: {m}");
            std::process::exit(2);
        }
    }
}

/// Saída de erro padronizada: causa do `ApiError` no stderr + exit code do
/// erro (06: erro nunca sai com exit 0; 09: causa sempre visível).
pub(crate) fn falhar(e: ApiError) -> ! {
    eprintln!("erro: {e}");
    std::process::exit(e.exit_code())
}

use crate::api::ApiError;
use flux_common::protocol::Env;
