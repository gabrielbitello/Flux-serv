//! Envelope MCP (22/10) — FONTE ÚNICA consumida por DOIS transportes: stdio
//! no flux-cli (`flux mcp`, processo local que fala com a API de controle
//! pelo túnel SSH) e HTTP LAN-only no fluxd (`:7430/mcp`, dispatch direto
//! pelos handlers internos). O protocolo (initialize/ping/tools) e o
//! catálogo de tools moram AQUI; transporte e execução das tools moram em
//! cada crate — o que é compartilhado não pode divergir.

use serde_json::{Value, json};

/// Versão anunciada quando o cliente não manda a dele (a resposta ECOA a
/// versão pedida — o cliente é quem fala o protocolo que entende).
pub const PROTOCOLO_PADRAO: &str = "2025-06-18";

/// Quem EXECUTA as tools. Cada transporte implementa com a fonte dele:
/// o flux-cli via API de controle (HTTP + túnel); o fluxd pelos handlers
/// internos. Falha de execução = `Err(causa)` — vira resultado `isError`
/// com a causa, NUNCA erro de protocolo nem panic.
pub trait Despachante {
    fn chamar(
        &self,
        nome: &str,
        args: &Value,
    ) -> impl std::future::Future<Output = Result<Value, String>> + Send;
}

/// Catálogo anunciado no `tools/list`. Toda tool aceita `projeto` (string) —
/// o servidor MCP é long-lived, então o cwd não é contexto confiável.
pub fn catalogo() -> Vec<Value> {
    vec![
        ferramenta(
            "flux_projetos",
            "Lista os projetos registrados no fluxd (name, framework, hosts). Comece por aqui quando não souber o nome do projeto.",
            json!({}),
            &[],
        ),
        ferramenta(
            "flux_status",
            "Estado do projeto: containers, sono (dev/prod), migrations aplicadas, hosts. Use para saber se o dev está acordado ou dormindo.",
            json!({
                "projeto": texto("Nome do projeto (pasta ou `name` do flux.toml)."),
                "env": texto("Ambiente: dev (padrão) ou prod.")
            }),
            &[],
        ),
        ferramenta(
            "flux_ps",
            "Espelho do `docker compose ps` de TODOS os projetos gerenciados, agrupado com estado agregado (ACORDADO/PARCIAL/DORMINDO).",
            json!({
                "projeto": texto("Filtra por projeto (prefixo do compose project).")
            }),
            &[],
        ),
        ferramenta(
            "flux_sync_status",
            "Saúde da sincronização Windows→VM do projeto. No transporte HTTP (fluxd): a visão do fluxd (status, problemas, rescan pendente). No transporte stdio rodando no Windows: também a sessão mutagen LOCAL com veredito ok/atenção (a sessão local é invisível de dentro da VM — para o veredito completo use `flux sync status` no Windows).",
            json!({
                "dir": texto("Raiz do projeto no Windows (só faz diferença no transporte stdio).")
            }),
            &[],
        ),
        ferramenta(
            "flux_db_status",
            "Banco próprio do projeto ([db] own): container do grupo, databases visíveis e TTL regressivo de cada database de dev.",
            json!({
                "projeto": texto("Nome do projeto.")
            }),
            &[],
        ),
        ferramenta(
            "flux_logs",
            "Cauda dos logs do container (SEM follow). `tail` entre 1 e 1000 (padrão 200).",
            json!({
                "projeto": texto("Nome do projeto."),
                "env": texto("Ambiente: dev (padrão) ou prod."),
                "tail": numero("Linhas do fim (1–1000).")
            }),
            &[],
        ),
        ferramenta(
            "flux_acordar",
            "Acorda o dev do projeto (up + lease) e ESPERA até `esperar_secs` (padrão 60, teto 180) polling o estado. Build frio (primeiro up) pode passar do teto — nesse caso siga com flux_status.",
            json!({
                "projeto": texto("Nome do projeto."),
                "env": texto("Ambiente: dev (padrão) ou prod."),
                "esperar_secs": numero("Quanto esperar pelo estado running (1–180).")
            }),
            &[],
        ),
        ferramenta(
            "flux_db_query",
            "SQL de LEITURA no banco próprio do projeto (SELECT/SHOW/DESCRIBE/EXPLAIN, um statement, sem trava de escrita — o fluxd recusa o resto). Passe `database` para o USE; sem ele, qualifique com `db.tabela`. Saída = TSV cru do mysql -N (sem header).",
            json!({
                "projeto": texto("Nome do projeto (dono do banco próprio)."),
                "sql": texto("Statement único de leitura."),
                "database": texto("Database de contexto (USE).")
            }),
            &["sql"],
        ),
        ferramenta(
            "flux_tool",
            "Roda toolchain de build/dependências NO container de dev da VM: `npm`, `npx`, `composer` ou `php artisan` (migrate, cache:clear, config:clear...). Whitelist — nada além disso. Teto de 120 s do exec.",
            json!({
                "projeto": texto("Nome do projeto."),
                "env": texto("Ambiente: dev (padrão) ou prod."),
                "comando": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Tokens do comando, ex.: [\"php\",\"artisan\",\"cache:clear\"]."
                }
            }),
            &["comando"],
        ),
    ]
}

/// UMA mensagem JSON-RPC já decodificada → a resposta, ou None quando não
/// há o que responder (linha vazia, notification, resposta do próprio
/// servidor). Rotas de protocolo são daqui; tools/call vai pro despachante.
pub async fn responder_mensagem(msg: &Value, despachante: &impl Despachante) -> Option<Value> {
    let metodo = msg.get("method").and_then(Value::as_str)?;
    let id = msg.get("id").cloned();
    // Notificação (sem id): `notifications/initialized` e afins não têm
    // resposta — responder seria corromper o framing/protocolo.
    if id.is_none() || id.as_ref() == Some(&Value::Null) {
        return None;
    }
    // A guarda acima garante Some — requisição SEMPRE ecoa o id do cliente.
    let id = id.unwrap_or(Value::Null);
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let resultado = match metodo {
        "initialize" => Ok(inicializar(&params)),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": catalogo() })),
        "tools/call" => chamar_tool(&params, despachante).await,
        outra => Err((
            -32601,
            format!(
                "método MCP desconhecido: {outra} (suportado: initialize, ping, tools/list, tools/call)"
            ),
        )),
    };
    Some(match resultado {
        Ok(valor) => json!({ "jsonrpc": "2.0", "id": id, "result": valor }),
        Err((codigo, mensagem)) => erro_rpc(id, codigo, &mensagem),
    })
}

/// Roteador das tools: nome de catálogo → despachante; tool fora do catálogo
/// é erro de protocolo (-32602), falha de execução é `isError` com a causa.
async fn chamar_tool(
    params: &Value,
    despachante: &impl Despachante,
) -> Result<Value, (i32, String)> {
    let nome = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| (-32602, "tools/call sem `name`".to_owned()))?;
    if !catalogo().iter().any(|t| t["name"] == json!(nome)) {
        return Err((
            -32602,
            format!("tool desconhecida: {nome} — consulte tools/list"),
        ));
    }
    let args = params.get("arguments").cloned().unwrap_or(json!({}));
    Ok(match despachante.chamar(nome, &args).await {
        Ok(valor) => json!({
            "content": [{ "type": "text", "text": serde_json::to_string_pretty(&valor).unwrap_or_default() }]
        }),
        Err(causa) => json!({
            "content": [{ "type": "text", "text": causa }],
            "isError": true
        }),
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
        "instructions": "Ferramentas da plataforma Flux (dev/prod servers na VM 192.168.140.2). Diagnóstico: flux_status, flux_ps, flux_projetos, flux_sync_status, flux_db_status, flux_logs. Ações: flux_acordar (acorda dev), flux_tool (npm/composer/artisan na VM), flux_db_query (SQL read-only no banco próprio). Passe `projeto` explícito — o servidor não infere projeto pelo diretório."
    })
}

fn erro_rpc(id: Value, codigo: i32, mensagem: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": codigo, "message": mensagem }
    })
}

// ------------------------------------------------------------ montagem pura

fn ferramenta(nome: &str, descricao: &str, propriedades: Value, obrigatorios: &[&str]) -> Value {
    json!({
        "name": nome,
        "description": descricao,
        "inputSchema": {
            "type": "object",
            "properties": propriedades,
            "required": obrigatorios
        }
    })
}

fn texto(descricao: &str) -> Value {
    json!({ "type": "string", "description": descricao })
}

fn numero(descricao: &str) -> Value {
    json!({ "type": "integer", "description": descricao })
}

// Argumento string do envelope (`args.get(...)`): ausente/estranho/vazio → None.
pub fn arg_str(args: &Value, chave: &str) -> Option<String> {
    let bruto = args.get(chave)?.as_str()?.trim();
    if bruto.is_empty() {
        return None;
    }
    Some(bruto.to_owned())
}

/// Whitelist do `flux_tool` (fonte ÚNICA: vale para o transporte stdio do
/// CLI e o HTTP do fluxd — pedido do Gabriel, 22/10): build/install/update
/// com npm e composer, ciclo Laravel com artisan. Primeiro token decide;
/// `php` só passa com `artisan` em segundo.
pub fn comandos_permitidos(cmd: &[String]) -> Result<(), String> {
    let Some(primeira) = cmd.first().map(String::as_str) else {
        return Err(
            "comando vazio — mande um array de tokens, ex.: [\"npm\", \"run\", \"build\"]".into(),
        );
    };
    match primeira {
        "npm" | "npx" | "composer" => Ok(()),
        "php" => {
            if cmd.get(1).map(String::as_str) == Some("artisan") {
                Ok(())
            } else {
                Err("php só pelo `php artisan …` — nada mais roda aqui".into())
            }
        }
        outra => Err(format!(
            "`{outra}` não é toolchain permitida — só npm, npx, composer e php artisan (whitelist)"
        )),
    }
}

/// Agrupa linhas de containers no formato do `flux_ps` (fonte única dos dois
/// transportes): cada linha = objeto com `compose_project`, `service`,
/// `state`, `status`. Saída: `{projetos: [{compose_project, estado,
/// containers: [...]}]}` com o estado agregado ACORDADO/PARCIAL/DORMINDO.
pub fn ps_agrupar(rows: &[Value], filtro: Option<&str>) -> Value {
    let mut grupos: Vec<Value> = Vec::new();
    for r in rows {
        let projeto = r["compose_project"].as_str().unwrap_or_default();
        let nome_curto = projeto.strip_prefix("flux-").unwrap_or(projeto);
        if let Some(f) = filtro
            && !nome_curto.starts_with(f)
        {
            continue;
        }
        let rodando = r["state"] == json!("running");
        match grupos
            .last_mut()
            .filter(|g| g["compose_project"] == r["compose_project"])
        {
            Some(g) => {
                g["containers"].as_array_mut().unwrap().push(r.clone());
                let todos = g["containers"].as_array().unwrap();
                let ativos = todos
                    .iter()
                    .filter(|c| c["state"] == json!("running"))
                    .count();
                g["estado"] = json!(estado_agregado(ativos, todos.len()));
            }
            None => {
                grupos.push(json!({
                    "compose_project": projeto,
                    "estado": estado_agregado(usize::from(rodando), 1),
                    "containers": [r],
                }));
            }
        }
    }
    json!({ "projetos": grupos })
}

/// Estado agregado de um compose project (a semântica do `flux ps`).
fn estado_agregado(rodando: usize, total: usize) -> &'static str {
    if rodando == 0 {
        "DORMINDO"
    } else if rodando == total {
        "ACORDADO"
    } else {
        "PARCIAL"
    }
}

/// Argumento inteiro com teto e padrão (tail/espera) — fora da faixa NÃO é
/// erro: cai no limite mais próximo (a IA acerta pelo description; o teto é
/// defesa, não contrato).
pub fn arg_int(args: &Value, chave: &str, padrao: u64, min: u64, max: u64) -> u64 {
    args.get(chave)
        .and_then(Value::as_u64)
        .unwrap_or(padrao)
        .clamp(min, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Despachante de teste: ecoa o nome — as rotas de protocolo não tocam
    /// nele (initialize/ping/list não executam tool).
    struct Eco;

    impl Despachante for Eco {
        async fn chamar(&self, nome: &str, _args: &Value) -> Result<Value, String> {
            Ok(json!({ "eco": nome }))
        }
    }

    async fn responder(texto: &str) -> Option<Value> {
        let msg: Value = serde_json::from_str(texto).unwrap();
        responder_mensagem(&msg, &Eco).await
    }

    fn pedido(metodo: &str, id: Value, params: Value) -> String {
        serde_json::json!({ "jsonrpc": "2.0", "method": metodo, "id": id, "params": params })
            .to_string()
    }

    #[tokio::test]
    async fn sem_method_ou_notification_nao_respondem() {
        // Resposta do servidor (só id/result) não tem `method` — ignora.
        let msg: Value = serde_json::from_str(r#"{"id":1,"result":{}}"#).unwrap();
        assert!(responder_mensagem(&msg, &Eco).await.is_none());
        // Notificação (sem id) não tem resposta.
        let msg: Value =
            serde_json::from_str(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
                .unwrap();
        assert!(responder_mensagem(&msg, &Eco).await.is_none());
    }

    #[tokio::test]
    async fn initialize_ecoa_versao_e_se_apresenta() {
        let r = responder(&pedido(
            "initialize",
            json!(7),
            json!({ "protocolVersion": "2024-11-05" }),
        ))
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
        let r = responder(&pedido("initialize", json!(1), json!({})))
            .await
            .unwrap();
        assert_eq!(r["result"]["protocolVersion"], PROTOCOLO_PADRAO);
    }

    #[tokio::test]
    async fn ping_e_tools_list_respondem() {
        let r = responder(&pedido("ping", json!(2), json!({})))
            .await
            .unwrap();
        assert_eq!(r["result"], json!({}));
        let r = responder(&pedido("tools/list", json!(3), json!({})))
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
    async fn metodo_e_tool_desconhecidos_tem_erro_classificado() {
        let r = responder(&pedido("resources/list", json!(4), json!({})))
            .await
            .unwrap();
        assert_eq!(r["error"]["code"], -32601, "método fora do MCP de tools");
        let r = responder(&pedido(
            "tools/call",
            json!(5),
            json!({ "name": "flux_nao_existe", "arguments": {} }),
        ))
        .await
        .unwrap();
        assert_eq!(r["error"]["code"], -32602, "tool fora do catálogo");
        // tools/call SEM name também é -32602.
        let r = responder(&pedido("tools/call", json!(6), json!({})))
            .await
            .unwrap();
        assert_eq!(r["error"]["code"], -32602);
    }

    #[tokio::test]
    async fn tools_call_com_despachante_ecoa_erro_como_iserror() {
        struct Falha;
        impl Despachante for Falha {
            async fn chamar(&self, _: &str, _: &Value) -> Result<Value, String> {
                Err("fluxd inacessível: causa real".into())
            }
        }
        let msg = serde_json::json!({
            "jsonrpc": "2.0", "method": "tools/call", "id": 9,
            "params": { "name": "flux_status", "arguments": {} }
        });
        let r = responder_mensagem(&msg, &Falha).await.unwrap();
        assert_eq!(r["id"], 9);
        assert_eq!(
            r["result"]["isError"],
            json!(true),
            "falha de execução NUNCA é erro de protocolo"
        );
        assert!(
            r["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("causa real"),
            "a causa vai verbatim"
        );
    }

    #[test]
    fn whitelist_so_deixa_toolchain_de_build() {
        let cmd = |t: &[&str]| -> Vec<String> { t.iter().map(|s| s.to_string()).collect() };
        for aceito in [
            vec!["npm", "run", "build"],
            vec!["npm", "install"],
            vec!["npm", "update"],
            vec!["npx", "vite", "build"],
            vec!["composer", "install"],
            vec!["composer", "update"],
            vec!["php", "artisan", "migrate", "--force"],
            vec!["php", "artisan", "cache:clear"],
        ] {
            assert!(
                comandos_permitidos(&cmd(&aceito)).is_ok(),
                "deveria passar: {aceito:?}"
            );
        }
    }

    #[test]
    fn whitelist_barra_o_resto_e_php_sem_artisan() {
        let cmd = |t: &[&str]| -> Vec<String> { t.iter().map(|s| s.to_string()).collect() };
        for recusado in [
            vec!["bash"],
            vec!["sh", "-c", "rm -rf /"],
            vec!["rm", "-rf", "/"],
            vec!["docker", "exec", "x"],
            vec!["php", "-r", "evil();"],
            vec!["php", "script.php"],
            vec!["mysql"],
            vec![],
        ] {
            assert!(
                comandos_permitidos(&cmd(&recusado)).is_err(),
                "deveria recusar: {recusado:?}"
            );
        }
    }

    #[test]
    fn ps_agrupar_agrega_estado_e_filtra() {
        let linha = |proj: &str, svc: &str, state: &str| json!({ "compose_project": proj, "service": svc, "state": state, "status": "Up" });
        let rows = vec![
            linha("flux-vippers-dev", "php", "running"),
            linha("flux-djm-dev", "php", "exited"),
        ];
        let r = ps_agrupar(&rows, None);
        assert_eq!(r["projetos"].as_array().unwrap().len(), 2);
        assert_eq!(r["projetos"][0]["estado"], json!("ACORDADO"));
        assert_eq!(r["projetos"][1]["estado"], json!("DORMINDO"));
        // filtro por nome curto (sem o prefixo flux-)
        let r = ps_agrupar(&rows, Some("vippers"));
        let projetos = r["projetos"].as_array().unwrap();
        assert_eq!(projetos.len(), 1);
        assert_eq!(projetos[0]["compose_project"], json!("flux-vippers-dev"));
        // PARCIAL: 1 de 2 rodando no mesmo compose project
        let rows = vec![
            linha("flux-x-dev", "php", "running"),
            linha("flux-x-dev", "queue", "exited"),
        ];
        let r = ps_agrupar(&rows, None);
        assert_eq!(r["projetos"][0]["estado"], json!("PARCIAL"));
        assert_eq!(r["projetos"][0]["containers"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn args_fora_da_faixa_caicam_no_limite() {
        let args = json!({ "tail": 999_999 });
        assert_eq!(arg_int(&args, "tail", 200, 1, 1000), 1000);
        let vazio = json!({});
        assert_eq!(arg_int(&vazio, "tail", 200, 1, 1000), 200, "padrão");
        assert_eq!(arg_int(&vazio, "esperar_secs", 60, 1, 180), 60);
    }

    #[test]
    fn arg_str_ignora_vazio_e_nao_string() {
        assert_eq!(
            arg_str(&json!({ "projeto": " vipers " }), "projeto").as_deref(),
            Some("vipers"),
            "trim no argumento"
        );
        assert_eq!(arg_str(&json!({ "projeto": "  " }), "projeto"), None);
        assert_eq!(arg_str(&json!({ "projeto": 7 }), "projeto"), None);
    }
}
