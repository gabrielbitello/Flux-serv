use super::*;
use axum::Json;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};

const TOKEN: &str = "segredo";

/// Mock do fluxd (07) com as rotas que o CLI fala — mesmo contrato, sem
/// Docker/SQLite: serve em 127.0.0.1:0 e devolve a base.
async fn mock_fluxd() -> String {
    /// Gate do mock (espelho do `auth` do fluxd): Bearer do header tem de
    /// bater com o token configurado — não com "algum token presente".
    async fn gate_com(
        token: String,
        h: HeaderMap,
    ) -> Result<(), (StatusCode, Json<serde_json::Value>)> {
        let ok = h
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v == format!("Bearer {token}"));
        if ok {
            Ok(())
        } else {
            Err((
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "token da API de controle recusado"})),
            ))
        }
    }

    async fn healthz() -> Json<serde_json::Value> {
        Json(serde_json::json!({"status": "ok", "version": "mock"}))
    }

    async fn register(
        State(token): State<String>,
        headers: HeaderMap,
        Json(b): Json<serde_json::Value>,
    ) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
        gate_com(token, headers).await?;
        let name = b["name"].as_str().unwrap_or("").to_owned();
        Ok(Json(serde_json::json!({
            "name": name,
            "framework": "laravel",
            "prod_host": format!("{name}.bitello.xyz"),
            "dev_host": format!("{name}-d.bitello.xyz"),
        })))
    }

    async fn project_status(
        State(token): State<String>,
        Path(name): Path<String>,
        headers: HeaderMap,
    ) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
        gate_com(token, headers).await?;
        if name == "vipers" {
            Ok(Json(serde_json::json!({
                "name": "vipers",
                "framework": "laravel",
                "prod_host": "vipers.bitello.xyz",
                "dev_host": "vipers-d.bitello.xyz",
                "toml_hash": "hash-teste",
                "created_at": "2026-10-13 12:00:00",
                "sleep": {"dev": {"mode": "awake", "last_interaction": null, "lease_until": 9_999_999_999i64, "cooldown_until": null},
                          "prod": {"mode": null, "last_interaction": null, "lease_until": null, "cooldown_until": null}},
                "migrations": {"dev": [], "prod": []},
            })))
        } else {
            Err((
                StatusCode::NOT_FOUND,
                Json(
                    serde_json::json!({"error": format!("projeto {name:?} não registrado no fluxd")}),
                ),
            ))
        }
    }

    async fn lease(
        State(token): State<String>,
        Path(name): Path<String>,
        headers: HeaderMap,
        Json(b): Json<serde_json::Value>,
    ) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
        gate_com(token, headers).await?;
        Ok(Json(serde_json::json!({
            "name": name,
            "env": b["env"],
            "ttl_secs": b["ttl_secs"],
            "lease_until": 9_999_999_999i64,
        })))
    }

    async fn futura() -> (StatusCode, Json<serde_json::Value>) {
        (
            StatusCode::NOT_IMPLEMENTED,
            Json(
                serde_json::json!({"error": "endpoint da API de controle ainda não implementado — fases F5–F17 (docs/specs/)"}),
            ),
        )
    }

    /// POST /projects/{name}/down?env= — contrato CLI ↔ fluxd. O query
    /// SEM env é recusado (o CLI tem de mandar sempre); projeto
    /// "quebrado" devolve o 500 do contrato com a causa real do compose.
    async fn parar_projeto(
        State(token): State<String>,
        Path(name): Path<String>,
        Query(q): Query<std::collections::HashMap<String, String>>,
        headers: HeaderMap,
    ) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
        gate_com(token, headers).await?;
        if q.get("env").map(String::as_str) != Some("dev") {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": format!("query do down divergente do contrato: env={:?} (esperado Some(\"dev\"))", q.get("env"))
                })),
            ));
        }
        if name == "quebrado" {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "docker compose stop falhou: exit 1"})),
            ));
        }
        Ok(Json(serde_json::json!({"stopped": true, "project": name})))
    }

    /// GET /projects/{name}/logs — contrato: sem follow, text/plain cru;
    /// com follow, corpo SSE (`data: <linha>` + linha vazia). O mock
    /// confere o query EXATO que o CLI deve mandar (env, tail, follow) —
    /// divergiu, 500 com a divergência apontada.
    async fn logs_stream(
        State(token): State<String>,
        Path(_name): Path<String>,
        Query(q): Query<std::collections::HashMap<String, String>>,
        headers: HeaderMap,
    ) -> Result<Response, (StatusCode, Json<serde_json::Value>)> {
        gate_com(token, headers).await?;
        if q.get("env").map(String::as_str) != Some("dev") {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": format!("query dos logs divergente do contrato: env={:?} (esperado Some(\"dev\"))", q.get("env"))
                })),
            ));
        }
        if q.get("tail").map(String::as_str) != Some("200") {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": format!("query dos logs divergente do contrato: tail={:?} (esperado Some(\"200\"))", q.get("tail"))
                })),
            ));
        }
        if q.get("follow").map(String::as_str) == Some("true") {
            let corpo = axum::body::Body::from("data: linha 1\n\ndata: linha 2\n\n");
            return Ok(axum::response::Response::builder()
                .header(header::CONTENT_TYPE, "text/event-stream")
                .body(corpo)
                .unwrap());
        }
        Ok((
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            "linha 1\nlinha 2\nlinha 3\n",
        )
            .into_response())
    }

    /// POST /projects/{name}/exec — contrato: corpo `{"cmd":[…]}` (o CLI
    /// tem de mandar `cmd`, não `command`); vazio → 422. Resposta
    /// text/plain com `exit=` na ÚLTIMA linha.
    async fn exec_projeto(
        State(token): State<String>,
        Path(_name): Path<String>,
        headers: HeaderMap,
        Json(b): Json<serde_json::Value>,
    ) -> Result<Response, (StatusCode, Json<serde_json::Value>)> {
        gate_com(token, headers).await?;
        let Some(cmd) = b["cmd"].as_array() else {
            return Err((
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(serde_json::json!({"error": "campo cmd ausente (contrato: {\"cmd\":[…]})"})),
            ));
        };
        if cmd.is_empty() {
            return Err((
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(
                    serde_json::json!({"error": "cmd vazio — nada para executar no container app"}),
                ),
            ));
        }
        let recebido: Vec<&str> = cmd.iter().filter_map(|v| v.as_str()).collect();
        let esperado = ["php", "artisan", "migrate", "--force"];
        if recebido != esperado {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": format!("corpo do exec divergente do contrato: cmd={recebido:?} (esperado {esperado:?})")
                })),
            ));
        }
        Ok((
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            "migrando…\nDIRED  2026_10_14_000000_teste\nexit=0",
        )
            .into_response())
    }

    async fn indisponivel() -> (StatusCode, Json<serde_json::Value>) {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(
                serde_json::json!({"error": "api de controle não configurada: não há api_token_hash no estado"}),
            ),
        )
    }

    async fn redroom_set(
        State(token): State<String>,
        headers: HeaderMap,
        Json(b): Json<serde_json::Value>,
    ) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
        gate_com(token, headers).await?;
        Ok(Json(serde_json::json!({ "redroom": b["on"] })))
    }

    async fn redroom_status(
        State(token): State<String>,
        headers: HeaderMap,
    ) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
        gate_com(token, headers).await?;
        Ok(Json(serde_json::json!({ "redroom": true })))
    }

    let app: Router = Router::new()
        .route("/healthz", get(healthz))
        .route(
            "/projects",
            post(register).get(|| async { futura_sem_auth().await }),
        )
        .route("/projects/{name}", get(project_status))
        .route("/projects/{name}/lease", post(lease))
        .route("/projects/{name}/down", post(parar_projeto))
        .route("/projects/{name}/logs", get(logs_stream))
        .route("/projects/{name}/exec", post(exec_projeto))
        .route("/redroom", post(redroom_set).get(redroom_status))
        .route("/fechada", get(indisponivel))
        .fallback(futura)
        .with_state(TOKEN.to_owned());

    async fn futura_sem_auth() -> (StatusCode, Json<serde_json::Value>) {
        futura().await
    }

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

fn cliente(base: &str, token: Option<&str>) -> ApiClient {
    ApiClient::new(base.to_owned(), token.map(str::to_owned))
}

#[tokio::test]
async fn ciclo_register_status_lease_no_mock() {
    let base = mock_fluxd().await;
    let cli = cliente(&base, Some(TOKEN));

    let h = cli.health().await.unwrap();
    assert_eq!(h.status, "ok");

    let r = cli
        .register("vipers", "name = \"vipers\"", "hash-teste")
        .await
        .unwrap();
    assert_eq!(r.dev_host, "vipers-d.bitello.xyz");

    let lista = cli.list_projects().await.unwrap_err(); // mock lista é 501
    assert!(matches!(lista, ApiError::BackendFuturo { .. }));

    let st = cli.status("vipers").await.unwrap();
    assert_eq!(st.sleep.dev.lease_until, Some(9_999_999_999));
    assert_eq!(st.migrations.dev.len(), 0);

    let l = cli.lease("vipers", "dev", 120).await.unwrap();
    assert_eq!(l.ttl_secs, 120);
}

#[tokio::test]
async fn token_ausente_vira_erro_de_token() {
    let base = mock_fluxd().await;
    let cli = cliente(&base, None);
    let err = cli.status("vipers").await.unwrap_err();
    assert!(matches!(err, ApiError::Token), "erro: {err}");
    assert!(err.to_string().contains("FLUX_API_TOKEN"), "causa: {err}");
}

/// Redroom (21/10): status, ligar e desligar no mock — e sem token a rota
/// fecha 401 (o interruptor da plataforma não tem janela aberta).
#[tokio::test]
async fn redroom_liga_desliga_e_consulta_no_mock() {
    let base = mock_fluxd().await;
    let cli = cliente(&base, Some(TOKEN));

    let st = cli.redroom_status().await.unwrap();
    assert!(st.redroom, "mock nasce ligado");

    let off = cli.redroom_definir(false).await.unwrap();
    assert!(!off.redroom, "off ecoa desligado");
    let on = cli.redroom_definir(true).await.unwrap();
    assert!(on.redroom, "on ecoa ligado");

    let anon = cliente(&base, None);
    let err = anon.redroom_status().await.unwrap_err();
    assert!(
        matches!(err, ApiError::Token),
        "sem token = ApiError::Token: {err}"
    );
    let err = anon.redroom_definir(true).await.unwrap_err();
    assert!(
        matches!(err, ApiError::Token),
        "sem token no POST também: {err}"
    );
}

#[tokio::test]
async fn conexao_recusada_vira_erro_com_causa() {
    // Porta livre: abre e fecha um listener — nada escuta ali.
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let cli = cliente(&format!("http://{addr}"), None);
    let err = cli.health().await.unwrap_err();
    assert!(matches!(err, ApiError::Conexao { .. }), "erro: {err}");
    assert!(err.to_string().contains("inacessível"), "causa: {err}");
    assert_eq!(err.exit_code(), 1);
}

#[tokio::test]
async fn endpoint_futuro_do_fallback_vira_exit_2() {
    let base = mock_fluxd().await;
    let cli = cliente(&base, Some(TOKEN));
    // restart ainda não tem backend real — cai no fallback 501 do mock
    // (down já é contrato real e tem rota própria no mock).
    let err = cli
        .call_raw("POST", "/projects/vipers/restart", None)
        .await
        .unwrap_err();
    assert!(matches!(err, ApiError::BackendFuturo { .. }), "erro: {err}");
    assert_eq!(err.exit_code(), 2, "herda semântica dos stubs");
    assert!(err.to_string().contains("ainda não implementado"));
}

#[tokio::test]
async fn fluxd_fechado_503_vira_indisponivel_com_a_mensagem() {
    let base = mock_fluxd().await;
    let cli = cliente(&base, Some(TOKEN));
    let err = cli.call_raw("GET", "/fechada", None).await.unwrap_err();
    assert!(matches!(err, ApiError::Indisponivel { .. }), "erro: {err}");
    assert!(err.to_string().contains("não configurada"), "causa: {err}");
}

#[tokio::test]
async fn guarda_de_lease_para_no_drop() {
    let base = mock_fluxd().await;
    let cli = cliente(&base, Some(TOKEN));
    // Primeiro lease bem-sucedido é pré-requisito da guarda.
    cli.lease("vipers", "dev", 60).await.unwrap();
    let guard = crate::api::manter_lease(cli, "vipers".into(), "dev".into(), 60);
    guard.parar.send(true).unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(guard.tarefa.is_finished(), "tarefa para ao receber o sinal");
}

#[tokio::test]
async fn down_do_contrato_para_e_reporta() {
    let base = mock_fluxd().await;
    let cli = cliente(&base, Some(TOKEN));
    let r = cli.parar("vipers", "dev").await.unwrap();
    assert!(r.stopped, "contrato: 200 traz stopped=true");
    assert_eq!(r.project, "vipers");
    // 500 do contrato traz a causa do compose — vira Status com exit 1
    // (06: erro nunca sai com exit 0).
    let err = cli.parar("quebrado", "dev").await.unwrap_err();
    assert!(
        err.to_string().contains("docker compose stop falhou"),
        "causa real no erro: {err}"
    );
    assert_eq!(err.exit_code(), 1);
}

#[tokio::test]
async fn down_sem_env_no_query_e_recusado_pelo_mock() {
    let base = mock_fluxd().await;
    let cli = cliente(&base, Some(TOKEN));
    // O contrato manda o CLI sempre levar ?env= — sem ele, o mock
    // devolve 500 apontando o que faltou.
    let err = cli
        .call_raw("POST", "/projects/vipers/down", None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("env=None"), "causa: {err}");
}

#[tokio::test]
async fn logs_sem_follow_traz_texto_cru() {
    let base = mock_fluxd().await;
    let cli = cliente(&base, Some(TOKEN));
    let corpo = cli
        .call_raw(
            "GET",
            "/projects/vipers/logs?env=dev&tail=200&follow=false",
            None,
        )
        .await
        .unwrap();
    assert_eq!(corpo, "linha 1\nlinha 2\nlinha 3\n");
}

#[tokio::test]
async fn logs_com_follow_abre_stream_sse_e_confere_query() {
    let base = mock_fluxd().await;
    let cli = cliente(&base, Some(TOKEN));
    let res = cli
        .abrir_stream("/projects/vipers/logs?env=dev&tail=200&follow=true")
        .await
        .unwrap();
    let bytes = http_body_util::BodyExt::collect(res.into_body())
        .await
        .unwrap()
        .to_bytes();
    let corpo = String::from_utf8_lossy(&bytes);
    assert!(corpo.contains("data: linha 1"), "corpo: {corpo}");
    assert!(corpo.contains("data: linha 2"), "corpo: {corpo}");
    // Query divergente (tail errado) → 500 do mock com a divergência.
    let err = cli
        .abrir_stream("/projects/vipers/logs?env=dev&tail=50&follow=true")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("tail="), "causa: {err}");
}

#[tokio::test]
async fn exec_manda_cmd_e_le_exit_da_ultima_linha() {
    let base = mock_fluxd().await;
    let cli = cliente(&base, Some(TOKEN));
    let cmd: Vec<String> = ["php", "artisan", "migrate", "--force"]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    let (saida, code) = cli.executar("vipers", "dev", &cmd).await.unwrap();
    assert_eq!(code, 0, "última linha exit=0 vira código do comando");
    assert!(saida.contains("migrando"), "saída: {saida}");
    assert!(
        !saida.contains("exit="),
        "a linha do exit não vaza como saída do comando: {saida}"
    );
    // cmd vazio → 422 do contrato → Recusado com a causa.
    let err = cli.executar("vipers", "dev", &[]).await.unwrap_err();
    assert!(matches!(err, ApiError::Recusado { .. }), "erro: {err}");
    assert!(err.to_string().contains("cmd vazio"), "causa: {err}");
}

#[test]
fn dado_sse_data_com_e_sem_espaco() {
    assert_eq!(dado_de_data("data: linha 1"), Some("linha 1"));
    assert_eq!(dado_de_data("data:linha 1"), Some("linha 1"));
    assert_eq!(dado_de_data(""), None, "linha vazia é separador SSE");
    assert_eq!(dado_de_data(": keepalive"), None, "comentário SSE");
    assert_eq!(dado_de_data("event: log"), None, "outro campo SSE");
    assert_eq!(
        dado_de_data("data:  dois espaços"),
        Some(" dois espaços"),
        "só UM espaço é consumido (RFC do SSE)"
    );
}

#[test]
fn exec_separa_saida_do_exit() {
    assert_eq!(
        separar_exec("migrando…\nexit=0"),
        Some(("migrando…".to_owned(), 0))
    );
    assert_eq!(
        separar_exec("a\nb\nexit=127\n"),
        Some(("a\nb".to_owned(), 127)),
        "\\n final depois do exit não atrapalha"
    );
    assert_eq!(separar_exec("exit=2\n"), Some((String::new(), 2)));
    assert_eq!(
        separar_exec("exit=1\nok\nexit=0"),
        Some(("exit=1\nok".to_owned(), 0)),
        "exit= no MEIO da saída não confunde — a ÚLTIMA linha vence"
    );
    assert_eq!(separar_exec("sem linha de exit"), None);
    assert_eq!(
        separar_exec("exit=abc"),
        None,
        "exit não numérico é protocolo quebrado"
    );
    assert_eq!(separar_exec(""), None);
}

#[test]
fn schema_in_serializa_seed_somente_quando_explícita() {
    let sem = serde_json::to_string(&SchemaIn {
        env: "dev",
        files: &[],
        seed: None,
    })
    .unwrap();
    assert_eq!(
        sem, r#"{"env":"dev","files":[]}"#,
        "sem flag: campo ausente — o fluxd decide pelo flux.toml"
    );
    let com = serde_json::to_string(&SchemaIn {
        env: "dev",
        files: &[],
        seed: Some(false),
    })
    .unwrap();
    assert_eq!(
        com, r#"{"env":"dev","files":[],"seed":false}"#,
        "flag explícita vence o toml"
    );
}
