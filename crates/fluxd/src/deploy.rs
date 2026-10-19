//! Rota pública de deploy (10/F4): `POST /api/projects/{name}/deploy` no host
//! do painel (`flux.bitello.xyz`), reconhecida pela decisão (decision.rs) e
//! gated aqui. O workflow do GitHub (docs/templates/deploy-workflow.yml) manda
//! `{ref, actor, run}` com Bearer e espera 202 (aceito) — o deploy é
//! ASSÍNCRONO: o request nunca espera build.
//!
//! Ordem de fechamento (cada etapa ANTES da seguinte):
//!
//! 1. **Método** — `!= POST` → 404 JSON (a decisão não vê método; o GET não
//!    gasta token do limiter nem chega perto do auth).
//! 2. **Rate limit PRÓPRIO, por projeto, ANTES do auth** — bucket dedicado
//!    (`AppState::deploy_limiter`, 10/min com burst 10): o 11º request em
//!    rajada leva `429` + `Retry-After`. A borda global R30 segue intocada;
//!    este bucket é SÓ da rota de deploy, isolado por projeto (abuso num
//!    projeto não seca o dos outros).
//! 3. **Projeto registrado?** — fantasma → 404.
//! 4. **Auth Bearer POR PROJETO, fail-closed** — compara sha256(token) com
//!    `state.deploy_token_hash(name)` (`deploy_tokens` V2, 10). Leitura do
//!    estado falhou → 503 (erro de banco nunca vira porta aberta). Sem
//!    token configurado → 503 com o comando que resolve. Token confere com
//!    OUTRO projeto → 403 com causa (distingue "token de quem?" sem abrir a
//!    rota). Divergente de todos → 401 + evento warn (padrão do gate 09).
//! 5. **Corpo** — `{ref, actor?, run?}` (o `ref` é o GITHUB_SHA completo,
//!    40 hex); qualquer desvio → 422.
//! 6. **Concorrência** — um deploy por projeto por vez (guard em memória);
//!    segundo request → 409.
//! 7. **202 + pipeline em background** com rastro OBRIGATÓRIO: cada passo
//!    grava em `deploy_log` (tabela da V1) e na trilha de `events` — nada
//!    silencioso, falha do pipeline é evento `error`, nunca fire-and-forget.
//!
//! RECORTE DECLARADO (cenário B do plano 10/F4): o pipeline de fundo faz
//! `git fetch` + guard de descendência (`git merge-base --is-ancestor` — SHA
//! órfão, não-empurrado, é recusado) e prepara o release-dir IMUTÁVEL
//! `/srv/flux/releases/{name}/{sha}` via `git worktree`. O build da imagem
//! `{name}-prod:{sha}` e a SUBIDA do container NÃO acontecem neste corte:
//! a subida de prod coerente é a blue-green da 10 (subir a cor INATIVA com
//! override de prod — o override de prod, com restart de verdade, é F10, e
//! o check síncrono de 409 por mutação de migration exige fetch no request).
//! Subir container fora desse contrato quebraria o `trocar_cor` (que espera
//! `flux-{name}-prod-{cor}`) e herdaria `restart: "no"` de dev. A intenção
//! fica GRAVADA (deploy_log + evento explicando o recorte); o gate do
//! workflow (202) permanece verdadeiro para "deploy aceito".

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, PoisonError};

use axum::Json;
use axum::extract::Request;
use axum::http::{Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use crate::AppState;
use crate::api::{bearer_token, hash_confere, sha256_hex};
use crate::events;

/// Rate do bucket PRÓPRIO da rota (10/F4): 10 deploys/min por projeto.
pub const DEPLOY_RATE_PER_MIN: u32 = 10;
/// Burst do bucket da rota: rajada de 10 passa; o 11º request em rajada
/// leva `429` + `Retry-After` (gate do workflow no Actions vira erro visível).
pub const DEPLOY_BURST: u32 = 10;

/// Raiz dos release-dirs imutáveis na VM (10): `{RELEASES_ROOT}/{name}/{sha}`.
pub const RELEASES_ROOT_PADRAO: &str = "/srv/flux/releases";

/// Teto do corpo JSON da rota (`ref`/`actor`/`run` são centenas de bytes —
/// 64 KiB é folga enorme e trava abuse pós-auth).
const MAX_BODY_BYTES: usize = 65_536;

/// Teto de cada comando git do pipeline de fundo. `git fetch origin` fala com
/// a rede — sem timeout, git pendurado segura o guard EM_VOO indefinidamente
/// e o projeto fica em 409 até o git retornar ou o fluxd reiniciar. 120 s
/// cobre fetch de repo normal; estouro vira `Err` e o chamador
/// (`preparar_release`) grava deploy_log + evento de erro — nada silencioso.
const GIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// Corpo do POST do workflow (`docs/templates/deploy-workflow.yml`):
/// `ref` é o SHA completo do commit; `actor`/`run` são contexto opcional do
/// GitHub para a trilha de deploy.
#[derive(Debug, Deserialize)]
struct DeployBody {
    r#ref: String,
    actor: Option<String>,
    run: Option<String>,
}

/// Um deploy por projeto por vez — guard em memória: morre com o processo,
/// nunca trava o projeto eternamente (sem estado em disco para sujar).
static EM_VOO: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

fn em_voo() -> &'static Mutex<HashSet<String>> {
    EM_VOO.get_or_init(|| Mutex::new(HashSet::new()))
}

/// `true` = este projeto era livre e agora está em deploy.
fn in_flight_claim(name: &str) -> bool {
    let mut set = em_voo().lock().unwrap_or_else(PoisonError::into_inner);
    set.insert(name.to_owned())
}

fn in_flight_release(name: &str) {
    em_voo()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .remove(name);
}

/// Linha na trilha de deploy (tabela `deploy_log`, schema V1). Falha de
/// gravação NÃO é engolida: erro no log do fluxd — a trilha é o rastro.
fn deploy_log(st: &crate::state::State, project: &str, kind: &str, detail_json: &str, ok: bool) {
    let r = st.with(|c| {
        c.execute(
            "INSERT INTO deploy_log(project, env, kind, detail_json, ok) VALUES (?1, 'prod', ?2, ?3, ?4)",
            rusqlite::params![project, kind, detail_json, ok],
        )
    });
    if let Err(e) = r {
        tracing::error!("falha gravando deploy_log ({project}/{kind}): {e}");
    }
}

pub async fn deploy(app: std::sync::Arc<AppState>, name: String, req: Request) -> Response {
    let (parts, body) = req.into_parts();

    // (1) Método: a decisão não vê verbo — aqui recusa ≠ POST com 404 (JSON:
    // path /api é rota de API, matriz 12 nunca manda HTML pra ela). Nada de
    // bucket nem auth gastos.
    if parts.method != Method::POST {
        return crate::api::erro(StatusCode::NOT_FOUND, "rota de deploy responde apenas POST");
    }

    // (2) Rate limit PRÓPRIO da rota, ANTES do auth (contrato 10/F4): abuso
    // anônimo não chega no gate de token e morre com 429 + Retry-After.
    if let Err(retry_after) = app.deploy_limiter.take_token(&name) {
        tracing::warn!(project = %name, retry_after, "deploy: 429 — bucket do projeto seco");
        return crate::edge::too_many_requests(true, retry_after);
    }

    // (3) Projeto registrado? (nomes de projeto são públicos — viram hosts.)
    // L5.2: mesma fonte de erro/projeto do resto do crate (api.rs) — os
    // 404/500 e as mensagens são contrato de fio, não podem divergir.
    if let Err(r) = crate::api::garante_projeto(&app, &name) {
        return *r;
    }

    // (4) Auth por projeto, fail-closed: o hash conferido é SEMPRE o do
    // projeto do path — token do projeto A nunca abre projeto B.
    let Some(token) = parts
        .headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(bearer_token)
    else {
        events::event(
            &app.state,
            "warn",
            "deploy",
            &format!("deploy de {name} recusado: Authorization Bearer ausente"),
        );
        return crate::api::erro(
            StatusCode::UNAUTHORIZED,
            format!("token de deploy ausente — Bearer do `flux project token {name}`"),
        );
    };
    let hash_alvo = match app.state.deploy_token_hash(&name) {
        Ok(h) => h,
        Err(e) => {
            return crate::api::erro(
                StatusCode::SERVICE_UNAVAILABLE,
                format!("estado indisponível — deploy fail-closed: {e}"),
            );
        }
    };
    let Some(hash_alvo) = hash_alvo else {
        return crate::api::erro(
            StatusCode::SERVICE_UNAVAILABLE,
            format!("token de deploy não configurado — rode `flux project token {name}`"),
        );
    };
    if !hash_confere(&sha256_hex(token.as_bytes()), &hash_alvo) {
        // Divergente do projeto do path: 403 quando o token é de OUTRO
        // projeto (causa clara, debug fácil); 401 quando não confere em
        // nenhum. Falha lendo os outros hashes → 503 (fail-closed: erro de
        // banco não decide 401).
        return match token_de_outro_projeto(&app, &name, token) {
            Ok(true) => {
                events::event(
                    &app.state,
                    "warn",
                    "deploy",
                    &format!("deploy de {name} recusado: Bearer é o token de OUTRO projeto (403)"),
                );
                crate::api::erro(
                    StatusCode::FORBIDDEN,
                    format!(
                        "token válido, mas de OUTRO projeto — use o Bearer do `flux project token {name}`"
                    ),
                )
            }
            Ok(false) => {
                events::event(
                    &app.state,
                    "warn",
                    "deploy",
                    &format!(
                        "deploy de {name} recusado: Bearer malformado ou divergente do hash do projeto (401)"
                    ),
                );
                crate::api::erro(
                    StatusCode::UNAUTHORIZED,
                    format!(
                        "token de deploy de {name} recusado — gere outro com `flux project token {name}`"
                    ),
                )
            }
            Err(e) => crate::api::erro(
                StatusCode::SERVICE_UNAVAILABLE,
                format!("estado indisponível — deploy fail-closed: {e}"),
            ),
        };
    }

    // (5) Corpo: JSON estrito do contrato do workflow.
    let bytes = match axum::body::to_bytes(body, MAX_BODY_BYTES).await {
        Ok(b) => b,
        Err(e) => {
            return crate::api::erro(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("corpo ilegível: {e}"),
            );
        }
    };
    let corpo: DeployBody = match serde_json::from_slice(&bytes) {
        Ok(c) => c,
        Err(e) => {
            return crate::api::erro(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("corpo inválido — esperado {{\"ref\", \"actor?\", \"run?\"}}: {e}"),
            );
        }
    };
    let sha = corpo.r#ref.trim().to_ascii_lowercase();
    if sha.len() != 40 || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
        return crate::api::erro(
            StatusCode::UNPROCESSABLE_ENTITY,
            "campo `ref` precisa ser o SHA completo do commit (40 hex — o GITHUB_SHA do workflow)",
        );
    }

    // (6) Um deploy por projeto: 409 honesto enquanto o anterior está em
    // andamento (guard solto no fim do pipeline — sucesso OU falha).
    if !in_flight_claim(&name) {
        return crate::api::erro(
            StatusCode::CONFLICT,
            format!("deploy de {name} já em andamento — acompanhe no painel/`flux status`"),
        );
    }

    // (7) 202 + rastro imediato + pipeline em background (NADA silencioso:
    // cada passo do pipeline grava deploy_log + evento, sucesso ou falha).
    deploy_log(
        &app.state,
        &name,
        "aceito",
        &serde_json::json!({
            "ref": sha,
            "actor": corpo.actor,
            "run": corpo.run,
        })
        .to_string(),
        true,
    );
    events::event(
        &app.state,
        "info",
        "deploy",
        &format!(
            "{name}: deploy público aceito (202) sha={sha}{}",
            corpo
                .actor
                .as_deref()
                .map(|a| format!(" actor={a}"))
                .unwrap_or_default()
        ),
    );

    let app_fundo = std::sync::Arc::clone(&app);
    let nome = name.clone();
    let (actor, run) = (corpo.actor.clone(), corpo.run.clone());
    let sha_fundo = sha.clone();
    tokio::spawn(async move {
        preparar_release(
            &app_fundo,
            &nome,
            &sha_fundo,
            actor.as_deref(),
            run.as_deref(),
        )
        .await;
    });

    (
        StatusCode::ACCEPTED,
        Json(serde_json::json!({
            "status": "aceito",
            "project": name,
            "ref": sha,
        })),
    )
        .into_response()
}

/// Token confere com o hash de algum projeto DIFERENTE do path? `Err` =
/// leitura do estado falhou (o chamador fecha com 503). O(n) consultas
/// SQLite — n de projetos é dezenas, custo trivial por request de deploy.
fn token_de_outro_projeto(
    app: &AppState,
    name: &str,
    token: &str,
) -> Result<bool, crate::state::StateError> {
    let hash_enviado = sha256_hex(token.as_bytes());
    for projeto in app.state.list_projects()? {
        if projeto == name {
            continue;
        }
        if let Some(h) = app.state.deploy_token_hash(&projeto)?
            && hash_confere(&hash_enviado, &h)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Pipeline de fundo do 202 (cenário B — ver doc do módulo): prepara o
/// release-dir imutável com rastro obrigatório e SOLTA o guard de
/// concorrência em qualquer desfecho.
async fn preparar_release(
    app: &std::sync::Arc<AppState>,
    name: &str,
    sha: &str,
    actor: Option<&str>,
    run: Option<&str>,
) {
    let repo = PathBuf::from(crate::paths::PROJECTS_ROOT_PADRAO).join(name);
    let release_root = PathBuf::from(RELEASES_ROOT_PADRAO);
    match preparar_release_em(&repo, &release_root, name, sha).await {
        Ok(dir) => {
            deploy_log(
                &app.state,
                name,
                "release_preparado",
                &serde_json::json!({
                    "ref": sha,
                    "release_dir": dir.display().to_string(),
                    "actor": actor,
                    "run": run,
                    "recorte": "cenário B: build/container não sobem aqui — subida blue-green de prod é F9/F10",
                })
                .to_string(),
                true,
            );
            events::event(
                &app.state,
                "info",
                "deploy",
                &format!(
                    "{name}: release {sha} preparado em {} — cenário B: build/container ainda \
                     não sobem (subida blue-green de prod é F9/F10)",
                    dir.display()
                ),
            );
        }
        Err(e) => {
            deploy_log(
                &app.state,
                name,
                "falha",
                &serde_json::json!({ "ref": sha, "erro": e }).to_string(),
                false,
            );
            events::event(
                &app.state,
                "error",
                "deploy",
                &format!("{name}: deploy de {sha} falhou: {e}"),
            );
        }
    }
    in_flight_release(name);
}

/// `git fetch` + guard de descendência + release-dir imutável. Função pura de
/// filesystem/testável: os caminhos vêm de fora (na VM: repo sincronizado da
/// 02 e `/srv/flux/releases`; no teste: tempdir com repositórios reais).
pub async fn preparar_release_em(
    repo: &Path,
    release_root: &Path,
    name: &str,
    sha: &str,
) -> Result<PathBuf, String> {
    if !repo.is_dir() {
        return Err(format!(
            "repo sincronizado {} não existe na VM — o sync (mutagen) roda antes do deploy (02)",
            repo.display()
        ));
    }
    // A verdade de origin, não a do working tree sincronizado.
    git(repo, &["fetch", "origin"]).await?;
    // Guard de descendência: o SHA tem que já estar no histórico que origin
    // mandou (FETCH_HEAD) — commit órfão (não empurrado) é recusado: a VM
    // não deploya o que o remote não conhece.
    let comando = tokio::process::Command::new("git")
        .args(["merge-base", "--is-ancestor", sha, "FETCH_HEAD"])
        .current_dir(repo)
        .kill_on_drop(true)
        .output();
    let saida = match tokio::time::timeout(GIT_TIMEOUT, comando).await {
        Ok(r) => r.map_err(|e| format!("git merge-base: {e}"))?,
        Err(_) => {
            return Err(format!(
                "git merge-base --is-ancestor {sha} FETCH_HEAD excedeu {} s \
                 (timeout — git morto, pipeline abortado)",
                GIT_TIMEOUT.as_secs()
            ));
        }
    };
    match saida.status.code() {
        Some(0) => {}
        Some(1) => {
            return Err(format!(
                "SHA {sha} não é alcançável do histórico de origin (commit órfão — \
                 empurre o commit antes de deployar)"
            ));
        }
        other => {
            return Err(format!(
                "git merge-base --is-ancestor {sha} FETCH_HEAD falhou (exit {other:?}): {}",
                cauda_stderr(&saida.stderr)
            ));
        }
    }
    let dir = release_root.join(name).join(sha);
    if dir.exists() {
        return Err(format!(
            "release {} já existe — release-dir é IMUTÁVEL (redeploy do mesmo SHA não \
             reescreve; novo deploy exige SHA novo)",
            dir.display()
        ));
    }
    std::fs::create_dir_all(release_root.join(name))
        .map_err(|e| format!("criando {}: {e}", release_root.join(name).display()))?;
    git(
        repo,
        &["worktree", "add", "--detach", &dir.to_string_lossy(), sha],
    )
    .await?;
    Ok(dir)
}

async fn git(dir: &Path, args: &[&str]) -> Result<(), String> {
    // `kill_on_drop(true)`: se o timeout vence, o future `output()` é dropado
    // e o git filho morre com ele — não fica processo pendurado na rede.
    let comando = tokio::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .kill_on_drop(true)
        .output();
    let saida = match tokio::time::timeout(GIT_TIMEOUT, comando).await {
        Ok(r) => r.map_err(|e| format!("git {}: {e}", args.first().unwrap_or(&"?")))?,
        Err(_) => {
            return Err(format!(
                "git {} excedeu {} s (timeout — git morto, pipeline abortado)",
                args.join(" "),
                GIT_TIMEOUT.as_secs()
            ));
        }
    };
    if saida.status.success() {
        return Ok(());
    }
    Err(format!(
        "git {} falhou: {}",
        args.join(" "),
        cauda_stderr(&saida.stderr)
    ))
}

fn cauda_stderr(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .lines()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join(" | ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use flux_common::config::GlobalConfig;
    use http_body_util::BodyExt;

    fn app_publica() -> std::sync::Arc<AppState> {
        let cfg = GlobalConfig::parse_str("[deploy]\npublic_route = true\n").unwrap();
        std::sync::Arc::new(AppState::new(
            crate::state::State::in_memory().unwrap(),
            &cfg,
            None,
        ))
    }

    async fn pedir(
        app: &std::sync::Arc<AppState>,
        method: &str,
        name: &str,
        token: Option<&str>,
        corpo: Option<String>,
    ) -> Response {
        let mut b = Request::builder()
            .method(method)
            .uri(format!(
                "http://flux.bitello.xyz/api/projects/{name}/deploy"
            ))
            .header(header::HOST, "flux.bitello.xyz");
        if let Some(t) = token {
            b = b.header(header::AUTHORIZATION, format!("Bearer {t}"));
        }
        if corpo.is_some() {
            b = b.header(header::CONTENT_TYPE, "application/json");
        }
        let req = b.body(Body::from(corpo.unwrap_or_default())).unwrap();
        deploy(std::sync::Arc::clone(app), name.to_owned(), req).await
    }

    async fn corpo_str(r: Response) -> String {
        String::from_utf8(
            r.into_body()
                .collect()
                .await
                .expect("body coletável")
                .to_bytes()
                .to_vec(),
        )
        .unwrap()
    }

    fn registra(app: &std::sync::Arc<AppState>, name: &str) {
        app.state
            .register_project(name, &format!("name = \"{name}\""), "hash-teste")
            .unwrap();
    }

    fn configura_token(app: &std::sync::Arc<AppState>, name: &str, token: &str) {
        app.state
            .set_deploy_token_hash(name, &sha256_hex(token.as_bytes()))
            .unwrap();
    }

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    /// Método ≠ POST → 404 ANTES de gastar bucket/auth (a decisão não vê
    /// método; o handler fecha).
    #[tokio::test]
    async fn metodo_diferente_de_post_e_404() {
        let app = app_publica();
        registra(&app, "vipers");
        let r = pedir(&app, "GET", "vipers", None, None).await;
        assert_eq!(r.status(), StatusCode::NOT_FOUND);
        assert!(corpo_str(r).await.contains("apenas POST"), "404 com causa");
    }

    /// Projeto fantasma → 404 (rota existe, projeto não).
    #[tokio::test]
    async fn projeto_nao_registrado_e_404() {
        let app = app_publica();
        let r = pedir(&app, "POST", "fantasma", Some("t"), Some(corpo_sha())).await;
        assert_eq!(r.status(), StatusCode::NOT_FOUND);
        assert!(
            corpo_str(r).await.contains("fantasma"),
            "404 nomeia o projeto"
        );
    }

    /// Sem Bearer → 401 + evento warn (padrão do gate 09: recusa é visível).
    #[tokio::test]
    async fn sem_token_e_401_com_evento() {
        let app = app_publica();
        registra(&app, "vipers");
        configura_token(&app, "vipers", "segredo");
        let r = pedir(&app, "POST", "vipers", None, Some(corpo_sha())).await;
        assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
        let n: i64 = app
            .state
            .with(|c| {
                c.query_row(
                    "SELECT COUNT(*) FROM events WHERE scope = 'deploy'",
                    [],
                    |r| r.get(0),
                )
            })
            .unwrap();
        assert_eq!(n, 1, "recusa vira evento (nada silencioso)");
    }

    /// Projeto sem token configurado → 503 com o comando que resolve
    /// (fail-closed: rota nunca abre por falta de cadastro).
    #[tokio::test]
    async fn token_nao_configurado_e_503_com_comando() {
        let app = app_publica();
        registra(&app, "vipers");
        let r = pedir(&app, "POST", "vipers", Some("qualquer"), Some(corpo_sha())).await;
        assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE);
        let corpo = corpo_str(r).await;
        assert!(
            corpo.contains("flux project token vipers"),
            "503 ensina o caminho: {corpo}"
        );
    }

    /// Token do projeto B NÃO abre o projeto A: 403 com causa — e o token de
    /// desconhecido segue 401.
    #[tokio::test]
    async fn token_de_outro_projeto_e_403_e_desconhecido_e_401() {
        let app = app_publica();
        registra(&app, "alpha");
        registra(&app, "beta");
        configura_token(&app, "alpha", "token-do-alpha");
        configura_token(&app, "beta", "token-do-beta");

        let r = pedir(
            &app,
            "POST",
            "alpha",
            Some("token-do-beta"),
            Some(corpo_sha()),
        )
        .await;
        assert_eq!(r.status(), StatusCode::FORBIDDEN, "token do beta em alpha");
        assert!(
            corpo_str(r).await.contains("OUTRO projeto"),
            "403 distingue a causa"
        );

        let r = pedir(
            &app,
            "POST",
            "alpha",
            Some("token-aleatorio"),
            Some(corpo_sha()),
        )
        .await;
        assert_eq!(r.status(), StatusCode::UNAUTHORIZED, "sem dono: 401");
    }

    /// Corpo fora do contrato → 422 com causa (JSON quebrado; `ref` curto).
    #[tokio::test]
    async fn corpo_invalido_e_422() {
        let app = app_publica();
        registra(&app, "vipers");
        configura_token(&app, "vipers", "segredo");

        let r = pedir(
            &app,
            "POST",
            "vipers",
            Some("segredo"),
            Some("nao-e-json".into()),
        )
        .await;
        assert_eq!(r.status(), StatusCode::UNPROCESSABLE_ENTITY);

        let r = pedir(
            &app,
            "POST",
            "vipers",
            Some("segredo"),
            Some(r#"{"ref": "abc123"}"#.into()),
        )
        .await;
        assert_eq!(r.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            corpo_str(r).await.contains("40 hex"),
            "422 exige SHA completo"
        );
    }

    fn corpo_sha() -> String {
        format!(r#"{{"ref": "{SHA}", "actor": "gabriel", "run": "https://runs/1"}}"#)
    }

    /// Deploy concorrente do MESMO projeto → 409; outro projeto segue livre.
    /// (Nomes ÚNICOS deste teste: `EM_VOO` é estático do processo e os testes
    /// rodam em paralelo.)
    #[tokio::test]
    async fn deploy_concorrente_e_409() {
        let app = app_publica();
        registra(&app, "concorrente-a");
        registra(&app, "concorrente-b");
        configura_token(&app, "concorrente-a", "segredo");
        configura_token(&app, "concorrente-b", "segredo");

        assert!(in_flight_claim("concorrente-a"), "teste segura o guard");
        let r = pedir(
            &app,
            "POST",
            "concorrente-a",
            Some("segredo"),
            Some(corpo_sha()),
        )
        .await;
        assert_eq!(r.status(), StatusCode::CONFLICT);
        assert!(
            corpo_str(r).await.contains("já em andamento"),
            "409 com causa"
        );
        // Projeto diferente não é afetado pelo guard do outro.
        let r = pedir(
            &app,
            "POST",
            "concorrente-b",
            Some("segredo"),
            Some(corpo_sha()),
        )
        .await;
        assert_eq!(r.status(), StatusCode::ACCEPTED, "isolado por projeto");
        in_flight_release("concorrente-a");
    }

    /// Caminho feliz: 202 + linha `aceito` no deploy_log; o guard é solto
    /// pelo pipeline (que falha nos caminhos da VM fora dela — e GRAVA a
    /// falha), então um segundo deploy volta a ser aceito. (Nome ÚNICO: o
    /// `EM_VOO` é estático do processo e os testes rodam em paralelo.)
    #[tokio::test]
    async fn aceito_202_grava_deploy_log_e_solta_guard() {
        let app = app_publica();
        registra(&app, "solta-guard");
        configura_token(&app, "solta-guard", "segredo");

        let r = pedir(
            &app,
            "POST",
            "solta-guard",
            Some("segredo"),
            Some(corpo_sha()),
        )
        .await;
        assert_eq!(r.status(), StatusCode::ACCEPTED);
        let (kind, ok): (String, i64) = app
            .state
            .with(|c| {
                c.query_row(
                    "SELECT kind, ok FROM deploy_log WHERE project = 'solta-guard' AND kind = 'aceito'",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
            })
            .unwrap();
        assert_eq!(kind, "aceito");
        assert_eq!(ok, 1, "aceito é rastro positivo");

        // Pipeline de fundo precisa terminar (falhar nos paths da VM) e soltar
        // o guard — espera curta, depois o MESMO SHA é aceito de novo.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let r = pedir(
            &app,
            "POST",
            "solta-guard",
            Some("segredo"),
            Some(corpo_sha()),
        )
        .await;
        assert_eq!(
            r.status(),
            StatusCode::ACCEPTED,
            "guard solto: sem 409 eterno"
        );
    }

    /// Bucket PRÓPRIO da rota: 10/min com burst 10 — a 11ª chamada em rajada
    /// seca o bucket e recebe retry (o handler vira 429 + Retry-After).
    #[test]
    fn bucket_da_rota_seca_no_11o_request() {
        let limiter = crate::edge::EdgeState::new(crate::edge::EdgeLimits {
            rate_per_min: DEPLOY_RATE_PER_MIN,
            burst: DEPLOY_BURST,
            ..crate::edge::EdgeLimits::default()
        });
        for i in 0..DEPLOY_BURST {
            assert!(
                limiter.take_token("vipers").is_ok(),
                "request {i} dentro do burst passa"
            );
        }
        let Err(retry) = limiter.take_token("vipers") else {
            panic!("11º request em rajada tem que levar 429");
        };
        assert!(
            retry >= 1,
            "Retry-After do segundo em que recarrega: {retry}"
        );
        // Isolamento por projeto: o bucket é por nome, não global.
        assert!(
            limiter.take_token("outro").is_ok(),
            "outro projeto tem bucket próprio"
        );
    }

    // --------------------------------------------- pipeline (git real, tempdir)

    fn git_cmd(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .expect("git disponível no ambiente de teste");
        assert!(
            out.status.success(),
            "git {args:?} falhou: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    /// Repo "origin" (bare) + clone local com um commit EMPURRADO e um
    /// ÓRFÃO (só local). Guard de descendência: órfão recusado, empurrado
    /// prepara o release-dir; release-dir existente é imutável.
    #[tokio::test]
    async fn sha_orfao_e_recusado_e_sha_empurrado_prepara_release_imutavel() {
        let tmp = tempfile::tempdir().unwrap();
        let origin = tmp.path().join("origin.git");
        let clone = tmp.path().join("clone");

        git_cmd(tmp.path(), &["init", "--bare", &origin.to_string_lossy()]);
        git_cmd(tmp.path(), &["init", &clone.to_string_lossy()]);
        std::fs::write(clone.join("f.txt"), "conteudo").unwrap();
        git_cmd(&clone, &["add", "f.txt"]);
        git_cmd(&clone, &["commit", "-m", "empurrado"]);
        git_cmd(
            &clone,
            &["remote", "add", "origin", &origin.to_string_lossy()],
        );
        git_cmd(&clone, &["push", "origin", "HEAD"]);
        let sha_bom = git_cmd(&clone, &["rev-parse", "HEAD"]);

        // Commit órfão: existe só no clone, origin não conhece.
        git_cmd(&clone, &["commit", "--allow-empty", "-m", "orfao"]);
        let sha_orfao = git_cmd(&clone, &["rev-parse", "HEAD"]);
        assert_ne!(sha_bom, sha_orfao);

        let releases = tmp.path().join("releases");
        let err = preparar_release_em(&clone, &releases, "vipers", &sha_orfao)
            .await
            .expect_err("órfão recusado");
        assert!(err.contains("órfão"), "erro nomeia a causa: {err}");

        let dir = preparar_release_em(&clone, &releases, "vipers", &sha_bom)
            .await
            .expect("empurrado passa");
        assert!(
            dir.is_dir(),
            "release-dir {} criado via worktree",
            dir.display()
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("f.txt")).unwrap(),
            "conteudo",
            "checkout do SHA certo no release-dir"
        );

        // Imutável: mesmo SHA de novo NUNCA reescreve.
        let err2 = preparar_release_em(&clone, &releases, "vipers", &sha_bom)
            .await
            .expect_err("release-dir existente é imutável");
        assert!(err2.contains("IMUTÁVEL"), "erro: {err2}");
    }

    /// Sem repo sincronizado na VM, o pipeline NÃO inventa sucesso: erro com
    /// o caminho na causa (e o chamador grava deploy_log + evento).
    #[tokio::test]
    async fn sem_repo_sincronizado_o_pipeline_falha_com_causa() {
        let tmp = tempfile::tempdir().unwrap();
        let err = preparar_release_em(
            &tmp.path().join("nao-existe"),
            &tmp.path().join("releases"),
            "vipers",
            SHA,
        )
        .await
        .expect_err("sem repo não há release");
        assert!(err.contains("nao-existe"), "erro cita o caminho: {err}");
    }
}
