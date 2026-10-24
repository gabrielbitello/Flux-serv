//! Escrita de eventos no SQLite (07): log de acesso estruturado (host, método,
//! caminho, status, duração, origem — sem corpo) + trilha geral para o painel
//! (08). Falha ao gravar é logada em nível erro, nunca engolida.

use crate::state::State;
use axum::http::{HeaderMap, header};

/// Origem do request (24/10, pedido do Gabriel — todo request de fora 100%
/// auditável): `xff` e `user_agent` crus em TODO access; `headers` é o dump
/// completo, e só existe para fonte EXTERNA — o gatilho de RCE que não deixa
/// rastro na URL mora em header (a lição do caso php2shell). Cookie e
/// Authorization são redigidos no dump: segredo de sessão não vira linha de
/// log legível no painel.
pub struct Origem {
    pub xff: String,
    pub user_agent: String,
    pub headers: Option<String>,
}

impl Origem {
    /// Request sem origem identificada (chamadas locais de teste).
    pub fn local() -> Self {
        Self {
            xff: String::new(),
            user_agent: String::new(),
            headers: None,
        }
    }
}

/// Headers cujo VALOR nunca entra no dump (segredo de sessão/credencial).
const REDIGIDOS: [&str; 2] = ["cookie", "authorization"];

/// Teto do dump por request: header gigante não vira linha gigante no estado.
const DUMP_TETO: usize = 2048;

/// Origem a partir dos headers do request. `externo` decide o dump — a
/// classificação é da MESMA régua do WAF (`waf::fonte_externa`), nunca
/// reinventada aqui.
pub fn origem_de(headers: &HeaderMap, externo: bool) -> Origem {
    let pega = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .replace('"', "'")
            .to_owned()
    };
    Origem {
        xff: pega("x-forwarded-for"),
        user_agent: pega(header::USER_AGENT.as_str()),
        headers: externo.then(|| dump_headers(headers)),
    }
}

/// Dump `nome=valor; nome=valor` com segredo redigido e teto de tamanho.
/// Ordem dos headers é a ordem recebida — forense lê substring, não tabela.
fn dump_headers(headers: &HeaderMap) -> String {
    let mut out = String::new();
    for (name, value) in headers {
        let val = if REDIGIDOS.contains(&name.as_str()) {
            format!("<redigido:{}B>", value.len())
        } else {
            String::from_utf8_lossy(value.as_bytes()).into_owned()
        };
        let sep = if out.is_empty() { "" } else { "; " };
        let linha = format!("{sep}{name}={val}");
        if out.len() + linha.len() > DUMP_TETO {
            out.push_str(";…(cortado)");
            break;
        }
        out.push_str(&linha);
    }
    out
}

/// A linha canônica do access — ÚNICO lugar que sabe o formato. `access` e o
/// evento do WAF (escopo diferente, mesma forma) montam a partir dela.
pub(crate) fn linha_access(
    host: &str,
    method: &str,
    path: &str,
    status: u16,
    dur_ms: u128,
    origem: &Origem,
) -> String {
    let mut message = format!(
        "{method} {path} {status} {dur_ms}ms host={host} xff={}",
        origem.xff
    );
    if !origem.user_agent.is_empty() {
        message.push_str(&format!(" ua=\"{}\"", origem.user_agent));
    }
    if let Some(h) = &origem.headers {
        message.push_str(&format!(" hdrs[{h}]"));
    }
    message
}

/// Log de acesso (07) com a origem do request.
pub fn access(
    st: &State,
    host: &str,
    method: &str,
    path: &str,
    status: u16,
    dur_ms: u128,
    origem: &Origem,
) {
    event(
        st,
        "info",
        "access",
        &linha_access(host, method, path, status, dur_ms, origem),
    );
}

pub fn event(st: &State, level: &str, scope: &str, message: &str) {
    let r = st.with(|c| {
        c.execute(
            "INSERT INTO events(level, scope, message) VALUES (?1, ?2, ?3)",
            rusqlite::params![level, scope, message],
        )
    });
    if let Err(e) = r {
        tracing::error!("falha ao gravar evento no estado ({scope}): {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers_backend() -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(header::USER_AGENT, HeaderValue::from_static("curl/8.0"));
        h.insert("x-forwarded-for", HeaderValue::from_static("192.168.140.1"));
        h
    }

    #[test]
    fn origem_interna_nao_tem_dump_de_headers() {
        let o = origem_de(&headers_backend(), false);
        assert_eq!(o.xff, "192.168.140.1");
        assert_eq!(o.user_agent, "curl/8.0");
        assert!(o.headers.is_none(), "interno não carrega dump");
    }

    #[test]
    fn origem_externa_carrega_dump_com_redacao_de_segredo() {
        let mut h = headers_backend();
        h.insert(
            header::COOKIE,
            HeaderValue::from_static("laravel_session=segredo-total"),
        );
        h.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer tok-en-secreto"),
        );
        let o = origem_de(&h, true);
        let dump = o.headers.expect("externo tem dump");
        assert!(dump.contains("curl/8.0"), "UA entra: {dump}");
        assert!(
            dump.contains("cookie=<redigido:29B>"),
            "cookie redigido: {dump}"
        );
        assert!(
            dump.contains("authorization=<redigido:21B>"),
            "authorization redigido: {dump}"
        );
        assert!(
            !dump.contains("segredo-total"),
            "valor de cookie NUNCA: {dump}"
        );
        assert!(
            !dump.contains("tok-en-secreto"),
            "valor de token NUNCA: {dump}"
        );
    }

    #[test]
    fn dump_cabe_no_teto_e_marca_o_corte() {
        let mut h = HeaderMap::new();
        for i in 0..80 {
            let nome = format!("x-filler-{i}");
            h.insert(
                axum::http::HeaderName::from_bytes(nome.as_bytes()).unwrap(),
                HeaderValue::from_str(&"a".repeat(100)).unwrap(),
            );
        }
        let dump = origem_de(&h, true).headers.expect("dump");
        assert!(
            dump.len() <= DUMP_TETO + 16,
            "dump estourou o teto: {}",
            dump.len()
        );
        assert!(dump.contains("…(cortado)"), "corte declarado: {dump}");
    }

    #[test]
    fn linha_access_tem_a_forma_fixa_com_origem_e_dump() {
        let mut h = headers_backend();
        h.insert("cf-connecting-ip", HeaderValue::from_static("203.0.113.9"));
        let o = origem_de(&h, true);
        let linha = linha_access("vippers-d.bitello.xyz", "GET", "/a?b=c", 404, 7, &o);
        // A ordem DENTRO do dump segue a iteração do HeaderMap (não
        // especificada) — o formato fixo é o esqueleto, não a ordem interna.
        for pedaco in [
            "GET /a?b=c 404 7ms host=vippers-d.bitello.xyz ",
            "xff=192.168.140.1",
            "ua=\"curl/8.0\"",
            "hdrs[",
            "cf-connecting-ip=203.0.113.9",
            "x-forwarded-for=192.168.140.1",
        ] {
            assert!(linha.contains(pedaco), "linha sem `{pedaco}`: {linha}");
        }
    }
}
