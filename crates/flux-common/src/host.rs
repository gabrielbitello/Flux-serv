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
    /// `vite-{dev-host}.bitello.xyz` — dev server (HMR) do projeto (06/live):
    /// roda quando o live server está ligado; assets e websocket saem pelo
    /// MESMO túnel HTTPS, sem mixed content.
    Vite {
        name: String,
        branch: Option<String>,
    },
    /// Host EXATO declarado em `[domains]` do `flux.toml` (09): override da
    /// derivação. `dev` diz o ambiente — branch de worktree NÃO se aplica a
    /// host custom (um host custom é de UMA pasta por registro).
    Custom { name: String, dev: bool },
    /// Fora da zona ou não gerenciado — 404 brandiado, único 404 da matriz (03).
    Unknown(String),
}

impl ManagedHost {
    pub fn is_dev(&self) -> bool {
        matches!(
            self,
            Self::Dev { .. } | Self::Vite { .. } | Self::Custom { dev: true, .. }
        )
    }

    /// Decodifica o header `Host` (host:port e caixa alta tolerados).
    /// `is_project` consulta os projetos registrados no estado do fluxd.
    /// Derivação PURA — sem consulta de hosts custom (09); para o router
    /// real, use `parse_custom`.
    pub fn parse(raw: &str, domain: &DomainConfig, is_project: &impl Fn(&str) -> bool) -> Self {
        Self::parse_custom(raw, domain, is_project, &|_| None)
    }

    /// Como `parse`, com a consulta de hosts custom de `[domains]` (09):
    /// `host_custom(host) → Some((projeto, dev))` vence ANTES da derivação por
    /// zona — o painel e a zona derivada continuam intocados.
    pub fn parse_custom(
        raw: &str,
        domain: &DomainConfig,
        is_project: &impl Fn(&str) -> bool,
        host_custom: &impl Fn(&str) -> Option<(String, bool)>,
    ) -> Self {
        let host = raw.trim().to_ascii_lowercase();
        let host = host.split(':').next().unwrap_or("").trim();

        if host == domain.panel_host {
            return Self::Panel;
        }

        // [domains] (09): batida EXATA do host inteiro declarado no registro.
        // Exata de propósito — prefixo/sufixo inventado não vira projeto.
        if let Some((name, dev)) = host_custom(host) {
            return Self::Custom { name, dev };
        }

        let zone_dot = format!(".{}", domain.zone);
        let Some(rest) = host.strip_suffix(&zone_dot) else {
            return Self::Unknown(raw.to_owned());
        };
        // "bitello.xyz" pelado não é host gerenciado.
        if rest.is_empty() {
            return Self::Unknown(raw.to_owned());
        }

        // `vite-{dev-host}`: prefixo resolvido RECURSIVAMENTE contra o resto —
        // sem match Dev, cai no fluxo normal (um projeto cujo host real
        // começa com "vite-" não quebra).
        if let Some(base) = rest.strip_prefix("vite-") {
            let inner = format!("{base}{zone_dot}");
            if let Self::Dev { name, branch } = Self::parse(&inner, domain, is_project) {
                return Self::Vite { name, branch };
            }
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

/// `[domains]` (09): validação dos hosts custom na borda do registro →
/// `(prod, dev)`. Regras — o host PRECISA terminar com `.{zone}` (o túnel
/// cobre `*.{zone}`; host fora da zona nunca rotearia, e aí a falha seria
/// silenciosa na borda do Cloudflare), não pode ser o painel, não pode
/// começar com `vite-` (prefixo reservado do dev server) nem o rótulo
/// terminar com o `dev_suffix` (colide com a derivação de dev). Charset
/// de host legal. Erro SEMPRE cita a chave. Os hosts derivados continuam
/// valendo — `[domains]` ADICIONA, nunca remove.
///
/// L2.3: aqui é o LAR da regra — `ProjectToml::hosts_custom` (config.rs)
/// é delegação de 1 linha. Validação (escrita) e interpretação (leitura,
/// `ManagedHost::parse`) no mesmo arquivo, senão a regra se parte.
pub fn validar_hosts_custom(
    domains: Option<&crate::config::DomainsSection>,
    domain: &DomainConfig,
) -> Result<(Option<String>, Option<String>), String> {
    let validar = |valor: Option<&String>, chave: &str| -> Result<Option<String>, String> {
        let Some(h) = valor else {
            return Ok(None);
        };
        let h = h.trim().to_ascii_lowercase();
        let zona = format!(".{}", domain.zone);
        let Some(rotulo) = h.strip_suffix(&zona) else {
            return Err(format!(
                "[domains] {chave} = \"{h}\" está fora da zona .{} — a plataforma só                  roteia hosts dentro da zona (o túnel cobre *.{}); host externo precisa                  de DNS/ingress próprios e NÃO é suportado",
                domain.zone, domain.zone
            ));
        };
        if rotulo.is_empty() {
            return Err(format!(
                "[domains] {chave} é a zona pelada — falta o rótulo"
            ));
        }
        if h == domain.panel_host {
            return Err(format!(
                "[domains] {chave} = \"{h}\" colide com o host do painel — escolha outro"
            ));
        }
        if rotulo.starts_with("vite-") {
            return Err(format!(
                "[domains] {chave}: prefixo \"vite-\" é reservado do dev server (06)"
            ));
        }
        if rotulo.ends_with(domain.dev_suffix.as_str()) {
            return Err(format!(
                "[domains] {chave}: rótulo termina com \"{}\" — colide com a derivação                  de dev; escolha um rótulo sem o sufixo",
                domain.dev_suffix
            ));
        }
        if !h
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '.')
        {
            return Err(format!(
                "[domains] {chave} = \"{h}\" tem caractere fora de [a-z0-9-.]"
            ));
        }
        Ok(Some(h))
    };
    let prod = validar(domains.and_then(|d| d.prod.as_ref()), "prod")?;
    let dev = validar(domains.and_then(|d| d.dev.as_ref()), "dev")?;
    Ok((prod, dev))
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
        let h = ManagedHost::parse("vipers.bitello.xyz", &domain(), &known);
        assert_eq!(
            h,
            ManagedHost::Prod {
                name: "vipers".into()
            }
        );
    }

    /// `[domains]` (09): `parse_custom` resolve o host EXATO da tabela ANTES
    /// da derivação; `parse` puro ignora a tabela (derivação só).
    #[test]
    fn host_custom_vence_a_derivacao_so_no_parse_custom() {
        let custom = |h: &str| {
            (h == "vip.bitello.xyz")
                .then(|| ("vipers".to_owned(), false))
                .or_else(|| (h == "vip-dev.bitello.xyz").then(|| ("vipers".to_owned(), true)))
        };
        let h = ManagedHost::parse_custom("vip.bitello.xyz", &domain(), &known, &custom);
        assert_eq!(
            h,
            ManagedHost::Custom {
                name: "vipers".into(),
                dev: false
            }
        );
        assert!(!h.is_dev());
        let h = ManagedHost::parse_custom("vip-dev.bitello.xyz", &domain(), &known, &custom);
        assert_eq!(
            h,
            ManagedHost::Custom {
                name: "vipers".into(),
                dev: true
            }
        );
        assert!(h.is_dev(), "custom dev conta como dev");
        // Host fora da tabela: Unknown mesmo com tabela populada.
        assert_eq!(
            ManagedHost::parse_custom("outro-vip.bitello.xyz", &domain(), &known, &custom),
            ManagedHost::Unknown("outro-vip.bitello.xyz".into())
        );
        // E o parse puro NÃO consulta custom — router real usa parse_custom.
        assert_eq!(
            ManagedHost::parse("vip.bitello.xyz", &domain(), &known),
            ManagedHost::Unknown("vip.bitello.xyz".into())
        );
        // Painel segue na frente de tudo.
        assert_eq!(
            ManagedHost::parse_custom("flux.bitello.xyz", &domain(), &known, &|_| Some((
                "outro".into(),
                false
            ))),
            ManagedHost::Panel
        );
    }

    #[test]
    fn dev_da_branch_atual() {
        let h = ManagedHost::parse("vipers-d.bitello.xyz", &domain(), &known);
        assert_eq!(
            h,
            ManagedHost::Dev {
                name: "vipers".into(),
                branch: None
            }
        );
    }

    #[test]
    fn vite_do_dev_resolvido() {
        let h = ManagedHost::parse("vite-vipers-d.bitello.xyz", &domain(), &known);
        assert_eq!(
            h,
            ManagedHost::Vite {
                name: "vipers".into(),
                branch: None
            }
        );
    }

    #[test]
    fn vite_com_branch_e_hifen_no_nome() {
        let h = ManagedHost::parse("vite-feat-my-app-d.bitello.xyz", &domain(), &known);
        assert_eq!(
            h,
            ManagedHost::Vite {
                name: "my-app".into(),
                branch: Some("feat".into())
            }
        );
    }

    #[test]
    fn vite_sem_dev_cai_no_fluxo_normal() {
        // "vite-zzz" não é dev registrado: host inteiro = Unknown (e um
        // projeto real chamado "vite-x" não quebra — o prefixo só vira Vite
        // quando o resto resolve como Dev).
        assert!(matches!(
            ManagedHost::parse("vite-zzz-d.bitello.xyz", &domain(), &known),
            ManagedHost::Unknown(_)
        ));
    }

    #[test]
    fn dev_com_branch_especifica() {
        let h = ManagedHost::parse("feat-x-vipers-d.bitello.xyz", &domain(), &known);
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
        let h = ManagedHost::parse("feat-my-app-d.bitello.xyz", &domain(), &known);
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
        let h = ManagedHost::parse("flux.bitello.xyz", &domain(), &known);
        assert_eq!(h, ManagedHost::Panel);
    }

    #[test]
    fn porta_e_caixa_alta_toleradas() {
        let h = ManagedHost::parse("VIPERS-d.Bitello.XYZ:8080", &domain(), &known);
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
            let h = ManagedHost::parse(raw, &domain(), &known);
            assert!(
                matches!(h, ManagedHost::Unknown(_)),
                "{raw:?} deveria ser Unknown"
            );
        }
    }

    #[test]
    fn prod_desconhecida_nao_vira_dev() {
        let h = ManagedHost::parse("naoexiste.bitello.xyz", &domain(), &known);
        assert!(matches!(h, ManagedHost::Unknown(_)));
    }

    /// `[domains]` (09): validação dos hosts custom — teste MOVIDO de config.rs
    /// (L2.3): vive ao lado da regra; passa pela delegação de ProjectToml.
    #[test]
    fn hosts_custom_valida_zona_reservas_e_charset() {
        let domain = DomainConfig::default();
        let parse = |src: &str| crate::config::ProjectToml::parse_str(src).unwrap();

        // Ambos válidos: vêm em minúsculas, host inteiro.
        let (prod, dev) = parse(
            "[domains]
prod = \"VIP.bitello.xyz\"
dev = \"Vip-Dev.bitello.xyz\"",
        )
        .hosts_custom(&domain)
        .unwrap();
        assert_eq!(prod.as_deref(), Some("vip.bitello.xyz"));
        assert_eq!(dev.as_deref(), Some("vip-dev.bitello.xyz"));

        // Sem seção: nada a validar.
        let (prod, dev) = parse("").hosts_custom(&domain).unwrap();
        assert_eq!(prod, None);
        assert_eq!(dev, None);

        // Fora da zona (inclui a zona pelada — sufixo não casa): erro com a
        // chave na mensagem.
        for (src, parte) in [
            (
                "[domains]
prod = \"vippers.com.br\"",
                "fora da zona",
            ),
            (
                "[domains]
prod = \"bitello.xyz\"",
                "fora da zona",
            ),
        ] {
            let err = parse(src).hosts_custom(&domain).unwrap_err();
            assert!(err.contains(parte), "caso {src}: {err}");
        }

        // Painel, prefixo vite-, sufixo de dev, charset.
        for (src, parte) in [
            (
                "[domains]
prod = \"flux.bitello.xyz\"",
                "painel",
            ),
            (
                "[domains]
dev = \"vite-x.bitello.xyz\"",
                "vite-",
            ),
            (
                "[domains]
prod = \"x-d.bitello.xyz\"",
                "-d\"",
            ),
            (
                "[domains]
prod = \"a_b.bitello.xyz\"",
                "[a-z0-9-.]",
            ),
        ] {
            let err = parse(src).hosts_custom(&domain).unwrap_err();
            assert!(err.contains(parte), "caso {src}: {err}");
        }
    }
}
