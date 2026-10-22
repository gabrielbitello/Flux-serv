//! Transporte stdio do `flux mcp` (22/10): JSON-RPC 2.0 com uma mensagem
//! por linha. O PROTOCOLO (initialize/ping/tools, catálogo, dispatch) é o
//! `flux-common::mcp` — aqui vive só o loop de I/O e o erro de parse
//! (-32700, que é do transporte). Toda saída de diagnóstico vai pra stderr;
//! stdout é EXCLUSIVAMENTE o protocolo.

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use super::ferramentas::DespachanteCli;

pub(crate) async fn servir() {
    let mut entrada = BufReader::new(tokio::io::stdin()).lines();
    let mut saida = tokio::io::stdout();
    let despachante = DespachanteCli;
    loop {
        let linha = match entrada.next_line().await {
            Ok(Some(l)) => l,
            Ok(None) => break, // EOF: o cliente fechou — despedida limpa
            Err(e) => {
                eprintln!("mcp: erro lendo o stdin: {e}");
                break;
            }
        };
        let Some(resposta) = processar_linha(&linha, &despachante).await else {
            continue; // linha vazia ou notificação — nada volta
        };
        if let Err(e) = saida.write_all(resposta.as_bytes()).await {
            eprintln!("mcp: erro escrevendo a resposta: {e}");
            break;
        }
        if let Err(e) = saida.write_all(b"\n").await {
            eprintln!("mcp: erro fechando a linha da resposta: {e}");
            break;
        }
        // Flush explícito: o cliente só segue depois de ler a resposta.
        if let Err(e) = saida.flush().await {
            eprintln!("mcp: erro no flush do stdout: {e}");
            break;
        }
    }
}

/// Uma linha de entrada → a resposta (já serializada, sem `\n` interno) ou
/// None quando não há o que responder. `to_string()` do serde_json é compacto
/// (o protocolo é uma mensagem por linha — pretty quebraria o framing).
async fn processar_linha(linha: &str, despachante: &DespachanteCli) -> Option<String> {
    let texto = linha.trim();
    if texto.is_empty() {
        return None;
    }
    let msg: Value = match serde_json::from_str(texto) {
        Ok(v) => v,
        Err(e) => {
            // Erro de PARSE é do transporte (a mensagem nem decodificou):
            // -32700 com id nulo, como manda o JSON-RPC.
            return Some(
                json!({
                    "jsonrpc": "2.0",
                    "id": null,
                    "error": { "code": -32700, "message": format!("JSON inválido: {e}") }
                })
                .to_string(),
            );
        }
    };
    flux_common::mcp::responder_mensagem(&msg, despachante)
        .await
        .map(|resposta| resposta.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn responder(texto: &str) -> Option<Value> {
        let d = DespachanteCli;
        processar_linha(texto, &d)
            .await
            .map(|s| serde_json::from_str(&s).unwrap())
    }

    #[tokio::test]
    async fn linha_vazia_nao_responde() {
        assert!(processar_linha("", &DespachanteCli).await.is_none());
        assert!(processar_linha("   ", &DespachanteCli).await.is_none());
    }

    #[tokio::test]
    async fn json_quebrado_vira_erro_de_parse_com_id_nulo() {
        let r = responder("{quebrado").await.unwrap();
        assert_eq!(r["id"], Value::Null);
        assert_eq!(r["error"]["code"], -32700);
    }

    #[tokio::test]
    async fn notificacao_e_resposta_sem_method_nao_respondem() {
        // Ambos os casos vêm do envelope compartilhado — aqui prova o
        // transporte repassando sem inventar resposta.
        assert!(
            processar_linha(
                r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
                &DespachanteCli,
            )
            .await
            .is_none(),
            "notificação não tem id — responder corromperia o framing"
        );
        assert!(
            processar_linha(r#"{"id":1,"result":{}}"#, &DespachanteCli)
                .await
                .is_none(),
            "resposta do servidor não tem `method` — ignora"
        );
    }

    #[tokio::test]
    async fn initialize_e_tools_list_passam_pelo_envelope_compartilhado() {
        let r = responder(
            r#"{"jsonrpc":"2.0","id":7,"method":"initialize","params":{"protocolVersion":"2024-11-05"}}"#,
        )
        .await
        .unwrap();
        assert_eq!(r["id"], 7);
        assert_eq!(
            r["result"]["protocolVersion"], "2024-11-05",
            "eco do cliente"
        );
        assert_eq!(r["result"]["serverInfo"]["name"], "flux");

        let r = responder(r#"{"jsonrpc":"2.0","id":3,"method":"tools/list"}"#)
            .await
            .unwrap();
        let tools = r["result"]["tools"].as_array().unwrap();
        assert!(
            tools.len() >= 9,
            "catálogo do flux-common anunciado pelo stdio"
        );

        let r = responder(r#"{"jsonrpc":"2.0","id":4,"method":"resources/list"}"#)
            .await
            .unwrap();
        assert_eq!(r["error"]["code"], -32601);
    }
}
