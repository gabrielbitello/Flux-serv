//! Módulo Docker (F3): o Docker é a FONTE DE VERDADE do estado dos containers
//! (07 — "estado interno nunca é verdade absoluta"). Esta camada traduz
//! inspect/list do bollard para o vocabulário da decisão e expõe as ações
//! (wake/pause/unpause/stop) que o sleep manager (F4) e o router usam.
//!
//! Convenção de nomes (spec 06, overrides de compose): projeto de compose
//! `flux-{name}-{env}`, service `app` — procurado por LABELS, nunca por nome
//! exato de container.
//!
//! Sem Docker (testes no Windows sem engine): `Docker::connect` falha →
//! router degrada para `SqliteSource` com warn — nunca pânico.

use std::collections::HashMap;

use bollard::Docker;
use bollard::container::LogOutput;
use bollard::exec::CreateExecOptions;
use bollard::models::{
    ContainerCreateBody, ContainerInspectResponse, ContainerStateStatusEnum, ContainerSummary,
    ContainerSummaryStateEnum,
};
use bollard::query_parameters::{CreateContainerOptionsBuilder, ListContainersOptionsBuilder};
use flux_common::protocol::{ContainerState, Env};
use futures_util::StreamExt;

use crate::decision::HostSource;
use crate::state::State;

/// Containers alvo: `{name}-{env}` vira projeto de compose `flux-{name}-{env}`.
pub fn compose_project(name: &str, env: Env) -> String {
    format!("flux-{name}-{env}")
}

/// Rótulo DEV com a regra F9 (06): RUNNING só é "awake" com o `[health]`
/// RESPONDENDO; app no ar sem responder = "waking" (o `flux up` continua
/// esperando em vez de celebrar cedo). Demais estados passam CRU — nunca
/// inventado. Função PURA: os 3 ramos caracterizados sem engine (L5.4).
pub(crate) fn rotulo_dev(estado: ContainerState, saudavel: bool) -> &'static str {
    if estado == ContainerState::Awake {
        if saudavel { "awake" } else { "waking" }
    } else {
        estado.as_str()
    }
}

/// Tudo que o handler de status precisa para o rótulo DEV: estado do engine
/// (verdade) + probe de saúde sobre o alvo do app. `probe` INJETÁVEL — o
/// real é `compose::saude_ok(&http, target, path)`; o path de health vem do
/// TOML gravado (default `/up`). Sem engine → estado cru da fonte SQLite.
pub(crate) async fn estado_dev_rotulado<P, Fut>(
    docker: &Docker,
    st: &crate::state::State,
    name: &str,
    client: crate::proxy::ProxyClient,
    probe: P,
) -> String
where
    P: FnOnce(crate::proxy::ProxyClient, String, String) -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    use crate::decision::HostSource as _;
    let estado = DockerSource::new(docker.clone(), st).env_state(name, Env::Dev);
    if estado != ContainerState::Awake {
        return estado.as_str().to_owned();
    }
    let saudavel = match alvo_app(docker, &compose_project(name, Env::Dev)).await {
        Ok(Some(target)) => {
            let path = st
                .project_toml(name)
                .ok()
                .flatten()
                .and_then(|s| flux_common::config::ProjectToml::parse_str(&s).ok())
                .map(|t| t.health_path().to_owned())
                .unwrap_or_else(|| "/up".into());
            probe(client.clone(), target, path).await
        }
        _ => false,
    };
    rotulo_dev(estado, saudavel).to_owned()
}

/// Filtro de label de compose project — montagem PURA (testável sem engine).
/// `Some(projeto)` = match exato; `None` = TODOS com o label (o chamador
/// filtra prefixo — forma do teardown de worktrees, temporary.rs).
fn filtros_label(projeto: Option<&str>) -> HashMap<String, Vec<String>> {
    let mut filters = HashMap::new();
    let valor = match projeto {
        Some(p) => format!("com.docker.compose.project={p}"),
        None => "com.docker.compose.project".to_owned(),
    };
    filters.insert("label".to_owned(), vec![valor]);
    filters
}

/// FONTE ÚNICA de listagem por label (L3.1 — eram 4 cópias com erros
/// divergentes: `unwrap_or_default` engolia falha de engine e wake/sleep
/// agiam como lista vazia; `.ok()?` não distinguia "sem container" de
/// "engine falhou"). ERRO PROPAGA — mudança DECLARADA no plano: os
/// chamadores de wake/sleep já tratam `Err` com evento/warn. Flux-db e cia
/// NUNCA entram no match exato: não têm o label.
pub(crate) async fn listar_label(
    docker: &Docker,
    projeto: Option<&str>,
) -> Result<Vec<ContainerSummary>, String> {
    let options = ListContainersOptionsBuilder::default()
        .all(true)
        .filters(&filtros_label(projeto))
        .build();
    docker
        .list_containers(Some(options))
        .await
        .map_err(|e| format!("listagem de containers (projeto {projeto:?}): {e}"))
}

/// TODOS os containers do compose project (sono de dev é do PROJETO inteiro:
/// só o app parado deixava worker/reverb rodando à toa — salada que o
/// Gabriel apontou).
async fn containers_do_projeto(
    docker: &Docker,
    project: &str,
) -> Result<Vec<ContainerSummary>, String> {
    listar_label(docker, Some(project)).await
}

pub async fn find_app_container(
    docker: &Docker,
    project: &str,
) -> Option<ContainerInspectResponse> {
    let list = match listar_label(docker, Some(project)).await {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!("find_app_container {project}: {e}");
            return None;
        }
    };
    let id = list
        .iter()
        .find(|c| {
            c.labels
                .as_ref()
                .and_then(|l| l.get("com.docker.compose.service"))
                .map(|s| s == "app")
                .unwrap_or(false)
        })
        .or_else(|| list.first())
        .cloned()?;
    docker.inspect_container(id.id.as_deref()?, None).await.ok()
}

/// Estado do container segundo o Docker (inspect), no vocabulário da decisão.
/// `Exited` vira `Sleeping` na fundação — crash loop/`Failed` é detecção do
/// sleep manager (F4, "≥ 3 restarts em 10 min", 03).
pub fn map_state(c: &ContainerInspectResponse) -> ContainerState {
    let Some(st) = c.state.as_ref() else {
        return ContainerState::Unknown;
    };
    match st.status {
        Some(ContainerStateStatusEnum::RUNNING) if st.paused.unwrap_or(false) => {
            ContainerState::Paused
        }
        Some(ContainerStateStatusEnum::RUNNING) => ContainerState::Awake,
        Some(ContainerStateStatusEnum::RESTARTING) => ContainerState::Starting,
        Some(ContainerStateStatusEnum::CREATED) => ContainerState::Starting,
        Some(ContainerStateStatusEnum::PAUSED) => ContainerState::Paused,
        _ => ContainerState::Sleeping,
    }
}

/// Especificação de um `docker exec` no container app (L3.3) — as diferenças
/// deliberadas dos 2 consumidores são CAMPOS, não cópias:
/// - `wrap_sh`: migrations roda `sh -c "<cmd>"` (string única); ciclo passa
///   argv verbatim.
/// - `workdir`: `Some` = fixo (ciclo passa o workdir do SERVICE derivado do
///   inspect); `None` = não seta (container default — migrations).
/// - `teto_bytes`: teto de saída combinada; estourou com o exec AINDA
///   RODANDO → exit `-1` (desconhecido, convenção do ciclo). `None` = sem
///   teto (evite: saída infinita enche a memória do fluxd).
pub(crate) struct ExecSpec {
    pub cmd: Vec<String>,
    pub wrap_sh: bool,
    pub workdir: Option<String>,
    pub teto_bytes: Option<usize>,
}

/// Saída combinada stdout+stderr do exec.
pub(crate) struct ExecOutput {
    pub saida: String,
    pub truncado: bool,
    pub code: i64,
}

/// `docker exec` no container `id` — esqueleto ÚNICO (L3.3: eram 2 cópias
/// com contratos já divergidos — db sem teto, ciclo com regra -1). create_exec
/// (attach stdout+stderr, SEM TTY) + start coletando a saída combinada +
/// inspect para o exit. Erros sempre com origem (`{projeto}: ...`), nunca
/// silenciosos.
pub(crate) async fn exec_no_app(
    docker: &Docker,
    projeto: &str,
    id: &str,
    spec: &ExecSpec,
) -> Result<ExecOutput, String> {
    let cmd = if spec.wrap_sh {
        vec!["sh".to_owned(), "-c".to_owned(), spec.cmd.join(" ")]
    } else {
        spec.cmd.clone()
    };
    let exec = docker
        .create_exec(
            id,
            CreateExecOptions {
                attach_stdout: Some(true),
                attach_stderr: Some(true),
                tty: Some(false),
                cmd: Some(cmd),
                working_dir: spec.workdir.clone(),
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
                if let Some(teto) = spec.teto_bytes {
                    let livre = teto.saturating_sub(saida.len());
                    if livre == 0 {
                        truncado = true;
                        break;
                    }
                    let fatia = &message[..message.len().min(livre)];
                    saida.push_str(&String::from_utf8_lossy(fatia));
                    if saida.len() >= teto {
                        truncado = true;
                        break;
                    }
                } else {
                    saida.push_str(&String::from_utf8_lossy(&message));
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
    // Contrato: truncado com o exec AINDA RODANDO = exit desconhecido (-1).
    // O ExitCode que o Docker serializa durante Running é 0 — repassar seria
    // falso sucesso (o CLI usa o código como exit próprio do `flux exec`).
    // Exec que TERMINOU com saída acima do teto mantém o exit real.
    let code = if truncado && inspect.running.unwrap_or(false) {
        -1
    } else {
        inspect.exit_code.unwrap_or(-1)
    };
    Ok(ExecOutput {
        saida,
        truncado,
        code,
    })
}

/// IP do container na rede do compose — alvo do proxy (03). Porta: `80` é o
/// default da plataforma (Laravel); sem `80/tcp` exposto, primeira TCP exposta.
pub fn proxy_target_from(c: &ContainerInspectResponse) -> Option<String> {
    let nets = c.network_settings.as_ref()?.networks.as_ref()?;
    let ip = nets
        .values()
        .filter_map(|n| n.ip_address.as_deref())
        .find(|ip| !ip.is_empty())?;
    let port = c
        .network_settings
        .as_ref()
        .and_then(|ns| ns.ports.as_ref())
        .and_then(|ports| {
            ports
                .iter()
                .filter(|(k, _)| k.ends_with("/tcp"))
                .map(|(k, _)| k.trim_end_matches("/tcp").to_owned())
                .min_by_key(|p| p.parse::<u16>().unwrap_or(u16::MAX))
        })
        .unwrap_or_else(|| "80".to_owned());
    Some(format!("{ip}:{port}"))
}

/// Alvo HTTP (IP:porta) do container `app` do projeto compose — para o probe
/// de saúde (`[health] path`, 06). `None` = container ausente ou sem rede;
/// erro = falha de inspect (grita com origem).
pub async fn alvo_app(docker: &Docker, projeto: &str) -> Result<Option<String>, String> {
    let Some(c) = find_app_container(docker, projeto).await else {
        return Ok(None);
    };
    let Some(id) = c.id else {
        return Ok(None);
    };
    let inspected = docker
        .inspect_container(&id, None)
        .await
        .map_err(|e| format!("inspect {id} ({projeto}): {e}"))?;
    Ok(proxy_target_from(&inspected))
}

/// Fonte de verdade: Docker (estado/alvo) + SQLite (projetos/flags de release).
pub struct DockerSource<'a> {
    docker: Docker,
    sqlite: &'a State,
}

impl<'a> DockerSource<'a> {
    pub fn new(docker: Docker, sqlite: &'a State) -> Self {
        Self { docker, sqlite }
    }

    /// Conecta no Docker: `FLUX_DOCKER_URL` (http, para dev/testes fora da VM)
    /// ou o socket default (`/var/run/docker.sock`). Falha → `None` (degrada
    /// para estado via SQLite com warn — a validação real acontece no 1º
    /// request; estado ausente = `Unknown` → tratado como `Starting`).
    pub fn connect() -> Option<Docker> {
        let result = match std::env::var("FLUX_DOCKER_URL") {
            Ok(url) => Docker::connect_with_http(&url, 120, bollard::API_DEFAULT_VERSION)
                .map_err(|e| (url, e)),
            Err(_) => Docker::connect_with_defaults().map_err(|e| ("socket default".to_owned(), e)),
        };
        match result {
            Ok(d) => Some(d),
            Err((origem, e)) => {
                tracing::warn!("Docker indisponível ({origem}: {e}) — estado vem do SQLite");
                None
            }
        }
    }

    /// Compose project efetivo: em Prod com blue-green (10), a cor ativa do
    /// SQLite decide (flux-{name}-prod-{cor}); sem cor, o base. Dev nunca
    /// tem cor.
    fn compose_project_cor(&self, name: &str, env: Env) -> String {
        let base = compose_project(name, env);
        if env == Env::Prod {
            match self.sqlite.active_color(name) {
                Ok(Some(cor)) => return format!("{base}-{cor}"),
                Ok(None) => {}
                // L3.2: falha de leitura NÃO é "sem cor" silencioso — em prod
                // isso decidiria o compose project ERRADO. Warn com a causa.
                Err(e) => {
                    tracing::warn!("active_color de {name} falhou — usando o project base: {e}")
                }
            }
        }
        base
    }

    /// Garante acordado antes do proxy: pausado → `unpause` (< 1 s, invisível,
    /// matriz 03); parado → `start`. Best-effort — erro é logado, o forward
    /// segue e reavalia (upstream falho vira página de estado, nunca 502).
    /// Devolve `true` quando EXECUTOU uma transição para acordar.
    pub async fn ensure_awake(&self, name: &str, env: Env) -> bool {
        let project = self.compose_project_cor(name, env);
        let Some(c) = find_app_container(&self.docker, &project).await else {
            return false;
        };
        let Some(st) = c.state.as_ref() else {
            return false;
        };
        let id = c.id.clone().unwrap_or_default();
        let (acao, r) = match st.status {
            Some(ContainerStateStatusEnum::RUNNING) if st.paused.unwrap_or(false) => {
                (true, self.docker.unpause_container(&id).await)
            }
            Some(ContainerStateStatusEnum::PAUSED) => {
                (true, self.docker.unpause_container(&id).await)
            }
            Some(ContainerStateStatusEnum::EXITED) | Some(ContainerStateStatusEnum::DEAD) => {
                (true, self.docker.start_container(&id, None).await)
            }
            _ => (false, Ok(())),
        };
        if let Err(e) = r {
            tracing::warn!("ensure_awake {project} falhou: {e}");
            return false;
        }
        acao
    }

    /// `docker start` — o wake da página de espera (03) e do sleep manager (F4).
    /// Devolve `true` quando o container estava parado e foi acordado.
    pub async fn wake(&self, name: &str, env: Env) -> Result<bool, String> {
        let project = self.compose_project_cor(name, env);
        let mut acordou_algum = false;
        for c in containers_do_projeto(&self.docker, &project).await? {
            let parado = c
                .state
                .as_ref()
                .is_some_and(|st| !matches!(st, ContainerSummaryStateEnum::RUNNING));
            if !parado {
                continue;
            }
            let id = c.id.clone().unwrap_or_default();
            self.docker
                .start_container(&id, None)
                .await
                .map_err(|e| e.to_string())?;
            acordou_algum = true;
        }
        Ok(acordou_algum)
    }

    /// `docker pause` — hibernação de prod (< 1 s, invisível, 11).
    pub async fn pause(&self, name: &str, env: Env) -> Result<(), bollard::errors::Error> {
        let project = self.compose_project_cor(name, env);
        if let Some(c) = find_app_container(&self.docker, &project).await {
            self.docker
                .pause_container(c.id.as_deref().unwrap_or_default())
                .await?;
        }
        Ok(())
    }

    /// `docker unpause` — prod pausado volta no request original (03, matriz).
    pub async fn unpause(&self, name: &str, env: Env) -> Result<(), bollard::errors::Error> {
        let project = self.compose_project_cor(name, env);
        if let Some(c) = find_app_container(&self.docker, &project).await {
            self.docker
                .unpause_container(c.id.as_deref().unwrap_or_default())
                .await?;
        }
        Ok(())
    }

    /// `docker stop` — sono de dev (04).
    pub async fn sleep(&self, name: &str, env: Env) -> Result<(), String> {
        let project = self.compose_project_cor(name, env);
        for c in containers_do_projeto(&self.docker, &project).await? {
            let rodando = c.state.as_ref().is_some_and(|st| {
                matches!(
                    st,
                    ContainerSummaryStateEnum::RUNNING | ContainerSummaryStateEnum::RESTARTING
                )
            });
            if !rodando {
                continue;
            }
            self.docker
                .stop_container(c.id.as_deref().unwrap_or_default(), None)
                .await
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}

impl HostSource for DockerSource<'_> {
    fn project_exists(&self, name: &str) -> bool {
        self.fonte_sqlite().project_exists(name)
    }

    fn env_state(&self, name: &str, env: Env) -> ContainerState {
        let project = self.compose_project_cor(name, env);
        match tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(find_app_container(&self.docker, &project))
        }) {
            Some(c) => map_state(&c),
            None => ContainerState::Unknown,
        }
    }

    fn branch_exists(&self, _name: &str, _branch: &str) -> bool {
        // Registro de branches chega do `flux watch` (02) na F11.
        true
    }

    /// Hosts custom de `[domains]` (09): consulta SQLite direta (a engine não
    /// participa — host é registro do estado).
    fn host_custom(&self, host: &str) -> Option<(String, bool)> {
        self.fonte_sqlite().host_custom(host)
    }

    fn proxy_target(&self, name: &str, env: Env) -> Option<String> {
        let project = self.compose_project_cor(name, env);
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(find_app_container(&self.docker, &project))
        })
        .as_ref()
        .and_then(proxy_target_from)
    }

    fn release_flags(&self, name: &str) -> (bool, bool) {
        self.fonte_sqlite().release_flags(name)
    }
}

/// L3.2: as consultas de estado de projeto (existência, flags, hosts custom)
/// têm UMA implementação — o `SqliteSource`. O `DockerSource` delega em vez
/// de copiar o SQL (as cópias engoliam erro SEM log: `.is_ok()` e
/// `unwrap_or((false, false))` — numa falha de leitura o router fingia
/// "projeto inexistente"/"sem flags").
impl DockerSource<'_> {
    fn fonte_sqlite(&self) -> crate::sqlite_source::SqliteSource<'_> {
        crate::sqlite_source::SqliteSource::new(self.sqlite)
    }
}

/// Helper de teste/dev: sobe um container dummy com as labels da convenção.
/// Puxa `alpine` (≈ 3 MB) se não existe e dorme — o alvo do proxy é o IP na
/// rede default.
pub async fn create_dummy_container(
    docker: &Docker,
    name: &str,
    env: Env,
) -> Result<String, bollard::errors::Error> {
    let project = compose_project(name, env);
    use bollard::query_parameters::CreateImageOptions;
    use futures_util::StreamExt;
    let mut pull = docker.create_image(
        Some(CreateImageOptions {
            from_image: Some("alpine:3.20".to_owned()),
            ..Default::default()
        }),
        None,
        None,
    );
    while let Some(progress) = pull.next().await {
        progress?;
    }
    let config = ContainerCreateBody {
        image: Some("alpine:3.20".to_owned()),
        cmd: Some(vec!["sleep".to_owned(), "3600".to_owned()]),
        labels: Some(HashMap::from([
            ("com.docker.compose.project".to_owned(), project),
            ("com.docker.compose.service".to_owned(), "app".to_owned()),
        ])),
        ..Default::default()
    };
    let options = CreateContainerOptionsBuilder::default()
        .name(format!("flux-{name}-{env}-app-1").as_str())
        .build();
    docker
        .create_container(Some(options), config)
        .await
        .map(|r| r.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Sem engine de Docker nos testes unitários: exercita só o mapeamento puro.

    /// L3.1: montagem de filtro pura — as duas formas (exato / todos com label).
    #[test]
    fn filtros_label_monta_exato_e_coringa() {
        let exato = filtros_label(Some("flux-pedi-dev"));
        assert_eq!(
            exato.get("label"),
            Some(&vec!["com.docker.compose.project=flux-pedi-dev".to_owned()])
        );
        let todos = filtros_label(None);
        assert_eq!(
            todos.get("label"),
            Some(&vec!["com.docker.compose.project".to_owned()])
        );
    }

    /// L5.4: os 3 ramos do rótulo F9, caracterizados SEM engine.
    #[test]
    fn rotulo_dev_cobre_os_tres_ramos() {
        use flux_common::protocol::ContainerState;
        assert_eq!(
            rotulo_dev(ContainerState::Sleeping, true),
            "sleeping",
            "não-Running passa cru — nunca inventado"
        );
        assert_eq!(
            rotulo_dev(ContainerState::Awake, true),
            "awake",
            "Running + health respondendo = awake"
        );
        assert_eq!(
            rotulo_dev(ContainerState::Awake, false),
            "waking",
            "Running sem health = waking (o up continua esperando)"
        );
    }

    #[test]
    fn compose_project_segue_convencao_da_spec_06() {
        assert_eq!(compose_project("vipers", Env::Dev), "flux-vipers-dev");
        assert_eq!(compose_project("vipers", Env::Prod), "flux-vipers-prod");
    }
}
