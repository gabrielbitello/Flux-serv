//! Escrita de eventos no SQLite (07): log de acesso estruturado (host, método,
//! caminho, status, duração, origem — sem corpo) + trilha geral para o painel
//! (08). Falha ao gravar é logada em nível erro, nunca engolida.
//!
//! Broadcast (fatia 2 do painel API+SSE, 02/11): TODO evento gravado vira
//! `EventoNovo` no canal — é o que alimenta o SSE `/api/events/follow` do
//! painel novo. Canal global com buffer: sem ouvinte o `send` é no-op, os
//! eventos custam o INSERT de sempre.

use std::sync::LazyLock;

use tokio::sync::broadcast;

use crate::state::State;
use axum::http::{HeaderMap, header};

/// Um evento acabou de entrar na tabela — payload do stream do painel.
#[derive(Clone, Debug)]
pub struct EventoNovo {
    pub id: i64,
    pub level: String,
    pub scope: String,
    pub message: String,
    pub created_at: String,
    /// Host da request (access) — vazio nos eventos internos (schema v7).
    pub host: String,
    /// IP real do cliente (coluna `ip`, schema v6) — vazio nos internos. O
    /// painel usa pra marcar "é meu" (mesma origem do navegador aberto).
    pub ip: String,
}

static CANAL: LazyLock<broadcast::Sender<EventoNovo>> =
    LazyLock::new(|| broadcast::channel(1024).0);

/// Assina o stream de eventos novos. Assinar ANTES de consultar o banco
/// (backlog) minimiza janela de corrida; duplicata residual é inofensiva —
/// o consumidor dedupe por `id`.
pub(crate) fn assinar() -> broadcast::Receiver<EventoNovo> {
    CANAL.subscribe()
}

/// Origem do request (24/10, pedido do Gabriel — todo request de fora 100%
/// auditável): `xff` e `user_agent` crus em TODO access; `headers` é o dump
/// completo, e só existe para fonte EXTERNA — o gatilho de RCE que não deixa
/// rastro na URL mora em header (a lição do caso php2shell). Cookie e
/// Authorization são redigidos no dump: segredo de sessão não vira linha de
/// log legível no painel.
pub struct Origem {
    /// IP real do cliente (régua `waf::ip_real`) — coluna `ip` da tabela
    /// `events` (schema v6): é o que o vigia agrega por SQL.
    pub ip: String,
    pub xff: String,
    pub user_agent: String,
    pub headers: Option<String>,
}

impl Origem {
    /// Request sem origem identificada (chamadas locais de teste).
    pub fn local() -> Self {
        Self {
            ip: String::new(),
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
/// reinventada aqui. `ip` é o IP real já resolvido pelo chamador (a mesma
/// régua `waf::ip_real` — o events.rs não resolve nada).
pub fn origem_de(headers: &HeaderMap, externo: bool, ip: &str) -> Origem {
    let pega = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .replace('"', "'")
            .to_owned()
    };
    Origem {
        ip: ip.to_owned(),
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
        "{method} {path} {status} {dur_ms}ms host={host} ip={} xff={}",
        origem.ip, origem.xff
    );
    if !origem.user_agent.is_empty() {
        message.push_str(&format!(" ua=\"{}\"", origem.user_agent));
    }
    if let Some(h) = &origem.headers {
        message.push_str(&format!(" hdrs[{h}]"));
    }
    message
}

/// Log de acesso (07) com a origem do request — grava na coluna `ip` (v6),
/// que é o que o vigia agrega.
pub fn access(
    st: &State,
    host: &str,
    method: &str,
    path: &str,
    status: u16,
    dur_ms: u128,
    origem: &Origem,
) {
    gravar(
        st,
        "info",
        "access",
        &linha_access(host, method, path, status, dur_ms, origem),
        &origem.ip,
        host,
    );
}

pub fn event(st: &State, level: &str, scope: &str, message: &str) {
    event_com_ip(st, level, scope, message, "");
}

pub fn event_com_ip(st: &State, level: &str, scope: &str, message: &str, ip: &str) {
    gravar(st, level, scope, message, ip, "");
}

/// INSERT de verdade (fatia 9a): `host` da request nos access (vazio nos
/// internos) — é a coluna do filtro projeto-origem/dev-prod do painel.
/// INSERT + leitura da linha numa única passagem no mesmo lock: o
/// `created_at` do broadcast é a VERDADE do banco (default do schema),
/// não relógio do Rust — e o id volta de graça pro canal.
fn gravar(st: &State, level: &str, scope: &str, message: &str, ip: &str, host: &str) {
    let gravado = st.with(|c| {
        c.execute(
            "INSERT INTO events(level, scope, message, ip, host) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![level, scope, message, ip, host],
        )?;
        c.query_row(
            "SELECT id, level, scope, message, created_at, host, ip FROM events WHERE id = ?1",
            rusqlite::params![c.last_insert_rowid()],
            |r| {
                Ok(EventoNovo {
                    id: r.get(0)?,
                    level: r.get(1)?,
                    scope: r.get(2)?,
                    message: r.get(3)?,
                    created_at: r.get(4)?,
                    host: r.get(5)?,
                    ip: r.get(6)?,
                })
            },
        )
    });
    match gravado {
        Ok(ev) => {
            // Sem ouvinte = Err e nada acontece: o canal não tem custo.
            let _ = CANAL.send(ev);
        }
        Err(e) => tracing::error!("falha ao gravar evento no estado ({scope}): {e}"),
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
        let o = origem_de(&headers_backend(), false, "");
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
        let o = origem_de(&h, true, "");
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
        let dump = origem_de(&h, true, "").headers.expect("dump");
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
        let o = origem_de(&h, true, "");
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

#[cfg(test)]
mod broadcast_tests {
    use super::*;
    use std::time::Duration;

    /// Fatia 2: TODO evento gravado vira broadcast com a VERDADE do banco
    /// (id do last_insert_rowid + created_at do default do schema).
    #[tokio::test]
    async fn evento_gravado_vira_broadcast_com_id_e_verdade_do_banco() {
        let st = crate::state::State::in_memory().unwrap();
        let mut rx = assinar();
        event(&st, "warn", "teste-bc", "mensagem-broadcast-1");
        let ev = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("evento chega no canal")
            .expect("canal aberto");
        assert!(ev.id > 0, "id do INSERT: {:?}", ev);
        assert_eq!(ev.scope, "teste-bc");
        assert_eq!(ev.message, "mensagem-broadcast-1");
        assert_eq!(ev.level, "warn");
        assert!(!ev.created_at.is_empty(), "created_at vem do banco");
    }

    /// Sem ouvinte não explode nem custa: send devolve Err e o evento segue
    /// gravado (o caso de TODO request de access quando ninguém escuta).
    #[tokio::test]
    async fn sem_ouvinte_evento_segue_gravado() {
        let st = crate::state::State::in_memory().unwrap();
        event(&st, "info", "teste-bc", "ninguem-escuta");
        let total: i64 = st
            .with(|c| c.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0)))
            .unwrap();
        assert_eq!(total, 1);
    }
}
