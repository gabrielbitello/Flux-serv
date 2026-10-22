//! MCP sobre HTTP, LAN-only (22/10 noite): o zcode do Windows conecta por
//! URL (`http://192.168.140.2:7430/mcp`, Bearer do token da API) e a IA
//! opera a plataforma SEM processo do flux no host. O listener vive no IP
//! host-only da VirtualBox — NUNCA 0.0.0.0 nem atrás do Cloudflare (a API
//! de controle continua 127.0.0.1; aqui é uma superfície NOVA, com o MESMO
//! gate de token da :7420, fail-closed). O dispatch chama os HANDLERS da
//! API de controle direto (a mesma fonte do que o CLI vê via HTTP); o
//! envelope e o catálogo são os do `flux-common::mcp` — fonte única com o
//! stdio do CLI.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{Value, json};

use crate::AppState;
use crate::api;

/// Porta do MCP HTTP (LAN-only, host-only da VirtualBox).
pub const MCP_PORT: u16 = 7430;
/// IP do bind: a interface host-only da VM. Nunca 0.0.0.0 (a superfície é
/// LAN da plataforma, não roteável).
pub const MCP_BIND_IP: &str = "192.168.140.2";

/// Teto do corpo de resposta lido dos handlers (logs/toolchain podem ser
/// grandes; 16 MiB é folga acima de qualquer teto interno).
const TETO_CORPO: usize = 16 * 1024 * 1024;

/// Lease do `flux_acordar` (o mesmo contrato da CLI: cobre uma sessão de
/// trabalho; renovado só enquanto houver interação real).
const LEASE_ACORDAR_SECS: i64 = 15 * 60;

pub fn build_mcp(app: Arc<AppState>) -> Router {
    Router::new()
        .route(
            "/mcp",
            post(endpoint)
                .get(somente_post)
                .delete(somente_post)
                .put(somente_post),
        )
        // O MESMO gate da API de controle (mesmo token, mesmo fail-closed
        // 503 sem api_token_hash) — a recusa vira evento na trilha, idem.
        .layer(middleware::from_fn_with_state(app.clone(), api::auth::auth))
        .with_state(app)
}

/// O transporte fala streamable HTTP no modo mais simples: POST com UM
/// JSON-RPC, resposta `application/json`. SSE do servidor (GET) não é
/// ofertado — 405, permitido pela spec.
async fn somente_post() -> Response {
    api::erro(
        StatusCode::METHOD_NOT_ALLOWED,
        "o MCP HTTP aceita só POST /mcp (stream iniciado pelo servidor não é ofertado)",
    )
}

async fn endpoint(State(app): State<Arc<AppState>>, corpo: axum::body::Bytes) -> Response {
    let msg: Value = match serde_json::from_slice(&corpo) {
        Ok(v) => v,
        Err(e) => {
            // Erro de PARSE é do transporte (-32700, id nulo — JSON-RPC).
            return Json(json!({
                "jsonrpc": "2.0",
                "id": null,
                "error": { "code": -32700, "message": format!("JSON inválido: {e}") }
            }))
            .into_response();
        }
    };
    let despachante = DespachanteFluxd(app);
    match flux_common::mcp::responder_mensagem(&msg, &despachante).await {
        Some(resposta) => Json(resposta).into_response(),
        // Notificação (sem id): 202 sem corpo — o contrato do streamable HTTP.
        None => StatusCode::ACCEPTED.into_response(),
    }
}

// ------------------------------------------------------------------ dispatch

struct DespachanteFluxd(Arc<AppState>);

impl flux_common::mcp::Despachante for DespachanteFluxd {
    async fn chamar(&self, nome: &str, args: &Value) -> Result<Value, String> {
        let app = self.0.clone();
        match nome {
            "flux_projetos" => corpo_json(api::list_projects(State(app)).await).await,
            "flux_status" => {
                let projeto = projeto_de(args)?;
                let st = corpo_json(api::project_status(State(app), Path(projeto.clone())).await)
                    .await?;
                Ok(status_curado(&st))
            }
            "flux_ps" => {
                let rows = corpo_json(api::listar_containers(State(app)).await).await?;
                let filtro = flux_common::mcp::arg_str(args, "projeto");
                let linhas = rows.as_array().cloned().unwrap_or_default();
                Ok(flux_common::mcp::ps_agrupar(&linhas, filtro.as_deref()))
            }
            "flux_sync_status" => visao_sync(app, args).await,
            "flux_db_status" => {
                let projeto = projeto_de(args)?;
                corpo_json(api::db_status(State(app), Path(projeto)).await).await
            }
            "flux_logs" => logs(app, args).await,
            "flux_acordar" => acordar(app, args).await,
            "flux_db_query" => {
                let projeto = projeto_de(args)?;
                let sql = flux_common::mcp::arg_str(args, "sql")
                    .ok_or_else(|| "argumento obrigatório ausente: `sql`".to_owned())?;
                let database = flux_common::mcp::arg_str(args, "database");
                let out = corpo_json(
                    api::db_query(
                        State(app),
                        Path(projeto.clone()),
                        Json(flux_common::protocol::DbQueryIn { sql, database }),
                    )
                    .await,
                )
                .await?;
                Ok(json!({ "projeto": projeto, "saida": out["saida"] }))
            }
            "flux_tool" => tool(app, args).await,
            outra => unreachable!("catálogo do flux-common e roteador divergiram: {outra}"),
        }
    }
}

/// O servidor MCP da VM não tem cwd de projeto — `projeto` é obrigatório
/// nas tools de projeto (o catálogo diz isso nas descriptions).
fn projeto_de(args: &Value) -> Result<String, String> {
    flux_common::mcp::arg_str(args, "projeto").ok_or_else(|| {
        "argumento `projeto` ausente — o MCP da VM não infere projeto pelo diretório".to_owned()
    })
}

/// Estado do projeto na MESMA forma curada do transporte stdio (fields do
/// `ProjectStatusOut` que a IA usa; hash/datas de registro ficam de fora).
fn status_curado(st: &Value) -> Value {
    json!({
        "name": st["name"],
        "framework": st["framework"],
        "prod_host": st["prod_host"],
        "dev_host": st["dev_host"],
        "dev_container_state": st["dev_container_state"],
        "sono": {
            "dev": { "mode": st["sleep"]["dev"]["mode"], "lease_until": st["sleep"]["dev"]["lease_until"] },
            "prod": { "mode": st["sleep"]["prod"]["mode"], "lease_until": st["sleep"]["prod"]["lease_until"] },
        },
        "migrations_aplicadas": {
            "dev": st["migrations"]["dev"].as_array().map(Vec::len),
            "prod": st["migrations"]["prod"].as_array().map(Vec::len),
        }
    })
}

/// Sync pela visão do fluxd (reports do `flux watch`). A sessão mutagen
/// LOCAL do Windows é invisível daqui — a nota vai na resposta pra nunca
/// parecer veredito completo.
async fn visao_sync(app: Arc<AppState>, args: &Value) -> Result<Value, String> {
    let st = corpo_json(api::sync_status(State(app)).await).await?;
    let nota = "visão do fluxd (reports do flux watch) — a sessão mutagen LOCAL do Windows é invisível da VM; veredito completo com `flux sync status` no Windows (ou MCP stdio)";
    let projetos = st["projects"].as_array().cloned().unwrap_or_default();
    match flux_common::mcp::arg_str(args, "projeto") {
        Some(p) => {
            let row = projetos
                .iter()
                .find(|r| r["project"] == json!(p))
                .cloned()
                .unwrap_or(Value::Null);
            Ok(json!({ "projeto": p, "fluxd": row, "nota": nota }))
        }
        None => Ok(json!({ "projetos": projetos, "nota": nota })),
    }
}

async fn logs(app: Arc<AppState>, args: &Value) -> Result<Value, String> {
    let projeto = projeto_de(args)?;
    let env = flux_common::mcp::arg_str(args, "env").unwrap_or_else(|| "dev".to_owned());
    let tail = flux_common::mcp::arg_int(args, "tail", 200, 1, 1000);
    let q = crate::ciclo::CicloQuery {
        env: Some(env.clone()),
        tail: Some(tail.to_string()),
        follow: Some("false".to_owned()),
    };
    let texto =
        corpo_texto(crate::ciclo::logs(State(app), Path(projeto.clone()), Query(q)).await).await?;
    Ok(json!({
        "projeto": projeto,
        "env": env,
        "tail": tail,
        "logs": texto,
    }))
}

/// Acorda: lease (mantém de pé como uma sessão de CLI) + up + poll do
/// estado até `running` ou teto.
async fn acordar(app: Arc<AppState>, args: &Value) -> Result<Value, String> {
    let projeto = projeto_de(args)?;
    let env = flux_common::mcp::arg_str(args, "env").unwrap_or_else(|| "dev".to_owned());
    let esperar = flux_common::mcp::arg_int(args, "esperar_secs", 60, 1, 180);
    corpo_json(
        api::grant_lease(
            State(app.clone()),
            Path(projeto.clone()),
            Json(api::LeaseBody {
                env: env.clone(),
                ttl_secs: LEASE_ACORDAR_SECS,
            }),
        )
        .await,
    )
    .await?;
    let aceito =
        corpo_json(api::subir(State(app.clone()), Path(projeto.clone()), None).await).await?;

    let inicio = tokio::time::Instant::now();
    let mut estado_final: Option<String> = None;
    while inicio.elapsed() < Duration::from_secs(esperar) {
        tokio::time::sleep(Duration::from_secs(3)).await;
        if let Ok(st) =
            corpo_json(api::project_status(State(app.clone()), Path(projeto.clone())).await).await
            && let Some(estado) = st["dev_container_state"].as_str().map(str::to_owned)
        {
            let running = estado == "running";
            estado_final = Some(estado);
            if running {
                break;
            }
        }
    }
    let acordou = estado_final.as_deref() == Some("running");
    Ok(json!({
        "aceito": aceito["started"],
        "project": aceito["project"],
        "env": env,
        "estado_final": estado_final,
        "acordou": acordou,
        "esperado_secs": inicio.elapsed().as_secs(),
        "dica": if acordou {
            "dev acordado — flux_status detalha"
        } else {
            "não chegou a `running` no teto — build frio demora minutos; siga com flux_status/flux_logs"
        }
    }))
}

/// Toolchain no container de dev: mesma whitelist do flux-common (fonte
/// única com o stdio), exec pelo handler da API, exit separado da saída.
async fn tool(app: Arc<AppState>, args: &Value) -> Result<Value, String> {
    let projeto = projeto_de(args)?;
    let env = flux_common::mcp::arg_str(args, "env").unwrap_or_else(|| "dev".to_owned());
    let cmd: Vec<String> = args
        .get("comando")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    flux_common::mcp::comandos_permitidos(&cmd)?;
    let q = crate::ciclo::CicloQuery {
        env: Some(env.clone()),
        tail: None,
        follow: None,
    };
    let texto = corpo_texto(
        crate::ciclo::exec(
            State(app),
            Path(projeto.clone()),
            Query(q),
            Json(crate::ciclo::ExecBody {
                cmd: Some(cmd.clone()),
            }),
        )
        .await,
    )
    .await?;
    let (saida, exit) = separar_exit(&texto).ok_or_else(|| {
        format!(
            "resposta do exec sem a linha final exit={{código}}: {}",
            &texto.chars().take(2_000).collect::<String>()
        )
    })?;
    Ok(json!({
        "projeto": projeto,
        "env": env,
        "comando": cmd,
        "exit": exit,
        "saida": saida,
    }))
}

/// Separa a ÚLTIMA linha `exit={code}` do corpo do exec (contrato do
/// handler do fluxd — o lado CLI tem o par dele, `separar_exec`).
fn separar_exit(corpo: &str) -> Option<(String, i32)> {
    let ultima = corpo.lines().rev().find(|l| !l.trim().is_empty())?.trim();
    let code: i32 = ultima.strip_prefix("exit=")?.parse().ok()?;
    let pos = corpo.rfind(ultima)?;
    let saida = corpo[..pos].trim_end_matches(['\n', '\r']);
    Some((saida.to_owned(), code))
}

// ------------------------------------------------------------- corpo/resposta

async fn corpo_json(resp: Response) -> Result<Value, String> {
    let texto = corpo_texto(resp).await?;
    serde_json::from_str(&texto).map_err(|e| {
        format!(
            "corpo fora do protocolo (JSON esperado): {e} — {}",
            &texto.chars().take(300).collect::<String>()
        )
    })
}

async fn corpo_texto(resp: Response) -> Result<String, String> {
    let (partes, corpo) = resp.into_parts();
    let bytes = axum::body::to_bytes(corpo, TETO_CORPO)
        .await
        .map_err(|e| format!("lendo o corpo da resposta: {e}"))?;
    let texto = String::from_utf8_lossy(&bytes).into_owned();
    if partes.status.is_success() {
        Ok(texto)
    } else {
        Err(causa_do_erro(partes.status, &texto))
    }
}

/// Erro do handler = JSON `{"error": causa}`; a causa vai VERBATIM no
/// `isError` da tool (nada de mensagem genérica engolindo o 422/404).
fn causa_do_erro(status: StatusCode, corpo: &str) -> String {
    let msg = serde_json::from_str::<Value>(corpo)
        .ok()
        .and_then(|v| v["error"].as_str().map(str::to_owned))
        .unwrap_or_else(|| corpo.trim().to_owned());
    format!("fluxd respondeu {}: {msg}", status.as_u16())
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_common::config::GlobalConfig;
    use http_body_util::BodyExt;
    use http_body_util::Full;
    use hyper::body::Bytes;
    use rusqlite::params;

    type TestClient = hyper_util::client::legacy::Client<
        hyper_util::client::legacy::connect::HttpConnector,
        Full<Bytes>,
    >;

    /// O MESMO padrão dos testes da API: socket real com State::in_memory
    /// (sem Docker) e o gate de token — exercita bind, auth e as rotas do
    /// jeito que o zcode vai falar.
    async fn servidor(token: Option<&str>) -> (String, Arc<AppState>) {
        let st = crate::state::State::in_memory().unwrap();
        if let Some(t) = token {
            let h = api::sha256_hex(t.as_bytes());
            st.with(|c| {
                c.execute(
                    "INSERT INTO settings(k, v) VALUES (?1, ?2)",
                    params![api::auth::TOKEN_KEY, h],
                )
            })
            .unwrap();
        }
        let app = Arc::new(AppState::new(st, &GlobalConfig::default(), None));
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let serve_app = app.clone();
        tokio::spawn(async move {
            axum::serve(listener, build_mcp(serve_app)).await.unwrap();
        });
        (format!("http://{addr}"), app)
    }

    fn cliente() -> TestClient {
        hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
            .build_http()
    }

    async fn post(cli: &TestClient, base: &str, token: Option<&str>, body: &str) -> (u16, String) {
        let mut b = hyper::Request::builder()
            .method(hyper::Method::POST)
            .uri(format!("{base}/mcp"))
            .header(hyper::header::CONTENT_TYPE, "application/json")
            .header(hyper::header::ACCEPT, "application/json");
        if let Some(t) = token {
            b = b.header(hyper::header::AUTHORIZATION, format!("Bearer {t}"));
        }
        let res = cli
            .request(b.body(Full::new(Bytes::from(body.to_owned()))).unwrap())
            .await
            .unwrap();
        let status = res.status().as_u16();
        let bytes = BodyExt::collect(res.into_body()).await.unwrap().to_bytes();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    #[tokio::test]
    async fn gate_igual_a_api_sem_token_fecha_e_401_com_token_errado() {
        let (base, _app) = servidor(None).await;
        let cli = cliente();
        let (s, c) = post(
            &cli,
            &base,
            None,
            r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#,
        )
        .await;
        assert_eq!(s, 503, "fail-closed sem api_token_hash: {c}");
        let (base, _app) = servidor(Some("segredo")).await;
        let (s, c) = post(
            &cli,
            &base,
            Some("errado"),
            r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#,
        )
        .await;
        assert_eq!(s, 401, "{c}");
    }

    #[tokio::test]
    async fn initialize_ping_e_tools_list_no_http() {
        let (base, _app) = servidor(Some("segredo")).await;
        let cli = cliente();
        let (s, c) = post(
            &cli,
            &base,
            Some("segredo"),
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05"}}"#,
        )
        .await;
        assert_eq!(s, 200, "{c}");
        let v: Value = serde_json::from_str(&c).unwrap();
        assert_eq!(
            v["result"]["protocolVersion"], "2024-11-05",
            "eco do cliente"
        );

        let (s, c) = post(
            &cli,
            &base,
            Some("segredo"),
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        )
        .await;
        assert_eq!(s, 200, "{c}");
        let v: Value = serde_json::from_str(&c).unwrap();
        assert!(v["result"]["tools"].as_array().unwrap().len() >= 9);

        // Notificação: 202 SEM corpo (contrato do streamable HTTP).
        let res = post(
            &cli,
            &base,
            Some("segredo"),
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        )
        .await;
        assert_eq!(res.0, 202, "{}", res.1);
    }

    #[tokio::test]
    async fn get_no_mcp_e_405() {
        let (base, _app) = servidor(Some("segredo")).await;
        let cli = cliente();
        let req = hyper::Request::builder()
            .method(hyper::Method::GET)
            .uri(format!("{base}/mcp"))
            .header(hyper::header::AUTHORIZATION, "Bearer segredo")
            .body(Full::new(Bytes::new()))
            .unwrap();
        let res = cli.request(req).await.unwrap();
        assert_eq!(res.status().as_u16(), 405);
    }

    #[tokio::test]
    async fn tools_sem_docker_falam_verdade_projetos_vazio_e_404_curado() {
        let (base, _app) = servidor(Some("segredo")).await;
        let cli = cliente();
        // Estado vazio (sem Docker): flux_projetos responde [] — verdade, não erro.
        let (s, c) = post(
            &cli,
            &base,
            Some("segredo"),
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"flux_projetos","arguments":{}}}"#,
        )
        .await;
        assert_eq!(s, 200, "{c}");
        let v: Value = serde_json::from_str(&c).unwrap();
        // Sucesso NÃO carrega isError (só falha); o conteúdo é a lista vazia —
        // verdade do estado, não erro disfarçado.
        assert!(v["result"].get("isError").is_none(), "{v}");
        assert_eq!(
            v["result"]["content"][0]["text"].as_str().unwrap().trim(),
            "[]"
        );

        // Projeto fantasma: 404 do handler vira isError com a causa.
        let (s, c) = post(
            &cli,
            &base,
            Some("segredo"),
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"flux_status","arguments":{"projeto":"fantasma"}}}"#,
        )
        .await;
        assert_eq!(s, 200, "{c}");
        let v: Value = serde_json::from_str(&c).unwrap();
        assert_eq!(v["result"]["isError"], json!(true));
        assert!(
            v["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("404"),
            "causa verbatim: {v}"
        );

        // Whitelist do flux_tool recusa antes de tocar no exec.
        let (s, c) = post(
            &cli,
            &base,
            Some("segredo"),
            r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"flux_tool","arguments":{"projeto":"x","comando":["bash","-c","boom"]}}}"#,
        )
        .await;
        assert_eq!(s, 200, "{c}");
        let v: Value = serde_json::from_str(&c).unwrap();
        assert_eq!(v["result"]["isError"], json!(true));
        assert!(
            v["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("whitelist"),
            "{v}"
        );
    }

    #[tokio::test]
    async fn json_quebrado_vira_32700_no_transporte() {
        let (base, _app) = servidor(Some("segredo")).await;
        let cli = cliente();
        let (s, c) = post(&cli, &base, Some("segredo"), "{quebrado").await;
        assert_eq!(s, 200, "{c}");
        let v: Value = serde_json::from_str(&c).unwrap();
        assert_eq!(v["error"]["code"], -32700);
    }

    #[test]
    fn separar_exit_e_o_par_do_contrato_do_exec() {
        let (saida, code) = separar_exit("linha 1\nlinha 2\nexit=42\n").unwrap();
        assert_eq!(code, 42);
        assert_eq!(saida, "linha 1\nlinha 2");
        assert!(separar_exit("sem a linha do exit").is_none());
    }
}
