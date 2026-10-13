//! Erros compartilhados. Falha é sempre visível: cada variante carrega origem
//! e mensagem pronta para o CLI/painel (princípios da spec 09 — nada
//! silencioso, nada de fallback embutido).

/// Causa classificada de indisponibilidade (R33, spec 17): quem está fora.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Causa {
    /// Só o site caiu — o fluxd sabe (container down/crash/health falhando).
    Site,
    /// Plataforma fora — o fluxhost serve o status estático.
    Plataforma,
    /// Internet do servidor caiu — só a testemunha externa vê.
    Internet,
}

#[derive(Debug, thiserror::Error)]
pub enum FluxError {
    #[error("configuração inválida em {file}: {source}")]
    Config {
        file: String,
        #[source]
        source: toml::de::Error,
    },

    #[error("falha lendo {file}: {source}")]
    ConfigIo {
        file: String,
        #[source]
        source: std::io::Error,
    },

    #[error(
        "estado do fluxd corrompido em {path} — restaure de /srv/flux/backups/ (o fluxd nunca recria em silêncio)"
    )]
    StateCorrupt { path: String },

    #[error("e/S no estado do fluxd ({path}): {source}")]
    StateIo {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error(
        "banco de estado é de um fluxd mais novo (schema {found}, este binário entende até {supported}) — atualize o fluxd"
    )]
    SchemaTooNew { found: i64, supported: i64 },

    #[error("{0}")]
    Other(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mensagens_de_erro_carregam_origem_e_causa() {
        let e = FluxError::SchemaTooNew {
            found: 9,
            supported: 1,
        };
        let msg = e.to_string();
        assert!(msg.contains("schema 9"), "mensagem: {msg}");
        assert!(msg.contains("atualize o fluxd"), "mensagem: {msg}");
    }
}
