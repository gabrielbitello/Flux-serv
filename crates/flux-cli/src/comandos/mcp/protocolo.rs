//! Envelope MCP sobre stdio (22/10): JSON-RPC 2.0 com uma mensagem por linha.
//! Métodos do ciclo de vida (`initialize`, `notifications/initialized`,
//! `ping`, `tools/list`, `tools/call`) — sem SDK: o protocolo de tools é
//! pequeno e o envelope é std, serde_json e nada mais. Toda saída de
//! diagnóstico vai pra stderr; stdout é EXCLUSIVAMENTE o protocolo.

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use super::ferramentas;

/// Versão anunciada quando o cliente não manda a dele (a resposta ECOA a
/// versão pedida — o cliente é quem fala o protocolo que entende).
const PROTOCOLO_PADRAO: &str = "2025-06-18";

pub(crate) async fn servir() {
    let mut entrada = BufReader::new(tokio::io::stdin()).lines();
    let mut saida = tokio::io::stdout();
    loop {
        let linha = match entrada.next_line().await {
            Ok(Some(l)) => l,
            Ok(None) => break, // EOF: o cliente fechou — despedida limpa
            Err(e) => {
                eprintln!("mcp: erro lendo o stdin: {e}");
                break;
            }
        };
        let Some(resposta) = processar_linha(&linha).await else {
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
async fn processar_linha(linha: &str) -> Option<String> {
    let texto = linha.trim();
    if texto.is_empty() {
        return None;
    }
    let msg: Value = match serde_json::from_str(texto) {
        Ok(v) => v,
        Err(e) => {
            return Some(erro_rpc(
                Value::Null,
                -32700,
                &format!("JSON inválido: {e}"),
            ));
        }
    };
    // Resposta do servidor pra mim ou batch estranho: sem `method` não há
    // requisição — ignora (nunca ecoa lixo pro canal de protocolo).
    let metodo = msg.get("method").and_then(Value::as_str)?;
    let id = msg.get("id").cloned();
    // Notificação (sem id): `notifications/initialized` e afins não têm
    // resposta — responder seria corromper o framing.
    if id.is_none() || id.as_ref() == Some(&Value::Null) {
        return None;
    }
    // Guarda acima garante Some — requisição SEMPRE ecoa o id do cliente.
    let id = id.unwrap_or(Value::Null);
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let resultado = match metodo {
        "initialize" => Ok(inicializar(&params)),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": ferramentas::catalogo() })),
        "tools/call" => ferramentas::chamar(&params).await,
        outra => Err((
            -32601,
            format!(
                "método MCP desconhecido: {outra} (suportado: initialize, ping, tools/list, tools/call)"
            ),
        )),
    };
    Some(match resultado {
        Ok(valor) => json!({ "jsonrpc": "2.0", "id": id, "result": valor }).to_string(),
        Err((codigo, mensagem)) => erro_rpc(id, codigo, &mensagem),
    })
}

/// Handshake: ecoa a versão do cliente (ou a padrão), anuncia tools sem
/// mudança de lista e se identifica. `instructions` dá o mapa mental das
/// tools pro cliente mostrar pra IA.
fn inicializar(params: &Value) -> Value {
    let versao = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .unwrap_or(PROTOCOLO_PADRAO)
        .to_owned();
    json!({
        "protocolVersion": versao,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": {
            "name": "flux",
            "version": env!("CARGO_PKG_VERSION")
        },
        "instructions": "Ferramentas da plataforma Flux (dev/prod servers na VM 192.168.140.2). Diagnóstico: flux_status, flux_ps, flux_projetos, flux_sync_status, flux_db_status, flux_logs. Ações: flux_acordar (acorda dev), flux_tool (npm/composer/artisan na VM), flux_db_query (SQL read-only no banco próprio). Passe `projeto` explícito quando o chamador não estiver na pasta do projeto."
    })
}

fn erro_rpc(id: Value, codigo: i32, mensagem: &str) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": codigo, "message": mensagem }
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn responder(texto: &str) -> Option<Value> {
        processar_linha(texto)
            .await
            .map(|s| serde_json::from_str(&s).unwrap())
    }

    #[tokio::test]
    async fn linha_vazia_e_lixo_sem_method_nao_respondem() {
        assert!(processar_linha("").await.is_none());
        assert!(processar_linha("   ").await.is_none());
        // Resposta do servidor (só id/result) não tem `method` — ignora.
        assert!(processar_linha(r#"{"id":1,"result":{}}"#).await.is_none());
    }

    #[tokio::test]
    async fn json_quebrado_vira_erro_de_parse_com_id_nulo() {
        let r = responder("{quebrado").await.unwrap();
        assert_eq!(r["id"], Value::Null);
        assert_eq!(r["error"]["code"], -32700);
    }
    #[tokio::test]
    async fn notificacao_sem_id_nao_tem_resposta() {
        assert!(
            processar_linha(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
                .await
                .is_none(),
            "notificação não tem id — responder corromperia o framing"
        );
    }

    #[tokio::test]
    async fn initialize_ecoa_versao_e_se_apresenta() {
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
        assert!(r["result"]["capabilities"]["tools"].is_object());
        // Sem versão no pedido: a padrão entra.
        let r = responder(r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#)
            .await
            .unwrap();
        assert_eq!(r["result"]["protocolVersion"], PROTOCOLO_PADRAO);
    }

    #[tokio::test]
    async fn ping_e_tools_list_respondem() {
        let r = responder(r#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#)
            .await
            .unwrap();
        assert_eq!(r["result"], json!({}));
        let r = responder(r#"{"jsonrpc":"2.0","id":3,"method":"tools/list"}"#)
            .await
            .unwrap();
        let tools = r["result"]["tools"].as_array().unwrap();
        assert!(tools.len() >= 9, "catálogo MVP completo: {}", tools.len());
        for t in tools {
            assert!(t["name"].is_string(), "toda tool tem name: {t}");
            assert_eq!(t["inputSchema"]["type"], "object", "{t}");
        }
    }

    #[tokio::test]
    async fn metodo_desconhecido_e_tool_desconhecida_tem_erro_classificado() {
        let r = responder(r#"{"jsonrpc":"2.0","id":4,"method":"resources/list"}"#)
            .await
            .unwrap();
        assert_eq!(r["error"]["code"], -32601, "método fora do MCP de tools");
        let r = responder(
            r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"flux_nao_existe","arguments":{}}}"#,
        )
        .await
        .unwrap();
        assert_eq!(r["error"]["code"], -32602, "tool fora do catálogo");
    }
}
