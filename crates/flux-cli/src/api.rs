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

// --------------------------------------------------------- redroom (21/10)

/// Espelho do corpo da API de controle (`GET`/`POST /redroom`).
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub struct RedroomOut {
    pub redroom: bool,
}

#[derive(Debug, Serialize)]
struct RedroomIn {
    on: bool,
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

    /// POST /redroom — liga (`on: true`) ou desliga a contenção GLOBAL: o
    /// router inteiro do fluxd atrás da página estática, nenhum container
    /// acordado/proxado. A API fica de pé FORA do router (listener próprio),
    /// então o desligar funciona mesmo com a contenção no ar.
    pub async fn redroom_definir(&self, on: bool) -> Result<RedroomOut, ApiError> {
        let corpo = serde_json::to_string(&RedroomIn { on }).map_err(|e| ApiError::Protocolo {
            msg: format!("serializando redroom: {e}"),
        })?;
        let r = self.call("POST", "/redroom", Some(corpo)).await?;
        self.json_out(r)
    }

    /// GET /redroom — estado atual do modo.
    pub async fn redroom_status(&self) -> Result<RedroomOut, ApiError> {
        let r = self.call("GET", "/redroom", None).await?;
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
mod tests;
