//! Formatação PURA dos registros do doorman (31/10): recebe cabeçalho/corpo,
//! devolve o bloco de texto do terminal. Sem I/O — tudo testável. A redação
//! segue a disciplina do painel (hdrs redigidos, `events.rs`); os CORPOS não
//! são redigidos de propósito: ver conteúdo é a razão de ser do doorman.

/// Tetos de exibição (o que passa íntegro; o que APARECE é cortado).
pub(crate) struct Aparencia {
    /// Não formata JSON/SSE — imprime cru.
    pub(crate) cru: bool,
    /// Teto do corpo de request mostrado.
    pub(crate) teto_corpo: usize,
    /// Teto do corpo de resposta mostrado (buffer para pretty ou fluxo vivo).
    pub(crate) teto_resposta: usize,
}

impl Default for Aparencia {
    fn default() -> Self {
        Self {
            cru: false,
            teto_corpo: 256 * 1024,
            teto_resposta: 512 * 1024,
        }
    }
}

/// Headers cujo VALOR nunca aparece: o terminal é tela do usuário, mas
/// transcrição/prints vazam — mesmo tratamento do dump de hdrs do fluxd.
const REDIGIDOS: [&str; 4] = ["authorization", "x-api-key", "cookie", "set-cookie"];

/// Bloco completo de uma request registrada.
pub(crate) fn bloco_request(
    hora: &str,
    ordem: u64,
    metodo: &str,
    uri: &str,
    headers: &hyper::http::HeaderMap,
    corpo: &[u8],
    apr: &Aparencia,
) -> String {
    let mut s = format!("── #{ordem} {hora} ── {metodo} {uri}\n");
    s.push_str(&cabecalhos(headers));
    if corpo.is_empty() {
        return s;
    }
    s.push_str(&format!("corpo ({} B):\n", corpo.len()));
    s.push_str(&corpo_texto(corpo, apr.teto_corpo, !apr.cru));
    s
}

/// Cabeçalho do bloco de resposta (impresso antes do corpo fluir).
pub(crate) fn bloco_resposta(
    hora: &str,
    ordem: u64,
    status: u16,
    headers: &hyper::http::HeaderMap,
) -> String {
    let tipo = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("-");
    format!(
        "── #{ordem} ← {status} {hora} ({tipo})\n{}",
        cabecalhos(headers)
    )
}

/// Pedaço de resposta em fluxo (SSE): imprime crú conforme chega.
pub(crate) fn pedaco_fluxo(pedaco: &[u8], ja_impresso: usize, teto: usize) -> Option<String> {
    if ja_impresso >= teto {
        return None;
    }
    let visivel = teto - ja_impresso;
    let fim = pedaco.len().min(visivel);
    let mut s = String::from_utf8_lossy(&pedaco[..fim]).into_owned();
    if fim < pedaco.len() {
        s.push_str("\n…(fluxo cortado na exibição)\n");
    }
    Some(s)
}

/// Corpo de resposta bufferizada (não-SSE): pretty JSON quando der.
pub(crate) fn bloco_resposta_corpo(corpo: &[u8], json: bool, apr: &Aparencia) -> Option<String> {
    if corpo.is_empty() {
        return None;
    }
    let mut s = String::from("\ncorpo:\n");
    s.push_str(&corpo_texto(corpo, apr.teto_resposta, json && !apr.cru));
    Some(s)
}

/// Linhas de headers: sensíveis redigidos, resto verbatim.
fn cabecalhos(headers: &hyper::http::HeaderMap) -> String {
    let mut s = String::new();
    for (nome, valor) in headers {
        let nome = nome.as_str();
        if REDIGIDOS.contains(&nome) {
            s.push_str(&format!("{nome}: <redigido:{}B>\n", valor.len()));
            continue;
        }
        let valor = String::from_utf8_lossy(valor.as_bytes());
        s.push_str(&format!("{nome}: {valor}\n"));
    }
    s
}

/// Corpo em texto: JSON bonito quando pedre e parses; corta no teto com
/// marcador (o que passou íntegro é sempre o corpo REAL, nunca a exibição).
fn corpo_texto(corpo: &[u8], teto: usize, bonificar: bool) -> String {
    let texto = if bonificar {
        serde_json::from_slice::<serde_json::Value>(corpo)
            .ok()
            .and_then(|v| serde_json::to_string_pretty(&v).ok())
            .unwrap_or_else(|| String::from_utf8_lossy(corpo).into_owned())
    } else {
        String::from_utf8_lossy(corpo).into_owned()
    };
    if texto.len() <= teto {
        return texto;
    }
    let mut corte = teto;
    while !texto.is_char_boundary(corte) {
        corte -= 1;
    }
    format!(
        "{}\n…(+{} B não mostrados — repassados íntegros)",
        &texto[..corte],
        texto.len() - corte
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(lista: &[(&str, &str)]) -> hyper::http::HeaderMap {
        let mut h = hyper::http::HeaderMap::new();
        for (n, v) in lista {
            h.insert(
                hyper::http::HeaderName::from_bytes(n.as_bytes()).unwrap(),
                hyper::http::HeaderValue::from_str(v).unwrap(),
            );
        }
        h
    }

    #[test]
    fn redige_sensiveis_e_mantem_o_resto() {
        let h = headers(&[
            ("authorization", "Bearer segredo-bem-grande"),
            ("x-api-key", "chave"),
            ("user-agent", "opencode/1.19.5"),
        ]);
        let s = cabecalhos(&h);
        assert!(s.contains("authorization: <redigido:25B>"), "{s}");
        assert!(s.contains("x-api-key: <redigido:5B>"), "{s}");
        assert!(s.contains("user-agent: opencode/1.19.5"), "{s}");
        assert!(!s.contains("segredo"), "{s}");
    }

    #[test]
    fn corpo_pretty_corta_com_marcador_e_nao_parte_char() {
        // {"a":"é"} — teto 7 cai no meio do é (0xC3|0xA9); a caminhada volta
        // pra fronteira e o prefixo continua UTF-8 válido.
        let mut corpo = br#"{"a":""#.to_vec();
        corpo.extend_from_slice(&[0xC3, 0xA9]);
        corpo.extend_from_slice(br#""}"#);
        let s = corpo_texto(&corpo, 7, false);
        // corte volta pra 6 (fronteira); sobem 4 B: é (2 B) + "}" (2 B)
        assert!(s.contains("(+4 B não mostrados"), "{s}");
        assert!(std::str::from_utf8(s.as_bytes()).is_ok());
    }

    #[test]
    fn json_bonito_quando_parseia_cru_quando_nao() {
        let ok = corpo_texto(br#"{"b":1,"a":2}"#, 4096, true);
        assert!(ok.contains("\"b\": 1"), "{ok}");
        let ru = corpo_texto(b"nao-json", 4096, true);
        assert_eq!(ru, "nao-json");
    }

    #[test]
    fn pedaco_fluxo_para_no_teto_e_marca() {
        assert_eq!(pedaco_fluxo(b"data: 1\n\n", 0, 100).unwrap(), "data: 1\n\n");
        assert!(pedaco_fluxo(b"mais", 100, 100).is_none());
        let cortado = pedaco_fluxo(b"0123456789", 8, 10).unwrap();
        assert_eq!(cortado, "01\n…(fluxo cortado na exibição)\n");
    }

    #[test]
    fn bloco_request_vazio_nao_mostra_corpo() {
        let h = headers(&[("host", "x.bitello.xyz")]);
        let s = bloco_request("21:00:00", 1, "GET", "/up", &h, b"", &Aparencia::default());
        assert!(s.starts_with("── #1 21:00:00 ── GET /up\n"), "{s}");
        assert!(!s.contains("corpo ("), "{s}");
    }
}
