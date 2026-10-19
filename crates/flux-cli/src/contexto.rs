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
}

pub(crate) fn contexto_atual() -> Contexto {
    let cwd = match std::env::current_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("erro: não consegui ler o diretório atual: {e}");
            std::process::exit(1);
        }
    };
    let pasta = cwd
        .file_name()
        .and_then(|n| n.to_str())
        .map(str::to_owned)
        .unwrap_or_else(|| {
            eprintln!(
                "erro: diretório atual sem nome de pasta utilizável: {}",
                cwd.display()
            );
            std::process::exit(1);
        });
    let caminho = cwd.join("flux.toml");
    let dir = cwd.clone();
    let (toml, name) = if caminho.exists() {
        let src = match std::fs::read_to_string(&caminho) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("erro: lendo {}: {e}", caminho.display());
                std::process::exit(1);
            }
        };
        let parsed = match ProjectToml::parse_str(&src) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("erro: {} inválido: {e}", caminho.display());
                std::process::exit(2);
            }
        };
        (src, parsed.effective_name(&pasta).to_owned())
    } else {
        eprintln!(
            "aviso: sem flux.toml em {} — projeto herdando o nome da pasta ({pasta}) e defaults da plataforma",
            cwd.display()
        );
        (String::new(), pasta)
    };
    Contexto {
        toml_hash: sha256_hex(toml.as_bytes()),
        name,
        toml,
        dir,
    }
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
