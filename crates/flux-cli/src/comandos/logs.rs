//! logs (L7.2): extraído verbatim do main.rs — movimento sem "melhoria".

use crate::contexto::{LEASE_UP_SECS, contexto_atual, falhar, parse_env};
use http_body_util::BodyExt;

use crate::api::{ApiClient, ApiError};

/// Tail padrão do `flux logs` (contrato CLI ↔ fluxd: o GET de logs leva
/// tail sempre explícito).
pub(crate) const TAIL_PADRAO: u64 = 200;

pub(crate) async fn logs(
    follow: bool,
    since: Option<String>,
    until: Option<String>,
    service: Option<String>,
    tail: Option<u64>,
    env: &str,
) {
    let env = parse_env(env);
    let ctx = contexto_atual();
    let cliente = ApiClient::from_env();

    // Janela histórica é fase 13 (06): o fluxd de hoje só aplica
    // env/tail/follow — enviar os parâmetros seria fingir que aplicaram.
    if since.is_some() || until.is_some() || service.is_some() {
        eprintln!(
            "aviso: --since/--until/--service chegam na fase 13 (janela histórica, spec 06) — o fluxd de hoje NÃO os aplica e eles não foram enviados"
        );
    }

    match cliente
        .lease(&ctx.name, &env.to_string(), LEASE_UP_SECS)
        .await
    {
        Ok(_) => {}
        Err(e) => falhar(e),
    }
    let _guarda = crate::api::manter_lease(
        cliente.clone(),
        ctx.name.clone(),
        env.to_string(),
        LEASE_UP_SECS,
    );

    let caminho = caminho_logs(
        &ctx.name,
        &env.to_string(),
        tail.unwrap_or(TAIL_PADRAO),
        follow,
    );
    if follow {
        let res = match cliente.abrir_stream(&caminho).await {
            Ok(r) => r,
            Err(e) => falhar(e),
        };
        if let Err(e) = consumir_sse(res).await {
            falhar(e);
        }
    } else {
        match cliente.call_raw("GET", &caminho, None).await {
            Ok(corpo) => {
                print!("{corpo}");
                if !corpo.ends_with('\n') {
                    println!();
                }
            }
            Err(e) => falhar(e),
        }
    }
}

/// Query do GET de logs (contrato CLI ↔ fluxd): env, tail e follow sempre
/// explícitos — o fluxd não tem default de query.
pub(crate) fn caminho_logs(name: &str, env: &str, tail: u64, follow: bool) -> String {
    format!("/projects/{name}/logs?env={env}&tail={tail}&follow={follow}")
}

/// Payloads `data:` das linhas completas (terminadas em \n) do buffer — o
/// contrato do /logs -f é um evento SSE por linha de log (`data: X` + linha
/// vazia). Separadores, comentários (`: ping`) e outros campos não viram
/// saída. Linha cortada entre frames fica no buffer para o próximo frame.
/// O buffer é BYTES crus e o lossy roda SÓ na linha completa: multibyte
/// cortado ENTRE chunks junta certo (String por chunk viraria U+FFFD).
pub(crate) fn drenar_linhas(buf: &mut Vec<u8>) -> Vec<String> {
    let mut saida = Vec::new();
    while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
        let linha: Vec<u8> = buf.drain(..=pos).collect();
        let texto = String::from_utf8_lossy(&linha[..linha.len() - 1]);
        if let Some(dado) = crate::api::dado_de_data(texto.trim_end_matches('\r')) {
            saida.push(dado.to_owned());
        }
    }
    saida
}

/// Payload de erro do engine no stream (contrato: o prefixo `[fluxd:` é
/// reservado do daemon — linha de log do app nunca começa assim). O frame
/// precisa virar erro do CLI, não linha de log com exit 0.
pub(crate) fn frame_de_erro(dado: &str) -> bool {
    dado.starts_with("[fluxd: erro")
}

/// Consome o stream SSE do /logs -f (contrato CLI ↔ fluxd): frame
/// `data: X` → imprime X; termina limpo quando o fluxd corta o stream (frame
/// None). Erro no meio do stream NUNCA é silencioso: sai com causa e
/// exit ≠ 0 (06) — inclusive o frame `[fluxd: erro ...]` do engine, que não
/// pode passar por linha de log comum.
pub(crate) async fn consumir_sse(
    res: hyper::Response<hyper::body::Incoming>,
) -> Result<(), ApiError> {
    let mut corpo = res.into_body();
    let mut buf: Vec<u8> = Vec::new();
    loop {
        match corpo.frame().await {
            Some(Ok(frame)) => {
                let chunk = frame.into_data().map_err(|_| ApiError::Protocolo {
                    msg: "frame do stream de logs sem payload de dados (trailer?)".into(),
                })?;
                buf.extend_from_slice(&chunk);
                for linha in drenar_linhas(&mut buf) {
                    if frame_de_erro(&linha) {
                        return Err(ApiError::Conexao {
                            url: "fluxd (stream de logs)".into(),
                            causa: linha,
                        });
                    }
                    println!("{linha}");
                }
            }
            Some(Err(e)) => {
                return Err(ApiError::Conexao {
                    url: "fluxd (stream de logs)".into(),
                    causa: format!("corte do stream com erro: {e}"),
                });
            }
            None => {
                // Fim limpo: sobra sem \n final ainda pode ser um payload.
                let resto = String::from_utf8_lossy(&buf).into_owned();
                if let Some(dado) = crate::api::dado_de_data(resto.trim_end_matches(['\r'])) {
                    if frame_de_erro(dado) {
                        return Err(ApiError::Conexao {
                            url: "fluxd (stream de logs)".into(),
                            causa: dado.to_owned(),
                        });
                    }
                    println!("{dado}");
                }
                return Ok(());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_de_logs_monta_env_tail_e_follow() {
        assert_eq!(
            caminho_logs("vipers", "dev", TAIL_PADRAO, false),
            "/projects/vipers/logs?env=dev&tail=200&follow=false",
            "padrão do contrato: tail sempre explícito"
        );
        assert_eq!(
            caminho_logs("djm", "prod", 5, true),
            "/projects/djm/logs?env=prod&tail=5&follow=true",
            "follow=true em minúsculas (query, não JSON)"
        );
    }

    #[test]
    fn drenar_linhas_extrai_so_payload_data() {
        let mut buf = Vec::from("data: linha 1\n\ndata: linha 2\n\n: keepalive\n".as_bytes());
        assert_eq!(
            drenar_linhas(&mut buf),
            vec!["linha 1".to_owned(), "linha 2".to_owned()],
            "cada evento data: vira uma linha de saída; separador some"
        );
        assert!(
            drenar_linhas(&mut buf).is_empty(),
            "comentário : não vira saída"
        );
        assert!(buf.is_empty(), "buffer drenado por completo");
    }

    #[test]
    fn drenar_linhas_monta_linha_cortada_entre_chunks() {
        let mut buf: Vec<u8> = Vec::new();
        buf.extend_from_slice(b"data: par");
        assert!(
            drenar_linhas(&mut buf).is_empty(),
            "sem \\n não há linha completa — nada sai cortado"
        );
        buf.extend_from_slice(b"cial\n\ndata: seguinte\n");
        assert_eq!(
            drenar_linhas(&mut buf),
            vec!["parcial".to_owned(), "seguinte".to_owned()],
            "payload remontado entre frames"
        );
    }

    /// Revisão: chunk SSE pode cortar dentro de um caractere UTF-8 — junção
    /// em BYTES e lossy só na linha completa ("ç" = 0xC3 0xA7 dividido).
    #[test]
    fn drenar_linhas_junta_multibyte_cortado_entre_frames() {
        let mut buf: Vec<u8> = Vec::new();
        buf.extend_from_slice(b"data: \xC3"); // 1º byte do "ç" no fim do chunk
        assert!(
            drenar_linhas(&mut buf).is_empty(),
            "sem \\n nada sai — byte parcial fica no buffer"
        );
        buf.extend_from_slice(&[0xA7, b'o', b'\n']); // 2º byte vem no chunk seguinte
        assert_eq!(
            drenar_linhas(&mut buf),
            vec!["ço".to_owned()],
            "byte parcial do multibyte remontado antes do lossy"
        );
    }

    #[test]
    fn frame_de_erro_so_para_prefixo_reservado_do_fluxd() {
        assert!(frame_de_erro(
            "[fluxd: erro no stream de logs do engine: boom]"
        ));
        assert!(
            !frame_de_erro("[app] 2026-10-14 erro comum"),
            "log do app é log"
        );
        assert!(!frame_de_erro("linha comum"), "linha comum é log");
    }
}
