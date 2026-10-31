//! Doorman (31/10): espião HTTP local — `flux doorman <host>`. Ouve em
//! 127.0.0.1, imprime cada request completa no terminal EM TEMPO REAL e
//! repassa ao host público de destino. Nada é gravado — nem na VM nem no PC:
//! só sai no terminal e morre no Ctrl+C. Comando GLOBAL (sem contexto de
//! projeto).
//!
//! O doorman fica NO CAMINHO: não é sniffer passivo — request que não passa
//! por ele não é vista. O banner manda apontar o cliente (ex.:
//! `ANTHROPIC_BASE_URL=http://127.0.0.1:7440 opencode`). O Host público é
//! preservado no upstream (a autoridade da URI vira o host alvo) — Caddy,
//! gate do dev e gate Bearer do zcode-api continuam vendo o mesmo tráfego de
//! sempre, só com um salto a mais.
//!
//! Formato é agnóstico: mostra o HTTP cru; `/v1/messages` é JSON Anthropic
//! (bonito quando parseia) e a resposta SSE flui viva, pedaço a pedaço.
//!
//! Limitações declaradas (v1):
//! - Só HTTP puro (zcode-api-d.bitello.xyz:80 é HTTP — coberto). Host HTTPS
//!   exigiria terminar TLS com certificado próprio (MITM) — fora do escopo.
//! - Corpo de request é bufferizado inteiro antes de repassar (payload de
//!   LLM cabe em RAM; corpo de gigas não é caso de uso).
//! - Sem websocket/upgrade e sem pool de upstream (é um tap de depuração).

mod registro;

use crate::comandos::doorman::registro::Aparencia;
use http_body_util::{BodyExt, Full};
use hyper::body::{Bytes, Frame, Incoming};
use hyper::header::{self, HeaderMap, HeaderName};
use hyper::http::Uri;
use hyper::http::request::Parts;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::client::legacy::{Client, connect::HttpConnector};
use hyper_util::rt::{TokioExecutor, TokioIo};
use std::convert::Infallible;
use std::future::pending;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::{Notify, mpsc};

/// Argumentos vindos do main.rs já desmembrados pelo clap.
pub(crate) struct Opcoes {
    pub(crate) host: String,
    pub(crate) porta: u16,
    pub(crate) ouvir: u16,
    pub(crate) respostas: bool,
    pub(crate) so_respostas: bool,
    pub(crate) cru: bool,
    pub(crate) n: Option<u64>,
    pub(crate) janela: Option<String>,
}

type ProxyClient = Client<HttpConnector, Full<Bytes>>;

struct Estado {
    cliente: ProxyClient,
    /// Único caminho de saída no terminal: produtores mandam BLOCOS prontos,
    /// a tarefa impressora exibe — blocos inteiros nunca se intercalam.
    eventos: mpsc::UnboundedSender<String>,
    ordem: AtomicU64,
    respondidas: AtomicU64,
    apr: Aparencia,
    mostrar_entrada: bool,
    mostrar_saida: bool,
    /// Autoridade upstream (`host` ou `host:porta`) — Host público preservado.
    alvo: String,
    /// --n: teto de entradas; atingido, acorda o accept loop pra parar.
    teto_entradas: Option<u64>,
    parar: Arc<Notify>,
}

pub(crate) async fn doorman(op: Opcoes) {
    let janela = match op.janela.as_deref().map(duracao) {
        None => None,
        Some(Ok(d)) => Some(d),
        Some(Err(e)) => {
            eprintln!("erro: {e}");
            std::process::exit(2);
        }
    };
    // Host público SEM porta default: é o Host que o Caddy roteia.
    let alvo = if op.porta == 80 {
        op.host.clone()
    } else {
        format!("{}:{}", op.host, op.porta)
    };

    // Falha cedo: sem destino alcançável não há doorman (resolução do SO,
    // inclusive hosts file — o mesmo caminho que o cliente real usaria).
    if let Err(e) = tokio::net::lookup_host((op.host.as_str(), op.porta)).await {
        eprintln!("erro: destino {alvo} não resolve: {e}");
        std::process::exit(1);
    }

    let listener = match TcpListener::bind(("127.0.0.1", op.ouvir)).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("erro: 127.0.0.1:{} não abre (porta em uso?): {e}", op.ouvir);
            std::process::exit(1);
        }
    };

    let (eventos, impressora) = impressora();
    let parar = Arc::new(Notify::new());
    let estado = Arc::new(Estado {
        cliente: Client::builder(TokioExecutor::new()).build_http(),
        eventos,
        ordem: AtomicU64::new(0),
        respondidas: AtomicU64::new(0),
        apr: Aparencia {
            cru: op.cru,
            ..Aparencia::default()
        },
        mostrar_entrada: !op.so_respostas,
        mostrar_saida: op.respostas || op.so_respostas,
        alvo: alvo.clone(),
        teto_entradas: op.n,
        parar: parar.clone(),
    });

    let modo = if op.so_respostas {
        "só respostas"
    } else if op.respostas {
        "entradas + respostas"
    } else {
        "só entradas"
    };
    estado
        .eventos
        .send(format!(
            "doorman → {alvo} | modo {modo} | http://127.0.0.1:{}\n\
             aponte o cliente: ex. ANTHROPIC_BASE_URL=http://127.0.0.1:{} opencode\n\
             ctrl+c encerra — nada é gravado em disco (corpos podem conter segredo: a tela é sua)",
            op.ouvir, op.ouvir
        ))
        .ok();

    let prazo = janela.map(|d| tokio::time::Instant::now() + d);
    let mut tarefas = tokio::task::JoinSet::new();
    // Init atrasado: todo break atribui — o compilador cobra.
    let motivo;
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                motivo = "ctrl+c".to_owned();
                break;
            }
            _ = async {
                match prazo {
                    Some(p) => tokio::time::sleep_until(p).await,
                    None => pending::<()>().await,
                }
            } => {
                motivo = "janela esgotada".to_owned();
                break;
            }
            _ = parar.notified() => {
                motivo = format!("teto de {} entrada(s)", op.n.unwrap_or(0));
                break;
            }
            aceito = listener.accept() => match aceito {
                Ok((socket, _)) => {
                    let st = estado.clone();
                    tarefas.spawn(async move { servir_conn(st, TokioIo::new(socket)).await });
                }
                Err(e) => {
                    estado.eventos.send(format!("— accept falhou: {e}\n")).ok();
                }
            },
        }
    }

    // Graça: fluxos em curso (SSE aberto) terminam ou morrem em 10 s.
    drop(listener);
    let _ = tokio::time::timeout(Duration::from_secs(10), async {
        while tarefas.join_next().await.is_some() {}
    })
    .await;

    let entradas = estado.ordem.load(Ordering::Relaxed);
    let respostas = estado.respondidas.load(Ordering::Relaxed);
    drop(estado);
    // JoinHandle::await devolve Result — a impressora não tem como falhar
    // além de pânico, e aí o runtime já propaga; explicito o descarte.
    let _ = impressora.await;
    println!(
        "doorman {motivo}: {entradas} entrada(s), {respostas} resposta(s) exibida(s). Nada foi gravado em disco."
    );
}

/// Aceita conexões http/1.1 (keep-alive incluído) até a conexão fechar.
async fn servir_conn(estado: Arc<Estado>, io: TokioIo<tokio::net::TcpStream>) {
    let st = estado.clone();
    let servico = service_fn(move |req| suprir(st.clone(), req));
    if let Err(e) = hyper::server::conn::http1::Builder::new()
        .serve_connection(io, servico)
        .await
    {
        // Fechamento limpo não é erro (serve_connection retorna Ok); o que
        // chega aqui é aborto no meio do pedido — fica observável.
        estado
            .eventos
            .send(format!("— conexão interrompida: {e}\n"))
            .ok();
    }
}

async fn suprir(
    estado: Arc<Estado>,
    req: Request<Incoming>,
) -> Result<Response<CorpoRepassado>, Infallible> {
    let ordem = estado.ordem.fetch_add(1, Ordering::Relaxed) + 1;
    // --n atingido: para de aceitar; a graça no accept loop drena o resto.
    if estado.teto_entradas.is_some_and(|t| ordem >= t) {
        estado.parar.notify_one();
    }
    let (mut parts, corpo) = req.into_parts();
    let bytes = match corpo.collect().await {
        Ok(c) => c.to_bytes(),
        Err(e) => {
            return Ok(resposta_erro(
                &estado,
                400,
                format!("doorman: corpo do cliente ilegível: {e}"),
            ));
        }
    };

    if estado.mostrar_entrada {
        let uri = parts
            .uri
            .path_and_query()
            .map(|p| p.as_str().to_owned())
            .unwrap_or_else(|| "/".to_owned());
        estado
            .eventos
            .send(registro::bloco_request(
                &hora(),
                ordem,
                parts.method.as_str(),
                &uri,
                &parts.headers,
                &bytes,
                &estado.apr,
            ))
            .ok();
    }

    let pedido = match montar_upstream(&mut parts, Full::new(bytes), &estado.alvo) {
        Ok(p) => p,
        Err(e) => return Ok(resposta_erro(&estado, 502, e)),
    };
    let resposta = match estado.cliente.request(pedido).await {
        Ok(r) => r,
        Err(e) => {
            return Ok(resposta_erro(
                &estado,
                502,
                format!("doorman: upstream {}: {e}", estado.alvo),
            ));
        }
    };
    let (mut p2, corpo2) = resposta.into_parts();
    strip_hop_by_hop(&mut p2.headers);
    // O corpo repassado é um canal sem tamanho conhecido: length/chunked do
    // upstream não valem para a perna final — o servidor hyper reformata.
    p2.headers.remove(header::CONTENT_LENGTH);
    p2.headers.remove(header::TRANSFER_ENCODING);

    let sse = p2
        .headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|t| t.contains("text/event-stream"));
    let json = p2
        .headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|t| t.contains("json"));

    if estado.mostrar_saida {
        estado.respondidas.fetch_add(1, Ordering::Relaxed);
        estado
            .eventos
            .send(registro::bloco_resposta(
                &hora(),
                ordem,
                p2.status.as_u16(),
                &p2.headers,
            ))
            .ok();
    }

    let (tx, rx) = mpsc::channel::<Result<Frame<Bytes>, hyper::Error>>(16);
    let st = estado.clone();
    tokio::spawn(async move {
        let mut corpo = corpo2;
        let mut buffer: Vec<u8> = Vec::new();
        let mut impresso = 0usize;
        loop {
            match corpo.frame().await {
                Some(Ok(frame)) => {
                    let pedaco = frame.data_ref().map(|b| b.as_ref()).unwrap_or(&[]).to_vec();
                    if st.mostrar_saida {
                        if sse {
                            if let Some(l) =
                                registro::pedaco_fluxo(&pedaco, impresso, st.apr.teto_resposta)
                            {
                                st.eventos.send(l).ok();
                            }
                            impresso += pedaco.len();
                        } else if buffer.len() < st.apr.teto_resposta {
                            buffer.extend_from_slice(&pedaco);
                        }
                    }
                    if tx.send(Ok(frame)).await.is_err() {
                        break; // cliente foi embora — para de drenar o upstream
                    }
                }
                Some(Err(e)) => {
                    st.eventos
                        .send(format!("— #{ordem} upstream falhou no meio: {e}\n"))
                        .ok();
                    let _ = tx.send(Err(e)).await;
                    return;
                }
                None => break,
            }
        }
        if st.mostrar_saida
            && !sse
            && let Some(b) = registro::bloco_resposta_corpo(&buffer, json, &st.apr)
        {
            st.eventos.send(b).ok();
        }
    });

    Ok(Response::from_parts(p2, CorpoRepassado { rx }))
}

/// Montagem do pedido upstream: path/query preservado, autoridade = host
/// público (Host que o Caddy roteia — DIFERENTE do proxy do fluxd, que
/// aponta pro container), hop-by-hop e content-length stripados (o corpo é
/// reenquadrado pelo hyper).
fn montar_upstream(
    parts: &mut Parts,
    corpo: Full<Bytes>,
    alvo: &str,
) -> Result<Request<Full<Bytes>>, String> {
    let pq = parts
        .uri
        .path_and_query()
        .map(|p| p.as_str().to_owned())
        .unwrap_or_else(|| "/".to_owned());
    let uri = Uri::builder()
        .scheme("http")
        .authority(alvo)
        .path_and_query(pq)
        .build()
        .map_err(|e| format!("doorman: alvo {alvo:?} inválido: {e}"))?;

    strip_hop_by_hop(&mut parts.headers);
    parts.headers.remove(header::HOST);
    parts.headers.remove(header::CONTENT_LENGTH);

    let mut builder = Request::builder().method(parts.method.clone()).uri(uri);
    if let Some(h) = builder.headers_mut() {
        *h = std::mem::take(&mut parts.headers);
    }
    builder
        .body(corpo)
        .map_err(|e| format!("doorman: montagem p/ {alvo:?}: {e}"))
}

/// Hop-by-hop (RFC 9110 §7.6.1) + nomes listados no `Connection`.
fn strip_hop_by_hop(headers: &mut HeaderMap) {
    // keep-alive/proxy-connection não têm constante em hyper::header.
    let hop: [HeaderName; 7] = [
        header::CONNECTION,
        HeaderName::from_static("keep-alive"),
        HeaderName::from_static("proxy-connection"),
        header::TE,
        header::TRAILER,
        header::TRANSFER_ENCODING,
        header::UPGRADE,
    ];
    let nomeados: Vec<HeaderName> = headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .filter_map(|p| HeaderName::try_from(p.trim()).ok())
        .collect();
    for nome in hop {
        headers.remove(nome);
    }
    for nome in nomeados {
        headers.remove(nome);
    }
}

/// Resposta de erro do PRÓPRIO doorman (upstream não respondeu etc.) —
/// nunca finge que foi o destino: texto curto, status correto, canal com um
/// frame só.
fn resposta_erro(estado: &Estado, status: u16, motivo: String) -> Response<CorpoRepassado> {
    estado.eventos.send(format!("— {motivo}\n")).ok();
    let (tx, rx) = mpsc::channel(1);
    tx.try_send(Ok(Frame::data(Bytes::from(format!("{motivo}\n")))))
        .ok();
    let status_code = hyper::StatusCode::from_u16(status).unwrap_or(hyper::StatusCode::BAD_GATEWAY);
    Response::builder()
        .status(status_code)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(CorpoRepassado { rx })
        .expect("resposta de erro do doorman com headers fixos")
}

/// Corpo repassado: frames que chegam do upstream via canal (tee). Receiver
/// do tokio já expõe `poll_recv` — o adapter é direto.
struct CorpoRepassado {
    rx: mpsc::Receiver<Result<Frame<Bytes>, hyper::Error>>,
}

impl http_body::Body for CorpoRepassado {
    type Data = Bytes;
    type Error = hyper::Error;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        Pin::new(&mut self.rx).poll_recv(cx)
    }
}

/// Tarefa impressora única: blocos inteiros, sem intercalar.
fn impressora() -> (mpsc::UnboundedSender<String>, tokio::task::JoinHandle<()>) {
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let handle = tokio::spawn(async move {
        while let Some(bloco) = rx.recv().await {
            println!("{bloco}");
        }
    });
    (tx, handle)
}

fn hora() -> String {
    chrono::Local::now().format("%H:%M:%S").to_string()
}

/// `30s` | `10m` | `1h` — número + unidade obrigatórios.
fn duracao(texto: &str) -> Result<Duration, String> {
    let (num, unidade) = texto.split_at(texto.len().saturating_sub(1));
    let n: u64 = num
        .parse()
        .map_err(|_| format!("janela inválida: {texto:?} — use 30s, 10m, 1h"))?;
    let seg = match unidade {
        "s" => n,
        "m" => n * 60,
        "h" => n * 3600,
        _ => return Err(format!("janela inválida: {texto:?} — unidade s|m|h")),
    };
    Ok(Duration::from_secs(seg))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duracao_aceita_s_m_h_e_recusa_resto() {
        assert_eq!(duracao("30s").unwrap(), Duration::from_secs(30));
        assert_eq!(duracao("10m").unwrap(), Duration::from_secs(600));
        assert_eq!(duracao("1h").unwrap(), Duration::from_secs(3600));
        assert!(duracao("10").is_err());
        assert!(duracao("10x").is_err());
        assert!(duracao("zm").is_err());
        assert!(duracao("").is_err());
    }

    #[test]
    fn montar_preserva_path_query_e_troca_autoridade() {
        let req = Request::builder()
            .uri("http://127.0.0.1:7440/v1/messages?beta=true")
            .header(header::CONTENT_LENGTH, "5")
            .header(header::CONNECTION, "keep-alive")
            .header(header::HOST, "127.0.0.1:7440")
            .body(())
            .unwrap();
        let (mut parts, ()) = req.into_parts();
        let up = montar_upstream(
            &mut parts,
            Full::new(Bytes::from_static(b"hello")),
            "zcode-api-d.bitello.xyz",
        )
        .unwrap();
        assert_eq!(up.uri().host(), Some("zcode-api-d.bitello.xyz"));
        assert_eq!(up.uri().path_and_query().unwrap(), "/v1/messages?beta=true");
        assert!(
            up.headers().get(header::HOST).is_none(),
            "Host sai da URI, não do header"
        );
        assert!(up.headers().get(header::CONTENT_LENGTH).is_none());
        assert!(up.headers().get(header::CONNECTION).is_none());
    }

    #[test]
    fn strip_remove_nome_listados_no_connection() {
        let req = Request::builder()
            .uri("http://x/")
            .header(header::CONNECTION, "close, X-Marcado")
            .header("x-marcado", "some")
            .header("x-fica", "fica")
            .body(())
            .unwrap();
        let (mut parts, ()) = req.into_parts();
        strip_hop_by_hop(&mut parts.headers);
        assert!(parts.headers.get("x-marcado").is_none());
        assert!(parts.headers.get("x-fica").is_some());
    }
}
