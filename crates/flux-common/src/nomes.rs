//! Nome de projeto — CONTRATO ÚNICO (L2.1): `[a-z0-9-]`, não vazio. Vira
//! sufixo de host (`{name}-d.bitello.xyz`), nome de compose project
//! (`flux-{name}-{env}`) e chave de estado; relaxar/endurecer exige mudar
//! AQUI e nenhum outro lugar. A variante de HOST custom (com '.') é OUTRO
//! contrato — vive em host.rs/config.rs e não se mistura.

/// O nome é um projeto válido? Predicado puro, sem alocação.
pub fn projeto_valido(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aceita_so_minuscula_digito_e_traco() {
        for nome in ["vipers", "vipers-ok-1", "a"] {
            assert!(projeto_valido(nome), "{nome} deveria valer");
        }
    }

    #[test]
    fn recusa_vazio_maiuscula_ponto_espaco_acento_e_escape() {
        // NOTA: "-" sozinho VALIA no contrato original (5 cópias) e CONTINUA
        // valendo — o movimento é verbatim, endurecer seria mudar
        // comportamento de registro/roteamento (fora do escopo).
        for nome in ["", "Vipers", "a.b", "a b", "viç", "../escapou"] {
            assert!(!projeto_valido(nome), "{nome:?} não deveria valer");
        }
    }
}
