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

/// Teto do CLI para o exec (contrato CLI ↔ fluxd): o fluxd corta o exec em
/// 120 s; a folga de 30 s é só de transporte — estourou, é erro com causa.
const EXEC_TIMEOUT: Duration = Duration::from_secs(150);

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
    /// Verdade do ENGINE (None = fluxd sem Docker) — o modo da linha de sono
    /// nasce 'awake' e NÃO serve de sinal de container no ar.
    #[serde(default)]
    pub dev_container_state: Option<String>,
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
#[derive(Debug, Deserialize)]
pub struct ContainerRowOut {
    pub compose_project: String,
    pub service: String,
    #[allow(dead_code)] // espelho do corpo da API — tabela do ps usa projeto/service
    pub container: String,
    pub state: String,
    pub status: String,
}

#[derive(Debug, Serialize)]
pub struct FreshIn {
    pub env: String,
    pub seed: Option<bool>,
}

#[derive(Debug, Serialize)]
struct UpIn<'a> {
    env: Option<&'a str>,
}

#[derive(Debug, Deserialize)]
pub struct UpAceitoOut {
    #[allow(dead_code)] // espelho do corpo da API — consumidor é o log do fluxd
    pub started: bool,
    #[allow(dead_code)]
    pub project: String,
    #[allow(dead_code)]
    pub compose_project: String,
}

/// POST /projects/{name}/down — corpo 200 do contrato CLI ↔ fluxd.
#[derive(Debug, Deserialize)]
pub struct DownOut {
    pub stopped: bool,
    pub project: String,
}

#[derive(Debug, Serialize)]
struct SchemaIn<'a> {
    env: &'a str,
    files: &'a [(String, String)],
    /// `--seed`/`--no-seed` explícitos no `flux up` (05). Ausente (None) =
    /// o fluxd decide pelo `seed_on_rebuild` do flux.toml — por isso o campo
    /// some do JSON em vez de ir como null.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed: Option<bool>,
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

/// POST /sync/report (contrato FIXO da 02, serviço `flux watch`): estado de
/// UMA sessão local para o painel. `status` é o vocabulário do contrato:
/// "watching" (sessão viva) ou "absent" (projeto conhecido do fluxd sem
/// sessão local). `rescan_applied=true` é o que LIMPA a flag de rescan do
/// projeto no fluxd — só vai no report pós-`sync::reset`.
#[derive(Debug, Serialize)]
pub struct SyncReportIn {
    pub project: String,
    pub session: String,
    pub status: String,
    pub paused: bool,
    /// Arquivos no último snapshot (`None` = sem scan ainda; vai null).
    pub files: Option<u64>,
    pub problems: u64,
    pub last_error: Option<String>,
    pub rescan_applied: bool,
}

/// GET /sync/status — espelho do contrato: uma linha por projeto que o fluxd
/// conhece, com a flag de rescan pedida pelo painel.
#[derive(Debug, Deserialize)]
pub struct SyncStatusOut {
    pub projects: Vec<SyncProjectRowOut>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub struct SyncProjectRowOut {
    pub project: String,
    pub status: String,
    pub paused: bool,
    pub files: Option<u64>,
    pub problems: u64,
    pub last_error: Option<String>,
    pub updated_at: String,
    pub rescan_requested: bool,
}

/// Payload de uma linha SSE `data: …` (contrato do /logs com follow: cada
/// evento é `data: <linha crua>` seguido de linha vazia). Aceita `data: X` e
/// `data:X`; separador vazio, comentário `: ping` e outros campos (`event:`,
/// `id:`) não são payload → None.
pub fn dado_de_data(linha: &str) -> Option<&str> {
    let resto = linha.strip_prefix("data:")?;
    Some(resto.strip_prefix(' ').unwrap_or(resto))
}

/// Separa a resposta do exec (contrato CLI ↔ fluxd): texto cru com a ÚLTIMA
/// linha `exit={code}`. Devolve (saída_sem_a_linha_do_exit, código); None
/// quando o corpo não termina com a linha do exit — protocolo quebrado vira
/// erro, nunca exit 0 inventado (06: nada silencioso).
pub fn separar_exec(corpo: &str) -> Option<(String, i32)> {
    let ultima = corpo.lines().rev().find(|l| !l.trim().is_empty())?.trim();
    let code: i32 = ultima.strip_prefix("exit=")?.parse().ok()?;
    let pos = corpo.rfind(ultima)?;
    let saida = corpo[..pos].trim_end_matches(['\n', '\r']);
    Some((saida.to_owned(), code))
}

/// Corta texto longo para caber em mensagem de erro (o teto do exec é 256 KB
/// — nunca despejar isso no terminal).
fn truncar(s: &str, max: usize) -> String {
    let total = s.chars().count();
    if total <= max {
        return s.to_owned();
    }
    let cortado: String = s.chars().take(max).collect();
    format!("{cortado}… (+{total} caracteres no total)")
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
        self.call_com_timeout(method, path, body, TIMEOUT).await
    }

    /// `call` com timeout ajustável: o exec legítimo pode rodar 120 s no
    /// fluxd (contrato CLI ↔ fluxd) — o teto de 30 s das rotas rápidas não
    /// serve aqui.
    async fn call_com_timeout(
        &self,
        method: &str,
        path: &str,
        body: Option<String>,
        teto: Duration,
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

        let res = tokio::time::timeout(teto, self.http.request(req))
            .await
            .map_err(|_| ApiError::Conexao {
                url: self.base.clone(),
                causa: format!("timeout de {} s esperando o fluxd", teto.as_secs()),
            })?
            .map_err(|e| ApiError::Conexao {
                url: self.base.clone(),
                causa: e.to_string(),
            })?;
        let status = res.status().as_u16();
        let bytes = tokio::time::timeout(teto, BodyExt::collect(res.into_body()))
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

    /// GET /containers (o "docker ps" do Flux): todos os gerenciados.
    pub async fn containers(&self) -> Result<Vec<ContainerRowOut>, ApiError> {
        let r = self.call("GET", "/containers", None).await?;
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
    /// `seed`: None = fluxd decide pelo flux.toml; Some = vence o toml.
    pub async fn schema(
        &self,
        name: &str,
        env: &str,
        files: &[(String, String)],
        seed: Option<bool>,
    ) -> Result<SchemaOut, ApiError> {
        let corpo = serde_json::to_string(&SchemaIn { env, files, seed }).map_err(|e| {
            ApiError::Protocolo {
                msg: format!("serializando schema: {e}"),
            }
        })?;
        let r = self
            .call("POST", &format!("/projects/{name}/schema"), Some(corpo))
            .await?;
        self.json_out(r)
    }

    // --------------------------- contrato CLI ↔ fluxd (down / logs / exec)

    /// POST /projects/{name}/down?env= (contrato CLI ↔ fluxd): o fluxd roda
    /// `docker compose -p flux-{name}-{env} stop` e responde 200 com
    /// `{"stopped":true,"project":…}`; 500 traz `{"error":causa}` — vira
    /// `ApiError::Status` com exit 1.
    pub async fn parar(&self, name: &str, env: &str) -> Result<DownOut, ApiError> {
        let r = self
            .call("POST", &format!("/projects/{name}/down?env={env}"), None)
            .await?;
        self.json_out(r)
    }

    /// GET de logs com follow (contrato CLI ↔ fluxd): abre o stream SSE SEM
    /// timeout de corpo — o stream vive até o cliente cortar, e um timeout
    /// aqui mataria o `flux logs -f` (06/07). Status de erro lê o corpo com
    /// timeout e classifica como qualquer outra rota.
    pub async fn abrir_stream(
        &self,
        path: &str,
    ) -> Result<hyper::Response<hyper::body::Incoming>, ApiError> {
        let mut b = hyper::Request::builder().method(hyper::Method::GET);
        if let Some(t) = &self.token {
            b = b.header(hyper::header::AUTHORIZATION, format!("Bearer {t}"));
        }
        let req = b
            .uri(format!("{}{}", self.base, path))
            .body(Full::new(Bytes::new()))
            .map_err(|e| ApiError::Protocolo {
                msg: format!("montando request {path}: {e}"),
            })?;
        let res = tokio::time::timeout(TIMEOUT, self.http.request(req))
            .await
            .map_err(|_| ApiError::Conexao {
                url: self.base.clone(),
                causa: format!(
                    "timeout de {} s esperando os headers do stream",
                    TIMEOUT.as_secs()
                ),
            })?
            .map_err(|e| ApiError::Conexao {
                url: self.base.clone(),
                causa: e.to_string(),
            })?;
        if res.status().is_success() {
            return Ok(res);
        }
        let status = res.status().as_u16();
        let bytes = tokio::time::timeout(TIMEOUT, BodyExt::collect(res.into_body()))
            .await
            .map_err(|_| ApiError::Conexao {
                url: self.base.clone(),
                causa: "timeout lendo o corpo do erro do stream".into(),
            })?
            .map_err(|e| ApiError::Conexao {
                url: self.base.clone(),
                causa: e.to_string(),
            })?
            .to_bytes();
        let r = Reply {
            status,
            body: String::from_utf8_lossy(&bytes).into_owned(),
        };
        match self.decodificar(r) {
            // Inalcançável para status de erro (decodificar só devolve Ok em
            // 2xx) — defensivo para nunca mascarar um status como sucesso.
            Ok(_) => Err(ApiError::Protocolo {
                msg: format!("stream de {path} terminou com status {status} sem corpo de erro"),
            }),
            Err(e) => Err(e),
        }
    }

    /// POST /projects/{name}/exec?env= (contrato CLI ↔ fluxd): corpo
    /// `{"cmd":[…]}`, resposta text/plain com a ÚLTIMA linha `exit={code}` —
    /// separada aqui para o CLI repassar o exit code real (06). O teto de
    /// 120 s do exec é do fluxd; o do CLI é só de transporte.
    pub async fn executar(
        &self,
        name: &str,
        env: &str,
        cmd: &[String],
    ) -> Result<(String, i32), ApiError> {
        #[derive(Serialize)]
        struct ExecIn<'a> {
            cmd: &'a [String],
        }
        let corpo = serde_json::to_string(&ExecIn { cmd }).map_err(|e| ApiError::Protocolo {
            msg: format!("serializando exec: {e}"),
        })?;
        let r = self
            .call_com_timeout(
                "POST",
                &format!("/projects/{name}/exec?env={env}"),
                Some(corpo),
                EXEC_TIMEOUT,
            )
            .await?;
        let texto = self.decodificar(r)?;
        separar_exec(&texto).ok_or_else(|| ApiError::Protocolo {
            msg: format!(
                "resposta do exec sem a linha final exit={{código}}: {}",
                truncar(&texto, 2_000)
            ),
        })
    }

    // ------------------------------------------ sync report/status (02, watch)

    /// POST /sync/report — contrato 204 sem corpo; só o erro classificado
    /// interessa ao chamador (o watch loga com origem e segue o loop).
    pub async fn sync_report(&self, r: &SyncReportIn) -> Result<(), ApiError> {
        let corpo = serde_json::to_string(r).map_err(|e| ApiError::Protocolo {
            msg: format!("serializando sync report: {e}"),
        })?;
        let resposta = self.call("POST", "/sync/report", Some(corpo)).await?;
        self.decodificar(resposta).map(|_| ())
    }

    /// GET /sync/status — estado dos projetos no fluxd (flag de rescan
    /// inclusive).
    pub async fn sync_status(&self) -> Result<SyncStatusOut, ApiError> {
        let r = self.call("GET", "/sync/status", None).await?;
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
                    Json(
                        serde_json::json!({"error": "campo cmd ausente (contrato: {\"cmd\":[…]})"}),
                    ),
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
}
