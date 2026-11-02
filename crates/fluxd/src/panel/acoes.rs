//! Ações do painel (fatia 6b, escopo 31/10–02/11): migrate, fresh, seed,
//! build, live ON/OFF, optimize e shield por projeto, executadas NO daemon
//! com saída em fluxo (terminal ao vivo via SSE).
//!
//! Regras fechadas com o Gabriel:
//! - prod ⊆ dev: TUDO que prod tem, dev tem; nem tudo de dev está em prod.
//!   PROD NUNCA executa fresh/seed/build (nada que apaga/recria banco ou
//!   mexe em assets); exceção segura dele: shield:generate (permissões do
//!   Filament, não destrutivo).
//! - UMA ação por projeto por vez (409 se já roda).
//! - Destrutivas (fresh/seed) são DEV-ONLY e confirmadas com DOIS CLIQUES
//!   na UI (o backend executa direto — a confirmação é camada de UI).
//! - Estado EM MEMÓRIA de propósito: reinício do fluxd perde a execução em
//!   curso (a página avisa). A saída tem teto — log infinito não enche RAM.
//! - build NÃO espera a janela de silêncio do Windows (isso é coisa da
//!   sync do CLI); o sync é contínuo e a janela é conservadorismo do CLI —
//!   trade-off DECLARADO.

use std::collections::{HashMap, VecDeque};
use std::convert::Infallible;
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::Mutex;
use std::time::Duration;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use bollard::exec::{CreateExecOptions, StartExecOptions};
use futures_util::StreamExt;
use tokio::sync::broadcast;

use super::PanelState;
use super::api_publica::{erro_json, resposta_sse};
use crate::AppState;

/// Teto de linhas no buffer de saída (o SSE re-replay manda no máximo isso).
const TETO_LINHAS: usize = 4000;
/// Teto global de duração: build legítimo roda minutos (BUILD_TIMEOUT_SECS
/// do CLI é 1800); o resto tem 900 s de sobra.
const TETO_BUILD: Duration = Duration::from_secs(1800);
const TETO_GERAL: Duration = Duration::from_secs(900);

// ------------------------------------------------------------------ catálogo

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Acao {
    Migrate,
    Fresh,
    FreshSeed,
    Seed,
    Build,
    LiveOn,
    LiveOff,
    Optimize,
    Shield,
}

impl Acao {
    pub(super) fn de_str(s: &str) -> Option<Self> {
        Some(match s {
            "migrate" => Self::Migrate,
            "fresh" => Self::Fresh,
            "fresh_seed" => Self::FreshSeed,
            "seed" => Self::Seed,
            "build" => Self::Build,
            "live_on" => Self::LiveOn,
            "live_off" => Self::LiveOff,
            "optimize" => Self::Optimize,
            "shield" => Self::Shield,
            _ => return None,
        })
    }

    pub(super) fn nome(&self) -> &'static str {
        match self {
            Self::Migrate => "migrate",
            Self::Fresh => "fresh",
            Self::FreshSeed => "fresh_seed",
            Self::Seed => "seed",
            Self::Build => "build",
            Self::LiveOn => "live_on",
            Self::LiveOff => "live_off",
            Self::Optimize => "optimize",
            Self::Shield => "shield",
        }
    }

    /// prod ⊆ dev: o que PROD tem. Fresh/seed/build são DEV-ONLY.
    pub(super) fn em_prod_ok(&self) -> bool {
        matches!(
            self,
            Self::Migrate | Self::LiveOn | Self::LiveOff | Self::Optimize | Self::Shield
        )
    }

    pub(super) fn teto(&self) -> Duration {
        match self {
            Self::Build => TETO_BUILD,
            _ => TETO_GERAL,
        }
    }
}

// ------------------------------------------------------------------ registro

/// Saída de uma ação: buffer (replay) + canal (ao vivo).
pub(super) struct SaidaAcao {
    buffer: Mutex<VecDeque<String>>,
    canal: broadcast::Sender<String>,
}

impl SaidaAcao {
    fn nova() -> (Self, broadcast::Receiver<String>) {
        let (canal, rx) = broadcast::channel(1024);
        (
            Self {
                buffer: Mutex::new(VecDeque::new()),
                canal,
            },
            rx,
        )
    }

    fn linha(&self, l: String) {
        let mut b = self.buffer.lock().expect("mutex de saída");
        if b.len() >= TETO_LINHAS {
            b.pop_front();
        }
        b.push_back(l.clone());
        drop(b);
        let _ = self.canal.send(l);
    }

    fn linhas(&self) -> Vec<String> {
        self.buffer
            .lock()
            .expect("mutex de saída")
            .iter()
            .cloned()
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(super) enum Estado {
    Rodando,
    Ok,
    Falhou(String),
}

impl Estado {
    fn nome(&self) -> &'static str {
        match self {
            Self::Rodando => "rodando",
            Self::Ok => "ok",
            Self::Falhou(_) => "falhou",
        }
    }
}

pub(super) struct AcaoEmCurso {
    pub(super) acao: &'static str,
    pub(super) ambiente: String,
    pub(super) inicio: std::time::SystemTime, // (exposto via início_seg no estado)
    pub(super) estado: Mutex<Estado>,
    pub(super) saida: SaidaAcao,
}

static REGISTRO: LazyLock<Mutex<HashMap<String, AcaoEmCurso>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static SEQUENCIA: LazyLock<Mutex<u64>> = LazyLock::new(|| Mutex::new(0));

fn com_registro<T>(f: impl FnOnce(&mut HashMap<String, AcaoEmCurso>) -> T) -> T {
    f(&mut REGISTRO.lock().expect("mutex do registro de ações"))
}

/// Estado serializável da ação (atual/última) do projeto — `None` = nunca
/// houve ação.
pub(super) fn estado_atual(nome: &str) -> Option<AcaoEstadoJson> {
    let reg = REGISTRO.lock().expect("mutex do registro");
    let seq = *SEQUENCIA.lock().expect("mutex da sequência");
    reg.get(nome).map(|a| {
        let estado = a.estado.lock().expect("mutex de estado");
        AcaoEstadoJson {
            id: seq,
            inicio_seg: a
                .inicio
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            acao: a.acao.to_owned(),
            ambiente: a.ambiente.to_owned(),
            estado: estado.nome().to_owned(),
            erro: match &*estado {
                Estado::Falhou(e) => Some(e.clone()),
                _ => None,
            },
            saida: a.saida.linhas(),
        }
    })
}

#[derive(serde::Serialize)]
pub(super) struct AcaoEstadoJson {
    pub(super) id: u64,
    /// Epoch do início (o cliente mostra "há X min" e o título do terminal).
    pub(super) inicio_seg: u64,
    pub(super) acao: String,
    pub(super) ambiente: String,
    pub(super) estado: String,
    pub(super) erro: Option<String>,
    pub(super) saida: Vec<String>,
}

// ------------------------------------------------------------------ execução

/// Dispara a ação em task própria. `Ok(id)` = criada; `Err(msg)` = recusa
/// com a causa (o handler converte em status certo).
pub(super) async fn disparar(
    app: &std::sync::Arc<AppState>,
    nome: &str,
    acao: Acao,
    ambiente: &str,
) -> Result<(), String> {
    let env = match ambiente {
        "dev" => flux_common::protocol::Env::Dev,
        "prod" => flux_common::protocol::Env::Prod,
        _ => return Err("ambiente tem que ser dev ou prod".into()),
    };
    if ambiente == "prod" && !acao.em_prod_ok() {
        return Err(format!(
            "{} não roda em prod: PROD NUNCA apaga/recria banco ou refaz assets (regra prod ⊆ dev)",
            acao.nome()
        ));
    }
    // Uma ação por projeto: em curso = 409 no handler (checagem aqui de
    // novo dentro do lock de inserção para não perder a corrida).
    let app_task = Arc::clone(app);
    let nome_dono = nome.to_owned();
    let ambiente_dono = ambiente.to_owned();
    com_registro(|reg| {
        if reg
            .get(nome)
            .is_some_and(|a| *a.estado.lock().expect("mutex") == Estado::Rodando)
        {
            return Err(format!("{nome}: já tem ação rodando — espera terminar"));
        }
        let (saida, _) = SaidaAcao::nova();
        reg.insert(
            nome_dono.clone(),
            AcaoEmCurso {
                acao: acao.nome(),
                ambiente: ambiente_dono.clone(),
                inicio: std::time::SystemTime::now(),
                estado: Mutex::new(Estado::Rodando),
                saida,
            },
        );
        let nome = nome_dono.clone();
        let ambiente = ambiente_dono.clone();
        tokio::spawn(async move {
            let resultado =
                tokio::time::timeout(acao.teto(), rodar(Arc::clone(&app_task), &nome, acao, env))
                    .await
                    .unwrap_or_else(|_| Err(format!("estourou o teto de {:?}", acao.teto())));
            let estado_final = match &resultado {
                Ok(()) => Estado::Ok,
                Err(e) => Estado::Falhou(e.clone()),
            };
            com_registro(|reg| {
                if let Some(a) = reg.get_mut(&nome) {
                    *a.estado.lock().expect("mutex") = estado_final.clone();
                }
            });
            crate::events::event(
                &app_task.state,
                if estado_final == Estado::Ok {
                    "info"
                } else {
                    "error"
                },
                "painel",
                &format!(
                    "ação {} {ambiente} em {nome}: {}",
                    acao.nome(),
                    match &resultado {
                        Ok(()) => "concluída".into(),
                        Err(e) => format!("FALHOU: {e}"),
                    }
                ),
            );
        });
        Ok(())
    })
}

async fn rodar(
    app: Arc<AppState>,
    nome: &str,
    acao: Acao,
    env: flux_common::protocol::Env,
) -> Result<(), String> {
    let toml_src = app
        .state
        .project_toml(nome)
        .map_err(|e| format!("{nome}: flux.toml ilegível no estado: {e}"))?
        .ok_or_else(|| format!("{nome}: flux.toml ausente no estado"))?;
    let toml = flux_common::config::ProjectToml::parse_str(&toml_src)
        .map_err(|e| format!("flux.toml de {nome} inválido: {e}"))?;
    let Some(docker) = app.docker.clone() else {
        return Err("fluxd sem Docker conectado — ações só na VM".into());
    };

    match acao {
        // artisan direto no container app (mesma via do exec da :7420).
        Acao::Migrate => {
            rodar_cmd(&app, &docker, nome, env, &cmd_migrate(&toml)?, nome).await
        }
        Acao::Seed => rodar_cmd(&app, &docker, nome, env, "php artisan db:seed --force", nome).await,
        Acao::Optimize => rodar_cmd(&app, &docker, nome, env, "php artisan optimize:clear", nome).await,
        Acao::Shield => rodar_cmd(
            &app,
            &docker,
            nome,
            env,
            "php artisan shield:generate --all --no-interaction",
            nome,
        )
        .await,
        // Fresh = o caminho CANÔNICO (05): laravel-only, limpa a trilha de
        // migrations, evento warn. FreshSeed = seed ligado incondicional.
        Acao::Fresh => {
            crate::migrations::fresh_dev_forcado(&docker, &app.state, nome, &toml_src, None)
                .await
                .map(|saida| {
                    for l in saida.lines() {
                        enviar(nome, l);
                    }
                })
        }
        Acao::FreshSeed => {
            crate::migrations::fresh_dev_forcado(&docker, &app.state, nome, &toml_src, Some(true))
                .await
                .map(|saida| {
                    for l in saida.lines() {
                        enviar(nome, l);
                    }
                })
        }
        // Build: comando do [build] (default npm run build p/ laravel) com o
        // teto de 1800 s. Janela de silêncio do CLI NÃO se aplica (trade-off
        // declarado no módulo).
        Acao::Build => {
            let cmd = toml
                .build
                .as_ref()
                .and_then(|b| b.command.clone())
                .unwrap_or_else(|| "npm run build".to_owned());
            rodar_cmd(&app, &docker, nome, env, &cmd, nome).await
        }
        // Live ON = o caminho idempotente do wake (decide sozinho: em pé =
        // pula; órfão = limpa e ressuscita).
        Acao::LiveOn => {
            crate::compose::religar_live_pos_acordar(&app.state, &app, &docker, nome, env).await;
            enviar(nome, "live religado (idempotente — em pé = pulado)");
            Ok(())
        }
        // Live OFF = os mesmos execs do CLI live: mata node, limpa hot.
        Acao::LiveOff => {
            rodar_cmd(
                &app,
                &docker,
                nome,
                env,
                "for d in /proc/[0-9]*; do c=$(tr '\\0' ' ' < $d/cmdline 2>/dev/null); \
                 case \"$c\" in *node*) kill ${d#/proc/} 2>/dev/null;; esac; done; rm -f public/hot; echo live OFF",
                nome,
            )
            .await
        }
    }
}

/// migrate: laravel = artisan --force; node/rust = migrate_cmd do toml
/// (obrigatório lá — mesmo contrato do ciclo de schema).
fn cmd_migrate(toml: &flux_common::config::ProjectToml) -> Result<String, String> {
    if toml.framework() == flux_common::framework::Framework::Laravel {
        return Ok("php artisan migrate --force".into());
    }
    toml.db
        .as_ref()
        .and_then(|d| d.migrate_cmd.clone())
        .filter(|c| !c.trim().is_empty())
        .ok_or_else(|| {
            "projeto não-laravel sem [db] migrate_cmd no flux.toml — declare o comando".into()
        })
}

/// Escreve uma linha na saída da ação do projeto (buffer + broadcast).
fn enviar(nome: &str, l: &str) {
    com_registro(|reg| {
        if let Some(a) = reg.get(nome) {
            a.saida.linha(l.to_owned());
        }
    });
}

/// Docker exec COM saída linha a linha no canal (terminal ao vivo). Exit
/// code ≠ 0 = falha com a última linha como contexto.
async fn rodar_cmd(
    _app: &AppState,
    docker: &bollard::Docker,
    nome: &str,
    env: flux_common::protocol::Env,
    cmd: &str,
    destino: &str,
) -> Result<(), String> {
    let projeto = crate::docker::compose_project(nome, env);
    let Some(c) = crate::docker::find_app_container(docker, &projeto).await else {
        return Err(format!(
            "{projeto}: container app não encontrado — suba o projeto antes"
        ));
    };
    let Some(id) = c.id.clone() else {
        return Err(format!("{projeto}: container sem id"));
    };
    enviar(destino, &format!("$ {cmd}"));

    let exec = docker
        .create_exec(
            &id,
            CreateExecOptions::<String> {
                cmd: Some(vec!["sh".to_owned(), "-c".to_owned(), cmd.to_owned()]),
                attach_stdout: Some(true),
                attach_stderr: Some(true),
                ..Default::default()
            },
        )
        .await
        .map_err(|e| format!("{projeto}: criando exec: {e}"))?;
    match docker
        .start_exec(&exec.id, None::<StartExecOptions>)
        .await
        .map_err(|e| format!("{projeto}: iniciando exec: {e}"))?
    {
        bollard::exec::StartExecResults::Attached { mut output, .. } => {
            while let Some(chunk) = output.next().await {
                match chunk {
                    Ok(bollard::container::LogOutput::StdOut { message })
                    | Ok(bollard::container::LogOutput::StdErr { message }) => {
                        for l in String::from_utf8_lossy(&message).lines() {
                            enviar(destino, l);
                        }
                    }
                    Ok(_) => {}
                    Err(e) => return Err(format!("{projeto}: stream do exec: {e}")),
                }
            }
        }
        bollard::exec::StartExecResults::Detached => {
            return Err(format!("{projeto}: exec detached inesperado"));
        }
    }
    let code = docker
        .inspect_exec(&exec.id)
        .await
        .map_err(|e| format!("{projeto}: inspect do exec: {e}"))?
        .exit_code
        .unwrap_or(-1);
    enviar(destino, &format!("exit={code}"));
    if code == 0 {
        Ok(())
    } else {
        Err(format!("comando saiu com exit {code}"))
    }
}

// ------------------------------------------------------------------ handlers

#[derive(serde::Deserialize)]
pub(super) struct CorpoAcao {
    pub(super) acao: String,
    /// Default dev.
    pub(super) ambiente: Option<String>,
}

/// POST /api/projects/{name}/acoes — AÇÃO (sessão; o middleware barra token
/// em não-GET). 202 com o estado inicial; 409 se já roda; 403 fora da
/// matriz prod ⊆ dev.
pub(super) async fn disparar_acao(
    State(st): State<PanelState>,
    Path(name): Path<String>,
    Json(corpo): Json<CorpoAcao>,
) -> Response {
    let Some(acao) = Acao::de_str(&corpo.acao) else {
        return erro_json(
            StatusCode::UNPROCESSABLE_ENTITY,
            "ação desconhecida — válidas: migrate, fresh, fresh_seed, seed, build, live_on, live_off, optimize, shield",
        );
    };
    let ambiente = corpo.ambiente.as_deref().unwrap_or("dev");
    if ambiente != "dev" && ambiente != "prod" {
        return erro_json(StatusCode::UNPROCESSABLE_ENTITY, "ambiente é dev ou prod");
    }
    if ambiente == "prod" {
        // Registrar em prod é raro e sério: projeto precisa existir.
        match leitura_projeto_existe(&st, &name).await {
            Ok(true) => {}
            Ok(false) => {
                return erro_json(
                    StatusCode::NOT_FOUND,
                    &format!("{name}: não registrado no fluxd"),
                );
            }
            Err(e) => return erro_json(StatusCode::SERVICE_UNAVAILABLE, &e),
        }
    }
    match disparar(&st.app, &name, acao, ambiente).await {
        Ok(()) => {
            let estado = estado_atual(&name).unwrap_or(AcaoEstadoJson {
                id: 0,
                inicio_seg: 0,
                acao: acao.nome().into(),
                ambiente: ambiente.into(),
                estado: "rodando".into(),
                erro: None,
                saida: Vec::new(),
            });
            (StatusCode::ACCEPTED, Json(estado)).into_response()
        }
        Err(e) if e.contains("já tem ação rodando") => erro_json(StatusCode::CONFLICT, &e),
        Err(e) if e.contains("PROD NUNCA") || e.contains("não roda em prod") => {
            erro_json(StatusCode::FORBIDDEN, &e)
        }
        Err(e) => erro_json(StatusCode::CONFLICT, &e),
    }
}

async fn leitura_projeto_existe(st: &PanelState, name: &str) -> Result<bool, String> {
    super::leitura::projetos(&st.app).map(|l| l.iter().any(|(n, _)| n == name))
}

/// GET /api/projects/{name}/acoes/atual — estado + saída da ação atual/
/// última (o cliente pinta o terminal com isso e abre o stream).
pub(super) async fn acao_atual(
    State(_st): State<PanelState>,
    Path(name): Path<String>,
) -> Response {
    match estado_atual(&name) {
        Some(estado) => Json(estado).into_response(),
        None => erro_json(
            StatusCode::NOT_FOUND,
            "nenhuma ação para este projeto ainda",
        ),
    }
}

/// GET /api/projects/{name}/acoes/stream — SSE: replay do buffer, segue ao
/// vivo e fecha com `event: fim` quando a ação termina.
pub(super) async fn acao_stream(
    State(_st): State<PanelState>,
    Path(name): Path<String>,
) -> Response {
    let Some((_, curso)) = com_registro(|reg| {
        reg.get(&name).map(|a| {
            (
                a.acao,
                (
                    a.saida.linhas(),
                    a.saida.canal.subscribe(),
                    a.estado.lock().expect("mutex").clone(),
                ),
            )
        })
    }) else {
        return erro_json(
            StatusCode::NOT_FOUND,
            "nenhuma ação para este projeto ainda",
        );
    };
    let (linhas, rx, estado) = curso;
    let fila: VecDeque<Result<String, Infallible>> = linhas
        .into_iter()
        .map(|l| Ok(format!("data: {l}\n\n")))
        .collect();
    let ja_terminou = estado != Estado::Rodando;
    let frames = futures_util::stream::unfold(
        (rx, fila, ja_terminou),
        |(mut rx, mut fila, mut ja_terminou)| async move {
            loop {
                if let Some(frame) = fila.pop_front() {
                    return Some((frame, (rx, fila, ja_terminou)));
                }
                if ja_terminou {
                    return Some((
                        Ok("event: fim\ndata: {}\n\n".to_owned()),
                        (rx, fila, ja_terminou),
                    ));
                }
                match rx.recv().await {
                    Ok(l) => {
                        return Some((Ok(format!("data: {l}\n\n")), (rx, fila, ja_terminou)));
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        return Some((
                            Ok(format!("event: lagged\ndata: {n} linhas perdidas\n\n")),
                            (rx, fila, ja_terminou),
                        ));
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        // Emissor sumiu (restart): termina com o que tem.
                        ja_terminou = true;
                    }
                }
            }
        },
    );
    resposta_sse(frames)
}
