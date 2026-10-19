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
use crate::ciclo;
use crate::events;
use crate::migrations::{self, SchemaOutcome};
// O extrator `State` do axum ocupa o nome; o estado do banco entra como DbState.
use crate::state::State as DbState;
use crate::sync;

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
        .route("/projects/{name}/switch", post(trocar))
        .route("/projects/{name}/fresh", post(fresh_forcado))
        // Ciclo de dev (06): handlers em ciclo.rs — rotas aqui, na frente
        // do fallback de fase futura (501).
        .route("/projects/{name}/down", post(ciclo::down))
        .route("/projects/{name}/logs", get(ciclo::logs))
        .route("/projects/{name}/exec", post(ciclo::exec))
        .route("/containers", get(listar_containers))
        // Sync do mutagen (watch no HOST Windows): estado das sessões para o
        // painel + fila de rescan por projeto. Atrás do gate como as demais
        // rotas de controle — persistência em sync.rs (tabela sync_status).
        .route("/sync/report", post(sync_report))
        .route("/sync/rescan", post(sync_rescan))
        .route("/sync/status", get(sync_status))
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

/// `Authorization: Bearer <token>` → `<token>`. `pub(crate)` SÓ para o
/// handler de deploy (deploy.rs) reusar o mesmo parse — build_api e as rotas
/// da :7420 seguem intocados (a rota de deploy NUNCA entra aqui).
pub(crate) fn bearer_token(v: &str) -> Option<&str> {
    v.strip_prefix("Bearer ").filter(|t| !t.is_empty())
}

/// sha256 em hex lowercase de 64 chars — formato gravado pelo
/// `flux project token` e esperado pelo handler de deploy nos DOIS lados.
pub(crate) fn sha256_hex(data: &[u8]) -> String {
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
/// `pub(crate)` SÓ para o handler de deploy comparar o token do projeto.
pub(crate) fn hash_confere(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub(crate) fn erro(status: StatusCode, msg: impl Into<String>) -> Response {
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
    if !flux_common::nomes::projeto_valido(nome) {
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
    // [domains] (09): validação na borda do registro — host inválido é 422
    // com a chave na mensagem, nunca host morto gravado.
    let (host_prod, host_dev) = match parsed.hosts_custom(&app.domain) {
        Ok(h) => h,
        Err(e) => {
            return erro(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("flux.toml de {nome}: {e}"),
            );
        }
    };
    // Colisão explícita: host custom já pertence a OUTRO projeto = 422 com o
    // dono na mensagem (o PK do SQLite pegaria, mas o erro seria críptico).
    for host in host_prod.iter().chain(host_dev.iter()) {
        match app.state.host_custom(host) {
            Err(e) => return erro(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
            Ok(Some((dono, _))) if dono != nome => {
                return erro(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    format!(
                        "[domains] host \"{host}\" já pertence ao projeto \"{dono}\" — \
                         um host custom pertence a um projeto só"
                    ),
                );
            }
            _ => {}
        }
    }
    if let Err(e) = app.state.register_project(nome, &b.toml, &b.toml_hash) {
        return erro(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("falha gravando {nome} no estado: {e}"),
        );
    }
    // Hosts custom gravados DEPOIS da linha do projeto (re-registro substitui).
    let hosts: Vec<(String, bool)> = host_prod
        .iter()
        .map(|h| (h.clone(), false))
        .chain(host_dev.iter().map(|h| (h.clone(), true)))
        .collect();
    if let Err(e) = app.state.set_project_hosts(nome, &hosts) {
        return erro(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("falha gravando [domains] de {nome} no estado: {e}"),
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
        prod_host: host_prod.unwrap_or_else(|| app.domain.prod_host(nome)),
        dev_host: host_dev.unwrap_or_else(|| app.domain.dev_host(nome)),
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
        .map(|n| {
            // [domains] (09): override quando gravado; derivação como fallback.
            let (prod, dev) = app.state.project_hosts(n).unwrap_or((None, None));
            ProjectSummary {
                name: n.clone(),
                framework: framework_label(&app, n),
                prod_host: prod.unwrap_or_else(|| app.domain.prod_host(n)),
                dev_host: dev.unwrap_or_else(|| app.domain.dev_host(n)),
            }
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
    /// Verdade do ENGINE para o dev (None = sem Docker conectado).
    #[serde(skip_serializing_if = "Option::is_none")]
    dev_container_state: Option<String>,
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

    // Estado REAL do container (engine), separado do modo do sono — a linha
    // de sleep_state nasce 'awake' por default e NÃO é verdade de container.
    // E RUNNING deixa de valer "awake" (06, fecha o F9): só merece "awake"
    // com o `[health] path` RESPONDENDO; app no ar sem responder = "waking" —
    // o `flux up` continua esperando em vez de celebrar cedo.
    let dev_container_state = match app.docker.as_ref() {
        Some(d) => Some({
            use crate::decision::HostSource;
            let estado =
                crate::docker::DockerSource::new(d.clone(), &app.state).env_state(&name, Env::Dev);
            if estado == flux_common::protocol::ContainerState::Awake {
                let saudavel = match crate::docker::alvo_app(
                    d,
                    &crate::docker::compose_project(&name, Env::Dev),
                )
                .await
                {
                    Ok(Some(target)) => {
                        let path = app
                            .state
                            .project_toml(&name)
                            .ok()
                            .flatten()
                            .and_then(|s| ProjectToml::parse_str(&s).ok())
                            .map(|t| t.health_path().to_owned())
                            .unwrap_or_else(|| "/up".into());
                        crate::compose::saude_ok(&app.http, &target, &path).await
                    }
                    _ => false,
                };
                if saudavel {
                    "awake".to_owned()
                } else {
                    "waking".to_owned()
                }
            } else {
                estado.as_str().to_owned()
            }
        }),
        None => None,
    };

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

    // Hosts efetivos ([domains], 09): override gravado, senão derivação.
    let (override_prod, override_dev) = app.state.project_hosts(&name).unwrap_or((None, None));
    let (prod_host, dev_host) = (
        override_prod.unwrap_or_else(|| app.domain.prod_host(&name)),
        override_dev.unwrap_or_else(|| app.domain.dev_host(&name)),
    );
    Json(ProjectStatus {
        name,
        framework,
        prod_host,
        dev_host,
        toml_hash: hash,
        created_at: criado,
        dev_container_state,
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
pub(crate) fn garante_projeto(app: &AppState, name: &str) -> Result<(), Box<Response>> {
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
    /// Override de seed da CLI (05): `Some` vence o toml; ausente no fio =
    /// decidir por `[db].seed_on_rebuild`, e sem toml o default da spec é
    /// `true`. O CLI manda o campo só com `--seed`/`--no-seed`.
    seed: Option<bool>,
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

/// POST /projects/{name}/schema — dispara `migrations::ensure_schema_com_seed` com os
/// arquivos informados e o override de seed do corpo (05: `Some` vence o
/// toml; `None` = decidir pelo flux.toml). Exige engine: sem Docker não há
/// container onde rodar migrate, e simular sucesso seria pior que recusar (05).
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
    match migrations::ensure_schema_com_seed(
        &docker, &app.state, &name, env, &toml_src, &b.files, b.seed,
    )
    .await
    {
        Ok(o) => Json(SchemaReply::from(o)).into_response(),
        Err(e) => erro(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

/// POST /projects/{name}/switch (10/R12): troca de versão invisível —
/// aponta a cor ativa (blue|green) e drena a velha com grace. Cor nova tem
/// que já estar no ar (o deploy sobe a inativa antes).
#[derive(Deserialize, Default)]
struct SwitchBody {
    color: Option<String>,
    drain_secs: Option<u64>,
}

#[derive(serde::Serialize)]
struct SwitchOut {
    project: String,
    cor_nova: String,
    cor_velha: Option<String>,
    drenada: bool,
}

async fn trocar(
    State(app): State<Arc<AppState>>,
    Path(name): Path<String>,
    body: Option<Json<SwitchBody>>,
) -> Response {
    let corpo = body.map(|Json(b)| b).unwrap_or_default();
    let color = corpo.color.unwrap_or_else(|| "blue".to_owned());
    let drain = corpo
        .drain_secs
        .unwrap_or(crate::compose::DRAIN_PADRAO_SECS);
    if drain > 300 {
        return erro(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("drain_secs {drain} acima do teto da 10 (300 s)"),
        );
    }
    match crate::compose::trocar_cor(app, &name, &color, drain).await {
        Ok((velha, drenada)) => Json(SwitchOut {
            project: name,
            cor_nova: color,
            cor_velha: velha,
            drenada,
        })
        .into_response(),
        Err(e) => erro(StatusCode::CONFLICT, e),
    }
}

/// POST /projects/{name}/fresh — RECONSTRUÇÃO forçada de dev por comando
/// (pedido do Gabriel): `migrate:fresh --force [--seed]` + trilha zerada.
/// DESTRUTIVO (apaga dados de dev) — a confirmação é o `--yes` do CLI.
/// PROD NUNCA: 403 incondicional (05).
#[derive(Deserialize, Default)]
struct FreshBody {
    env: Option<String>,
    seed: Option<bool>,
}

async fn fresh_forcado(
    State(app): State<Arc<AppState>>,
    Path(name): Path<String>,
    body: Option<Json<FreshBody>>,
) -> Response {
    let corpo = body.map(|Json(b)| b).unwrap_or_default();
    let env = corpo.env.unwrap_or_else(|| "dev".to_owned());
    if env != "dev" {
        return erro(
            StatusCode::FORBIDDEN,
            "fresh forçado é EXCLUSIVO de dev — prod nunca é reconstruído (05)",
        );
    }
    let Some(docker) = app.docker.clone() else {
        return erro(
            StatusCode::SERVICE_UNAVAILABLE,
            "fluxd sem Docker conectado — fresh só roda na VM",
        );
    };
    let toml_src = match toml_gravado(&app, &name) {
        Ok(t) => t,
        Err(r) => return *r,
    };
    match migrations::fresh_dev_forcado(&docker, &app.state, &name, &toml_src, corpo.seed).await {
        Ok(output) => (StatusCode::OK, output).into_response(),
        Err(e) => erro(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

/// GET /containers — TODOS os containers gerenciados pela plataforma
/// (labels compose `flux-*`), o "docker ps" do Flux: `flux ps` fora de
/// projeto lista tudo (pedido do Gabriel 14/10). Estado cru do engine.
#[derive(serde::Serialize)]
struct ContainerRow {
    compose_project: String,
    service: String,
    container: String,
    state: String,
    status: String,
}

async fn listar_containers(State(app): State<Arc<AppState>>) -> Response {
    use bollard::query_parameters::ListContainersOptionsBuilder;
    use std::collections::HashMap;

    let Some(docker) = &app.docker else {
        return erro(
            StatusCode::SERVICE_UNAVAILABLE,
            "fluxd sem Docker conectado — lista de containers só na VM",
        );
    };
    let mut filters: HashMap<String, Vec<String>> = HashMap::new();
    filters.insert(
        "label".to_owned(),
        vec!["com.docker.compose.project".to_owned()],
    );
    let opts = ListContainersOptionsBuilder::new()
        .all(true)
        .filters(&filters)
        .build();
    let containers = match docker.list_containers(Some(opts)).await {
        Ok(c) => c,
        Err(e) => {
            return erro(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("listando containers: {e}"),
            );
        }
    };
    let mut rows: Vec<ContainerRow> = containers
        .iter()
        .filter_map(|c| {
            let labels = c.labels.as_ref()?;
            let projeto = labels.get("com.docker.compose.project")?;
            projeto.starts_with("flux-").then(|| ContainerRow {
                compose_project: projeto.clone(),
                service: labels
                    .get("com.docker.compose.service")
                    .cloned()
                    .unwrap_or_default(),
                container: c
                    .names
                    .as_ref()
                    .and_then(|ns| ns.first().map(|n| n.trim_start_matches('/').to_owned()))
                    .unwrap_or_default(),
                state: c
                    .state
                    .map(|st| st.to_string())
                    .unwrap_or_else(|| "unknown".into()),
                status: c.status.clone().unwrap_or_default(),
            })
        })
        .collect();
    rows.sort_by(|a, b| a.compose_project.cmp(&b.compose_project));
    Json(rows).into_response()
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
    let dir = std::path::Path::new(crate::paths::PROJECTS_ROOT_PADRAO).join(&name);
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

// --------------------------------------------------------------------- sync

/// Corpo do POST /sync/rescan — pedido do painel/CLI ao watch.
#[derive(Debug, Deserialize)]
struct RescanBody {
    project: String,
}

/// Corpo do GET /sync/status — contrato fixo: lista de linhas por projeto.
#[derive(Debug, Serialize)]
struct SyncStatusBody {
    projects: Vec<sync::SyncRow>,
}

/// POST /sync/report — upsert do relato de UMA sessão do `flux watch` (HOST)
/// no estado do fluxd. 204 sem corpo: o watch não lê resposta. `rescan_applied`
/// = true limpa a flag de rescan do projeto (persistência em sync.rs).
/// Projeto NÃO registrado é 404 com causa + warn no log do fluxd: linha de
/// projeto fantasma ficaria invisível no painel (que casa pelo nome
/// registrado, painel.rs) e sobreviveria a qualquer purge (que apaga pelo
/// nome exato) — é o mesmo padrão do /sync/rescan ("flag para fantasma o
/// watch nunca consumiria"). O watch loga o 404 no host; a divergência fica
/// visível dos dois lados.
async fn sync_report(
    State(app): State<Arc<AppState>>,
    Json(b): Json<sync::SyncReport>,
) -> Response {
    if b.project.trim().is_empty() || b.status.trim().is_empty() {
        return erro(
            StatusCode::UNPROCESSABLE_ENTITY,
            "sync/report exige project e status não vazios",
        );
    }
    if let Err(r) = garante_projeto(&app, &b.project) {
        tracing::warn!(
            "sync: relato descartado — projeto não registrado (project {:?}, session {:?})",
            b.project,
            b.session
        );
        return *r;
    }
    if let Err(e) = sync::upsert_report(&app, &b) {
        return erro(StatusCode::INTERNAL_SERVER_ERROR, e);
    }
    StatusCode::NO_CONTENT.into_response()
}

/// POST /sync/rescan — seta `rescan_requested` do projeto (202). Projeto
/// desconhecido é 404 com causa: flag para fantasma o watch nunca consumiria.
async fn sync_rescan(State(app): State<Arc<AppState>>, Json(b): Json<RescanBody>) -> Response {
    match garante_projeto(&app, &b.project) {
        Ok(()) => {}
        Err(r) => return *r,
    }
    if let Err(e) = sync::request_rescan(&app, &b.project) {
        return erro(StatusCode::INTERNAL_SERVER_ERROR, e);
    }
    StatusCode::ACCEPTED.into_response()
}

/// GET /sync/status — estado de sync de todos os projetos que já reportaram
/// (ou têm rescan pendente), no formato do contrato.
async fn sync_status(State(app): State<Arc<AppState>>) -> Response {
    Json(SyncStatusBody {
        projects: sync::sync_states(&app),
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

    /// `[domains]` (09): registro persiste os hosts custom (resumo mostra o
    /// override), host inválido é 422 com a chave, colisão entre projetos é
    /// 422 com o dono.
    #[tokio::test]
    async fn register_com_domains_persiste_valida_e_detecta_colisao() {
        let (base, _app) = servidor(Some("segredo")).await;
        let cli = cliente();

        // Válido: resposta do registro JÁ mostra os hosts declarados.
        let (s, c) = registra(
            &cli,
            &base,
            "com-domains",
            "[domains]\nprod = \"vip.bitello.xyz\"\ndev = \"vip-dev.bitello.xyz\"",
        )
        .await;
        assert_eq!(s, 200, "{c}");
        assert!(c.contains("\"vip.bitello.xyz\""), "{c}");
        assert!(c.contains("\"vip-dev.bitello.xyz\""), "{c}");

        // Inválido: fora da zona → 422 com a chave na mensagem.
        let (s, c) = registra(
            &cli,
            &base,
            "domains-ruim",
            "[domains]\nprod = \"vippers.com.br\"",
        )
        .await;
        assert_eq!(s, 422, "{c}");
        assert!(c.contains("[domains] prod"), "{c}");

        // Colisão: host do 1º projeto pertence a ele; 2º projeto recebe 422
        // com o dono na mensagem. O PRÓPRIO dono re-registrando passa (replace).
        let (s, c) = registra(
            &cli,
            &base,
            "colidente",
            "[domains]\ndev = \"vip-dev.bitello.xyz\"",
        )
        .await;
        assert_eq!(s, 422, "{c}");
        assert!(c.contains("com-domains"), "erro cita o dono: {c}");

        let (s, _c) = registra(
            &cli,
            &base,
            "com-domains",
            "[domains]\nprod = \"vip.bitello.xyz\"",
        )
        .await;
        assert_eq!(s, 200, "dono re-registrando substitui: {_c}");
    }

    #[tokio::test]
    async fn rota_futura_no_fallback_volta_501() {
        let (base, _app) = servidor(Some("segredo")).await;
        let cli = cliente();
        // `/down` já é rota real (ciclo.rs, 06) — o fallback 501 segue para
        // endpoints que NENHUMA fase entregou.
        let (s, c) = req(
            &cli,
            &base,
            "POST",
            "/projects/vipers/teleporte",
            Some("segredo"),
            None,
        )
        .await;
        assert_eq!(s, 501, "{c}");
        assert!(c.contains("ainda não implementado"), "causa: {c}");
    }

    #[tokio::test]
    async fn rotas_do_ciclo_montadas_e_atras_do_gate() {
        let (base, app) = servidor(Some("segredo")).await;
        let cli = cliente();

        // Projeto fantasma nas três rotas → 404 com causa (rota EXISTE e o
        // handler validou o registro; sem montagem seria 401/501, não 404).
        let (s, c) = req(
            &cli,
            &base,
            "POST",
            "/projects/fantasma/down?env=dev",
            Some("segredo"),
            None,
        )
        .await;
        assert_eq!(s, 404, "{c}");
        assert!(c.contains("não registrado"), "causa do down: {c}");

        let (s, c) = req(
            &cli,
            &base,
            "GET",
            "/projects/fantasma/logs?env=dev&tail=10",
            Some("segredo"),
            None,
        )
        .await;
        assert_eq!(s, 404, "{c}");
        assert!(c.contains("não registrado"), "causa dos logs: {c}");

        let corpo = serde_json::json!({ "cmd": [] });
        let (s, c) = req(
            &cli,
            &base,
            "POST",
            "/projects/fantasma/exec?env=dev",
            Some("segredo"),
            Some(&corpo.to_string()),
        )
        .await;
        assert_eq!(s, 404, "{c}");

        // Projeto registrado SEM pasta sincronizada → 409 do down (02: sync é
        // pré-condição), com evento gravado — nunca silêncio.
        let _ = registra(&cli, &base, "vipers", "").await;
        let (s, c) = req(
            &cli,
            &base,
            "POST",
            "/projects/vipers/down?env=dev",
            Some("segredo"),
            None,
        )
        .await;
        assert_eq!(s, 409, "{c}");
        assert!(c.contains("não existe na VM"), "causa: {c}");
        let n: i64 = app
            .state
            .with(|c| {
                c.query_row(
                    "SELECT COUNT(*) FROM events WHERE scope = 'down'",
                    [],
                    |r| r.get(0),
                )
            })
            .unwrap();
        assert!(n >= 1, "falha do down vira evento (nada silencioso)");

        // exec com cmd vazio → 422 com a forma certa no corpo (recusa ANTES
        // do 503 de engine ausente — contrato do ciclo.rs).
        let corpo = serde_json::json!({ "cmd": [] });
        let (s, c) = req(
            &cli,
            &base,
            "POST",
            "/projects/vipers/exec?env=dev",
            Some("segredo"),
            Some(&corpo.to_string()),
        )
        .await;
        assert_eq!(s, 422, "{c}");
        assert!(c.contains("cmd vazio"), "causa: {c}");

        // /schema aceita o campo `seed` no corpo (override da CLI, 05): sem
        // Docker a rota segue recusando 503 com causa — o campo não quebra
        // o parse nem muda o fechamento.
        let corpo = serde_json::json!({ "env": "dev", "files": [], "seed": true });
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

    // ---------------------------------------------------- sync (watch no HOST)

    #[tokio::test]
    async fn sync_report_e_status_roundtrip() {
        let (base, _app) = servidor(Some("segredo")).await;
        let cli = cliente();
        let _ = registra(&cli, &base, "vipa", "").await;

        let corpo = serde_json::json!({
            "project": "vipa", "session": "flux-vipa", "status": "watching",
            "paused": false, "files": 123, "problems": 0,
            "last_error": null, "rescan_applied": false
        });
        let (s, c) = req(
            &cli,
            &base,
            "POST",
            "/sync/report",
            Some("segredo"),
            Some(&corpo.to_string()),
        )
        .await;
        assert_eq!(s, 204, "report aceito sem corpo de resposta: {c}");

        let (s, c) = req(&cli, &base, "GET", "/sync/status", Some("segredo"), None).await;
        assert_eq!(s, 200, "{c}");
        let v: serde_json::Value = serde_json::from_str(&c).unwrap();
        let rows = v["projects"].as_array().unwrap();
        assert_eq!(rows.len(), 1, "um relato = uma linha: {c}");
        let row = &rows[0];
        assert_eq!(row["project"], "vipa");
        assert_eq!(row["status"], "watching");
        assert_eq!(row["paused"], false);
        assert_eq!(row["files"], 123);
        assert_eq!(row["problems"], 0);
        assert!(row["last_error"].is_null());
        assert_eq!(row["rescan_requested"], false);
        let updated = row["updated_at"].as_str().unwrap();
        assert!(
            updated.contains('T') && updated.ends_with('Z'),
            "updated_at em RFC3339: {updated}"
        );

        // Segundo relato atualiza a MESMA linha (upsert, não insert).
        let mut corpo2 = corpo.clone();
        corpo2["files"] = serde_json::json!(124);
        corpo2["last_error"] = serde_json::json!("boom");
        let (s, _c) = req(
            &cli,
            &base,
            "POST",
            "/sync/report",
            Some("segredo"),
            Some(&corpo2.to_string()),
        )
        .await;
        assert_eq!(s, 204);
        let (_s, c) = req(&cli, &base, "GET", "/sync/status", Some("segredo"), None).await;
        let v: serde_json::Value = serde_json::from_str(&c).unwrap();
        let rows = v["projects"].as_array().unwrap();
        assert_eq!(rows.len(), 1, "upsert não duplica linha: {c}");
        assert_eq!(rows[0]["files"], 124);
        assert_eq!(rows[0]["last_error"], "boom");

        // Report sem project/status → 422 com causa (nada silencioso). O
        // `session` presente: é o parse do axum que recusa corpo sem ele.
        let (s, c) = req(
            &cli,
            &base,
            "POST",
            "/sync/report",
            Some("segredo"),
            Some(r#"{"project":"","status":"","session":"s"}"#),
        )
        .await;
        assert_eq!(s, 422, "{c}");
        assert!(c.contains("project"), "causa: {c}");

        // Sessão de worktree deriva projeto que NÃO está registrado
        // (`flux-vipa-branch` → "vipa-branch"): 404 com causa nomeada e
        // NENHUMA linha fantasma — o painel casa pelo projeto registrado e o
        // purge apaga pelo nome exato, então a linha nunca seria vista nem
        // apagada (o relato é descartado na porta, com log).
        let corpo_wt = serde_json::json!({
            "project": "vipa-branch", "session": "flux-vipa-branch", "status": "watching",
            "paused": false, "files": 1, "problems": 0,
            "last_error": null, "rescan_applied": false
        });
        let (s, c) = req(
            &cli,
            &base,
            "POST",
            "/sync/report",
            Some("segredo"),
            Some(&corpo_wt.to_string()),
        )
        .await;
        assert_eq!(s, 404, "{c}");
        assert!(c.contains("vipa-branch"), "404 nomeia o projeto: {c}");
        let (_s, c) = req(&cli, &base, "GET", "/sync/status", Some("segredo"), None).await;
        let v: serde_json::Value = serde_json::from_str(&c).unwrap();
        let rows = v["projects"].as_array().unwrap();
        assert!(
            rows.iter().all(|r| r["project"] != "vipa-branch"),
            "fantasma não cria linha: {c}"
        );
    }

    #[tokio::test]
    async fn sync_rescan_set_e_clear_da_flag() {
        let (base, app) = servidor(Some("segredo")).await;
        let cli = cliente();
        let _ = registra(&cli, &base, "vipa", "").await;

        // Flag ANTES de qualquer relato: linha nasce só com o pedido — é o
        // que o watch precisa encontrar no próximo GET /sync/status.
        let (s, c) = req(
            &cli,
            &base,
            "POST",
            "/sync/rescan",
            Some("segredo"),
            Some(r#"{"project":"vipa"}"#),
        )
        .await;
        assert_eq!(s, 202, "{c}");
        let (_s, c) = req(&cli, &base, "GET", "/sync/status", Some("segredo"), None).await;
        let v: serde_json::Value = serde_json::from_str(&c).unwrap();
        assert_eq!(v["projects"][0]["rescan_requested"], true, "{c}");
        assert_eq!(
            v["projects"][0]["status"], "unknown",
            "sem relato ainda: {c}"
        );

        // Relato SEM applied NÃO limpa a flag (o pedido sobrevive ao watch).
        let corpo = serde_json::json!({
            "project": "vipa", "session": "flux-vipa", "status": "watching",
            "paused": false, "files": 1, "problems": 0,
            "last_error": null, "rescan_applied": false
        });
        let (s, _c) = req(
            &cli,
            &base,
            "POST",
            "/sync/report",
            Some("segredo"),
            Some(&corpo.to_string()),
        )
        .await;
        assert_eq!(s, 204);
        let (_s, c) = req(&cli, &base, "GET", "/sync/status", Some("segredo"), None).await;
        let v: serde_json::Value = serde_json::from_str(&c).unwrap();
        assert_eq!(v["projects"][0]["rescan_requested"], true, "{c}");

        // Relato COM applied limpa.
        let mut aplicado = corpo.clone();
        aplicado["rescan_applied"] = serde_json::json!(true);
        let (s, _c) = req(
            &cli,
            &base,
            "POST",
            "/sync/report",
            Some("segredo"),
            Some(&aplicado.to_string()),
        )
        .await;
        assert_eq!(s, 204);
        let (_s, c) = req(&cli, &base, "GET", "/sync/status", Some("segredo"), None).await;
        let v: serde_json::Value = serde_json::from_str(&c).unwrap();
        assert_eq!(v["projects"][0]["rescan_requested"], false, "{c}");

        // Projeto fantasma → 404 com causa nomeada (rota EXISTE, registro é
        // que falta — igual ao padrão do lease).
        let (s, c) = req(
            &cli,
            &base,
            "POST",
            "/sync/rescan",
            Some("segredo"),
            Some(r#"{"project":"fantasma"}"#),
        )
        .await;
        assert_eq!(s, 404, "{c}");
        assert!(c.contains("fantasma"), "404 nomeia o projeto: {c}");

        // Trilha de eventos do sync visível no painel (nada silencioso): 1
        // rescan pedido + 1 rescan aplicado; o do fantasma não grava (falha
        // antes da escrita).
        let n: i64 = app
            .state
            .with(|c| {
                c.query_row(
                    "SELECT COUNT(*) FROM events WHERE scope = 'sync'",
                    [],
                    |r| r.get(0),
                )
            })
            .unwrap();
        assert!(n >= 2, "eventos do sync gravados (n={n})");
    }

    #[tokio::test]
    async fn sync_rotas_bloqueiam_sem_token() {
        let (base, _app) = servidor(Some("segredo")).await;
        let cli = cliente();
        let (s1, c1) = req(&cli, &base, "GET", "/sync/status", None, None).await;
        assert_eq!(s1, 401, "{c1}");
        let corpo = serde_json::json!({
            "project": "vipa", "session": "s", "status": "watching"
        });
        let (s2, c2) = req(
            &cli,
            &base,
            "POST",
            "/sync/report",
            None,
            Some(&corpo.to_string()),
        )
        .await;
        assert_eq!(s2, 401, "{c2}");
        let (s3, c3) = req(
            &cli,
            &base,
            "POST",
            "/sync/rescan",
            None,
            Some(r#"{"project":"vipa"}"#),
        )
        .await;
        assert_eq!(s3, 401, "{c3}");
    }
}
