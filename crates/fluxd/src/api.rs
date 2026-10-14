//! API de controle :7420 (07) — o bind em 127.0.0.1 vive em `lib.rs`; aqui
//! está só o app axum. Fase F6: registro e leitura de projetos, lease do
//! sleep manager (04) e disparo do ciclo de schema (05).
//!
//! Autenticação (09): o token existe CLARO só no arquivo do usuário; o fluxd
//! guarda `settings.api_token_hash` (sha256 hex) e compara com o sha256 do
//! Bearer recebido. Duas regras de fechamento: (1) sem hash no estado, TODA
//! rota fora de `/healthz` responde 503 "não configurada" — a API nunca nasce
//! aberta; (2) hash presente e Bearer ausente/divergente → 401, com a recusa
//! gravada na trilha de `events` (09, critério 3: falha é visível para o
//! painel). `/healthz` fica fora do gate de propósito: é a sonda da testemunha
//! externa (17) e não expõe nada além de versão.

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::{Path, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use flux_common::config::ProjectToml;
use flux_common::framework::Framework;
use flux_common::protocol::{ApiErrorBody, Env, HealthBody, ProjectSummary};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::AppState;
use crate::db::{self, SchemaOutcome};
use crate::events;
// O extrator `State` do axum ocupa o nome; o estado do banco entra como DbState.
use crate::state::State as DbState;

/// Chave em `settings` com o sha256 do token da API (09: hash no SQLite,
/// token claro só no arquivo do usuário).
const TOKEN_KEY: &str = "api_token_hash";

/// Teto de lease aceito pela API (04): uma sessão de CLI/painel renova em
/// linha; nada legítimo pede meia hora de garantia de uma vez.
pub const LEASE_MAX_SECS: i64 = 6 * 3600;

pub fn build_api(app: Arc<AppState>) -> Router {
    // Rotas de controle nascem JÁ atrás do gate `auth` — endpoint novo não
    // tem janela aberta (a fundação respondia 501 no fallback; o fallback
    // segue lá para rotas de fases futuras, também atrás do gate).
    let protegida = Router::new()
        .route("/projects", post(register).get(list_projects))
        .route("/projects/{name}", get(project_status))
        .route("/projects/{name}/lease", post(grant_lease))
        .route("/projects/{name}/schema", post(run_schema))
        .route("/projects/{name}/up", post(subir))
        // Fallback DENTRO do router protegido: rota de fase futura sem token é
        // 401 (503 sem hash), nunca 501 de graça — probe de superfície sem
        // credencial fica invisível, então o gate grava a recusa no `events`
        // como nas rotas nomeadas (09, critério 3).
        .fallback(not_implemented)
        .layer(middleware::from_fn_with_state(app.clone(), auth));

    Router::new()
        .route("/healthz", get(healthz))
        .merge(protegida)
        .with_state(app)
}

// ------------------------------------------------------------------ gate 09

async fn auth(
    State(app): State<Arc<AppState>>,
    headers: HeaderMap,
    req: Request,
    next: Next,
) -> Response {
    let Some(hash) = stored_token_hash(&app.state) else {
        return erro(
            StatusCode::SERVICE_UNAVAILABLE,
            "api de controle não configurada: não há api_token_hash no estado — gere o token no `flux setup vm` e defina-o no banco (spec 09)",
        );
    };
    let enviado = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(bearer_token);
    // Recusa quando: sem header OU hash divergente (comparação em tempo
    // constante — ver `hash_confere`).
    if enviado.is_none_or(|t| !hash_confere(&sha256_hex(t.as_bytes()), &hash)) {
        // Recusa vira evento (09 critério 3) — o painel mostra tentativa de
        // acesso com token ruim, o CLI recebe a causa.
        events::event(
            &app.state,
            "warn",
            "auth",
            "token da API de controle recusado (header ausente, malformado ou hash divergente)",
        );
        return erro(
            StatusCode::UNAUTHORIZED,
            "token da API de controle recusado — repasse o Bearer gerado no `flux setup vm` (no CLI, env FLUX_API_TOKEN)",
        );
    }
    next.run(req).await
}

/// Lê o hash do token do estado. Qualquer falha de leitura fecha a API
/// (fail closed): erro de banco não pode virar porta aberta.
fn stored_token_hash(st: &DbState) -> Option<String> {
    match st.with(|c| {
        c.query_row(
            "SELECT v FROM settings WHERE k = ?1",
            params![TOKEN_KEY],
            |r| r.get::<_, String>(0),
        )
    }) {
        Ok(h) => Some(h),
        Err(crate::state::StateError::Sql(rusqlite::Error::QueryReturnedNoRows)) => None,
        Err(e) => {
            tracing::error!("não consegui ler {TOKEN_KEY} do estado — recusando requisições: {e}");
            None
        }
    }
}

/// `Authorization: Bearer <token>` → `<token>`.
fn bearer_token(v: &str) -> Option<&str> {
    v.strip_prefix("Bearer ").filter(|t| !t.is_empty())
}

fn sha256_hex(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    let mut s = String::with_capacity(64);
    for b in h.finalize() {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Compara dois hashes hex em TEMPO CONSTANTE (XOR acumulado, sem early-exit):
/// o `!=` de String abandona no primeiro byte divergente e mede timing por
/// byte num processo local. O bind 127.0.0.1 já mitiga (lib.rs), mas o gate
/// de auth usa o padrão — custo trivial. Divergência de tamanho é pública
/// (sha256 hex são sempre 64 chars) e sai cedo sem vazar conteúdo.
fn hash_confere(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn erro(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(ApiErrorBody { error: msg.into() })).into_response()
}

// ----------------------------------------------------------------- handlers

async fn healthz() -> Json<HealthBody> {
    Json(HealthBody {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
    })
}

#[derive(Debug, Deserialize)]
struct RegisterBody {
    name: String,
    /// Conteúdo integral do `flux.toml` (06) — o fluxd é a fonte da verdade
    /// e re-renderiza overrides quando o hash muda (F2+).
    toml: String,
    /// sha256 do toml calculado pelo CLI; o fluxd confia no transporte por
    /// ser o mesmo canal autenticado (compare-aqui seria redundância).
    toml_hash: String,
}

/// POST /projects — registra/atualiza (idempotente). Validação estrita AQUI:
/// gravação de toml que o fluxd não consegue reler depois é typo silencioso
/// adiante (06: chave desconhecida é erro).
async fn register(State(app): State<Arc<AppState>>, Json(b): Json<RegisterBody>) -> Response {
    let nome = b.name.trim();
    if nome.is_empty()
        || !nome
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return erro(
            StatusCode::UNPROCESSABLE_ENTITY,
            "nome de projeto inválido: use minúsculas, dígitos e '-' (vira host {name}.bitello.xyz)",
        );
    }
    let parsed = match ProjectToml::parse_str(&b.toml) {
        Ok(t) => t,
        Err(e) => {
            return erro(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("flux.toml de {nome} inválido: {e}"),
            );
        }
    };
    // node/rust EXIGEM [health] path declarado — contrato validado no
    // registro (06; o mesmo combinado documentado em config.rs).
    if matches!(parsed.framework(), Framework::Node | Framework::Rust)
        && parsed
            .health
            .as_ref()
            .and_then(|h| h.path.as_deref())
            .is_none()
    {
        return erro(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!(
                "framework {} exige [health] path no flux.toml de {nome} (spec 06)",
                parsed.framework()
            ),
        );
    }
    if let Err(e) = app.state.register_project(nome, &b.toml, &b.toml_hash) {
        return erro(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("falha gravando {nome} no estado: {e}"),
        );
    }
    events::event(
        &app.state,
        "info",
        "api",
        &format!("projeto {nome} registrado (toml_hash {})", b.toml_hash),
    );
    Json(ProjectSummary {
        name: nome.to_owned(),
        framework: parsed.framework().to_string(),
        prod_host: app.domain.prod_host(nome),
        dev_host: app.domain.dev_host(nome),
    })
    .into_response()
}

/// GET /projects — lista registrada com resumo derivado do toml gravado.
async fn list_projects(State(app): State<Arc<AppState>>) -> Response {
    let nomes = match app.state.list_projects() {
        Ok(n) => n,
        Err(e) => {
            return erro(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("falha listando projetos: {e}"),
            );
        }
    };
    let resumo: Vec<ProjectSummary> = nomes
        .iter()
        .map(|n| ProjectSummary {
            name: n.clone(),
            framework: framework_label(&app, n),
            prod_host: app.domain.prod_host(n),
            dev_host: app.domain.dev_host(n),
        })
        .collect();
    Json(resumo).into_response()
}

/// Rótulo do framework do toml gravado; parse falho NUNCA vira um framework
/// inventado — "desconhecido" e o erro real aparece no registro seguinte.
fn framework_label(app: &AppState, name: &str) -> String {
    toml_gravado(app, name)
        .ok()
        .and_then(|src| ProjectToml::parse_str(&src).ok())
        .map(|t| t.framework().to_string())
        .unwrap_or_else(|| "desconhecido".into())
}

/// Fonte do toml gravado; erro de ausência vira 404 com causa (`Box` só para
/// o clippy ficar quieto com o tamanho do `Response` no Err).
fn toml_gravado(app: &AppState, name: &str) -> Result<String, Box<Response>> {
    app.state
        .with(|c| {
            c.query_row(
                "SELECT toml FROM projects WHERE name = ?1",
                params![name],
                |r| r.get::<_, String>(0),
            )
        })
        .map_err(|e| {
            Box::new(erro(
                StatusCode::NOT_FOUND,
                format!("projeto {name:?} não registrado no fluxd (leia falhou: {e})"),
            ))
        })
}

#[derive(Debug, Serialize)]
struct ProjectStatus {
    name: String,
    framework: String,
    prod_host: String,
    dev_host: String,
    toml_hash: String,
    created_at: String,
    /// Sono por ambiente (04); `mode: null` = nunca houve sinal.
    sleep: SleepStatus,
    /// Migrations JÁ APLICADAS por ambiente (05). A lista de PENDENTES é
    /// computada contra os arquivos no disco do projeto — que o fluxd só vê
    /// no POST /schema (a sync de arquivos é a F2); aqui vai o estado gravado.
    migrations: MigrationsStatus,
}

#[derive(Debug, Serialize)]
struct SleepStatus {
    dev: SleepRowDto,
    prod: SleepRowDto,
}

#[derive(Debug, Clone, Serialize)]
struct SleepRowDto {
    mode: Option<String>,
    last_interaction: Option<i64>,
    lease_until: Option<i64>,
    cooldown_until: Option<i64>,
}

#[derive(Debug, Serialize)]
struct MigrationsStatus {
    dev: Vec<MigrationAplicada>,
    prod: Vec<MigrationAplicada>,
}

#[derive(Debug, Serialize)]
struct MigrationAplicada {
    file: String,
    sha256: String,
}

/// GET /projects/{name} — status consolidado para `flux status` e painel.
async fn project_status(State(app): State<Arc<AppState>>, Path(name): Path<String>) -> Response {
    let (toml_src, hash, criado) = match app.state.with(|c| {
        c.query_row(
            "SELECT toml, toml_hash, created_at FROM projects WHERE name = ?1",
            params![name],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            },
        )
    }) {
        Ok(v) => v,
        Err(crate::state::StateError::Sql(rusqlite::Error::QueryReturnedNoRows)) => {
            return erro(
                StatusCode::NOT_FOUND,
                format!("projeto {name:?} não registrado no fluxd — rode `flux up` no repo dele"),
            );
        }
        Err(e) => {
            return erro(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("falha lendo {name} do estado: {e}"),
            );
        }
    };
    let framework = ProjectToml::parse_str(&toml_src)
        .map(|t| t.framework().to_string())
        .unwrap_or_else(|_| "desconhecido".into());

    let mut sleep_dev = SleepRowDto {
        mode: None,
        last_interaction: None,
        lease_until: None,
        cooldown_until: None,
    };
    let mut sleep_prod = sleep_dev.clone();
    for (env, alvo) in [(Env::Dev, &mut sleep_dev), (Env::Prod, &mut sleep_prod)] {
        match app.state.sleep_row(&name, env) {
            Ok((last, lease, cooldown, mode)) => {
                *alvo = SleepRowDto {
                    mode,
                    last_interaction: last,
                    lease_until: lease,
                    cooldown_until: cooldown,
                };
            }
            Err(e) => {
                return erro(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("falha lendo sono de {name}/{env}: {e}"),
                );
            }
        }
    }

    let mut migs = MigrationsStatus {
        dev: Vec::new(),
        prod: Vec::new(),
    };
    for (env, alvo) in [(Env::Dev, &mut migs.dev), (Env::Prod, &mut migs.prod)] {
        match app.state.migration_rows(&name, env) {
            Ok(rows) => {
                *alvo = rows
                    .into_iter()
                    .map(|(file, sha256)| MigrationAplicada { file, sha256 })
                    .collect();
            }
            Err(e) => {
                return erro(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("falha lendo migrations de {name}/{env}: {e}"),
                );
            }
        }
    }

    // Hosts derivados ANTES de mover `name` para o corpo (ordem de campos).
    let (prod_host, dev_host) = (app.domain.prod_host(&name), app.domain.dev_host(&name));
    Json(ProjectStatus {
        name,
        framework,
        prod_host,
        dev_host,
        toml_hash: hash,
        created_at: criado,
        sleep: SleepStatus {
            dev: sleep_dev,
            prod: sleep_prod,
        },
        migrations: migs,
    })
    .into_response()
}

// -------------------------------------------------------------------- lease

#[derive(Debug, Deserialize)]
struct LeaseBody {
    /// "dev" | "prod" (string no fio; parse com causa — `Env` não tem serde).
    env: String,
    /// TTL em segundos (04): o sleep manager respeita `lease_until`.
    ttl_secs: i64,
}

#[derive(Debug, Serialize)]
struct LeaseGranted {
    name: String,
    env: String,
    ttl_secs: i64,
    /// Epoch de expiração (agora + ttl) para o CLI/painel exibirem.
    lease_until: i64,
}

/// POST /projects/{name}/lease — mantém o projeto acordado enquanto a sessão
/// do CLI renova (04; `flux up` abre e renova a própria).
async fn grant_lease(
    State(app): State<Arc<AppState>>,
    Path(name): Path<String>,
    Json(b): Json<LeaseBody>,
) -> Response {
    let env: Env = match b.env.parse() {
        Ok(e) => e,
        Err(m) => return erro(StatusCode::UNPROCESSABLE_ENTITY, m),
    };
    if b.ttl_secs <= 0 || b.ttl_secs > LEASE_MAX_SECS {
        return erro(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!(
                "ttl_secs fora da faixa (1..={LEASE_MAX_SECS}): {}",
                b.ttl_secs
            ),
        );
    }
    match garante_projeto(&app, &name) {
        Ok(()) => {}
        Err(r) => return *r,
    }
    if let Err(e) = app.state.set_lease(&name, env, b.ttl_secs) {
        return erro(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("falha gravando lease de {name}: {e}"),
        );
    }
    events::event(
        &app.state,
        "info",
        "sleep",
        &format!(
            "lease de {name}/{env} por {} s (API de controle)",
            b.ttl_secs
        ),
    );
    Json(LeaseGranted {
        name,
        env: env.to_string(),
        ttl_secs: b.ttl_secs,
        lease_until: crate::sleep::now_epoch() + b.ttl_secs,
    })
    .into_response()
}

/// Existência do projeto — erro vira 404 com causa (nunca criar sleep_state
/// de projeto fantasma).
fn garante_projeto(app: &AppState, name: &str) -> Result<(), Box<Response>> {
    match app.state.list_projects() {
        Ok(l) if l.iter().any(|n| n == name) => Ok(()),
        Ok(_) => Err(Box::new(erro(
            StatusCode::NOT_FOUND,
            format!("projeto {name:?} não registrado no fluxd — rode `flux up` no repo dele"),
        ))),
        Err(e) => Err(Box::new(erro(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("falha consultando projetos: {e}"),
        ))),
    }
}

// ------------------------------------------------------------------- schema

#[derive(Debug, Deserialize)]
struct SchemaBody {
    env: String,
    /// `(arquivo, sha256)` das migrations no disco AGORA — o CLI lê do repo
    /// e manda; laravel usa, stacks com `migrate_cmd` mandam vazio (05).
    files: Vec<(String, String)>,
}

/// Espelho serializável do `SchemaOutcome` de db.rs (tipo de lá não é serde).
#[derive(Debug, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
enum SchemaReply {
    LaravelRan { fresh: bool, aplicadas: usize },
    MigrateCmd { output: String },
    NoPipeline,
}

impl From<SchemaOutcome> for SchemaReply {
    fn from(o: SchemaOutcome) -> Self {
        match o {
            SchemaOutcome::LaravelRan { fresh, aplicadas } => Self::LaravelRan { fresh, aplicadas },
            SchemaOutcome::MigrateCmd { output } => Self::MigrateCmd { output },
            SchemaOutcome::NoPipeline => Self::NoPipeline,
        }
    }
}

/// POST /projects/{name}/schema — dispara `db::ensure_schema` com os arquivos
/// informados. Exige engine: sem Docker não há container onde rodar migrate,
/// e simular sucesso seria pior que recusar (05).
async fn run_schema(
    State(app): State<Arc<AppState>>,
    Path(name): Path<String>,
    Json(b): Json<SchemaBody>,
) -> Response {
    let env: Env = match b.env.parse() {
        Ok(e) => e,
        Err(m) => return erro(StatusCode::UNPROCESSABLE_ENTITY, m),
    };
    let Some(docker) = app.docker.clone() else {
        return erro(
            StatusCode::SERVICE_UNAVAILABLE,
            "fluxd sem Docker conectado — o ciclo de schema só roda na VM com o engine disponível",
        );
    };
    let toml_src = match toml_gravado(&app, &name) {
        Ok(src) => src,
        Err(r) => return *r,
    };
    match db::ensure_schema(&docker, &app.state, &name, env, &toml_src, &b.files).await {
        Ok(o) => Json(SchemaReply::from(o)).into_response(),
        Err(e) => erro(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

/// POST /projects/{name}/up (06, passo 5): valida o registro e DEVOLVE 202 —
/// o compose up (com build) roda em background; o CLI apura pelo
/// GET /projects/{name} (estado vem do engine). Build de primeira subida é
/// de MINUTOS: request HTTP síncrono estouraria timeout de cliente e do edge.
#[derive(Deserialize, Default)]
struct UpBody {
    env: Option<String>,
}

#[derive(serde::Serialize)]
struct UpAceito {
    started: bool,
    project: String,
    compose_project: String,
}

async fn subir(
    State(app): State<Arc<AppState>>,
    Path(name): Path<String>,
    body: Option<Json<UpBody>>,
) -> Response {
    let env = body
        .and_then(|Json(b)| b.env)
        .unwrap_or_else(|| "dev".to_owned());
    if env != "dev" && env != "prod" {
        return erro(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("ambiente '{env}' inválido (use dev|prod)"),
        );
    }
    // Projeto precisa estar registrado (o toml é a fonte do override).
    if let Err(r) = toml_gravado(&app, &name) {
        return *r;
    }
    // Pasta sincronizada tem que existir — sem sync não há compose para subir.
    let dir = std::path::Path::new(crate::temporary::PROJECTS_ROOT_PADRAO).join(&name);
    if !dir.is_dir() {
        return erro(
            StatusCode::CONFLICT,
            format!(
                "pasta {} não existe na VM — o sync (mutagen) roda antes do up (02)",
                dir.display()
            ),
        );
    }
    let compose_project = format!("flux-{name}-{env}");
    events::event(
        &app.state,
        "info",
        "up",
        &format!("{compose_project}: up aceito (202)"),
    );
    tokio::spawn(crate::compose::subir_projeto(app, name.clone(), env));
    Json(UpAceito {
        started: true,
        project: name,
        compose_project,
    })
    .into_response()
}

async fn not_implemented() -> Response {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(ApiErrorBody {
            error:
                "endpoint da API de controle ainda não implementado — fases F5–F17 (docs/specs/)"
                    .into(),
        }),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_common::config::GlobalConfig;
    use http_body_util::BodyExt;
    use http_body_util::Full;
    use hyper::body::Bytes;

    type TestClient = hyper_util::client::legacy::Client<
        hyper_util::client::legacy::connect::HttpConnector,
        Full<Bytes>,
    >;

    /// Sobe a MESMA `build_api` do `serve` num socket real de 127.0.0.1 com
    /// State::in_memory (sem Docker) — exercita bind, gate e rotas de ponta a
    /// ponta, do jeito que o CLI vai falar.
    async fn servidor(token: Option<&str>) -> (String, Arc<AppState>) {
        let st = DbState::in_memory().unwrap();
        if let Some(t) = token {
            let h = sha256_hex(t.as_bytes());
            st.with(|c| {
                c.execute(
                    "INSERT INTO settings(k, v) VALUES (?1, ?2)",
                    params![TOKEN_KEY, h],
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
            axum::serve(listener, build_api(serve_app)).await.unwrap();
        });
        (format!("http://{addr}"), app)
    }

    fn cliente() -> TestClient {
        hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
            .build_http()
    }

    async fn req(
        cli: &TestClient,
        base: &str,
        method: &str,
        path: &str,
        token: Option<&str>,
        body: Option<&str>,
    ) -> (u16, String) {
        let mut b = hyper::Request::builder()
            .method(hyper::Method::from_bytes(method.as_bytes()).unwrap())
            .uri(format!("{base}{path}"));
        if let Some(t) = token {
            b = b.header(hyper::header::AUTHORIZATION, format!("Bearer {t}"));
        }
        let payload = body.unwrap_or("");
        if body.is_some() {
            // Exigência do extrator Json do axum.
            b = b.header(hyper::header::CONTENT_TYPE, "application/json");
        }
        let r = b.body(Full::new(Bytes::from(payload.to_owned()))).unwrap();
        let res = cli.request(r).await.unwrap();
        let status = res.status().as_u16();
        let bytes = BodyExt::collect(res.into_body()).await.unwrap().to_bytes();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    async fn registra(cli: &TestClient, base: &str, name: &str, toml: &str) -> (u16, String) {
        let body = serde_json::json!({ "name": name, "toml": toml, "toml_hash": "hash-teste" });
        req(
            cli,
            base,
            "POST",
            "/projects",
            Some("segredo"),
            Some(&body.to_string()),
        )
        .await
    }

    #[tokio::test]
    async fn healthz_responde_sem_token() {
        let (base, _app) = servidor(None).await;
        let cli = cliente();
        let (status, corpo) = req(&cli, &base, "GET", "/healthz", None, None).await;
        assert_eq!(status, 200, "corpo: {corpo}");
        assert!(corpo.contains("\"ok\""), "corpo: {corpo}");
    }

    #[tokio::test]
    async fn sem_token_configurado_a_api_fecha_fail_closed() {
        let (base, _app) = servidor(None).await;
        let cli = cliente();
        // 09: a API nunca nasce aberta — leitura E escrita recusam com causa,
        // e o fallback (rota de fase futura) fica atrás do gate também.
        for (method, path) in [
            ("GET", "/projects"),
            ("POST", "/projects"),
            ("GET", "/rota-fantasma"),
        ] {
            let (status, corpo) = req(&cli, &base, method, path, None, None).await;
            assert_eq!(status, 503, "{method} {path}: {corpo}");
            assert!(
                corpo.contains("não configurada"),
                "{method} {path}: {corpo}"
            );
        }
    }

    #[tokio::test]
    async fn token_ausente_ou_errado_recusa_401_e_registra_evento() {
        let (base, app) = servidor(Some("segredo")).await;
        let cli = cliente();
        let (s1, c1) = req(&cli, &base, "GET", "/projects", None, None).await;
        assert_eq!(s1, 401, "{c1}");
        let (s2, c2) = req(&cli, &base, "GET", "/projects", Some("errado"), None).await;
        assert_eq!(s2, 401, "{c2}");
        assert!(c2.contains("recusado"), "401 precisa de causa clara: {c2}");
        // O fallback (501 futura) NÃO é janela aberta: sem token é 401 como
        // nas rotas nomeadas (09) — e a recusa vira evento como qualquer outra.
        let (s3, c3) = req(&cli, &base, "GET", "/rota-fantasma", None, None).await;
        assert_eq!(s3, 401, "fallback atrás do gate: {c3}");
        let n: i64 = app
            .state
            .with(|c| {
                c.query_row(
                    "SELECT COUNT(*) FROM events WHERE scope = 'auth'",
                    [],
                    |r| r.get(0),
                )
            })
            .unwrap();
        assert_eq!(n, 3, "cada recusa vira evento (09 critério 3)");
    }

    #[tokio::test]
    async fn register_lista_e_status_ciclo_completo() {
        let (base, _app) = servidor(Some("segredo")).await;
        let cli = cliente();

        let (s, c) = registra(&cli, &base, "vipers", "name = \"vipers\"").await;
        assert_eq!(s, 200, "{c}");
        assert!(c.contains("vipers.bitello.xyz"), "resumo com hosts: {c}");
        assert!(c.contains("laravel"), "framework derivado do toml: {c}");

        let (s, c) = req(&cli, &base, "GET", "/projects", Some("segredo"), None).await;
        assert_eq!(s, 200, "{c}");
        assert!(c.contains("vipers"), "lista: {c}");

        let (s, c) = req(
            &cli,
            &base,
            "GET",
            "/projects/vipers",
            Some("segredo"),
            None,
        )
        .await;
        assert_eq!(s, 200, "{c}");
        assert!(c.contains("\"toml_hash\":\"hash-teste\""), "status: {c}");
        assert!(c.contains("\"mode\":null"), "sono zerado: {c}");
        assert!(
            c.contains("\"dev\"") && c.contains("\"prod\""),
            "dois ambientes: {c}"
        );

        let (s, c) = req(&cli, &base, "GET", "/projects/zzz", Some("segredo"), None).await;
        assert_eq!(s, 404, "{c}");
        assert!(c.contains("zzz"), "404 nomeia o projeto: {c}");
    }

    #[tokio::test]
    async fn register_valida_toml_e_health_path_por_stack() {
        let (base, _app) = servidor(Some("segredo")).await;
        let cli = cliente();

        // toml com chave desconhecida → 422 com o nome da chave (06).
        let (s, c) = registra(&cli, &base, "quebrado", "naame = \"x\"").await;
        assert_eq!(s, 422, "{c}");
        assert!(c.contains("naame"), "erro cita a chave ruim: {c}");

        // node sem [health] path → 422 (exigência do registro, 06).
        let (s, c) = registra(&cli, &base, "node-sem-health", "framework = \"node\"").await;
        assert_eq!(s, 422, "{c}");
        assert!(c.contains("[health]"), "causa: {c}");

        // node COM health path → aceito.
        let (s, c) = registra(
            &cli,
            &base,
            "node-ok",
            "framework = \"node\"\n[health]\npath = \"/healthz\"\n",
        )
        .await;
        assert_eq!(s, 200, "{c}");
    }

    #[tokio::test]
    async fn lease_grava_no_estado_e_recusa_invalidos() {
        let (base, app) = servidor(Some("segredo")).await;
        let cli = cliente();
        let _ = registra(&cli, &base, "vipers", "").await;

        let corpo = serde_json::json!({ "env": "dev", "ttl_secs": 120 });
        let (s, c) = req(
            &cli,
            &base,
            "POST",
            "/projects/vipers/lease",
            Some("segredo"),
            Some(&corpo.to_string()),
        )
        .await;
        assert_eq!(s, 200, "{c}");
        assert!(c.contains("\"lease_until\""), "resposta: {c}");

        // O lease realmente está no banco (é o que o sweep do 04 consulta).
        let (last, lease, cooldown, mode) = app.state.sleep_row("vipers", Env::Dev).unwrap();
        let agora = crate::sleep::now_epoch();
        assert!(lease.is_some_and(|t| t > agora), "lease futuro: {lease:?}");
        assert!(mode.as_deref() == Some("awake"), "mode: {mode:?}");
        assert!(last.is_none() && cooldown.is_none());

        // Projeto fantasma → 404 (nunca cria linha de sono de fantasma).
        let (s, c) = req(
            &cli,
            &base,
            "POST",
            "/projects/fantasma/lease",
            Some("segredo"),
            Some(&corpo.to_string()),
        )
        .await;
        assert_eq!(s, 404, "{c}");

        // TTL inválido e ambiente inválido → 422 com causa.
        let ruim = serde_json::json!({ "env": "dev", "ttl_secs": 0 });
        let (s, c) = req(
            &cli,
            &base,
            "POST",
            "/projects/vipers/lease",
            Some("segredo"),
            Some(&ruim.to_string()),
        )
        .await;
        assert_eq!(s, 422, "{c}");
        let ruim = serde_json::json!({ "env": "homolog", "ttl_secs": 60 });
        let (s, c) = req(
            &cli,
            &base,
            "POST",
            "/projects/vipers/lease",
            Some("segredo"),
            Some(&ruim.to_string()),
        )
        .await;
        assert_eq!(s, 422, "{c}");
        assert!(c.contains("ambiente inválido"), "causa: {c}");
    }

    #[tokio::test]
    async fn schema_sem_docker_recusa_503_com_causa() {
        let (base, _app) = servidor(Some("segredo")).await;
        let cli = cliente();
        let _ = registra(&cli, &base, "vipers", "").await;
        let corpo = serde_json::json!({ "env": "dev", "files": [] });
        let (s, c) = req(
            &cli,
            &base,
            "POST",
            "/projects/vipers/schema",
            Some("segredo"),
            Some(&corpo.to_string()),
        )
        .await;
        assert_eq!(s, 503, "{c}");
        assert!(c.contains("Docker"), "causa: {c}");
    }

    #[tokio::test]
    async fn rota_futura_no_fallback_volta_501() {
        let (base, _app) = servidor(Some("segredo")).await;
        let cli = cliente();
        let (s, c) = req(
            &cli,
            &base,
            "POST",
            "/projects/vipers/down",
            Some("segredo"),
            None,
        )
        .await;
        assert_eq!(s, 501, "{c}");
        assert!(c.contains("ainda não implementado"), "causa: {c}");
    }
}
