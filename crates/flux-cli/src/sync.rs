//! Orquestração da sync contínua Windows ↔ VM com o mutagen (spec 02).
//!
//! Mecanismo: o Windows é o **alpha** (fonte da verdade — onde o Gabriel edita)
//! e a VM é o **beta** (réplica exata), em modo `one-way-replica` — igual ao
//! modelo `rsync --delete` do devfactory, mas com watcher nativo dos dois lados
//! (latência típica < 1 s em LAN, pior caso < 5 s). Uma sessão mutagen por
//! projeto (`flux-{name}`, label `flux` para seleção); para worktrees, uma
//! sessão própria por branch com slug sanitizado (`flux-{name}-{slug}`),
//! espelhando `/srv/flux/projects/{name}@{slug}` na VM. O agente mutagen do
//! lado VM é copiado e iniciado pelo próprio mutagen via SSH — nada a instalar.
//!
//! Este módulo não fala com rede nem com Docker: monta argumentos, executa o
//! binário `mutagen` (PATH ou env `MUTAGEN_BIN` — o daemon do mutagen sobe
//! sozinho no primeiro comando) e interpreta a saída. Tudo que é puro é
//! testável sem binário; os testes gated com `MUTAGEN_BIN` provam a sync real
//! local<->local em tempdir, inclusive as exclusões obrigatórias (critérios de
//! aceite 2, 3 e 5 da spec 02).
//!
//! Detalhes do mutagen REAL (verificados no fonte oficial mutagen-io/mutagen —
//! o texto da spec assume um `-f json` que não existe):
//!
//! - `sync list` não tem formato JSON nativo. A saída estruturada sai por
//!   `--template` com o builtin `json` (`cmd/mutagen/common/templating`):
//!   uma linha JSON por sessão, no formato do modelo público
//!   `pkg/api/models/synchronization/session.go`.
//! - O campo `status` é enum protobuf e o `encoding/json` o serializa como
//!   NÚMERO; a tabela dos valores está em `pkg/synchronization/state.proto`
//!   (ex.: Watching=6, halted=1/2/3). Número desconhecido vira
//!   [`StatusState::Desconhecido`] em vez de quebrar o parse — versões novas
//!   do mutagen podem adicionar valores.
//! - `--label=flux` grava a chave com valor vazio, que é válido (a validação
//!   de label é a do Kubernetes, `pkg/selection/labels.go`, e valor vazio
//!   passa) e é selecionável por `--label-selector flux`.
//! - `mutagen sync reset` só limpa HISTÓRICO de sincronização
//!   (`cmd/mutagen/sync/reset.go`); o `flux sync reset` da spec 02 ("mata
//!   sessão e recria com full rescan") é implementado como terminate + create,
//!   que força rescan completo nos dois lados.
//! - O mutagen respeita `.gitignore` por padrão (`respectGitignore = true` da
//!   spec 02) e ignora diretórios VCS (`.git`) por padrão — nenhum flag extra
//!   é preciso para isso.

// Módulo de biblioteca do CLI: a superfície pública (create/pause/resume/
// terminate/reset/status) é consumida pelo main.rs quando o integrador ligar
// o `mod sync`. Até lá, o lint de código morto dispara no bin — allow no
// módulo INTEIRO com esta justificativa, não silenciamento de erro real.
#![allow(dead_code)]

use std::path::Path;
use std::process::Command;

use flux_common::config::ProjectToml;
use serde_json::Value;

/// Env que aponta o binário do mutagen (convenção de testes gated do projeto:
/// sem a env definida, os testes de sync real PASSAM vazios).
pub const ENV_BIN: &str = "MUTAGEN_BIN";

/// Label que marca sessões gerenciadas pelo Flux (spec 02: `--label=flux`) —
/// valor vazio é válido e selecionável por `--label-selector flux`.
pub const LABEL: &str = "flux";

/// Endpoint da VM (spec 01/02): usuário, IP fixo e raiz dos projetos.
pub const VM_USER: &str = "flux";
pub const VM_HOST: &str = "192.168.140.2";
pub const VM_PROJECTS_ROOT: &str = "/srv/flux/projects";

/// Excludes GLOBAIS, verbatim da spec 02 ("Padrão global, não configurável por
/// segurança/estabilidade"). O projeto SÓ ADICIONA ignores via `flux.toml`
/// (`[sync].extra_ignore`); remover daqui é impossível pela API do módulo —
/// é o que protege o container de perder estado a cada sync (spec 02, "o que
/// NUNCA fazer": nunca sincronizar storage/, vendor/, node_modules/, artefatos
/// de build).
pub const GLOBAL_IGNORES: &[&str] = &[
    ".env",
    ".env.*",
    "public/hot",
    "vendor/",
    "node_modules/",
    "public/build/",
    "public/fonts/",
    "public/css/",
    "public/js/",
    "storage/",
];

/// Template que transforma `mutagen sync list` em JSON Lines — um objeto de
/// sessão (modelo público do mutagen) por linha. O `\\n` do fonte é o escape
/// de string do GO TEMPLATE (chega ao mutagen como `{{ "\n" }}` e vira newline
/// na saída), não do Rust.
pub const LIST_TEMPLATE: &str = "{{ range . }}{{ json . }}{{ \"\\n\" }}{{ end }}";

// ----------------------------------------------------------------------- erros

/// Falha da orquestração. Cada variante carrega origem e mensagem pronta para o
/// CLI (nada silencioso — princípio da spec 09).
#[derive(Debug)]
pub enum SyncError {
    /// O binário não pôde ser lançado (mutagen fora do PATH e sem
    /// `MUTAGEN_BIN`, por exemplo). A mensagem orienta a correção.
    Binario { bin: String, source: std::io::Error },
    /// O mutagen terminou com erro — stderr cru vai junto.
    Comando {
        cmd: String,
        code: i32,
        stderr: String,
    },
    /// Saída do mutagen em formato inesperado — bug de integração, nunca
    /// fallback silencioso.
    Saida { cmd: String, motivo: String },
    /// Configuração inválida (flux.toml, branch insalvável, nome vazio).
    Config(String),
}

impl std::fmt::Display for SyncError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Binario { bin, source } => {
                write!(
                    f,
                    "não consegui lançar `{bin}`: {source} — instale o mutagen ou aponte a env {ENV_BIN}"
                )
            }
            Self::Comando { cmd, code, stderr } => {
                write!(f, "`{cmd}` falhou (exit {code}): {}", stderr.trim())
            }
            Self::Saida { cmd, motivo } => {
                write!(f, "saída inesperada de `{cmd}`: {motivo}")
            }
            Self::Config(m) => write!(f, "configuração da sync inválida: {m}"),
        }
    }
}

impl std::error::Error for SyncError {}

// -------------------------------------------------------------------- endpoints

/// Endpoint beta (réplica). Na vida real é a VM via SSH; em dev/testes, um
/// caminho local (local<->local, que é o que os testes gated provam).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Beta {
    /// VM via SSH: o mutagen injeta o agente automaticamente (spec 02).
    Ssh {
        user: String,
        host: String,
        path: String,
    },
    /// Caminho local — dev/testes. Path Windows com drive (`C:\...`) é tratado
    /// pelo parser de URL do próprio mutagen (letra de drive não vira protocolo).
    Local(std::path::PathBuf),
}

impl Beta {
    /// URL no formato que o mutagen consome como argumento posicional.
    pub fn to_url(&self) -> String {
        match self {
            Self::Ssh { user, host, path } => format!("{user}@{host}:{path}"),
            Self::Local(p) => p.display().to_string(),
        }
    }
}

/// Beta padrão da plataforma: `{user}@{host}:{root}/{projeto}` ou, para
/// worktree, `{root}/{projeto}@{slug}` (spec 02, "um diretório na VM por
/// worktree"). O slug é o mesmo da sessão — determinístico por branch.
pub fn vm_beta(project: &str, branch_slug: Option<&str>) -> Beta {
    let nome = match branch_slug {
        Some(slug) => format!("{project}@{slug}"),
        None => project.to_owned(),
    };
    Beta::Ssh {
        user: VM_USER.to_owned(),
        host: VM_HOST.to_owned(),
        path: format!("{VM_PROJECTS_ROOT}/{nome}"),
    }
}

/// Slug determinístico de branch (spec 02): fora de `[a-z0-9-]` vira `-`
/// (inclusive `/`), repetidos colapsam, `-` das pontas saem. Vazio é erro —
/// branch só de caracteres inválidos (ex.: `---`) não tem host derivável.
pub fn branch_slug(branch: &str) -> Result<String, SyncError> {
    let mut slug = String::with_capacity(branch.len());
    let mut ultimo_traco = true; // já "cola" traço inicial
    for c in branch.chars().flat_map(|c| c.to_lowercase()) {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            slug.push(c);
            ultimo_traco = false;
        } else if !ultimo_traco {
            slug.push('-');
            ultimo_traco = true;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    if slug.is_empty() {
        return Err(SyncError::Config(format!(
            "branch `{branch}` não produz slug [a-z0-9-] — renomeie a branch ou use a pasta principal"
        )));
    }
    Ok(slug)
}

// ------------------------------------------------------------------ SyncSpec

/// Especificação de uma sessão de sync de projeto (spec 02): quem é a sessão,
/// de onde para onde vai, e o que NUNCA cruza.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncSpec {
    /// Nome do projeto (identidade da sessão: `flux-{name}`).
    pub project: String,
    /// Slug da branch de worktree (`None` = pasta principal do projeto).
    pub branch_slug: Option<String>,
    /// Pasta do projeto no Windows (alpha, fonte da verdade).
    pub alpha: std::path::PathBuf,
    /// Réplica (beta): VM por SSH ou caminho local para dev/testes.
    pub beta: Beta,
    /// Ignores ADICIONAIS do `flux.toml` (`[sync].extra_ignore`) — nunca
    /// substituem os globais (spec 02).
    pub extra_ignore: Vec<String>,
}

impl SyncSpec {
    /// Especificação nova. Valida project não vazio e sanitiza a branch uma
    /// única vez — erro cedo, nunca na hora do comando.
    pub fn new(
        project: &str,
        alpha: std::path::PathBuf,
        beta: Beta,
        branch: Option<&str>,
    ) -> Result<Self, SyncError> {
        if project.is_empty() {
            return Err(SyncError::Config(
                "nome do projeto vazio — sessão mutagen precisa de nome".into(),
            ));
        }
        let slug = branch.map(branch_slug).transpose()?;
        Ok(Self {
            project: project.to_owned(),
            branch_slug: slug,
            alpha,
            beta,
            extra_ignore: Vec::new(),
        })
    }

    /// Especificação lida da pasta do projeto: nome efetivo do `flux.toml`
    /// (senão o nome da pasta, spec 06) e `[sync].extra_ignore`. `flux.toml`
    /// ausente é suportado (defaults da plataforma); presente e quebrado é
    /// ERRO — nunca fallback silencioso.
    pub fn for_project(dir: &Path, beta: Beta, branch: Option<&str>) -> Result<Self, SyncError> {
        let pasta = dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let toml_path = dir.join("flux.toml");
        let mut spec = if toml_path.exists() {
            let t = ProjectToml::load(&toml_path).map_err(|e| SyncError::Config(e.to_string()))?;
            let nome = t.effective_name(&pasta).to_owned();
            let mut s = Self::new(&nome, dir.to_path_buf(), beta, branch)?;
            s.extra_ignore = t
                .sync
                .as_ref()
                .and_then(|s| s.extra_ignore.clone())
                .unwrap_or_default();
            s
        } else {
            Self::new(&pasta, dir.to_path_buf(), beta, branch)?
        };
        // Ignora extra vazio/whitespace — typo de toml não vira ignore vazio.
        spec.extra_ignore.retain(|i| !i.trim().is_empty());
        Ok(spec)
    }

    /// Nome da sessão: `flux-{name}` ou `flux-{name}-{slug}` (spec 02 —
    /// identificação da sessão por nome do projeto).
    pub fn session(&self) -> String {
        match &self.branch_slug {
            Some(slug) => format!("flux-{}-{slug}", self.project),
            None => format!("flux-{}", self.project),
        }
    }

    /// Ignores efetivos: globais OBRIGATÓRIOS primeiro (ordem estável),
    /// extras do projeto depois, sem duplicar os globais.
    pub fn ignores(&self) -> Vec<String> {
        let mut todos: Vec<String> = GLOBAL_IGNORES.iter().map(|s| s.to_string()).collect();
        for extra in &self.extra_ignore {
            if !todos.iter().any(|g| g == extra) {
                todos.push(extra.clone());
            }
        }
        todos
    }

    // -------------------------------------------------- montagem de argumentos

    /// Argumentos do `sync create`. Formato do label/nome verbatim da spec 02;
    /// `-i` por par (o mutagen aceita repetição); alpha e beta posicionais no
    /// fim, nessa ordem.
    pub fn create_args(&self) -> Vec<String> {
        let mut args = vec![
            "sync".into(),
            "create".into(),
            format!("--name={}", self.session()),
            format!("--label={LABEL}"),
            "--mode=one-way-replica".into(),
        ];
        for ig in self.ignores() {
            args.push("-i".into());
            args.push(ig);
        }
        args.push(self.alpha.display().to_string());
        args.push(self.beta.to_url());
        args
    }

    /// Argumentos de comando de sessão única por nome (`pause`, `resume`,
    /// `terminate`). Nome completo — nunca prefixo, para não casar sessão de
    /// worktree vizinha.
    pub fn session_args(sub: &str, name: &str) -> Vec<String> {
        vec!["sync".into(), sub.into(), name.into()]
    }

    /// Argumentos do `sync list` estruturado, filtrado por nome quando dado.
    pub fn list_args(name: Option<&str>) -> Vec<String> {
        let mut args = vec![
            "sync".into(),
            "list".into(),
            format!("--template={LIST_TEMPLATE}"),
        ];
        if let Some(n) = name {
            args.push(n.into());
        }
        args
    }
}

// ------------------------------------------------------------------- execução

/// Binário do mutagen: env `MUTAGEN_BIN` vence (testes e instalações fora do
/// PATH); senão a instalação padrão do Flux em `%USERPROFILE%\.flux\bin\`
/// (`install-fluxd.sh` não toca no host — o download é manual/setup); por
/// último o nome no PATH.
pub fn mutagen_bin() -> String {
    if let Ok(b) = std::env::var(ENV_BIN) {
        return b;
    }
    if let Ok(home) = std::env::var("USERPROFILE") {
        let padrao = std::path::PathBuf::from(home)
            .join(".flux")
            .join("bin")
            .join("mutagen.exe");
        if padrao.is_file() {
            return padrao.display().to_string();
        }
    }
    "mutagen".into()
}

/// Roda o mutagen e devolve stdout podado. Exit != 0 carrega o stderr — erro
/// do mutagen nunca some.
fn run(bin: &str, args: &[String]) -> Result<String, SyncError> {
    let cmd_desc = format!("{bin} {}", args.join(" "));
    let saida = Command::new(bin)
        .args(args)
        .output()
        .map_err(|source| SyncError::Binario {
            bin: bin.to_owned(),
            source,
        })?;
    let stderr = String::from_utf8_lossy(&saida.stderr).to_string();
    if !saida.status.success() {
        return Err(SyncError::Comando {
            cmd: cmd_desc,
            code: saida.status.code().unwrap_or(-1),
            stderr,
        });
    }
    Ok(String::from_utf8_lossy(&saida.stdout).trim().to_string())
}

/// Extrai o identifier da resposta do `sync create`
/// ("Created session <identifier>", `cmd/mutagen/sync/create.go`). Público
/// porque o CLI também usa para exibir o id em modo verboso.
pub fn parse_created_id(saida: &str) -> Result<String, SyncError> {
    // A saída do mutagen vem com texto de progresso ANTES da confirmação
    // ("Creating session… Connecting to agent… Created session sync_X") —
    // procura a marca em qualquer posição, não no prefixo.
    let marca = "Created session ";
    saida
        .find(marca)
        .and_then(|pos| {
            saida[pos + marca.len()..]
                .split_whitespace()
                .next()
                .map(ToOwned::to_owned)
        })
        .filter(|id| id.starts_with("sync_") && !id.is_empty())
        .ok_or_else(|| SyncError::Saida {
            cmd: "mutagen sync create".into(),
            motivo: format!("esperava `Created session sync_<id>`, veio `{saida}`"),
        })
}

/// Cria (ou reusa) a sessão da especificação e devolve o identifier do mutagen.
/// Idempotente por nome: sessão já existente NÃO é erro — `flux up` pode rodar
/// sempre; a config vigente é a da sessão original (trocar ignores passa por
/// `reset`, que recria de verdade).
pub fn create(spec: &SyncSpec) -> Result<String, SyncError> {
    if let Some(atual) = status_of(&spec.session())? {
        // Reuso NUNCA silencioso: a config vigente é a da criação ORIGINAL —
        // extra_ignore novo no flux.toml só entra na sessão viva com `reset`
        // (recria de verdade com full rescan, spec 02). Sem este aviso, um
        // ignore novo de segredo simplesmente não aplica e ninguém sabe.
        eprintln!(
            "aviso: sessão `{}` já existe — config vigente é a da criação original; ignores novos só entram com `flux sync reset`",
            spec.session()
        );
        return Ok(atual.identifier);
    }
    let args = spec.create_args();
    let saida = run(&mutagen_bin(), &args)?;
    parse_created_id(&saida)
}

/// Pausa a sync do projeto (ex.: operação destrutiva na VM — spec 02, tabela
/// de controles). Banner de pausada é responsabilidade do painel.
pub fn pause(name: &str) -> Result<(), SyncError> {
    run(&mutagen_bin(), &SyncSpec::session_args("pause", name)).map(|_| ())
}

/// Retoma a sync pausada.
pub fn resume(name: &str) -> Result<(), SyncError> {
    run(&mutagen_bin(), &SyncSpec::session_args("resume", name)).map(|_| ())
}

/// Mata a sessão (sem recriar). O `reset` público é quem orquestra.
pub fn terminate(name: &str) -> Result<(), SyncError> {
    run(&mutagen_bin(), &SyncSpec::session_args("terminate", name)).map(|_| ())
}

/// `flux sync reset` (spec 02: "mata sessão e recria com full rescan; termina
/// sozinho, mostra progresso"). O `reset` NATIVO do mutagen só limpa histórico,
/// então: terminate (se existir) + create — sessão nova força rescan completo
/// dos dois lados e converge sozinha.
pub fn reset(spec: &SyncSpec) -> Result<String, SyncError> {
    let nome = spec.session();
    if status_of(&nome)?.is_some() {
        terminate(&nome)?;
    }
    create(spec)
}

// --------------------------------------------------------------------- status

/// Classe do status da sessão, derivada do enum protobuf `Status` do mutagen
/// (números de `pkg/synchronization/state.proto`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusState {
    /// Watching (6): estável, vigiando o filesystem — alvo do aceite 5 da
    /// spec 02 ("converge para ok · sem pendências").
    Ociosa,
    /// Conectando (4/5), scan (7), reconciliação (9), staging (10/11),
    /// transição (12) e gravação de histórico (13): ciclo em andamento.
    Trabalhando,
    /// Desconectado (0) ou esperando re-scan após erro de scan (8): sync viva
    /// mas com pendência real.
    Atencao,
    /// Halted por segurança (1: raiz esvaziada, 2: raiz apagada, 3: tipo da
    /// raiz mudou) — parada DE PROPÓSITO; sai com `reset` (spec 02).
    Parada,
    /// Valor fora da tabela (mutagen mais novo/velho): segue cru, sem chute.
    Desconhecido(i64),
}

impl StatusState {
    /// Tabela número → classe (fonte: `pkg/synchronization/state.proto`).
    /// `0 | 8` fica em OR mesmo — valores não contíguos do enum.
    pub fn from_mutagen(n: i64) -> Self {
        match n {
            6 => Self::Ociosa,
            4..=5 | 7 | 9..=13 => Self::Trabalhando,
            0 | 8 => Self::Atencao,
            1..=3 => Self::Parada,
            outro => Self::Desconhecido(outro),
        }
    }

    /// Aceita também string (mutagen futuro usando protojson) — os nomes
    /// `Description()` do mutagen, em caixa livre. A ordem importa: "waiting
    /// for RESCAN" contém "scan", então o caso de atenção vem antes do
    /// casamento genérico de trabalho.
    pub fn from_json(v: &Value) -> Self {
        if let Some(n) = v.as_i64() {
            return Self::from_mutagen(n);
        }
        match v.as_str().map(|s| s.to_ascii_lowercase()) {
            Some(s) if s.contains("watching") => Self::Ociosa,
            Some(s) if s.contains("halted") => Self::Parada,
            Some(s) if s.contains("rescan") || s.contains("disconnect") => Self::Atencao,
            Some(s)
                if s.contains("connect")
                    || s.contains("scan")
                    || s.contains("reconcil")
                    || s.contains("staging")
                    || s.contains("transition")
                    || s.contains("saving") =>
            {
                Self::Trabalhando
            }
            _ => Self::Desconhecido(-1),
        }
    }

    /// Rótulo curto para o CLI/painel.
    pub fn rotulo(&self) -> String {
        match self {
            Self::Ociosa => "ociosa".into(),
            Self::Trabalhando => "sincronizando".into(),
            Self::Atencao => "atenção".into(),
            Self::Parada => "parada (halted) — flux sync reset".into(),
            Self::Desconhecido(n) => format!("desconhecido ({n})"),
        }
    }
}

/// Estado de um endpoint (alpha ou beta) extraído do JSON do mutagen.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EndpointStatus {
    /// Raiz sincronizada no endpoint.
    pub path: String,
    /// Endpoint remoto (SSH/Docker) ou local.
    pub remote: bool,
    /// Conexão do controller com o endpoint está de pé.
    pub connected: bool,
    /// Problemas de scan (lista + truncados) — pendências de conteúdo.
    pub scan_problems: u64,
    /// Problemas de transição (lista + truncados) — falhas de escrita.
    pub transition_problems: u64,
    /// Arquivos sincronizáveis no último snapshot (`None` = ainda sem scan).
    pub files: Option<u64>,
}

/// Estado de uma sessão do mutagen, já digerido para o CLI/painel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionStatus {
    /// Nome da sessão (`flux-{name}...`); vazio quando o mutagen não tem nome.
    pub name: String,
    /// Identifier único gerado pelo mutagen.
    pub identifier: String,
    /// Pausada? (`flux sync pause` — banner no painel, spec 02.)
    pub paused: bool,
    /// Classe do status.
    pub status: StatusState,
    pub alpha: EndpointStatus,
    pub beta: EndpointStatus,
    /// Último erro de ciclo (`lastError`), quando presente.
    pub last_error: Option<String>,
    /// Conflitos (não deveriam existir em one-way-replica — spec 02; se
    /// aparecer, é sinal de escrita externa no beta).
    pub conflicts: u64,
}

impl SessionStatus {
    /// "ok · sem pendências" do aceite 5 da spec 02: não pausada, ociosa, sem
    /// problemas em nenhum endpoint, sem conflitos e sem erro de ciclo.
    pub fn saudavel(&self) -> bool {
        !self.paused
            && self.status == StatusState::Ociosa
            && self.alpha.scan_problems == 0
            && self.alpha.transition_problems == 0
            && self.beta.scan_problems == 0
            && self.beta.transition_problems == 0
            && self.conflicts == 0
            && self.last_error.is_none()
    }
}

/// Converte o contador de problemas de um endpoint (lista + excluídos por
/// truncamento — `EndpointState` do mutagen).
fn problemas(endpoint: &Value, lista: &str, excluidos: &str) -> u64 {
    let na_lista = endpoint[lista].as_array().map(|a| a.len()).unwrap_or(0) as u64;
    let truncados = endpoint[excluidos].as_u64().unwrap_or(0);
    na_lista + truncados
}

/// Extrai um endpoint do JSON do modelo público do mutagen
/// (`pkg/api/models/synchronization/endpoint.go`: `path`, `host`, `connected`,
/// `scanProblems`, `transitionProblems`, `files`...).
fn endpoint_de(v: &Value) -> EndpointStatus {
    let host = v["host"].as_str().unwrap_or_default();
    EndpointStatus {
        path: v["path"].as_str().unwrap_or_default().to_owned(),
        remote: !host.is_empty(),
        connected: v["connected"].as_bool().unwrap_or(false),
        scan_problems: problemas(v, "scanProblems", "excludedScanProblems"),
        transition_problems: problemas(v, "transitionProblems", "excludedTransitionProblems"),
        files: v["files"].as_u64(),
    }
}

/// Converte UMA linha do `sync list` (objeto do modelo público) em
/// [`SessionStatus`]. Linha sem `identifier` não é sessão — descartada.
pub fn session_de(v: &Value) -> Option<SessionStatus> {
    let identifier = v.get("identifier")?.as_str()?.to_owned();
    let last_error = v["lastError"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_owned());
    Some(SessionStatus {
        name: v["name"].as_str().unwrap_or_default().to_owned(),
        identifier,
        paused: v["paused"].as_bool().unwrap_or(false),
        status: StatusState::from_json(&v["status"]),
        alpha: endpoint_de(&v["alpha"]),
        beta: endpoint_de(&v["beta"]),
        last_error,
        conflicts: v["conflicts"].as_array().map(|a| a.len()).unwrap_or(0) as u64
            + v["excludedConflicts"].as_u64().unwrap_or(0),
    })
}

/// Parse da saída estruturada do `sync list`. Formato principal: JSON Lines
/// (uma sessão por linha — é o que [`LIST_TEMPLATE`] produz). Uma saída que
/// seja um único array JSON também é aceita (mesmos dados, outro corte —
/// tolerância a quem troque o template). Linha que não é objeto de sessão é
/// descartada: o template é nosso, linha estranha é ruído de transporte, não
/// dado perdido.
pub fn parse_list(saida: &str) -> Vec<SessionStatus> {
    let saida = saida.trim();
    if saida.is_empty() {
        return Vec::new();
    }
    if let Ok(Value::Array(itens)) = serde_json::from_str::<Value>(saida) {
        return itens.iter().filter_map(session_de).collect();
    }
    saida
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter_map(|v| session_de(&v))
        .collect()
}

/// Estado de TODAS as sessões do daemon (todas, não só as do Flux — o label
/// `flux` permite filtrar na camada de apresentação).
pub fn status_all() -> Result<Vec<SessionStatus>, SyncError> {
    let args = SyncSpec::list_args(None);
    let saida = run(&mutagen_bin(), &args)?;
    Ok(parse_list(&saida))
}

/// Estado da sessão do projeto, se existir. Sessão ausente NÃO é erro — é
/// estado legítimo antes do primeiro `flux up` (o CLI apresenta a instrução).
/// Por isso a listagem NÃO usa selector: no mutagen real, `sync list` com
/// selector que não casa NENHUMA sessão sai com ERRO e exit ≠ 0
/// ("specification ... did not match any sessions",
/// `pkg/synchronization/manager.go`) — o PRIMEIRO `create`/`reset` de um
/// projeto quebraria na pergunta "já existe?". Lista TUDO (sem selector, que
/// com zero sessões sai limpo) e filtra pelo nome aqui: ausente vira `None`
/// de verdade.
pub fn status_of(name: &str) -> Result<Option<SessionStatus>, SyncError> {
    Ok(status_all()?.into_iter().find(|s| s.name == name))
}
