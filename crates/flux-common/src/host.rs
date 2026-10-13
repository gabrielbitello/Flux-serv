//! Modelo de hosts gerenciados (03): dispatch por `Host` exato, com resolução
//! de projeto por sufixo mais longo para separar `{branch}-{name}` quando o
//! nome do projeto (ou da branch) contém hífen.

use crate::config::DomainConfig;

/// Host decodificado do header `Host` da requisição.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManagedHost {
    /// `flux.bitello.xyz` — painel.
    Panel,
    /// `{name}.bitello.xyz` — prod, cor ativa do blue-green (10).
    Prod { name: String },
    /// `{name}-d.bitello.xyz` (branch atual do Windows) ou
    /// `{branch}-{name}-d.bitello.xyz` (worktree específico, 02).
    Dev {
        name: String,
        branch: Option<String>,
    },
    /// Fora da zona ou não gerenciado — 404 brandiado, único 404 da matriz (03).
    Unknown(String),
}

impl ManagedHost {
    pub fn is_dev(&self) -> bool {
        matches!(self, Self::Dev { .. })
    }

    /// Decodifica o header `Host` (host:port e caixa alta tolerados).
    /// `is_project` consulta os projetos registrados no estado do fluxd.
    pub fn parse(raw: &str, domain: &DomainConfig, is_project: impl Fn(&str) -> bool) -> Self {
        let host = raw.trim().to_ascii_lowercase();
        let host = host.split(':').next().unwrap_or("").trim();

        if host == domain.panel_host {
            return Self::Panel;
        }

        let zone_dot = format!(".{}", domain.zone);
        let Some(rest) = host.strip_suffix(&zone_dot) else {
            return Self::Unknown(raw.to_owned());
        };
        // "bitello.xyz" pelado não é host gerenciado.
        if rest.is_empty() {
            return Self::Unknown(raw.to_owned());
        }

        let suffix = domain.dev_suffix.as_str();
        if let Some(base) = rest.strip_suffix(suffix) {
            if base.is_empty() {
                return Self::Unknown(raw.to_owned());
            }
            // 1) tudo antes do sufixo é o projeto (branch atual do Windows);
            if is_project(base) {
                return Self::Dev {
                    name: base.to_owned(),
                    branch: None,
                };
            }
            // 2) senão `{branch}-{name}`: varre os separadores da ESQUERDA —
            //    o primeiro match dá o SUFIXO de nome mais longo registrado
            //    (03: evita ambiguidade com nomes que contêm hífen).
            for (idx, _) in base.match_indices('-') {
                let name = &base[idx + 1..];
                if is_project(name) {
                    return Self::Dev {
                        name: name.to_owned(),
                        branch: Some(base[..idx].to_owned()),
                    };
                }
            }
            return Self::Unknown(raw.to_owned());
        }

        if is_project(rest) {
            return Self::Prod {
                name: rest.to_owned(),
            };
        }
        Self::Unknown(raw.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn domain() -> DomainConfig {
        DomainConfig::default()
    }

    fn known(name: &str) -> bool {
        matches!(name, "vipers" | "my-app" | "pedi")
    }

    #[test]
    fn prod_resolvida() {
        let h = ManagedHost::parse("vipers.bitello.xyz", &domain(), known);
        assert_eq!(
            h,
            ManagedHost::Prod {
                name: "vipers".into()
            }
        );
    }

    #[test]
    fn dev_da_branch_atual() {
        let h = ManagedHost::parse("vipers-d.bitello.xyz", &domain(), known);
        assert_eq!(
            h,
            ManagedHost::Dev {
                name: "vipers".into(),
                branch: None
            }
        );
    }

    #[test]
    fn dev_com_branch_especifica() {
        let h = ManagedHost::parse("feat-x-vipers-d.bitello.xyz", &domain(), known);
        assert_eq!(
            h,
            ManagedHost::Dev {
                name: "vipers".into(),
                branch: Some("feat-x".into())
            }
        );
    }

    #[test]
    fn projeto_com_hifen_pega_sufixo_mais_longo() {
        // "feat" poderia ser o projeto e "my-app" não é; o nome registrado
        // "my-app" vence porque é o sufixo mais longo.
        let h = ManagedHost::parse("feat-my-app-d.bitello.xyz", &domain(), known);
        assert_eq!(
            h,
            ManagedHost::Dev {
                name: "my-app".into(),
                branch: Some("feat".into())
            }
        );
    }

    #[test]
    fn painel_resolvida() {
        let h = ManagedHost::parse("flux.bitello.xyz", &domain(), known);
        assert_eq!(h, ManagedHost::Panel);
    }

    #[test]
    fn porta_e_caixa_alta_toleradas() {
        let h = ManagedHost::parse("VIPERS-d.Bitello.XYZ:8080", &domain(), known);
        assert_eq!(
            h,
            ManagedHost::Dev {
                name: "vipers".into(),
                branch: None
            }
        );
    }

    #[test]
    fn host_desconhecido_e_zona_pelada() {
        for raw in [
            "zzz.bitello.xyz",
            "zzz-d.bitello.xyz",
            "bitello.xyz",
            "-d.bitello.xyz",
            "outro-dominio.com",
            "",
        ] {
            let h = ManagedHost::parse(raw, &domain(), known);
            assert!(
                matches!(h, ManagedHost::Unknown(_)),
                "{raw:?} deveria ser Unknown"
            );
        }
    }

    #[test]
    fn prod_desconhecida_nao_vira_dev() {
        let h = ManagedHost::parse("naoexiste.bitello.xyz", &domain(), known);
        assert!(matches!(h, ManagedHost::Unknown(_)));
    }
}
