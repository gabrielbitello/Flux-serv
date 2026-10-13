//! Stack do projeto (R26). `laravel` tem suporte máximo; `node` e `rust` são
//! cidadãos de primeira classe via `[health] path` + `[db] migrate_cmd` (06).
//! Adapter novo (python, go…) = variante nova + 3 implementações.

use std::fmt;
use std::str::FromStr;

use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Framework {
    #[default]
    Laravel,
    Node,
    Rust,
    Static,
    Custom,
}

impl FromStr for Framework {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "laravel" => Ok(Self::Laravel),
            "node" => Ok(Self::Node),
            "rust" => Ok(Self::Rust),
            "static" => Ok(Self::Static),
            "custom" => Ok(Self::Custom),
            other => Err(format!(
                "framework desconhecido: {other:?} — valores válidos: laravel, node, rust, static, custom"
            )),
        }
    }
}

impl fmt::Display for Framework {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Laravel => "laravel",
            Self::Node => "node",
            Self::Rust => "rust",
            Self::Static => "static",
            Self::Custom => "custom",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aceita_todos_os_valores_da_spec() {
        for v in ["laravel", "node", "rust", "static", "custom"] {
            assert!(v.parse::<Framework>().is_ok(), "{v} deveria ser válido");
        }
    }

    #[test]
    fn recusa_valor_desconhecido_com_mensagem_clara() {
        let e = "python".parse::<Framework>().unwrap_err();
        assert!(e.contains("python"), "mensagem: {e}");
    }

    #[test]
    fn default_e_laravel() {
        assert_eq!(Framework::default(), Framework::Laravel);
    }
}
