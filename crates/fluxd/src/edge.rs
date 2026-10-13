//! Borda do router — rate limit e teto de body (R30; 07 "Borda do router";
//! matriz da 03, linhas de `429` e `413`). Cloudflare segura o ataque
//! volumétrico; o que chega na VM é abuso fino (crawler mal-educado, script
//! em loop) e corpo gigante — tudo isso morre AQUI, antes da decisão (a 03
//! manda camadas de borda entrarem como layers tower ANTES do dispatch).
//!
//! Mecanismo, por camada de admissão, na ordem em que rodam:
//!
//! 1. **Teto de body** — `Content-Length` acima de `max_body_bytes` (default
//!    100 MB do config global, 09) responde `413` na hora, sem ler o corpo.
//!    Corpo sem `Content-Length` (chunked) não é mensurável por header: o
//!    stream é embrulhado num limitador e ABORTA ao cruzar o teto (07:
//!    "stream aborta cedo — upload de 10 GB não enchendo RAM da VM").
//! 2. **Rate limit por Host** — token bucket em memória com recarga contínua:
//!    300 req/min com burst 100 (07). Estourou → `429` + `Retry-After` do
//!    segundo em que o próximo token recarrega. Override por projeto
//!    (`[limits] rate`, 07) entra no futuro trocando o bucket consultado pelo
//!    projeto resolvido do host — a struct de limites já é um valor plano
//!    pronto para virar consulta por projeto.
//! 3. **Conexões concorrentes por Host** — semáforo com 100 permissões por
//!    host; sem slot livre, o request entra numa fila curta (`queue_wait`,
//!    2 s) e, sem vaga nela, leva `429` imediato (07: "fila curta, depois
//!    429"). A permissão vive até a RESPOSTA ficar pronta; o body pode
//!    continuar streamando depois — mesma semântica do `ConcurrencyLimit` do
//!    tower, suficiente para segurar in-flight no nível de headers.
//!
//! Formato dos erros segue a matriz da 03: browser recebe HTML brandiado da
//! família Flux (sem `meta refresh` de propósito — auto-reload num `429`
//! viraria novo flood da própria página); rota de API recebe JSON +
//! `Retry-After`. A detecção de API reusa a MESMA heurística do dispatch
//! (`Accept: application/json` sem `text/html`, ou path `/api` — 12), então
//! borda e decisão nunca discordam sobre quem é rota de API. NUNCA 500 cru.
//!
//! Estado é `HashMap<Host, entrada>` sob `Mutex`, com limpeza lazy: acima do
//! teto de hosts (`max_hosts`, um flood de header `Host` forjado não pode
//! crescer a memória sem limite), entradas ociosas há mais de
//! `host_idle_secs` são evitadas; sem vaga nem lixo, o host NOVO é limitado
//! (hosts já conhecidos continuam passando — o flood não despeja tráfego
//! legítimo estabelecido).
//!
//! Self-contained de propósito: não lê `AppState`, nem SQLite, nem Docker —
//! o integrador monta com
//! `Router::layer(EdgeLayer::new(EdgeLimits::from_global(&global)))`.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll};
use std::time::Duration;

use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::Response;
use flux_common::config::GlobalConfig;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tower::{Layer, Service};

use crate::decision;

/// Rate default por host (07): 300 req/min.
pub const DEFAULT_RATE_PER_MIN: u32 = 300;
/// Burst do bucket (07): rajada de 100 antes de secar.
pub const DEFAULT_BURST: u32 = 100;
/// Teto de conexões concorrentes por host (07).
pub const DEFAULT_MAX_CONCURRENT: usize = 100;
/// Fila curta do teto de concorrência (07: "fila curta, depois 429").
pub const DEFAULT_QUEUE_WAIT: Duration = Duration::from_secs(2);
/// Teto de body do config global (09): 100 MB — espelho do default de
/// `GlobalConfig::max_body_bytes` (a fonte real é o config; este valor só
/// alimenta o `Default`).
pub const DEFAULT_MAX_BODY_BYTES: u64 = 104_857_600;
/// Teto de entradas no mapa de hosts — flood de header `Host` forjado não
/// cresce a memória sem limite.
pub const DEFAULT_MAX_HOSTS: usize = 4096;
/// Host sem uso há mais que isto vira candidato à limpeza lazy.
pub const HOST_IDLE_SECS: f64 = 600.0;

/// Limites da borda. Valores planos de propósito: o override por projeto
/// (`[limits] rate`, 07) é uma consulta futura que produz estes números por
/// request, não uma mudança de formato.
#[derive(Debug, Clone)]
pub struct EdgeLimits {
    pub rate_per_min: u32,
    pub burst: u32,
    pub max_concurrent: usize,
    pub queue_wait: Duration,
    pub max_body_bytes: u64,
    pub max_hosts: usize,
    pub host_idle_secs: f64,
}

impl Default for EdgeLimits {
    fn default() -> Self {
        Self {
            rate_per_min: DEFAULT_RATE_PER_MIN,
            burst: DEFAULT_BURST,
            max_concurrent: DEFAULT_MAX_CONCURRENT,
            queue_wait: DEFAULT_QUEUE_WAIT,
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
            max_hosts: DEFAULT_MAX_HOSTS,
            host_idle_secs: HOST_IDLE_SECS,
        }
    }
}

impl EdgeLimits {
    /// Defaults da 07 com o teto de body vindo do config global (09).
    pub fn from_global(global: &GlobalConfig) -> Self {
        Self {
            max_body_bytes: global.max_body_bytes(),
            ..Self::default()
        }
    }
}

/// Token bucket com recarga contínua. O relógio é `f64` em segundos
/// monótonos (base `Instant` capturada no `EdgeState`), o que deixa a matemática
/// de recarga testável sem dormir.
#[derive(Debug)]
struct Bucket {
    tokens: f64,
    last: f64,
}

impl Bucket {
    fn new(burst: f64, now: f64) -> Self {
        Self {
            tokens: burst,
            last: now,
        }
    }

    /// Consome 1 token. `None` = passa; `Some(segundos)` = `429` com
    /// `Retry-After` do segundo em que o próximo token fica pronto.
    fn take(&mut self, now: f64, rate_per_sec: f64, burst: f64) -> Option<u64> {
        // Recarga contínua (não em pacotes): tempo parado vira tokens,
        // limitado pelo burst — rajada nunca acumula além do teto.
        let elapsed = (now - self.last).max(0.0);
        self.tokens = (self.tokens + elapsed * rate_per_sec).min(burst);
        self.last = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            None
        } else {
            let secs = ((1.0 - self.tokens) / rate_per_sec).ceil().max(1.0);
            Some(secs as u64)
        }
    }
}

/// Entrada de um host: bucket de rate + semáforo de concorrência. O `last`
/// do bucket é também o "visto por último" da limpeza lazy — todo request
/// admitido passa pelo bucket antes do semáforo.
#[derive(Debug)]
struct HostEdge {
    bucket: Bucket,
    conc: Arc<Semaphore>,
}

impl HostEdge {
    fn new(now: f64, limits: &EdgeLimits) -> Self {
        Self {
            bucket: Bucket::new(limits.burst as f64, now),
            conc: Arc::new(Semaphore::new(limits.max_concurrent)),
        }
    }
}

/// Estado compartilhado da borda: mapas por host sob um mutex só. O request
/// bate no mapa 2× no máximo (bucket + semáforo), ambos O(1) — contenção não
/// é preocupação nesse volume (mesma aposta do `State` do SQLite).
pub struct EdgeState {
    limits: EdgeLimits,
    /// Base monótona: o relógio da borda é `epoch.elapsed()` em segundos.
    epoch: std::time::Instant,
    hosts: Mutex<HashMap<String, HostEdge>>,
}

impl EdgeState {
    pub fn new(limits: EdgeLimits) -> Self {
        Self {
            limits,
            epoch: std::time::Instant::now(),
            hosts: Mutex::new(HashMap::new()),
        }
    }

    fn now_secs(&self) -> f64 {
        self.epoch.elapsed().as_secs_f64()
    }

    /// Mutex envenenado = pânico num bucket: o estado é auto-curável (bucket
    /// recriado no próximo acesso), então recupera em vez de derrubar request
    /// com 500 (03: nunca 500 cru).
    fn lock(&self) -> MutexGuard<'_, HashMap<String, HostEdge>> {
        self.hosts.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Entrada do host, criando com limpeza lazy quando o mapa está no teto.
    /// `None` = sem vaga de memória nem lixo para evictar — host novo limitado.
    fn entry<'m>(
        &self,
        hosts: &'m mut HashMap<String, HostEdge>,
        host: &str,
    ) -> Option<&'m mut HostEdge> {
        if !hosts.contains_key(host) {
            if !ensure_capacity(hosts, self.now_secs(), &self.limits) {
                tracing::warn!(
                    hosts = hosts.len(),
                    "borda: teto de hosts na memória — host novo limitado"
                );
                return None;
            }
            hosts.insert(
                host.to_owned(),
                HostEdge::new(self.now_secs(), &self.limits),
            );
        }
        hosts.get_mut(host)
    }

    /// Consome 1 token do bucket do host. `Err(segundos)` = `429` com
    /// `Retry-After`.
    pub fn take_token(&self, host: &str) -> Result<(), u64> {
        let now = self.now_secs();
        let rate_per_sec = self.limits.rate_per_min as f64 / 60.0;
        let mut hosts = self.lock();
        match self.entry(&mut hosts, host) {
            Some(h) => match h.bucket.take(now, rate_per_sec, self.limits.burst as f64) {
                None => Ok(()),
                Some(retry_after) => Err(retry_after),
            },
            None => Err(1),
        }
    }

    /// Pega um slot de concorrência do host: tenta na hora, senão fila curta
    /// (`queue_wait`) e desiste com `None` (vira `429`). O lock do mapa é
    /// solto ANTES de qualquer await — só o `Arc<Semaphore>` segue adiante.
    pub async fn acquire_slot(&self, host: &str) -> Option<OwnedSemaphorePermit> {
        let conc = {
            let mut hosts = self.lock();
            match self.entry(&mut hosts, host) {
                Some(h) => h.conc.clone(),
                None => return None,
            }
        };
        if let Ok(permit) = conc.clone().try_acquire_owned() {
            return Some(permit);
        }
        match tokio::time::timeout(self.limits.queue_wait, conc.acquire_owned()).await {
            Ok(Ok(permit)) => Some(permit),
            // Sem slot dentro da fila curta (ou semáforo fechado): 429.
            _ => None,
        }
    }
}

/// Limpeza lazy (07 não quer varredura em background): só roda quando o mapa
/// bate no teto e evita hosts ociosos. Devolve `true` se há vaga para um
/// host novo.
fn ensure_capacity(hosts: &mut HashMap<String, HostEdge>, now: f64, limits: &EdgeLimits) -> bool {
    if hosts.len() < limits.max_hosts {
        return true;
    }
    let idle = limits.host_idle_secs;
    hosts.retain(|_, h| now - h.bucket.last < idle);
    hosts.len() < limits.max_hosts
}

/// A layer. Clonável — cada worker do server carrega a sua cópia do service,
/// todas apontando para o mesmo `EdgeState`.
#[derive(Clone)]
pub struct EdgeLayer {
    state: Arc<EdgeState>,
}

impl EdgeLayer {
    pub fn new(limits: EdgeLimits) -> Self {
        Self {
            state: Arc::new(EdgeState::new(limits)),
        }
    }
}

impl<S> Layer<S> for EdgeLayer {
    type Service = EdgeService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        EdgeService {
            inner,
            state: Arc::clone(&self.state),
        }
    }
}

/// O service da borda: admissão na frente do service de dentro (o router
/// axum). `Clone` obrigatório pelo modelo de service do axum.
#[derive(Clone)]
pub struct EdgeService<S> {
    inner: S,
    state: Arc<EdgeState>,
}

impl<S> Service<Request> for EdgeService<S>
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
        let state = Arc::clone(&self.state);
        Box::pin(async move {
            let (parts, body) = req.into_parts();
            let host = host_key(&parts);
            let path = parts.uri.path().to_owned();
            // Mesma heurística do dispatch (12): borda e decisão nunca
            // discordam sobre quem é rota de API.
            let wants_json = decision::wants_json(parts.headers.get(header::ACCEPT), &path);

            // 1) Teto de body: check de header O(1), sem estado — o `413` sai
            //    ANTES de qualquer leitura (matriz 03). Chunked não tem como:
            //    o stream segue embrulhado e aborta cedo no teto.
            if let Some(len) = content_length(&parts.headers)
                && len > state.limits.max_body_bytes
            {
                tracing::warn!(
                    host = %host,
                    len,
                    cap = state.limits.max_body_bytes,
                    "borda: 413 — corpo acima do teto"
                );
                return Ok(too_large(wants_json, state.limits.max_body_bytes));
            }

            // 2) Rate limit por host (token bucket).
            if let Err(retry_after) = state.take_token(&host) {
                tracing::debug!(host = %host, retry_after, "borda: 429 — bucket vazio");
                return Ok(too_many_requests(wants_json, retry_after));
            }

            // 3) Conexões concorrentes: tenta agora, fila curta, depois 429.
            let permit = match state.acquire_slot(&host).await {
                Some(permit) => permit,
                None => {
                    tracing::warn!(
                        host = %host,
                        limit = state.limits.max_concurrent,
                        "borda: 429 — teto de conexões concorrentes"
                    );
                    return Ok(overloaded(wants_json));
                }
            };

            // Chunked segue limitado no stream (aborta cedo no teto); com
            // Content-Length o check do passo 1 já bastou.
            let capped = !parts.headers.contains_key(header::CONTENT_LENGTH);
            let req = Request::from_parts(
                parts,
                if capped {
                    limited_body(body, state.limits.max_body_bytes)
                } else {
                    body
                },
            );

            // O slot vive até a resposta ficar PRONTA (headers); o body pode
            // continuar streamando depois — aposta idêntica à do tower
            // ConcurrencyLimit, suficiente para segurar in-flight.
            let fut = inner.call(req);
            let _slot = permit;
            fut.await
        })
    }
}

/// Chave do host para os mapas da borda — mesma normalização de
/// `ManagedHost::parse` (host.rs: minúsculas, sem porta), extraída como no
/// router (header `Host`, com fallback para o host da URI). IPv6 tem corte
/// próprio: o `:` do literal não é separador de porta (RFC 3986, 3.2.2) —
/// `split(':')` cego viraria a chave `[` para `[::1]:8080` e `""` para
/// `::1`, colapsando hosts distintos num MESMO bucket de rate limit e no
/// MESMO semáforo (um vizinho ruidoso esgotaria o limite de todos).
fn host_key(parts: &axum::http::request::Parts) -> String {
    let raw = parts
        .headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
        .or_else(|| parts.uri.host().map(str::to_owned))
        .unwrap_or_default();
    let host = raw.trim().to_ascii_lowercase();
    if host.starts_with('[') {
        // Header `Host: [::1]:8080` → chave `[::1]` (recorta NO colchete de
        // fechamento, inclusive).
        return match host.find(']') {
            Some(fim) => host[..=fim].to_owned(),
            None => String::new(),
        };
    }
    if host.matches(':').count() > 1 {
        // IPv6 literal SEM colchete (o host da URI vem assim) não carrega
        // porta — o literal inteiro é a chave.
        return host;
    }
    host.split(':').next().unwrap_or("").to_owned()
}

fn content_length(headers: &HeaderMap) -> Option<u64> {
    headers
        .get(header::CONTENT_LENGTH)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// Embrulha o body num limitador que ERRA ao cruzar o teto: hyper aborta a
/// leitura na hora — o upload gigante sem `Content-Length` não enche a RAM da
/// VM (07: "stream aborta cedo").
fn limited_body(body: Body, max_body_bytes: u64) -> Body {
    let cap = usize::try_from(max_body_bytes).unwrap_or(usize::MAX);
    Body::new(http_body_util::Limited::new(body, cap))
}

// --------------------------------------------------------------- respostas

/// `429` por rate limit (matriz 03): HTML brandiado para browser, JSON +
/// `Retry-After` para rota de API. O header vai nas DUAS formas — a matriz
/// só o garante no JSON, mas RFC 9110 recomenda em todo `429` e não custa
/// nada para o browser.
pub fn too_many_requests(wants_json: bool, retry_after: u64) -> Response {
    let mut r = if wants_json {
        json_error(
            StatusCode::TOO_MANY_REQUESTS,
            format!(r#"{{"error":"rate_limited","retry_after":{retry_after}}}"#),
        )
    } else {
        html_error(
            StatusCode::TOO_MANY_REQUESTS,
            "Flux — muitas requisições",
            "Muitas requisições",
            &format!(
                "<p>Este host passou do limite de requisições por minuto.</p>\
<p>Tente de novo em <code>{retry_after}</code> s.</p>"
            ),
        )
    };
    r.headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from(retry_after));
    r
}

/// `429` por teto de conexões concorrentes (mesma matriz, motivo distinto no
/// JSON para diagnóstico).
pub fn overloaded(wants_json: bool) -> Response {
    let mut r = if wants_json {
        json_error(
            StatusCode::TOO_MANY_REQUESTS,
            r#"{"error":"overloaded","retry_after":1}"#,
        )
    } else {
        html_error(
            StatusCode::TOO_MANY_REQUESTS,
            "Flux — servidor ocupado",
            "Servidor ocupado",
            "<p>Este host está no limite de conexões simultâneas. Tente de novo \
em instantes.</p>",
        )
    };
    r.headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
    r
}

/// `413` antes de ler o corpo (matriz 03).
pub fn too_large(wants_json: bool, max_body_bytes: u64) -> Response {
    if wants_json {
        return json_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!(r#"{{"error":"body_too_large","max_body_bytes":{max_body_bytes}}}"#),
        );
    }
    html_error(
        StatusCode::PAYLOAD_TOO_LARGE,
        "Flux — corpo muito grande",
        "Corpo muito grande",
        &format!(
            "<p>O corpo da requisição passa do teto de \
<code>{max_body_bytes}</code> bytes da plataforma.</p>"
        ),
    )
}

/// JSON de erro com forma fixa (sem entrada de usuário — não há o que
/// escapar) e sem `serde` no fluxd: dois campos numéricos não justificam a
/// dependência no binário.
fn json_error(status: StatusCode, body: impl Into<String>) -> Response {
    let mut r = Response::new(Body::from(body.into()));
    *r.status_mut() = status;
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

fn html_error(status: StatusCode, title: &str, heading: &str, body_html: &str) -> Response {
    let mut r = Response::new(Body::from(html_error_page(title, heading, body_html)));
    *r.status_mut() = status;
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

/// Mesma família visual das páginas de `pages.rs` (15), SEM `meta refresh`:
/// auto-reload num `429`/`413` viraria o próprio browser re-floodando. O
/// `noindex` de dev (R22) fica com o robots `Disallow: /` interceptado pela
/// decisão — a borda não conhece domínio gerenciado de propósito.
fn html_error_page(title: &str, heading: &str, body_html: &str) -> String {
    format!(
        r#"<!doctype html>
<html lang="pt-br">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title}</title>
<style>{CSS}</style>
</head>
<body>
<main>
<h1>{heading}</h1>
{body_html}
<footer>FLUX</footer>
</main>
</body>
</html>"#
    )
}

const CSS: &str = "body{margin:0;min-height:100vh;display:flex;align-items:center;justify-content:center;\
background:linear-gradient(160deg,#0f172a,#1e293b);color:#e2e8f0;\
font-family:system-ui,-apple-system,sans-serif;text-align:center}\
main{max-width:640px;padding:2rem}h1{font-size:1.4rem;font-weight:600;margin:0 0 .5rem}\
p{color:#94a3b8;margin:.4rem 0;line-height:1.5}code{color:#7dd3fc;font-size:.95em}\
footer{margin-top:2.5rem;font-size:.8rem;color:#64748b;letter-spacing:.08em}";

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use http_body_util::BodyExt;

    /// Limites pequenos para testar sem dormir: 1 req/s, burst 3,
    /// 1 conexão por host, fila de 50 ms, teto de body 16 bytes,
    /// 8 hosts no mapa.
    fn test_limits() -> EdgeLimits {
        EdgeLimits {
            rate_per_min: 60,
            burst: 3,
            max_concurrent: 1,
            queue_wait: Duration::from_millis(50),
            max_body_bytes: 16,
            max_hosts: 8,
            host_idle_secs: 60.0,
        }
    }

    /// IPv6 não colapsa na chave `[`/`""`: colchetes são recortados NO `]`
    /// (o `:` interno não é porta) e o literal sem colchete fica inteiro —
    /// hosts distintos dividem buckets NUNCA (vizinho ruidoso).
    #[test]
    fn host_key_trata_ipv6() {
        let chave = |host: Option<&str>, uri: Option<&str>| {
            let mut b = axum::http::Request::builder().uri(uri.unwrap_or("http://x/"));
            if let Some(h) = host {
                b = b.header(header::HOST, h);
            }
            let req = b.body(()).unwrap();
            let (parts, _) = req.into_parts();
            host_key(&parts)
        };
        assert_eq!(chave(Some("[::1]:8080"), None), "[::1]");
        assert_eq!(chave(None, Some("http://[fe80::1]:8080/")), "[fe80::1]");
        assert_eq!(chave(Some("::1"), None), "::1");
        // Domínio com porta segue a regra antiga (sem porta na chave).
        assert_eq!(
            chave(Some("VIPERS.bitello.xyz:443"), None),
            "vipers.bitello.xyz"
        );
    }

    /// Service de dentro do mock: devolve 200 e conta chamadas — prova que a
    /// borda rejeita ANTES de encaminhar.
    #[derive(Clone)]
    struct EchoOk {
        hits: Arc<AtomicUsize>,
    }

    impl Service<Request> for EchoOk {
        type Response = Response;
        type Error = std::convert::Infallible;
        type Future = std::future::Ready<Result<Response, Self::Error>>;

        fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, _req: Request) -> Self::Future {
            self.hits.fetch_add(1, Ordering::SeqCst);
            std::future::ready(Ok(Response::new(Body::from("ok"))))
        }
    }

    fn edge_service(hits: Arc<AtomicUsize>) -> EdgeService<EchoOk> {
        EdgeLayer::new(test_limits()).layer(EchoOk { hits })
    }

    fn req(host: &str, path: &str, accept: Option<&str>, content_length: Option<u64>) -> Request {
        let mut b = Request::builder().uri(path).header(header::HOST, host);
        if let Some(a) = accept {
            b = b.header(header::ACCEPT, a);
        }
        if let Some(cl) = content_length {
            b = b.header(header::CONTENT_LENGTH, cl.to_string());
        }
        b.body(Body::empty()).unwrap()
    }

    async fn call(svc: &mut EdgeService<EchoOk>, r: Request) -> Response {
        svc.call(r).await.unwrap()
    }

    async fn body_str(r: Response) -> String {
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

    #[test]
    fn bucket_recarrega_de_forma_continua_e_respeita_burst() {
        // 1 token/s, burst 2.
        let mut b = Bucket::new(2.0, 0.0);
        assert_eq!(b.take(0.0, 1.0, 2.0), None);
        assert_eq!(b.take(0.0, 1.0, 2.0), None);
        // Burst seco: Retry-After é o ceil de (1 - tokens)/rate = 1 s.
        assert_eq!(b.take(0.0, 1.0, 2.0), Some(1));
        assert!(
            b.take(0.5, 1.0, 2.0).is_some(),
            "0,5 s não recarrega 1 token"
        );
        // 2 s parados recarregam exatamente 2 tokens (burst de novo).
        assert_eq!(b.take(2.5, 1.0, 2.0), None);
        assert_eq!(b.take(2.5, 1.0, 2.0), None);
        assert!(b.take(2.5, 1.0, 2.0).is_some());
        // 100 s parados NÃO acumulam além do burst.
        assert_eq!(b.take(102.5, 1.0, 2.0), None);
        assert_eq!(b.take(102.5, 1.0, 2.0), None);
        assert!(
            b.take(102.5, 1.0, 2.0).is_some(),
            "burst nunca acumula além do teto"
        );
    }

    #[tokio::test]
    async fn rate_limit_estoura_em_429_e_isola_por_host() {
        let hits = Arc::new(AtomicUsize::new(0));
        let mut svc = edge_service(Arc::clone(&hits));
        for _ in 0..3 {
            let r = call(&mut svc, req("vipers-d.bitello.xyz", "/", None, None)).await;
            assert_eq!(r.status(), StatusCode::OK);
        }
        // 4ª do mesmo host: burst esgotou → 429 + Retry-After.
        let r = call(&mut svc, req("vipers-d.bitello.xyz", "/", None, None)).await;
        assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);
        let retry = r
            .headers()
            .get(header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .unwrap()
            .to_owned();
        assert!(retry.parse::<u64>().unwrap() >= 1, "Retry-After: {retry}");
        // O inner não foi perturbado pela rejeição (3 passes, o 429 não encaminha).
        assert_eq!(hits.load(Ordering::SeqCst), 3);
        // Host isolado tem bucket próprio: 1ª requisição passa normalmente.
        let r = call(&mut svc, req("outro.bitello.xyz", "/", None, None)).await;
        assert_eq!(r.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn corpo_acima_do_teto_e_413_antes_do_inner() {
        let hits = Arc::new(AtomicUsize::new(0));
        let mut svc = edge_service(Arc::clone(&hits));
        // Content-Length 32 > teto simulado de 16 bytes.
        let r = call(
            &mut svc,
            req("vipers.bitello.xyz", "/upload", None, Some(32)),
        )
        .await;
        assert_eq!(r.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(
            hits.load(Ordering::SeqCst),
            0,
            "413 tem que sair ANTES de encaminhar"
        );
        // Dentro do teto passa.
        let r = call(
            &mut svc,
            req("vipers.bitello.xyz", "/upload", None, Some(8)),
        )
        .await;
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn erros_de_borda_json_vs_html_por_accept() {
        let hits = Arc::new(AtomicUsize::new(0));

        // --- 429: browser (Accept html) recebe HTML da família Flux.
        let mut svc = edge_service(Arc::clone(&hits));
        for _ in 0..3 {
            call(
                &mut svc,
                req(
                    "vipers.bitello.xyz",
                    "/",
                    Some("text/html,application/xhtml+xml"),
                    None,
                ),
            )
            .await;
        }
        let r = call(
            &mut svc,
            req(
                "vipers.bitello.xyz",
                "/",
                Some("text/html,application/xhtml+xml"),
                None,
            ),
        )
        .await;
        assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);
        let body = body_str(r).await;
        assert!(body.contains("Muitas requisições"), "body: {body}");
        assert!(body.contains("FLUX"), "brandiado: {body}");
        assert!(
            !body.contains("http-equiv=\"refresh\""),
            "sem auto-flood: {body}"
        );

        // --- 429: API (Accept json) recebe JSON + Retry-After.
        let mut svc = edge_service(Arc::clone(&hits));
        for _ in 0..3 {
            call(
                &mut svc,
                req("api.bitello.xyz", "/", Some("application/json"), None),
            )
            .await;
        }
        let r = call(
            &mut svc,
            req("api.bitello.xyz", "/", Some("application/json"), None),
        )
        .await;
        assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            r.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/json"
        );
        let body = body_str(r).await;
        assert_eq!(body, r#"{"error":"rate_limited","retry_after":1}"#);

        // --- 413: path /api manda MESMO com Accept html (rota de API nunca
        // recebe HTML, 12).
        let mut svc = edge_service(Arc::clone(&hits));
        let r = call(
            &mut svc,
            req(
                "vipers.bitello.xyz",
                "/api/upload",
                Some("text/html"),
                Some(32),
            ),
        )
        .await;
        assert_eq!(r.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(
            r.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/json"
        );
        let body = body_str(r).await;
        assert_eq!(body, r#"{"error":"body_too_large","max_body_bytes":16}"#);
    }

    #[tokio::test]
    async fn fila_curta_de_concorrencia_desiste_e_nao_contamina_outro_host() {
        let st = Arc::new(EdgeState::new(test_limits()));
        // Único slot do host ocupado.
        let p1 = st.acquire_slot("vipers.bitello.xyz").await;
        assert!(p1.is_some());
        // Segunda tentativa espera a fila curta (50 ms) e desiste com None.
        let antes = std::time::Instant::now();
        let p2 = st.acquire_slot("vipers.bitello.xyz").await;
        assert!(p2.is_none(), "excedente tem que levar 429");
        assert!(
            antes.elapsed() >= Duration::from_millis(45),
            "fila curta esperou antes de desistir: {:?}",
            antes.elapsed()
        );
        // Outro host tem semáforo próprio: não é afetado.
        assert!(
            st.acquire_slot("outro.bitello.xyz").await.is_some(),
            "isolamento por host"
        );
    }

    #[tokio::test]
    async fn corpo_sem_content_length_aborta_o_stream_cedo() {
        let st = EdgeState::new(test_limits());
        // 32 bytes > teto de 16: a leitura ERRA em vez de tragar 10 GB na RAM.
        let body = limited_body(Body::from(vec![0u8; 32]), st.limits.max_body_bytes);
        assert!(
            body.collect().await.is_err(),
            "stream tem que abortar ao cruzar o teto"
        );
        // Dentro do teto o stream passa inteiro.
        let body = limited_body(Body::from(vec![0u8; 8]), st.limits.max_body_bytes);
        let bytes = body
            .collect()
            .await
            .expect("dentro do teto flui")
            .to_bytes();
        assert_eq!(bytes.len(), 8);
    }

    #[tokio::test]
    async fn mapa_de_hosts_tem_teto_e_limpeza_lazy() {
        // Teto de 8 hosts, todos frescos: o 9º host novo é limitado (sem vaga,
        // sem lixo) — flood de Host forjado não cresce a memória.
        let st = EdgeState::new(test_limits());
        for i in 0..8 {
            assert!(
                st.take_token(&format!("h{i}.bitello.xyz")).is_ok(),
                "host {i} devia passar"
            );
        }
        assert!(
            st.take_token("novo.bitello.xyz").is_err(),
            "sem vaga de memória, host novo é limitado"
        );
        // Host já conhecido continua passando: rejeição não despeja tráfego
        // legítimo estabelecido.
        assert!(st.take_token("h0.bitello.xyz").is_ok());

        // Limpeza lazy: com ociosidade 0 (tudo vira lixo na hora), o host
        // novo entra evitando os velhos.
        let mut limits = test_limits();
        limits.host_idle_secs = 0.0;
        let st2 = EdgeState::new(limits);
        for i in 0..8 {
            st2.take_token(&format!("h{i}.bitello.xyz")).unwrap();
        }
        assert!(
            st2.take_token("novo.bitello.xyz").is_ok(),
            "limpeza lazy libera vagas de hosts velhos"
        );
    }

    #[test]
    fn defaults_batem_a_spec_07() {
        let d = EdgeLimits::default();
        assert_eq!(d.rate_per_min, 300);
        assert_eq!(d.burst, 100);
        assert_eq!(d.max_concurrent, 100);
        assert_eq!(d.queue_wait, Duration::from_secs(2));
        assert_eq!(d.max_body_bytes, 104_857_600);
        // Config global com default: mesmo teto de 100 MB (09).
        assert_eq!(
            EdgeLimits::from_global(&GlobalConfig::default()).max_body_bytes,
            d.max_body_bytes
        );
    }
}
