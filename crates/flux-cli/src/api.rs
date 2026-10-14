//! Cliente HTTP da API de controle do fluxd (07) — o CLI NUNCA fala com
//! Docker/compose direto (06): tudo passa por :7420. O endpoint vem da env
//! `FLUX_API_URL` (default `http://127.0.0.1:7420`; na instalação real, o
//! túnel SSH do provisionamento aponta para a VM) e o token da env
//! `FLUX_API_TOKEN`, mandado como `Authorization: Bearer` — o fluxd compara o
//! sha256 com o hash do estado (09). Erro é sempre classificado com causa:
//! conexão recusada, token recusado, backend futuro (501) — nada silencioso
//! (06) e nada de retry infinito escondendo falha (09).

use std::time::Duration;

use flux_common::protocol::ProjectSummary;
use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper_util::client::legacy::{Client, connect::HttpConnector};
use hyper_util::rt::TokioExecutor;
use serde::{Deserialize, Serialize};

/// Default da 09 (`base_url = "http://127.0.0.1:7420"`).
pub const DEFAULT_API_URL: &str = "http://127.0.0.1:7420";

/// Timeout por requisição: o fluxd responde rápido (SQLite local); 30 s já é
/// anomalia e vale mensagem de causa, não espera infinita.
const TIMEOUT: Duration = Duration::from_secs(30);

/// Erro classificado do cliente — cada variante vira mensagem pronta no CLI
/// com a origem (CLI→API ou backend do fluxd).
#[derive(Debug)]
pub enum ApiError {
    /// fluxd inacessível (conexão recusada, timeout) — origem: transporte.
    Conexao { url: String, causa: String },
    /// 401: token ausente/divergente do hash do estado.
    Token,
    /// 503: fluxd sem api_token_hash (API fechada, 09) ou sem Docker p/ schema.
    Indisponivel { msg: String },
    /// 501: endpoint ainda não existe no fluxd — fase futura do backend.
    BackendFuturo { msg: String },
    /// 404: projeto não registrado.
    NaoEncontrado { msg: String },
    /// 422: recusa de validação (toml, ttl, ambiente).
    Recusado { msg: String },
    /// Outro status de erro — mensagem do corpo, verbatim.
    Status { status: u16, msg: String },
    /// Resposta fora do formato do protocolo (JSON quebrado etc.).
    Protocolo { msg: String },
}

impl ApiError {
    /// Exit code do comando (06: erro sai com exit ≠ 0 correto). 2 = recusa
    /// "ainda não implementado" (herda a semântica dos stubs); 1 = falha real.
    pub fn exit_code(&self) -> i32 {
        match self {
            ApiError::BackendFuturo { .. } => 2,
            _ => 1,
        }
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::Conexao { url, causa } => write!(
                f,
                "fluxd inacessível em {url}: {causa} — o fluxd está no ar? (fluxd serve na VM; FLUX_API_URL aponta o endpoint)"
            ),
            ApiError::Token => write!(
                f,
                "token recusado pelo fluxd — FLUX_API_TOKEN confere com o api_token_hash do estado? (gerado no `flux setup vm`)"
            ),
            ApiError::Indisponivel { msg } => write!(f, "fluxd indisponível: {msg}"),
            ApiError::BackendFuturo { msg } => {
                write!(f, "backend do comando ainda não existe no fluxd: {msg}")
            }
            ApiError::NaoEncontrado { msg } => write!(f, "não encontrado no fluxd: {msg}"),
            ApiError::Recusado { msg } => write!(f, "recusado pelo fluxd: {msg}"),
            ApiError::Status { status, msg } => write!(f, "fluxd respondeu {status}: {msg}"),
            ApiError::Protocolo { msg } => write!(f, "resposta fora do protocolo: {msg}"),
        }
    }
}

/// Corpo de erro padrão da API de controle (`ApiErrorBody` do protocolo; o
/// tipo de lá só tem Serialize — o cliente precisa do espelho Deserialize).
#[derive(Debug, Deserialize)]
struct CorpoErro {
    error: String,
}

// Espelhos do protocolo (07): estruturas COMPLETAS mesmo quando o CLI não
// imprime todos os campos hoje — o fio é contrato, e campos a menos aqui
// quebrariam em silêncio quando o painel/reuso precisar deles.
#[allow(dead_code)]
/// GET /healthz.
#[derive(Debug, Deserialize)]
pub struct HealthOut {
    pub status: String,
    pub version: String,
}

/// POST /projects.
#[derive(Debug, Serialize)]
struct RegisterIn<'a> {
    name: &'a str,
    toml: &'a str,
    toml_hash: &'a str,
}

#[derive(Debug, Deserialize)]
pub struct RegisterOut {
    pub name: String,
    pub framework: String,
    pub prod_host: String,
    pub dev_host: String,
}

/// GET /projects/{name} — espelho do `ProjectStatus` do fluxd.
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub struct ProjectStatusOut {
    pub name: String,
    pub framework: String,
    pub prod_host: String,
    pub dev_host: String,
    pub toml_hash: String,
    pub created_at: String,
    pub sleep: SleepStatusOut,
    pub migrations: MigrationsStatusOut,
}

#[derive(Debug, Deserialize)]
pub struct SleepStatusOut {
    pub dev: SleepRowOut,
    pub prod: SleepRowOut,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub struct SleepRowOut {
    pub mode: Option<String>,
    pub last_interaction: Option<i64>,
    pub lease_until: Option<i64>,
    pub cooldown_until: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct MigrationsStatusOut {
    pub dev: Vec<MigrationAplicadaOut>,
    pub prod: Vec<MigrationAplicadaOut>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub struct MigrationAplicadaOut {
    pub file: String,
    pub sha256: String,
}

/// POST /projects/{name}/lease.
#[derive(Debug, Serialize)]
struct LeaseIn<'a> {
    env: &'a str,
    ttl_secs: i64,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub struct LeaseOut {
    pub name: String,
    pub env: String,
    pub ttl_secs: i64,
    pub lease_until: i64,
}

/// POST /projects/{name}/schema.
// Contrato pronto e testado contra o mock; entra no fluxo do `flux up`
// completo na F7 (hoje o up não dispara migrate — o fluxd precisa do sync
// de arquivos para o plano de migrations ser real).
#[derive(Debug, Serialize)]
struct UpIn<'a> {
    env: Option<&'a str>,
}

#[derive(Debug, Deserialize)]
pub struct UpAceitoOut {
    pub started: bool,
    pub project: String,
    pub compose_project: String,
}

#[derive(Debug, Serialize)]
struct SchemaIn<'a> {
    env: &'a str,
    files: &'a [(String, String)],
}

/// Espelho do `SchemaReply` (tag = outcome).
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub struct SchemaOut {
    pub outcome: String,
    #[serde(default)]
    pub fresh: bool,
    #[serde(default)]
    pub aplicadas: usize,
    #[serde(default)]
    pub output: String,
}

struct Reply {
    status: u16,
    body: String,
}

/// Cliente da API de controle. Barato de clonar (hyper client é Arc interno).
#[derive(Clone)]
pub struct ApiClient {
    base: String,
    token: Option<String>,
    http: Client<HttpConnector, Full<Bytes>>,
}

impl ApiClient {
    /// `FLUX_API_URL`/config do usuário (09) + `FLUX_API_TOKEN`. Com a URL
    /// DEFAULT (127.0.0.1:7420), garante o túnel SSH antes da primeira
    /// chamada — a API nunca sai do localhost da VM (09), então o CLI sobe
    /// `ssh -L` sob demanda (09: "base_url via forward SSH pelo CLI").
    pub fn from_env() -> Self {
        let env_url = std::env::var("FLUX_API_URL")
            .ok()
            .filter(|s| !s.trim().is_empty());
        let cfg = crate::tunel::config_do_usuario();
        let base = env_url
            .clone()
            .or_else(|| cfg.base_url.clone())
            .unwrap_or_else(|| DEFAULT_API_URL.to_owned());
        let token = std::env::var("FLUX_API_TOKEN")
            .ok()
            .filter(|t| !t.is_empty())
            .or(cfg.token);
        let is_default = env_url.is_none() && base == DEFAULT_API_URL;
        if let Err(e) = crate::tunel::garantir_forward(is_default) {
            // AVISO, não fatal: o erro real (com causa) vem do call() abaixo.
            eprintln!("aviso: túnel SSH para a API: {e}");
        }
        Self::new(base, token)
    }

    pub fn new(base: String, token: Option<String>) -> Self {
        Self {
            base: base.trim_end_matches('/').to_owned(),
            token,
            http: Client::builder(TokioExecutor::new()).build_http(),
        }
    }

    /// Base configurada — para mensagens de diagnóstico.
    pub fn base_url(&self) -> &str {
        &self.base
    }

    async fn call(
        &self,
        method: &str,
        path: &str,
        body: Option<String>,
    ) -> Result<Reply, ApiError> {
        let mut b = hyper::Request::builder().method(
            hyper::Method::from_bytes(method.as_bytes()).map_err(|e| ApiError::Protocolo {
                msg: format!("método {method}: {e}"),
            })?,
        );
        // URI inválida surfaces no .body() abaixo (contrato do http::Builder).
        if let Some(t) = &self.token {
            b = b.header(hyper::header::AUTHORIZATION, format!("Bearer {t}"));
        }
        if body.is_some() {
            b = b.header(hyper::header::CONTENT_TYPE, "application/json");
        }
        let req = b
            .uri(format!("{}{}", self.base, path))
            .body(Full::new(Bytes::from(body.unwrap_or_default())))
            .map_err(|e| ApiError::Protocolo {
                msg: format!("montando request {}: {e}", self.base),
            })?;

        let res = tokio::time::timeout(TIMEOUT, self.http.request(req))
            .await
            .map_err(|_| ApiError::Conexao {
                url: self.base.clone(),
                causa: format!("timeout de {} s esperando o fluxd", TIMEOUT.as_secs()),
            })?
            .map_err(|e| ApiError::Conexao {
                url: self.base.clone(),
                causa: e.to_string(),
            })?;
        let status = res.status().as_u16();
        let bytes = tokio::time::timeout(TIMEOUT, BodyExt::collect(res.into_body()))
            .await
            .map_err(|_| ApiError::Conexao {
                url: self.base.clone(),
                causa: "timeout lendo o corpo da resposta".into(),
            })?
            .map_err(|e| ApiError::Conexao {
                url: self.base.clone(),
                causa: e.to_string(),
            })?
            .to_bytes();
        Ok(Reply {
            status,
            body: String::from_utf8_lossy(&bytes).into_owned(),
        })
    }

    /// Converte status+corpo em sucesso (corpo cru) ou erro classificado. O
    /// JSON de erro tem a mensagem do fluxd verbatim — causa real, nunca
    /// genérica.
    fn decodificar(&self, r: Reply) -> Result<String, ApiError> {
        let msg = serde_json::from_str::<CorpoErro>(&r.body)
            .map(|e| e.error)
            .unwrap_or_else(|_| r.body.trim().to_owned());
        match r.status {
            200..=299 => Ok(r.body),
            401 => Err(ApiError::Token),
            404 => Err(ApiError::NaoEncontrado { msg }),
            422 => Err(ApiError::Recusado { msg }),
            501 => Err(ApiError::BackendFuturo { msg }),
            503 => Err(ApiError::Indisponivel { msg }),
            s => Err(ApiError::Status { status: s, msg }),
        }
    }

    fn json_out<T: serde::de::DeserializeOwned>(&self, r: Reply) -> Result<T, ApiError> {
        let corpo = self.decodificar(r)?;
        serde_json::from_str(&corpo).map_err(|e| ApiError::Protocolo {
            msg: format!("JSON inesperado: {e} — corpo: {corpo}"),
        })
    }

    // ------------------------------------------------------------ endpoints

    pub async fn health(&self) -> Result<HealthOut, ApiError> {
        let r = self.call("GET", "/healthz", None).await?;
        self.json_out(r)
    }

    /// Registro idempotente do projeto (06 passo 3): manda o flux.toml e o
    /// hash local; o fluxd revalida e grava.
    pub async fn register(
        &self,
        name: &str,
        toml: &str,
        toml_hash: &str,
    ) -> Result<RegisterOut, ApiError> {
        let corpo = serde_json::to_string(&RegisterIn {
            name,
            toml,
            toml_hash,
        })
        .map_err(|e| ApiError::Protocolo {
            msg: format!("serializando registro: {e}"),
        })?;
        let r = self.call("POST", "/projects", Some(corpo)).await?;
        self.json_out(r)
    }

    pub async fn list_projects(&self) -> Result<Vec<ProjectSummary>, ApiError> {
        let r = self.call("GET", "/projects", None).await?;
        self.json_out(r)
    }
    pub async fn status(&self, name: &str) -> Result<ProjectStatusOut, ApiError> {
        let r = self.call("GET", &format!("/projects/{name}"), None).await?;
        self.json_out(r)
    }

    /// Lease do sleep manager (04): mantém o ambiente acordado por ttl_secs.
    pub async fn lease(&self, name: &str, env: &str, ttl_secs: i64) -> Result<LeaseOut, ApiError> {
        let corpo =
            serde_json::to_string(&LeaseIn { env, ttl_secs }).map_err(|e| ApiError::Protocolo {
                msg: format!("serializando lease: {e}"),
            })?;
        let r = self
            .call("POST", &format!("/projects/{name}/lease"), Some(corpo))
            .await?;
        self.json_out(r)
    }

    /// POST /projects/{name}/up (06): aceito = 202, o compose roda em
    /// background no fluxd; o CLI apura pelo status.
    pub async fn subir(&self, name: &str, env: &str) -> Result<UpAceitoOut, ApiError> {
        let corpo =
            serde_json::to_string(&UpIn { env: Some(env) }).map_err(|e| ApiError::Protocolo {
                msg: format!("serializando up: {e}"),
            })?;
        let r = self
            .call("POST", &format!("/projects/{name}/up"), Some(corpo))
            .await?;
        self.json_out(r)
    }

    /// Dispara o ciclo de schema (05) com as migrations do disco local.
    pub async fn schema(
        &self,
        name: &str,
        env: &str,
        files: &[(String, String)],
    ) -> Result<SchemaOut, ApiError> {
        let corpo =
            serde_json::to_string(&SchemaIn { env, files }).map_err(|e| ApiError::Protocolo {
                msg: format!("serializando schema: {e}"),
            })?;
        let r = self
            .call("POST", &format!("/projects/{name}/schema"), Some(corpo))
            .await?;
        self.json_out(r)
    }

    // ------------------------------------ rotas cujo backend chega em fases

    /// Chamada genérica para endpoints cujo backend do fluxd vem depois — o
    /// CLI já fala o protocolo; um 501 do fluxd vira `BackendFuturo` com
    /// exit 2, e quando a fase do backend chegar, o comando funciona sem
    /// mudança aqui.
    pub async fn call_raw(
        &self,
        method: &str,
        path: &str,
        body: Option<String>,
    ) -> Result<String, ApiError> {
        let r = self.call(method, path, body).await?;
        self.decodificar(r)
    }
}

/// Guarda de lease (04): renova a cada ttl/2 enquanto o comando vive. No fim
/// (Drop) para de renovar — o lease morre sozinho no TTL, que é exatamente o
/// comportamento da spec (`flux logs -f` segura acordado; Ctrl+C solta).
pub struct LeaseGuard {
    parar: tokio::sync::watch::Sender<bool>,
    tarefa: tokio::task::JoinHandle<()>,
}

impl Drop for LeaseGuard {
    fn drop(&mut self) {
        let _ = self.parar.send(true);
        self.tarefa.abort();
    }
}

pub fn manter_lease(cliente: ApiClient, name: String, env: String, ttl_secs: i64) -> LeaseGuard {
    let (parar, mut olho) = tokio::sync::watch::channel(false);
    let tarefa = tokio::spawn(async move {
        // Renovação a meio do TTL: folga para um tick perdido não deixar o
        // container dormir (o sweep do fluxd consulta lease_until).
        let intervalo = Duration::from_secs((ttl_secs / 2).clamp(5, 300) as u64);
        loop {
            tokio::select! {
                _ = tokio::time::sleep(intervalo) => {
                    if let Err(e) = cliente.lease(&name, &env, ttl_secs).await {
                        // Nada silencioso: a falha aparece, mas não mata o
                        // comando — tenta de novo no próximo tick.
                        eprintln!("aviso: renovação de lease de {name}/{env} falhou: {e}");
                    }
                }
                _ = olho.changed() => break,
            }
        }
    });
    LeaseGuard { parar, tarefa }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Json;
    use axum::Router;
    use axum::extract::{Path, State};
    use axum::http::{HeaderMap, StatusCode, header};
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

        async fn indisponivel() -> (StatusCode, Json<serde_json::Value>) {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(
                    serde_json::json!({"error": "api de controle não configurada: não há api_token_hash no estado"}),
                ),
            )
        }

        let app: Router = Router::new()
            .route("/healthz", get(healthz))
            .route(
                "/projects",
                post(register).get(|| async { futura_sem_auth().await }),
            )
            .route("/projects/{name}", get(project_status))
            .route("/projects/{name}/lease", post(lease))
            .route("/projects/{name}/down", post(futura))
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
        let err = cli
            .call_raw("POST", "/projects/vipers/down", None)
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
}
