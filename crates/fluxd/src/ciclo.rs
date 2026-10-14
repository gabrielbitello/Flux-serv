//! Ciclo de dev do projeto na API de controle (06): `down`, `logs` e `exec` —
//! os comandos básicos do docker/compose com prefixo `flux`, servidos pelo
//! fluxd (o CLI só fala HTTP; o lease de sono é responsabilidade DELE, 04 —
//! aqui o fluxd "só serve" e não toca em sleep_state).
//!
//! Contrato CLI ↔ fluxd:
//!
//! - `POST /projects/{name}/down?env=dev` → para o compose project
//!   `flux-{name}-{env}` com `docker compose -p <projeto> stop`
//!   (tokio::process, na pasta sincronizada do projeto), grava evento e
//!   responde 200 `{"stopped":true,"project":name}`; falha → 500 com a causa.
//!   Projeto não registrado → 404; pasta ausente (sync não rodou, 02) → 409.
//! - `GET /projects/{name}/logs?env=dev&tail=200&follow=true|false` → sem
//!   follow: `text/plain` com as ÚLTIMAS N linhas (default 200) do container
//!   app (padrão `find_app_container`, stdout+stderr, sem timestamps). Com
//!   follow: SSE (`text/event-stream`) — cada linha vira `data: <linha>` +
//!   linha vazia, stream vivo até o cliente cortar (sem timeout; quebras
//!   parciais de chunk são acumuladas no buffer até o `\n`).
//! - `POST /projects/{name}/exec?env=dev` com `{"cmd":["php","artisan",
//!   "migrate","--force"]}` → `docker exec` NO container app (bollard
//!   create_exec/start, sem TTY, workdir `/var/www`), stdout+stderr
//!   combinados com teto de 256 KB e timeout de 120 s, `text/plain` com a
//!   ÚLTIMA linha `exit={code}` (inspect do exec). `cmd` vazio/ausente →
//!   422; container sem rodar → 409.
//!
//! Os handlers são `pub` prontos para axum (`State<Arc<AppState>>` +
//! extratores padrão); AS ROTAS são do integrador (api.rs). Erros seguem o
//! padrão `{"error": causa}` da api.rs — o helper `erro` é copiado aqui
//! porque não é `pub` lá.

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use bollard::Docker;
use bollard::container::LogOutput;
use bollard::exec::CreateExecOptions;
use bollard::query_parameters::{LogsOptions, LogsOptionsBuilder};
use flux_common::protocol::{ApiErrorBody, Env};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::docker::{self, find_app_container};
use crate::events;

/// Últimas N linhas quando o CLI não pede outra coisa (contrato CLI ↔ fluxd).
pub const LOGS_TAIL_PADRAO: usize = 200;

/// Teto do `exec`: 120 s de relógio (timeout do tokio) e 256 KB de saída
/// combinada — migrate de dev é segundos; comando preso não segura a conexão.
pub const EXEC_TIMEOUT: Duration = Duration::from_secs(120);
pub const EXEC_TETO_BYTES: usize = 256 * 1024;

/// Workdir do exec: raiz do app dentro do container (convenção da plataforma,
/// 05/06 — `php artisan` roda de lá).
const EXEC_WORKDIR: &str = "/var/www";

/// Query comum das três rotas: tudo `Option` com default no handler
/// (`env=dev`, `tail=200`, `follow=false`) — parse com causa própria 422
/// em vez da rejection seca do axum.
#[derive(Debug, Deserialize)]
pub struct CicloQuery {
    pub env: Option<String>,
    pub tail: Option<String>,
    pub follow: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ExecBody {
    /// Comando e argumentos, ex. `["php","artisan","migrate","--force"]`.
    pub cmd: Option<Vec<String>>,
}

#[derive(Debug, Serialize)]
struct DownReply {
    stopped: bool,
    project: String,
}

/// Copiado da api.rs (o helper de lá não é `pub`): todo erro sai com a causa
/// no corpo `{"error": ...}` — parsers do CLI dependem disso (12).
fn erro(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(ApiErrorBody { error: msg.into() })).into_response()
}

/// Ambiente da query com default `dev`; inválido → 422 com causa (mesma
/// mensagem do `Env::from_str` do protocolo). `Box` no Err: mesmo motivo do
/// `toml_gravado` na api.rs — o clippy reclama do tamanho do `Response`.
fn env_de(q: &CicloQuery) -> Result<Env, Box<Response>> {
    let bruto = q.env.as_deref().unwrap_or("dev");
    bruto
        .parse::<Env>()
        .map_err(|m| Box::new(erro(StatusCode::UNPROCESSABLE_ENTITY, m)))
}

/// `tail` da query com default 200; `0` = tudo (semântica do `--tail` do
/// docker); lixo → 422 com causa.
fn tail_de(q: &CicloQuery) -> Result<usize, Box<Response>> {
    match q.tail.as_deref() {
        None | Some("") => Ok(LOGS_TAIL_PADRAO),
        Some(s) => s.parse::<usize>().map_err(|_| {
            Box::new(erro(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("tail inválido: {s:?} (inteiro ≥ 0; 0 = tudo)"),
            ))
        }),
    }
}

/// `follow` da query com default `false`; só `true`/`false` são aceitos.
fn follow_de(q: &CicloQuery) -> Result<bool, Box<Response>> {
    match q.follow.as_deref() {
        None | Some("") | Some("false") => Ok(false),
        Some("true") => Ok(true),
        Some(outro) => Err(Box::new(erro(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("follow inválido: {outro:?} (use true|false)"),
        ))),
    }
}

/// Existência do projeto no registro — espelho do `garante_projeto` da api.rs
/// (não é `pub` lá). Fantasma → 404 com causa; erro de leitura → 500.
fn projeto_registrado(app: &AppState, name: &str) -> Result<(), Box<Response>> {
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

// ---------------------------------------------------------------------- down

/// Argumentos exatos do compose stop (contrato): `docker compose -p <projeto>
/// stop`. Montagem separada para o teste de params.
fn args_down(projeto: &str) -> Vec<String> {
    vec![
        "compose".to_owned(),
        "-p".to_owned(),
        projeto.to_owned(),
        "stop".to_owned(),
    ]
}

/// POST /projects/{name}/down?env=dev — `docker compose -p flux-{name}-{env}
/// stop` na pasta sincronizada do projeto. Containers do app param; o
/// `flux-db` (fora do projeto de compose) segue de pé (06).
pub async fn down(
    State(app): State<Arc<AppState>>,
    Path(name): Path<String>,
    Query(q): Query<CicloQuery>,
) -> Response {
    let env = match env_de(&q) {
        Ok(e) => e,
        Err(r) => return *r,
    };
    if let Err(r) = projeto_registrado(&app, &name) {
        return *r;
    }
    let projeto = docker::compose_project(&name, env);
    let dir = std::path::Path::new(crate::temporary::PROJECTS_ROOT_PADRAO).join(&name);
    if !dir.is_dir() {
        // Sem pasta não há compose nem containers do projeto: 409 (o sync é
        // pré-condição do ciclo, 02) — nunca 500 genérico.
        let msg = format!(
            "pasta {} não existe na VM — o sync (mutagen) roda antes do down (02)",
            dir.display()
        );
        events::event(&app.state, "error", "down", &format!("{projeto}: {msg}"));
        return erro(StatusCode::CONFLICT, msg);
    }
    let saida = match tokio::process::Command::new("docker")
        .args(args_down(&projeto))
        .current_dir(&dir)
        .output()
        .await
    {
        Ok(s) => s,
        Err(e) => {
            let msg = format!("{projeto}: docker compose stop não rodou: {e}");
            events::event(&app.state, "error", "down", &msg);
            return erro(StatusCode::INTERNAL_SERVER_ERROR, msg);
        }
    };
    if !saida.status.success() {
        let msg = format!(
            "{projeto}: compose stop falhou: {}",
            String::from_utf8_lossy(&saida.stderr).trim()
        );
        events::event(&app.state, "error", "down", &msg);
        return erro(StatusCode::INTERNAL_SERVER_ERROR, msg);
    }
    events::event(
        &app.state,
        "info",
        "down",
        &format!("{projeto}: containers parados (compose stop)"),
    );
    Json(DownReply {
        stopped: true,
        project: name,
    })
    .into_response()
}

// ---------------------------------------------------------------------- logs

/// Montagem dos params do `docker logs` (contrato): stdout+stderr, sem
/// timestamps, últimas `tail` linhas ("0" = tudo no engine, igual ao CLI).
fn logs_options(tail: usize, follow: bool) -> LogsOptions {
    LogsOptionsBuilder::new()
        .follow(follow)
        .stdout(true)
        .stderr(true)
        .timestamps(false)
        .tail(&tail.to_string())
        .build()
}

/// Acumula a quebra parcial no buffer (em BYTES crus) e devolve SÓ as linhas
/// completas (terminadas em `\n`), cada uma decodificada de UMA vez — chunks
/// do engine não respeitam fronteira de linha NEM de caractere UTF-8: juntar
/// String por chunk converteria multibyte cortado entre chunks em U+FFFD.
fn linhas_completas(chunk: &[u8], buffer: &mut Vec<u8>) -> Vec<String> {
    buffer.extend_from_slice(chunk);
    let mut linhas = Vec::new();
    while let Some(pos) = buffer.iter().position(|&b| b == b'\n') {
        let bruta: Vec<u8> = buffer.drain(..=pos).collect();
        // O último byte é o próprio '\n' — a linha crua vai sem ele.
        linhas.push(String::from_utf8_lossy(&bruta[..bruta.len() - 1]).into_owned());
    }
    linhas
}

/// Últimas `n` linhas do texto; `n = 0` devolve tudo (semântica do `--tail`
/// do docker). Usada no caminho SEM follow, em cima do que o engine já
/// filtrou com o mesmo `tail`.
fn ultimas_linhas(s: &str, n: usize) -> String {
    if n == 0 {
        return s.to_owned();
    }
    let linhas: Vec<&str> = s.lines().collect();
    let inicio = linhas.len().saturating_sub(n);
    linhas[inicio..].join("\n")
}

/// Frame SSE de uma linha crua: `data: <linha>` + linha vazia.
fn frame_sse(linha: &str) -> String {
    format!("data: {linha}\n\n")
}

/// GET /projects/{name}/logs?env=dev&tail=200&follow=true|false — logs do
/// container app. Sem follow: `text/plain` com as últimas N linhas. Com
/// follow: SSE infinito (`data: <linha>` + linha vazia por linha), vivo até
/// o cliente cortar — o drop do body encerra o stream do engine, sem timeout.
pub async fn logs(
    State(app): State<Arc<AppState>>,
    Path(name): Path<String>,
    Query(q): Query<CicloQuery>,
) -> Response {
    let env = match env_de(&q) {
        Ok(e) => e,
        Err(r) => return *r,
    };
    let tail = match tail_de(&q) {
        Ok(t) => t,
        Err(r) => return *r,
    };
    let follow = match follow_de(&q) {
        Ok(f) => f,
        Err(r) => return *r,
    };
    if let Err(r) = projeto_registrado(&app, &name) {
        return *r;
    }
    let Some(docker) = app.docker.clone() else {
        return erro(
            StatusCode::SERVICE_UNAVAILABLE,
            "fluxd sem Docker conectado — logs de container só na VM",
        );
    };
    let projeto = docker::compose_project(&name, env);
    let Some(container) = find_app_container(&docker, &projeto).await else {
        return erro(
            StatusCode::CONFLICT,
            format!(
                "{projeto}: container app não encontrado — o projeto nunca subiu? (rode `flux up`)"
            ),
        );
    };
    let Some(id) = container.id.clone() else {
        return erro(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("{projeto}: container app sem id no inspect"),
        );
    };
    let stream = docker.logs(&id, Some(logs_options(tail, follow)));

    if !follow {
        let mut tudo: Vec<u8> = Vec::new();
        let mut stream = stream;
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(LogOutput::StdOut { message }) | Ok(LogOutput::StdErr { message }) => {
                    tudo.extend_from_slice(&message);
                }
                Ok(_) => {}
                Err(e) => {
                    return erro(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("{projeto}: stream de logs: {e}"),
                    );
                }
            }
        }
        // Lossy SÓ no texto completo: bytes multibyte cortados ENTRE chunks
        // do engine voltam a valer antes da decodificação.
        let texto = String::from_utf8_lossy(&tudo).into_owned();
        return (
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            ultimas_linhas(&texto, tail),
        )
            .into_response();
    }

    // follow=true: SSE. Cada linha completa vira um frame `data: <linha>\n\n`;
    // erro do engine é visível como frame de erro e ENCERRA o stream (nada
    // silencioso). O cliente corta = drop do body = stream termina limpo.
    let mut buffer: Vec<u8> = Vec::new();
    let frames = stream.flat_map(move |chunk| {
        let linhas = match chunk {
            Ok(LogOutput::StdOut { message }) | Ok(LogOutput::StdErr { message }) => {
                linhas_completas(&message, &mut buffer)
            }
            Ok(_) => Vec::new(),
            // SÓ o conteúdo: o envelope `data: ...\n\n` é do frame_sse —
            // embrulhar o frame pronto duplicava o prefixo no fio.
            Err(e) => vec![format!("[fluxd: erro no stream de logs do engine: {e}]")],
        };
        futures_util::stream::iter(
            linhas
                .into_iter()
                .map(|linha| Ok::<String, std::convert::Infallible>(frame_sse(&linha))),
        )
    });
    let mut res = Response::new(Body::from_stream(frames));
    *res.status_mut() = StatusCode::OK;
    res.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    res.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    res
}

// ---------------------------------------------------------------------- exec

/// `docker exec` no container app: create_exec (attach stdout+stderr, SEM
/// TTY, workdir `/var/www`) + start coletando a saída combinada. Devolve
/// `(saída, truncado_no_teto, exit_code)` — exit vem do inspect; truncado
/// com o exec ainda rodando devolve `-1` (desconhecido, mesma convenção do
/// db.rs). Erros sempre com origem, nunca silenciosos.
async fn rodar_exec(
    docker: &Docker,
    projeto: &str,
    id: &str,
    cmd: &[String],
) -> Result<(String, bool, i64), String> {
    // Workdir do SERVICE, não do flux: o inspect traz o working_dir efetivo
    // do container (WORKDIR da imagem ou o declarado no compose). Fallback =
    // convenção histórica /var/www.
    let workdir = match docker.inspect_container(id, None).await {
        Ok(info) => info
            .config
            .as_ref()
            .and_then(|c| c.working_dir.as_deref())
            .filter(|w| !w.is_empty())
            .unwrap_or(EXEC_WORKDIR)
            .to_owned(),
        Err(_) => EXEC_WORKDIR.to_owned(),
    };
    let exec = docker
        .create_exec(
            id,
            CreateExecOptions {
                attach_stdin: Some(false),
                attach_stdout: Some(true),
                attach_stderr: Some(true),
                tty: Some(false),
                cmd: Some(cmd.to_vec()),
                working_dir: Some(workdir),
                ..Default::default()
            },
        )
        .await
        .map_err(|e| format!("{projeto}: create_exec: {e}"))?;

    let output = match docker.start_exec(&exec.id, None).await {
        Ok(bollard::exec::StartExecResults::Attached { output, .. }) => output,
        Ok(bollard::exec::StartExecResults::Detached) => {
            return Err(format!(
                "{projeto}: exec voltou Detached inesperadamente (esperava attach)"
            ));
        }
        Err(e) => return Err(format!("{projeto}: start_exec: {e}")),
    };

    let mut saida = String::new();
    let mut truncado = false;
    let mut output = output;
    while let Some(chunk) = output.next().await {
        match chunk {
            Ok(LogOutput::StdOut { message }) | Ok(LogOutput::StdErr { message }) => {
                let livre = EXEC_TETO_BYTES.saturating_sub(saida.len());
                if livre == 0 {
                    truncado = true;
                    break;
                }
                let fatia = &message[..message.len().min(livre)];
                saida.push_str(&String::from_utf8_lossy(fatia));
                if saida.len() >= EXEC_TETO_BYTES {
                    truncado = true;
                    break;
                }
            }
            Ok(_) => {}
            Err(e) => return Err(format!("{projeto}: stream do exec: {e}")),
        }
    }

    let inspect = docker
        .inspect_exec(&exec.id)
        .await
        .map_err(|e| format!("{projeto}: inspect_exec: {e}"))?;
    // Contrato do módulo: truncado com o exec AINDA RODANDO = exit
    // desconhecido (-1). O ExitCode que o Docker serializa durante Running é
    // 0 — repassar seria falso sucesso (o CLI usa o código como exit próprio
    // do `flux exec`). Exec que TERMINOU com saída acima do teto mantém o
    // exit real do inspect.
    let code = if truncado && inspect.running.unwrap_or(false) {
        -1
    } else {
        inspect.exit_code.unwrap_or(-1)
    };
    Ok((saida, truncado, code))
}

/// POST /projects/{name}/exec?env=dev — roda `cmd` NO container app e devolve
/// `text/plain` com a saída combinada terminando em `exit={code}`. Teto de
/// 256 KB com marker explícito de truncamento; timeout de 120 s → 500 com
/// causa (comando longo não é sucesso silencioso).
pub async fn exec(
    State(app): State<Arc<AppState>>,
    Path(name): Path<String>,
    Query(q): Query<CicloQuery>,
    Json(b): Json<ExecBody>,
) -> Response {
    let env = match env_de(&q) {
        Ok(e) => e,
        Err(r) => return *r,
    };
    if let Err(r) = projeto_registrado(&app, &name) {
        return *r;
    }
    let cmd = match b.cmd {
        Some(c) if !c.is_empty() => c,
        Some(_) | None => {
            return erro(
                StatusCode::UNPROCESSABLE_ENTITY,
                "cmd vazio ou ausente: envie {\"cmd\":[\"php\",\"artisan\",\"migrate\",\"--force\"]}",
            );
        }
    };
    let Some(docker) = app.docker.clone() else {
        return erro(
            StatusCode::SERVICE_UNAVAILABLE,
            "fluxd sem Docker conectado — exec só roda na VM",
        );
    };
    let projeto = docker::compose_project(&name, env);
    let Some(container) = find_app_container(&docker, &projeto).await else {
        return erro(
            StatusCode::CONFLICT,
            format!(
                "{projeto}: container app não encontrado — o projeto nunca subiu? (rode `flux up`)"
            ),
        );
    };
    // Exec exige container RODANDO (pausado também não aceita exec).
    let rodando = container.state.as_ref().is_some_and(|st| {
        st.status == Some(bollard::models::ContainerStateStatusEnum::RUNNING)
            && !st.paused.unwrap_or(false)
    });
    if !rodando {
        let estado = container
            .state
            .as_ref()
            .and_then(|st| st.status)
            .map(|s| format!("{s:?}"))
            .unwrap_or_else(|| "desconhecido".to_owned());
        let msg = format!(
            "{projeto}: container app não está rodando (estado: {estado}) — suba o projeto antes (`flux up`)"
        );
        events::event(&app.state, "warn", "exec", &msg);
        return erro(StatusCode::CONFLICT, msg);
    }
    let Some(id) = container.id.clone() else {
        return erro(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("{projeto}: container app sem id no inspect"),
        );
    };

    let resultado =
        tokio::time::timeout(EXEC_TIMEOUT, rodar_exec(&docker, &projeto, &id, &cmd)).await;
    let (saida, truncado, code) = match resultado {
        Ok(Ok(v)) => v,
        Ok(Err(msg)) => {
            events::event(&app.state, "error", "exec", &msg);
            return erro(StatusCode::INTERNAL_SERVER_ERROR, msg);
        }
        Err(_) => {
            let msg = format!(
                "{projeto}: exec estourou o timeout de {} s — comando preso ou longo demais para o exec síncrono",
                EXEC_TIMEOUT.as_secs()
            );
            events::event(&app.state, "error", "exec", &msg);
            return erro(StatusCode::INTERNAL_SERVER_ERROR, msg);
        }
    };

    let mut partes: Vec<String> = Vec::new();
    let saida = saida.trim_end();
    if !saida.is_empty() {
        partes.push(saida.to_owned());
    }
    if truncado {
        partes.push(format!(
            "[fluxd: saída truncada no teto de {} KB]",
            EXEC_TETO_BYTES / 1024
        ));
    }
    partes.push(format!("exit={code}"));
    events::event(
        &app.state,
        "info",
        "exec",
        &format!(
            "{projeto}: exec {cmd:?} → exit {code}{}",
            if truncado { " (saída truncada)" } else { "" }
        ),
    );
    (
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        partes.join("\n"),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::State as DbState;
    use axum::Router;
    use axum::routing::{get, post};
    use flux_common::config::GlobalConfig;
    use http_body_util::BodyExt;

    /// Estado sem engine (docker: None) — as validações sem Docker são todas
    /// testáveis aqui; o caminho real fica no teste GATED mais abaixo.
    fn app_teste() -> Arc<AppState> {
        let st = DbState::in_memory().unwrap();
        Arc::new(AppState::new(st, &GlobalConfig::default(), None))
    }

    /// Router com as MESMAS rotas que o integrador liga na api.rs.
    fn router_teste(app: Arc<AppState>) -> Router {
        Router::new()
            .route("/projects/{name}/down", post(down))
            .route("/projects/{name}/logs", get(logs))
            .route("/projects/{name}/exec", post(exec))
            .with_state(app)
    }

    async fn chama(
        app: &Arc<AppState>,
        method: &str,
        uri: &str,
        body: Option<String>,
    ) -> (u16, String) {
        use tower::ServiceExt;
        let svc = router_teste(app.clone());
        let builder = hyper::Request::builder()
            .method(hyper::Method::from_bytes(method.as_bytes()).unwrap())
            .uri(uri);
        let req = match body {
            Some(b) => builder
                .header(hyper::header::CONTENT_TYPE, "application/json")
                .body(Body::from(b))
                .unwrap(),
            None => builder.body(Body::empty()).unwrap(),
        };
        let res = svc.oneshot(req).await.unwrap();
        let status = res.status().as_u16();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    fn com_projeto(nome: &str) -> Arc<AppState> {
        let app = app_teste();
        app.state
            .register_project(nome, &format!("name = \"{nome}\""), "h")
            .unwrap();
        app
    }

    #[tokio::test]
    async fn env_ruim_e_params_ruins_viram_422_com_causa() {
        let app = com_projeto("vipers");
        for (method, uri, body) in [
            ("POST", "/projects/vipers/down?env=homolog", None),
            ("GET", "/projects/vipers/logs?env=homolog", None),
            ("GET", "/projects/vipers/logs?env=dev&tail=abc", None),
            ("GET", "/projects/vipers/logs?env=dev&follow=banana", None),
            (
                "POST",
                "/projects/vipers/exec?env=homolog",
                Some(r#"{"cmd":["echo"]}"#.to_owned()),
            ),
        ] {
            let (s, c) = chama(&app, method, uri, body).await;
            assert_eq!(s, 422, "{method} {uri}: {c}");
            assert!(c.contains("inválido"), "{method} {uri}: causa: {c}");
        }
    }

    #[tokio::test]
    async fn cmd_vazio_ou_ausente_vira_422_antes_do_engine() {
        let app = com_projeto("vipers");
        // docker: None no estado de teste — 422 de validação vem ANTES do 503.
        for corpo in [r#"{"cmd":[]}"#, r#"{}"#] {
            let (s, c) = chama(
                &app,
                "POST",
                "/projects/vipers/exec?env=dev",
                Some(corpo.to_owned()),
            )
            .await;
            assert_eq!(s, 422, "corpo {corpo}: {c}");
            assert!(c.contains("cmd vazio"), "corpo {corpo}: causa: {c}");
        }
    }

    #[tokio::test]
    async fn projeto_fantasma_vira_404_nas_tres_rotas() {
        let app = app_teste();
        for (method, uri, body) in [
            ("POST", "/projects/fantasma/down?env=dev", None),
            ("GET", "/projects/fantasma/logs?env=dev", None),
            (
                "POST",
                "/projects/fantasma/exec?env=dev",
                Some(r#"{"cmd":["echo"]}"#.to_owned()),
            ),
        ] {
            let (s, c) = chama(&app, method, uri, body).await;
            assert_eq!(s, 404, "{method} {uri}: {c}");
            assert!(c.contains("fantasma"), "{method} {uri}: causa: {c}");
            assert!(c.contains("não registrado"), "{method} {uri}: {c}");
        }
    }

    #[tokio::test]
    async fn down_sem_pasta_vira_409_com_evento() {
        let app = com_projeto("vipers");
        let (s, c) = chama(&app, "POST", "/projects/vipers/down?env=dev", None).await;
        assert_eq!(s, 409, "{c}");
        assert!(c.contains("pasta"), "causa: {c}");
        assert!(c.contains("sync"), "aponta o pré-requisito (02): {c}");
        // Erro nunca silencioso: a recusa está na trilha de eventos.
        let n: i64 = app
            .state
            .with(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM events WHERE scope = 'down' AND level = 'error'",
                    [],
                    |r| r.get(0),
                )
            })
            .unwrap();
        assert!(n >= 1, "recusa de down vira evento");
    }

    #[tokio::test]
    async fn logs_e_exec_sem_engine_viram_503_com_causa() {
        let app = com_projeto("vipers");
        let (s, c) = chama(&app, "GET", "/projects/vipers/logs?env=dev", None).await;
        assert_eq!(s, 503, "{c}");
        assert!(c.contains("Docker"), "causa: {c}");
        let (s, c) = chama(
            &app,
            "POST",
            "/projects/vipers/exec?env=dev",
            Some(r#"{"cmd":["echo","oi"]}"#.to_owned()),
        )
        .await;
        assert_eq!(s, 503, "{c}");
        assert!(c.contains("Docker"), "causa: {c}");
    }

    #[test]
    fn montagem_de_params_segue_o_contrato() {
        // down: comando exato `docker compose -p flux-{name}-{env} stop`
        assert_eq!(
            args_down("flux-vipers-dev"),
            vec![
                "compose".to_owned(),
                "-p".to_owned(),
                "flux-vipers-dev".to_owned(),
                "stop".to_owned()
            ]
        );
        // logs: stdout+stderr, sem timestamps, tail como string, follow repassado
        let o = logs_options(LOGS_TAIL_PADRAO, false);
        assert_eq!(o.tail, "200");
        assert!(!o.follow);
        assert!(o.stdout && o.stderr);
        assert!(!o.timestamps);
        let o = logs_options(0, true);
        assert_eq!(
            o.tail, "0",
            "0 = tudo no engine (igual ao --tail do docker)"
        );
        assert!(o.follow);
    }

    #[test]
    fn montagem_de_query_tem_defaults_e_recusa_lixo() {
        let q = |tail: Option<&str>, follow: Option<&str>| CicloQuery {
            env: None,
            tail: tail.map(ToOwned::to_owned),
            follow: follow.map(ToOwned::to_owned),
        };
        assert_eq!(tail_de(&q(None, None)).unwrap(), LOGS_TAIL_PADRAO);
        assert_eq!(tail_de(&q(Some(""), None)).unwrap(), LOGS_TAIL_PADRAO);
        assert_eq!(tail_de(&q(Some("50"), None)).unwrap(), 50);
        assert!(tail_de(&q(Some("abc"), None)).is_err());
        assert!(!follow_de(&q(None, None)).unwrap());
        assert!(!follow_de(&q(None, Some("false"))).unwrap());
        assert!(follow_de(&q(None, Some("true"))).unwrap());
        assert!(follow_de(&q(None, Some("1"))).is_err());
    }

    #[test]
    fn linhas_completas_acumula_quebra_parcial() {
        let mut buf: Vec<u8> = Vec::new();
        assert!(linhas_completas(b"abc", &mut buf).is_empty());
        let linhas = linhas_completas(b"de\nfg\n", &mut buf);
        assert_eq!(linhas, vec!["abcde".to_owned(), "fg".to_owned()]);
        assert!(
            linhas_completas(b"hi", &mut buf).is_empty(),
            "resto fica no buffer"
        );
        assert_eq!(buf, b"hi");
        // `\r\n` do engine (se vier): só o `\n` é consumido, o `\r` segue CRU
        let mut buf: Vec<u8> = Vec::new();
        assert_eq!(
            linhas_completas(b"x\r\n", &mut buf),
            vec!["x\r".to_owned()],
            "linha crua: nada de limpeza além do \\n"
        );
    }

    /// Revisão: chunk do engine pode cortar DENTRO de um caractere UTF-8 —
    /// a junção é em BYTES e o lossy só na linha completa; por-chunk viraria
    /// U+FFFD ("ç" = 0xC3 0xA7 dividido entre chunks).
    #[test]
    fn linhas_completas_junta_multibyte_cortado_entre_chunks() {
        let mut buf: Vec<u8> = Vec::new();
        assert!(linhas_completas(&[b'a', 0xC3], &mut buf).is_empty());
        let linhas = linhas_completas(&[0xA7, b'o', b'\n'], &mut buf);
        assert_eq!(linhas, vec!["aço".to_owned()], "byte parcial junta certo");
    }

    #[test]
    fn ultimas_linhas_e_frame_sse_seguem_contrato() {
        let s = "a\nb\nc\nd\ne";
        assert_eq!(ultimas_linhas(s, 2), "d\ne");
        assert_eq!(ultimas_linhas(s, 10), s, "pediu mais do que há = tudo");
        assert_eq!(ultimas_linhas(s, 0), s, "0 = tudo (docker --tail)");
        assert_eq!(ultimas_linhas("", 5), "");
        assert_eq!(frame_sse("linha crua"), "data: linha crua\n\n");
    }

    // ---------------------------------------------------------------- GATED
    // Caminho real contra engine de Docker (FLUX_DOCKER_URL, convenção do
    // repo): exec parado → 409, exec real com exit code, teto de saída e
    // follow SSE ao vivo. Sem a env, pula com aviso — nunca falha.

    #[tokio::test(flavor = "multi_thread")]
    async fn exec_e_logs_reais_com_engine_gated() {
        if std::env::var("FLUX_DOCKER_URL").is_err() {
            eprintln!("pulado: FLUX_DOCKER_URL ausente — exec/follow reais não testados aqui");
            return;
        }
        let Some(engine) = docker::DockerSource::connect() else {
            eprintln!("pulado: Docker não conectou mesmo com FLUX_DOCKER_URL");
            return;
        };
        let st = DbState::in_memory().unwrap();
        st.register_project("cicloexec", "name = \"cicloexec\"", "h")
            .unwrap();
        st.register_project("ciclofollow", "name = \"ciclofollow\"", "h")
            .unwrap();
        let app = Arc::new(AppState::new(
            st,
            &GlobalConfig::default(),
            Some(engine.clone()),
        ));

        use bollard::query_parameters::RemoveContainerOptionsBuilder;
        let rm = RemoveContainerOptionsBuilder::default().force(true).build();
        for nome in ["flux-cicloexec-dev-app-1", "flux-ciclofollow-dev-app-1"] {
            let _ = engine.remove_container(nome, Some(rm.clone())).await;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;

        // dummy PARADO (create_dummy_container não starta): exec → 409 com causa
        let id = docker::create_dummy_container(&engine, "cicloexec", Env::Dev)
            .await
            .expect("criar dummy");
        let (s, c) = chama(
            &app,
            "POST",
            "/projects/cicloexec/exec?env=dev",
            Some(r#"{"cmd":["echo","oi"]}"#.to_owned()),
        )
        .await;
        assert_eq!(s, 409, "container parado: {c}");
        assert!(c.contains("não está rodando"), "causa: {c}");

        // acordado: exec real devolve a saída e termina com exit=0
        engine
            .start_container(&id, None)
            .await
            .expect("startar dummy");
        let (s, c) = chama(
            &app,
            "POST",
            "/projects/cicloexec/exec?env=dev",
            Some(r#"{"cmd":["echo","flux-ok"]}"#.to_owned()),
        )
        .await;
        assert_eq!(s, 200, "{c}");
        assert!(c.contains("flux-ok"), "saída no corpo: {c}");
        assert!(c.ends_with("exit=0"), "última linha exit: {c:?}");

        // exit code não-zero do comando propaga no corpo
        let (s, c) = chama(
            &app,
            "POST",
            "/projects/cicloexec/exec?env=dev",
            Some(r#"{"cmd":["sh","-c","exit 3"]}"#.to_owned()),
        )
        .await;
        assert_eq!(s, 200, "{c}");
        assert!(c.ends_with("exit=3"), "exit code do comando: {c:?}");

        // teto de 256 KB: marker de truncamento + exit por último, corpo limitado
        let gigante = r#"{"cmd":["sh","-c","i=0; while [ $i -lt 40000 ]; do echo 0123456789012345678901234567890123456789; i=$((i+1)); done"]}"#;
        let (s, c) = chama(
            &app,
            "POST",
            "/projects/cicloexec/exec?env=dev",
            Some(gigante.to_owned()),
        )
        .await;
        assert_eq!(s, 200, "{c}");
        assert!(c.contains("truncada"), "marker de teto: {c:?}");
        assert!(
            c.len() < EXEC_TETO_BYTES + 512,
            "corpo respeita o teto: {}",
            c.len()
        );
        // Revisão: break no teto deixa o exec AINDA RODANDO no engine — o
        // ExitCode que o Docker serializa durante Running é 0 (falso sucesso
        // para `php artisan migrate`); contrato = exit desconhecido (-1).
        assert!(
            c.ends_with("exit=-1"),
            "truncado em execução: exit=-1: {c:?}"
        );

        // container que emite linhas contínuas: logs históricos e follow SSE
        use bollard::models::ContainerCreateBody;
        use bollard::query_parameters::CreateContainerOptionsBuilder;
        use std::collections::HashMap;
        let config = ContainerCreateBody {
            image: Some("alpine:3.20".to_owned()),
            cmd: Some(vec![
                "sh".to_owned(),
                "-c".to_owned(),
                "while true; do echo tick; sleep 1; done".to_owned(),
            ]),
            labels: Some(HashMap::from([
                (
                    "com.docker.compose.project".to_owned(),
                    "flux-ciclofollow-dev".to_owned(),
                ),
                ("com.docker.compose.service".to_owned(), "app".to_owned()),
            ])),
            ..Default::default()
        };
        let opts = CreateContainerOptionsBuilder::default()
            .name("flux-ciclofollow-dev-app-1")
            .build();
        let idf = engine
            .create_container(Some(opts), config)
            .await
            .expect("criar follow")
            .id;
        engine
            .start_container(&idf, None)
            .await
            .expect("startar follow");
        tokio::time::sleep(Duration::from_millis(2500)).await;

        let (s, c) = chama(
            &app,
            "GET",
            "/projects/ciclofollow/logs?env=dev&tail=5&follow=false",
            None,
        )
        .await;
        assert_eq!(s, 200, "{c}");
        assert!(c.contains("tick"), "logs históricos (tail): {c:?}");

        // follow=true → SSE: content-type certo, frames `data: tick\n\n`,
        // stream vivo; cliente corta (drop do body) sem erro nem travar.
        use tower::ServiceExt;
        let svc = router_teste(app.clone());
        let req = hyper::Request::builder()
            .method("GET")
            .uri("/projects/ciclofollow/logs?env=dev&follow=true&tail=2")
            .body(Body::empty())
            .unwrap();
        let res = svc.oneshot(req).await.unwrap();
        assert_eq!(res.status().as_u16(), 200);
        assert_eq!(
            res.headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("text/event-stream"),
            "follow devolve SSE"
        );
        let mut body = res.into_body();
        let mut vistos = String::new();
        let prazo = tokio::time::Instant::now() + Duration::from_secs(30);
        while vistos.matches("data: tick").count() < 3 {
            let frame = tokio::time::timeout_at(prazo, body.frame())
                .await
                .expect("timeout esperando frames SSE")
                .expect("body encerrou antes de 3 ticks");
            match frame {
                Ok(f) => {
                    if let Ok(d) = f.into_data() {
                        vistos.push_str(&String::from_utf8_lossy(&d));
                    }
                }
                Err(e) => panic!("erro no stream SSE: {e}"),
            }
        }
        assert!(vistos.contains("data: tick\n\n"), "formato SSE: {vistos:?}");
        drop(body); // cliente corta — stream termina limpo

        engine
            .remove_container(&id, Some(rm.clone()))
            .await
            .expect("limpar exec");
        engine
            .remove_container(&idf, Some(rm))
            .await
            .expect("limpar follow");
    }
}
