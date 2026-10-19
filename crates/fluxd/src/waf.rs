//! WAF local (R32; spec 16, camada 2 — SQLi + path scan na borda). Layer
//! tower no padrão do `edge.rs`: self-contained (não lê `AppState`, nem
//! SQLite, nem Docker). O integrador monta DEPOIS da `EdgeLayer` —
//! `Router.layer(EdgeLayer::new(..)).layer(WafLayer::new())` — então o
//! request chega aqui já admitido pela borda (413/429 primeiro, WAF depois).
//!
//! Mecanismo:
//!
//! 1. **Alvo** — path + query string do `Uri` (v1 NÃO lê corpo: o streaming
//!    passa intocado; inspecionar corpo até 64 KB é evolução da spec 16). O
//!    alvo é decodificado %XX UMA vez (tolerante: `%` inválido/truncado fica
//!    literal, nunca pânico) e, na query, `+` vira espaço (form encoding) —
//!    `union+select` não escapa. Match é substring case-insensitive sobre o
//!    alvo pronto (custo O(n) por request, aposta da spec 16).
//! 2. **Padrões** — duas famílias, consts documentadas abaixo: SQLI
//!    (subconjunto OWASP da spec 16) e path scan (`../`, `/etc/passwd`,
//!    `/.env`, `/.git`, WordPress, phpMyAdmin, credenciais). `%2e%2e` segue
//!    na lista DEPOIS do decode de propósito: ele só casa com entrada
//!    DUPLAMENTE codificada (`%252e%252e` → `%2e%2e`) — travessia simples já
//!    vira `../` no decode. `..` solto NÃO é padrão (falso positivo trivial:
//!    `?faixa=1..5`). O comentário SQL `--` com aspas (`admin'--`) é regra
//!    combinada — aspas E `--` no mesmo alvo; substring cega de `--` sozinho
//!    derrubaria slug legítimo (`arquivo--especial`).
//! 3. **IP real** — primeiro valor de `x-forwarded-for` (o Caddy da própria
//!    VM seta — confiável, 09; o router só aceita conexão dele, bind
//!    127.0.0.1), senão `ConnectInfo` (mesmo fallback do `IpDoCliente` do
//!    painel, panel.rs). XFF sem IP parseável cai para o `ConnectInfo` —
//!    fallback logado em debug, nunca silencioso total.
//! 4. **Allowlist de rede** (decisão do Gabriel) — `192.168.140.0/24`
//!    inteira, `127.0.0.1` e `::1`. IP da allowlist com match NUNCA leva
//!    `403`, mas o match fica registrado como evento `waf` nível warn. Sem
//!    IP identificável não há como provar allowlist: bloqueia (fail-closed;
//!    no `serve` real `ConnectInfo` sempre existe, lib.rs). IP de fora com
//!    match → `403`: JSON `{"error":"blocked","reason":"waf"}` em rota de API
//!    (mesma heurística `decision::wants_json` da borda e do dispatch — 12)
//!    ou HTML da família Flux (15). O HTML é página própria SEM
//!    `meta refresh`, como as de erro do `edge.rs`: auto-reload num 403
//!    re-dispararia o request bloqueado; as fns públicas de `pages.rs` não
//!    cobrem 403 (são wait/failed/404/503), então a página vive aqui — mesmo
//!    desenho do `edge.rs` para 429/413. Corpo genérico de propósito: não
//!    ecoa path nem padrão casado (não ensina o scanner).
//!
//! ENFORCE direto: a spec 16 pede shadow mode antes de bloquear; nesta v1
//! vale a decisão explícita do Gabriel (workflow 14/10/2026) de entrar
//! bloqueando, com a LAN de fora do alcance. Sem bad bots, brute-force e
//! reputação — camadas seguintes da spec 16, frentes próprias.
//!
//! Eventos: bloqueio e skip-de-LAN viram log `tracing` com `target: "waf"` e
//! campos (ip, path SEM query, padrão) — path sem query de propósito (spec
//! 16: o log nunca carrega o corpo; a query carrega o payload). Gravar na
//! tabela `events` do SQLite (07) é passo do integrador: a layer é
//! self-contained e não tem `State`.

use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::pin::Pin;
use std::task::{Context, Poll};

use axum::body::Body;
use axum::extract::{ConnectInfo, Request};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::Response;
use tower::{Layer, Service};

use crate::pages::CSS;
use crate::decision;

// ------------------------------------------------- padrões SQLI (spec 16)

/// SQLi — `union select` (injeção clássica de colunas; `%20`/`+` viram
/// espaço no alvo, então `union+select` e `union%20select` casam).
pub const SQLI_UNION_SELECT: &str = "union select";
/// SQLi — tautologia `or 1=1` (bypass de WHERE).
pub const SQLI_OR_1_1: &str = "or 1=1";
/// SQLi — `sleep(` (blind SQLi por tempo, MySQL/Postgres).
pub const SQLI_SLEEP: &str = "sleep(";
/// SQLi — `benchmark(` (blind SQLi por tempo, MySQL).
pub const SQLI_BENCHMARK: &str = "benchmark(";
/// SQLi — `information_schema` (reconhecimento de catálogo).
pub const SQLI_INFORMATION_SCHEMA: &str = "information_schema";
/// SQLi — `load_file(` (leitura de arquivo pelo MySQL).
pub const SQLI_LOAD_FILE: &str = "load_file(";
/// SQLi — `drop table` (destruição de esquema).
pub const SQLI_DROP_TABLE: &str = "drop table";
/// SQLi — comentário `--` com aspas (`admin'--`): regra COMBINADA, não é
/// substring — o alvo precisa ter aspas E `--` (ver
/// `casa_comentario_com_aspas`). Este valor é o rótulo que nomeia o evento.
pub const SQLI_COMENTARIO_COM_ASPAS: &str = "-- com aspas";

/// Família SQLI, na ordem de casamento (a 1ª que casa nomeia o evento).
pub const SQLI_PADROES: &[&str] = &[
    SQLI_UNION_SELECT,
    SQLI_OR_1_1,
    SQLI_SLEEP,
    SQLI_BENCHMARK,
    SQLI_INFORMATION_SCHEMA,
    SQLI_LOAD_FILE,
    SQLI_DROP_TABLE,
];

// --------------------------------------------- padrões de path scan (16)

/// Path scan — travessia `../` (após o decode, pega `%2e%2e%2f` simples).
pub const PATH_TRAVERSAL: &str = "../";
/// Path scan — travessia DUPLAMENTE codificada: `%252e%252e` vira `%2e%2e`
/// depois de UM decode — é o caso que esta literal pega.
pub const PATH_TRAVERSAL_DUPLA: &str = "%2e%2e";
/// Path scan — leitura de `/etc/passwd` (Unix).
pub const PATH_ETC_PASSWD: &str = "/etc/passwd";
/// Path scan — vazamento de `/.env` (segredos de app; prefixo pega variantes
/// como `/.env.backup` — deliberado, spec 16 manda 403 "na hora").
pub const PATH_DOT_ENV: &str = "/.env";
/// Path scan — exposição de repositório `/.git`.
pub const PATH_DOT_GIT: &str = "/.git";
/// Path scan — sonda de WordPress (`wp-admin`; sem `/` para pegar
/// `/wp-admin` e subcaminhos).
pub const PATH_WP_ADMIN: &str = "wp-admin";
/// Path scan — sonda de WordPress (`wp-login`).
pub const PATH_WP_LOGIN: &str = "wp-login";
/// Path scan — sonda de phpMyAdmin.
pub const PATH_PHPMYADMIN: &str = "phpmyadmin";
/// Path scan — credenciais AWS expostas (`.aws/credentials`).
pub const PATH_AWS_CREDENTIALS: &str = ".aws/credentials";
/// Path scan — chave privada exposta (`id_rsa`).
pub const PATH_ID_RSA: &str = "id_rsa";
/// Path scan — diretório SSH exposto (`.ssh`).
pub const PATH_DOT_SSH: &str = ".ssh";

/// Família path scan, na ordem de casamento.
pub const PATH_PADROES: &[&str] = &[
    PATH_TRAVERSAL,
    PATH_TRAVERSAL_DUPLA,
    PATH_ETC_PASSWD,
    PATH_DOT_ENV,
    PATH_DOT_GIT,
    PATH_WP_ADMIN,
    PATH_WP_LOGIN,
    PATH_PHPMYADMIN,
    PATH_AWS_CREDENTIALS,
    PATH_ID_RSA,
    PATH_DOT_SSH,
];

// ------------------------------------------------------- allowlist (16)

/// Prefixo da rede LAN da VM (decisão do Gabriel): `192.168.140.0/24` — a
/// spec 16 já mandava nunca bloquear o IP do host (`192.168.140.1`); a
/// decisão estende o perdão à rede inteira.
pub const LAN_PREFIXO: [u8; 3] = [192, 168, 140];

/// IP da allowlist de rede? `None` = sem IP identificável — fail-closed
/// (não há como provar allowlist, então bloqueia se o padrão casar).
pub fn na_allowlist(ip: Option<IpAddr>) -> bool {
    match ip {
        Some(IpAddr::V4(v4)) => {
            let o = v4.octets();
            o[0..3] == LAN_PREFIXO || v4 == Ipv4Addr::LOCALHOST
        }
        Some(IpAddr::V6(v6)) => v6 == Ipv6Addr::LOCALHOST,
        None => false,
    }
}

/// IP real do cliente, por precedência:
/// 1. `cf-connecting-ip` — chegou PELO TÚNEL (cloudflared seta com o IP real
///    do visitante na internet e SOBRESCREVE o do cliente; não dá para
///    forjar pela cadeia). Sem isso, request via túnel chegaria com o IP do
///    cloudflared (LAN) e a allowlist desligaria o WAF para a internet
///    inteira — o furo que a prova ao vivo achou (14/10).
/// 2. primeiro valor de `x-forwarded-for` (o Caddy da própria VM seta no
///    acesso LAN direto — confiável, 09).
/// 3. `ConnectInfo`. XFF sem IP parseável cai para o de baixo com log em
///    debug — sem fallback silencioso.
pub fn ip_real(headers: &axum::http::HeaderMap, connect: Option<IpAddr>) -> Option<IpAddr> {
    if let Some(v) = headers.get("cf-connecting-ip")
        && let Ok(s) = v.to_str()
        && let Ok(ip) = s.trim().parse::<IpAddr>()
    {
        return Some(ip);
    }
    if let Some(v) = headers.get("x-forwarded-for")
        && let Ok(s) = v.to_str()
    {
        let primeiro = s.split(',').next().unwrap_or(s).trim();
        match primeiro.parse::<IpAddr>() {
            Ok(ip) => return Some(ip),
            Err(e) => tracing::debug!(
                target: "waf",
                valor = %s,
                erro = %e,
                "x-forwarded-for sem IP parseável — caindo para o ConnectInfo"
            ),
        }
    }
    connect
}

// ---------------------------------------------------- detecção (spec 16)

/// Alvo inspecionado: path + query decodificados %XX UMA vez, em minúsculas.
/// Na query, `+` vira espaço (form encoding) — `union+select` não escapa;
/// no path `+` permanece literal (legítimo em nome de arquivo).
pub fn alvo_inspecionado(path: &str, query: Option<&str>) -> String {
    let mut alvo = percent_decode(path).to_ascii_lowercase();
    if let Some(q) = query {
        let q = percent_decode(q).replace('+', " ");
        alvo.push('?');
        alvo.push_str(&q.to_ascii_lowercase());
    }
    alvo
}

/// Decodifica %XX UMA vez, tolerante: `%` sem dois dígitos hexadecimais fica
/// literal (nunca pânico; bytes inválidos em UTF-8 viram replacement char).
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(h), Some(l)) = (hex_val(b[i + 1]), hex_val(b[i + 2]))
        {
            out.push(h * 16 + l);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Comentário SQL `--` com aspas: o alvo precisa ter aspas E `--` — assinatura
/// de `admin'--` / `' OR 1=1 --`. Substring cega de `--` derrubaria slug
/// legítimo (`arquivo--especial`); aspas sozinhas também não (o'brien).
fn casa_comentario_com_aspas(alvo: &str) -> bool {
    alvo.contains("--") && (alvo.contains('\'') || alvo.contains('"'))
}

/// Padrão que casou no alvo (já minúsculas — ver `alvo_inspecionado`), na
/// ordem das famílias; o rótulo nomeia o evento do WAF. `None` = limpo.
pub fn detecta(alvo: &str) -> Option<&'static str> {
    for &p in SQLI_PADROES {
        if alvo.contains(p) {
            return Some(p);
        }
    }
    for &p in PATH_PADROES {
        if alvo.contains(p) {
            return Some(p);
        }
    }
    if casa_comentario_com_aspas(alvo) {
        return Some(SQLI_COMENTARIO_COM_ASPAS);
    }
    None
}

// ------------------------------------------------- layer + service tower

/// A layer. Unitária e sem estado (a detecção é pura) — `WafLayer::new()`.
#[derive(Clone, Default)]
pub struct WafLayer;

impl WafLayer {
    pub fn new() -> Self {
        Self
    }
}

impl<S> Layer<S> for WafLayer {
    type Service = WafService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        WafService { inner }
    }
}

/// O service do WAF: inspeção na frente do service de dentro. `Clone`
/// obrigatório pelo modelo de service do axum (mesmo desenho do `edge.rs`).
#[derive(Clone)]
pub struct WafService<S> {
    inner: S,
}

impl<S> Service<Request> for WafService<S>
where
    S: Service<Request, Response = Response> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = Response;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Response, S::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request) -> Self::Future {
        let mut inner = self.inner.clone();
        Box::pin(async move {
            let (parts, body) = req.into_parts();
            // Alvo = path + query decodificados UMA vez. O corpo NÃO é lido —
            // streaming segue intocado (v1).
            let alvo = alvo_inspecionado(parts.uri.path(), parts.uri.query());
            let Some(padrao) = detecta(&alvo) else {
                let req = Request::from_parts(parts, body);
                return inner.call(req).await;
            };

            let connect = parts
                .extensions
                .get::<ConnectInfo<std::net::SocketAddr>>()
                .map(|ConnectInfo(a)| a.ip());
            let ip = ip_real(&parts.headers, connect);
            // Log leva path SEM query (spec 16: o log nunca carrega o payload).
            let path = parts.uri.path().to_owned();
            // Fonte do IP: na VM os binds são 127.0.0.1 e TODO request chega
            // pelo Caddy — ConnectInfo é SEMPRE loopback, então sem
            // X-Forwarded-For o "externo" entraria como allowlist e o
            // bloqueio NÃO ocorreria. A diferença precisa ser visível no log.
            let com_xff = parts.headers.get("x-forwarded-for").is_some();
            let fonte_ip = if com_xff {
                "xff"
            } else if connect.is_some() {
                "connectinfo"
            } else {
                "nenhum"
            };

            if na_allowlist(ip) {
                // Decisão do Gabriel: LAN NUNCA leva 403 — mas o match fica
                // registrado como evento waf nível warn. Allowlist vinda do
                // ConnectInfo (sem XFF) em produção é o sinal clássico de
                // Caddy sem X-Forwarded-For: warn com a instrução, nunca
                // desligar o WAF em silêncio.
                if fonte_ip == "connectinfo" {
                    tracing::warn!(
                        target: "waf",
                        ip = ?ip,
                        path = %path,
                        padrao,
                        "waf: padrão casou e o allowlist veio do ConnectInfo SEM \
                         X-Forwarded-For — na VM (bind 127.0.0.1) todo request \
                         chega pelo Caddy; XFF ausente faz o externo entrar como \
                         loopback e o bloqueio NÃO ocorre — confira o \
                         X-Forwarded-For do Caddy"
                    );
                } else {
                    tracing::warn!(
                        target: "waf",
                        ip = ?ip,
                        path = %path,
                        padrao,
                        "waf: padrão casou em IP da allowlist — passa sem bloqueio"
                    );
                }
                let req = Request::from_parts(parts, body);
                return inner.call(req).await;
            }

            tracing::warn!(
                target: "waf",
                ip = ?ip,
                path = %path,
                padrao,
                "waf: 403 — request bloqueado"
            );
            let wants_json = decision::wants_json(parts.headers.get(header::ACCEPT), &path);
            Ok(bloqueado(wants_json))
        })
    }
}

// ---------------------------------------------------------- respostas 403

/// Corpo JSON do bloqueio, forma fixa da frente (string manual como no
/// `edge.rs` — sem entrada do usuário, não há o que escapar).
const JSON_BLOQUEADO: &str = r#"{"error":"blocked","reason":"waf"}"#;

/// `403` do WAF (spec 16): JSON fixo em rota de API, HTML da família Flux
/// para browser — SEM `meta refresh` (mesmo racional do `edge.rs`: auto-
/// reload re-dispararia o request bloqueado). `no-store` como nas páginas
/// da família.
pub fn bloqueado(wants_json: bool) -> Response {
    if wants_json {
        let mut r = Response::new(Body::from(JSON_BLOQUEADO));
        *r.status_mut() = StatusCode::FORBIDDEN;
        r.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        r.headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        return r;
    }
    let mut r = Response::new(Body::from(html_bloqueado_page()));
    *r.status_mut() = StatusCode::FORBIDDEN;
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

/// Mesma família visual de `pages.rs`/`edge.rs` (15). Corpo genérico de
/// propósito: não ecoa path nem padrão casado (não ensina o scanner).
fn html_bloqueado_page() -> String {
    format!(
        r#"<!doctype html>
<html lang="pt-br">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Flux — acesso bloqueado</title>
<style>{CSS}</style>
</head>
<body>
<main>
<h1>Acesso bloqueado</h1>
<p>A requisição foi recusada pelo sistema de proteção da plataforma.</p>
<footer>FLUX</footer>
</main>
</body>
</html>"#
    )
}

/// CSS da família Flux (15) — cópia local do padrão, como o `edge.rs` já
/// faz para suas páginas de erro (as consts das páginas são privadas nos
/// módulos de origem).
// L4.2: cópia local REMOVIDA — dono único em pages.rs.

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn ip(s: &str) -> IpAddr {
        s.parse().expect("ip de teste válido")
    }

    /// Recorta path/query de uma URI de teste e roda a detecção pura.
    fn casa_uri(uri: &str) -> Option<&'static str> {
        let (p, q) = match uri.split_once('?') {
            Some((p, q)) => (p, Some(q)),
            None => (uri, None),
        };
        detecta(&alvo_inspecionado(p, q))
    }

    // ------------------------------------------------------ detecção pura

    #[test]
    fn sqli_cada_familia_casa() {
        let casos = [
            ("/?id=1%20union%20select%20password", SQLI_UNION_SELECT),
            ("/?id=1%27+or+1%3D1--", SQLI_OR_1_1),
            ("/?t=1;sleep(5)", SQLI_SLEEP),
            ("/?t=benchmark(500000,md5(1))", SQLI_BENCHMARK),
            (
                "/?s=1+from+information_schema.tables",
                SQLI_INFORMATION_SCHEMA,
            ),
            ("/?u=1+and+load_file(0x2F)", SQLI_LOAD_FILE),
            ("/?x=1;drop+table+users", SQLI_DROP_TABLE),
            ("/?user=admin%27--", SQLI_COMENTARIO_COM_ASPAS),
        ];
        for (uri, esperado) in casos {
            assert_eq!(casa_uri(uri), Some(esperado), "uri: {uri}");
        }
        // Case-insensitive + decode %XX: maiúsculas codificadas casam igual.
        assert_eq!(
            casa_uri("/?id=1%20UNION%20SELECT%20x"),
            Some(SQLI_UNION_SELECT)
        );
        // `+` como espaço na query: a forma clássica de scanner não escapa.
        assert!(alvo_inspecionado("/x", Some("1+union+select+senha")).contains("union select"));
    }

    #[test]
    fn path_scan_cada_familia_casa() {
        let casos = [
            ("/download?file=..%2F..%2Fetc%2Fpasswd", PATH_TRAVERSAL),
            ("/%252e%252e/config", PATH_TRAVERSAL_DUPLA),
            ("/?p=/etc/passwd", PATH_ETC_PASSWD),
            ("/.env", PATH_DOT_ENV),
            ("/.git/config", PATH_DOT_GIT),
            ("/wp-admin/setup.php", PATH_WP_ADMIN),
            ("/wp-login.php", PATH_WP_LOGIN),
            ("/phpmyadmin/index.php", PATH_PHPMYADMIN),
            ("/.aws/credentials", PATH_AWS_CREDENTIALS),
            ("/backup/id_rsa", PATH_ID_RSA),
            ("/.ssh/authorized_keys", PATH_DOT_SSH),
        ];
        for (uri, esperado) in casos {
            assert_eq!(casa_uri(uri), Some(esperado), "uri: {uri}");
        }
        // Case-insensitive no path também.
        assert_eq!(casa_uri("/WP-ADMIN/setup"), Some(PATH_WP_ADMIN));
        assert_eq!(casa_uri("/.ENV"), Some(PATH_DOT_ENV));
    }

    #[test]
    fn limpo_passa_e_guards_de_falso_positivo_ficam_de_pe() {
        let limpos = [
            "/produtos?page=2&q=caf%C3%A9",
            "/blog/o%27brien",    // aspas sem `--`
            "/arquivo--especial", // `--` sem aspas
            "/?faixa=1..5",       // `..` solto NÃO é padrão (documentado)
        ];
        for uri in limpos {
            assert_eq!(casa_uri(uri), None, "limpo não pode casar: {uri}");
        }
        // Prefixo do .env pega variantes — deliberado (spec 16: scanner leva
        // 403 na hora).
        assert_eq!(casa_uri("/.env.backup"), Some(PATH_DOT_ENV));
    }

    #[test]
    fn decode_de_percent_e_mais_na_query() {
        // %XX minúsculo/maiúsculo e multibyte UTF-8.
        assert_eq!(percent_decode("%2e%2e%2f"), "../");
        assert_eq!(percent_decode("%2E%2E%2F"), "../");
        assert_eq!(percent_decode("%C3%A9"), "é");
        // % inválido/truncado fica literal — tolerante, nunca pânico.
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz"), "%zz");
        assert_eq!(percent_decode("%2"), "%2");
        // `+` vira espaço SÓ na query; no path permanece literal.
        assert!(alvo_inspecionado("/x", Some("a+union+select")).contains("union select"));
        assert!(!alvo_inspecionado("/a+b.html", None).contains(' '));
    }

    // ----------------------------------------------------- ip + allowlist

    #[test]
    fn ip_real_xff_primeiro_valor_senao_connectinfo() {
        let mut h = axum::http::HeaderMap::new();
        h.insert(
            "x-forwarded-for",
            HeaderValue::from_static("192.168.140.5, 10.0.0.1"),
        );
        assert_eq!(
            ip_real(&h, Some(ip("203.0.113.1"))),
            Some(ip("192.168.140.5")),
            "primeiro valor do XFF vence"
        );

        let mut h2 = axum::http::HeaderMap::new();
        h2.insert("x-forwarded-for", HeaderValue::from_static(" 203.0.113.1 "));
        assert_eq!(
            ip_real(&h2, None),
            Some(ip("203.0.113.1")),
            "trim antes do parse"
        );

        // XFF lixo → ConnectInfo; sem nada dos dois → None (fail-closed).
        let mut h3 = axum::http::HeaderMap::new();
        h3.insert("x-forwarded-for", HeaderValue::from_static("lixo"));
        assert_eq!(
            ip_real(&h3, Some(ip("198.51.100.7"))),
            Some(ip("198.51.100.7"))
        );
        assert_eq!(ip_real(&axum::http::HeaderMap::new(), None), None);
    }

    #[test]
    fn allowlist_lan_localhost_e_nada_mais() {
        for ok in [
            "192.168.140.0",
            "192.168.140.1",
            "192.168.140.77",
            "192.168.140.255",
            "127.0.0.1",
            "::1",
        ] {
            assert!(na_allowlist(Some(ip(ok))), "{ok} é allowlist");
        }
        for no in [
            "192.168.141.1",
            "192.168.14.5",
            "10.0.0.1",
            "8.8.8.8",
            "127.0.0.2",
            "::2",
            "2001:db8::1",
        ] {
            assert!(!na_allowlist(Some(ip(no))), "{no} NÃO é allowlist");
        }
        assert!(!na_allowlist(None), "sem IP: fail-closed");
    }

    // ------------------------------------------------- layer via oneshot

    /// Inner de teste: conta chamadas e ECOA o corpo — prova que o WAF não
    /// toca no stream.
    #[derive(Clone)]
    struct Echo {
        hits: Arc<AtomicUsize>,
    }

    impl Service<Request> for Echo {
        type Response = Response;
        type Error = std::convert::Infallible;
        type Future = std::future::Ready<Result<Response, Self::Error>>;

        fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, req: Request) -> Self::Future {
            self.hits.fetch_add(1, Ordering::SeqCst);
            std::future::ready(Ok(Response::new(req.into_body())))
        }
    }

    fn waf_svc(hits: Arc<AtomicUsize>) -> WafService<Echo> {
        WafLayer::new().layer(Echo { hits })
    }

    fn req(
        uri: &str,
        xff: Option<&str>,
        accept: Option<&str>,
        connect: Option<SocketAddr>,
    ) -> Request {
        let mut b = Request::builder().uri(uri);
        if let Some(x) = xff {
            b = b.header("x-forwarded-for", x);
        }
        if let Some(a) = accept {
            b = b.header(header::ACCEPT, a);
        }
        let mut r = b.body(Body::from("corpo-teste")).unwrap();
        if let Some(sa) = connect {
            r.extensions_mut().insert(ConnectInfo(sa));
        }
        r
    }

    async fn corpo(r: Response) -> String {
        String::from_utf8(
            r.into_body()
                .collect()
                .await
                .expect("body coletável")
                .to_bytes()
                .to_vec(),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn request_limpo_passa_ate_o_inner() {
        let hits = Arc::new(AtomicUsize::new(0));
        let svc = waf_svc(Arc::clone(&hits));
        let r = svc
            .oneshot(req("/produtos?page=2", Some("203.0.113.9"), None, None))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    /// Allowlist via service (decisão do Gabriel): LAN com match passa SEMPRE
    /// — por XFF (192.168.140.0/24), loopback e ConnectInfo do host da VM
    /// (192.168.140.1, spec 16). O registro do match é o `tracing::warn!` no
    /// branch da allowlist — por construção no código; não há subscriber de
    /// teste nas deps para afirmar a emissão.
    #[tokio::test]
    async fn lan_com_match_passa_e_externo_toma_403_antes_do_inner() {
        let hits = Arc::new(AtomicUsize::new(0));

        for xff in ["192.168.140.77", "127.0.0.1", "::1"] {
            let svc = waf_svc(Arc::clone(&hits));
            let r = svc
                .oneshot(req("/.env", Some(xff), None, None))
                .await
                .unwrap();
            assert_eq!(r.status(), StatusCode::OK, "allowlist {xff} tem que passar");
        }

        // IP do host da VM via ConnectInfo (sem XFF) — dentro do /24.
        let svc = waf_svc(Arc::clone(&hits));
        let r = svc
            .oneshot(req(
                "/.env",
                None,
                None,
                Some("192.168.140.1:50555".parse().unwrap()),
            ))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);

        // Match na query com IP da LAN também passa.
        let svc = waf_svc(Arc::clone(&hits));
        let r = svc
            .oneshot(req(
                "/?id=1%20union%20select%20x",
                Some("192.168.140.10"),
                None,
                None,
            ))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);

        // Externo: 403 e o inner NÃO é chamado.
        let antes = hits.load(Ordering::SeqCst);
        let svc = waf_svc(Arc::clone(&hits));
        let r = svc
            .oneshot(req("/.env", Some("203.0.113.9"), None, None))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            hits.load(Ordering::SeqCst),
            antes,
            "403 tem que sair ANTES de encaminhar"
        );

        // Sem XFF, o ConnectInfo decide: externo bloqueado.
        let svc = waf_svc(Arc::clone(&hits));
        let r = svc
            .oneshot(req(
                "/.env",
                None,
                None,
                Some("203.0.113.5:1234".parse().unwrap()),
            ))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::FORBIDDEN);

        // XFF lixo cai para o ConnectInfo — externo bloqueado, LAN passa.
        let svc = waf_svc(Arc::clone(&hits));
        let r = svc
            .oneshot(req(
                "/.env",
                Some("lixo, 10.0.0.1"),
                None,
                Some("198.51.100.7:80".parse().unwrap()),
            ))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::FORBIDDEN);
        let svc = waf_svc(Arc::clone(&hits));
        let r = svc
            .oneshot(req(
                "/.env",
                Some("lixo"),
                None,
                Some("192.168.140.9:80".parse().unwrap()),
            ))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn bloqueio_json_em_api_e_html_familia_para_browser() {
        // API (Accept json): JSON exato da frente + no-store.
        let hits = Arc::new(AtomicUsize::new(0));
        let svc = waf_svc(Arc::clone(&hits));
        let r = svc
            .oneshot(req(
                "/x?id=1%20union%20select%20a",
                Some("203.0.113.9"),
                Some("application/json"),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            r.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/json"
        );
        assert_eq!(r.headers().get(header::CACHE_CONTROL).unwrap(), "no-store");
        assert_eq!(corpo(r).await, r#"{"error":"blocked","reason":"waf"}"#);

        // Rota /api manda MESMO com Accept html (rota de API nunca recebe
        // HTML, 12 — mesma heurística da borda).
        let svc = waf_svc(Arc::clone(&hits));
        let r = svc
            .oneshot(req(
                "/api/../.env",
                Some("203.0.113.9"),
                Some("text/html"),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(corpo(r).await, r#"{"error":"blocked","reason":"waf"}"#);

        // Browser: HTML da família Flux, sem meta refresh, sem ecoar o que casou.
        let svc = waf_svc(Arc::clone(&hits));
        let r = svc
            .oneshot(req(
                "/.env",
                Some("203.0.113.9"),
                Some("text/html,application/xhtml+xml"),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            r.headers().get(header::CONTENT_TYPE).unwrap(),
            "text/html; charset=utf-8"
        );
        assert_eq!(r.headers().get(header::CACHE_CONTROL).unwrap(), "no-store");
        let body = corpo(r).await;
        assert!(
            !body.contains("http-equiv=\"refresh\""),
            "auto-reload re-dispararia o bloqueio: {body}"
        );
        assert!(
            !body.contains(".env") && !body.contains("union select"),
            "página não ensina o que casou: {body}"
        );
    }

    #[tokio::test]
    async fn corpo_passa_intocado_e_v1_nao_inspeciona_body() {
        let hits = Arc::new(AtomicUsize::new(0));
        let svc = waf_svc(Arc::clone(&hits));
        // Corpo com payload SQLi e alvo limpo: v1 NÃO lê corpo — passa.
        let mut r0 = Request::builder()
            .uri("/upload")
            .body(Body::from("union select password from users"))
            .unwrap();
        r0.extensions_mut().insert(ConnectInfo(
            "203.0.113.9:1234".parse::<SocketAddr>().unwrap(),
        ));
        let r = svc.oneshot(r0).await.unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(
            corpo(r).await,
            "union select password from users",
            "corpo chega intocado (streaming intocado)"
        );
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }
}

#[cfg(test)]
mod precedencia_ip {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().expect("ip de teste válido")
    }

    #[test]
    fn cf_connecting_ip_vence_tudo_quando_parseia() {
        let mut h = axum::http::HeaderMap::new();
        h.insert("cf-connecting-ip", "203.0.113.7".parse().unwrap());
        h.insert("x-forwarded-for", "192.168.140.1".parse().unwrap());
        assert_eq!(
            ip_real(&h, Some(ip("127.0.0.1"))),
            Some(ip("203.0.113.7")),
            "túnel: IP real do visitante, não o do cloudflared"
        );
    }

    #[test]
    fn sem_cf_usa_xff_lan() {
        let mut h = axum::http::HeaderMap::new();
        h.insert("x-forwarded-for", "192.168.140.1".parse().unwrap());
        assert_eq!(ip_real(&h, None), Some(ip("192.168.140.1")));
    }

    #[test]
    fn cf_lixo_cai_para_xff() {
        let mut h = axum::http::HeaderMap::new();
        h.insert("cf-connecting-ip", "não-sou-ip".parse().unwrap());
        h.insert("x-forwarded-for", "10.0.0.9".parse().unwrap());
        assert_eq!(ip_real(&h, None), Some(ip("10.0.0.9")));
    }
}
