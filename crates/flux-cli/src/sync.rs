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

// Módulo de biblioteca do CLI: a superfície pública é consumida pelo main.rs
// (Cmd::Sync, `flux build`, `flux watch`, `flux up`). O allow de dead_code
// sumiu junto com o motivo: tudo aqui tem consumidor real.

use std::path::Path;
use std::process::Command;

use flux_common::config::{ProjectToml, SyncSection};
use serde_json::Value;
use sha2::{Digest, Sha256};

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

/// Excludes GLOBAIS, da spec 02 ("Padrão global, não configurável por
/// segurança/estabilidade"). O projeto SÓ ADICIONA ignores via `flux.toml`
/// (`[sync].extra_ignore` e, opt-in, o `.dockerignore` — `ignores_do_projeto`);
/// remover daqui é impossível pela API do módulo —
/// é o que protege o container de perder estado a cada sync (spec 02, "o que
/// NUNCA fazer": nunca sincronizar storage/, vendor/, node_modules/, artefatos
/// de build).
/// 18/10: `public/{fonts,css,js}` saíram da lista — o vippers provou que esses
/// diretórios também hospedam asset PUBLICADO commitado no repo (60 arquivos
/// de `filament:assets` + estáticos do app): asset trackeado pelo git é fonte
/// da verdade e TEM que sincronizar; artefato de build do vite vive em
/// `public/build/`, que continua ignorado. Ignorado ≠ commitado.
pub const GLOBAL_IGNORES: &[&str] = &[
    ".env",
    ".env.*",
    "public/hot",
    "vendor/",
    "node_modules/",
    "public/build/",
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

/// Ignores de projeto do `[sync]` do `flux.toml`: `extra_ignore` + padrões do
/// `.dockerignore` quando `use_dockerignore = true` (append-only — nunca
/// re-inclui os globais). Comentários `#` e linhas vazias do arquivo caem
/// fora; os padrões passam VERBATIM — a semântica exata do dockerignore
/// (last-match-wins, exceções `!` com precedência própria) NÃO é reproduzida:
/// quem interpreta é o ignore do mutagen (sessão) e o `caminho_ignorado` da
/// plataforma (estabilidade), então padrões exóticos do Docker podem divergir.
/// Toggle ligado e arquivo ausente é ERRO — config declarada nunca vira
/// fallback silencioso.
pub fn ignores_do_projeto(dir: &Path, sync: Option<&SyncSection>) -> Result<Vec<String>, String> {
    let Some(sync) = sync else {
        return Ok(Vec::new());
    };
    let mut todos: Vec<String> = sync
        .extra_ignore
        .iter()
        .flatten()
        .map(|i| i.trim().to_owned())
        .filter(|i| !i.is_empty())
        .collect();
    if sync.use_dockerignore() {
        let caminho = dir.join(sync.dockerignore_path());
        let conteudo = std::fs::read_to_string(&caminho).map_err(|e| {
            format!(
                "[sync] use_dockerignore ligado mas {} não lê: {e}",
                caminho.display()
            )
        })?;
        for linha in conteudo.lines() {
            let linha = linha.trim();
            if linha.is_empty() || linha.starts_with('#') {
                continue;
            }
            if !todos.iter().any(|g| g == linha) {
                todos.push(linha.to_owned());
            }
        }
    }
    Ok(todos)
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
    /// (senão o nome da pasta, spec 06) e ignores do `[sync]` — `extra_ignore`
    /// mais `.dockerignore` quando ligado, via `ignores_do_projeto`.
    /// `flux.toml` ausente é suportado (defaults da plataforma); presente e
    /// quebrado é ERRO — nunca fallback silencioso.
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
            s.extra_ignore = ignores_do_projeto(dir, t.sync.as_ref()).map_err(SyncError::Config)?;
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
    /// Host do endpoint no formato `user@host` (recombinado dos campos
    /// SEPARADOS `user`/`host` do JSON do mutagen; vazio quando local) — é o
    /// que permite reconstruir a `Beta` de uma sessão viva (rescan pedido
    /// pelo painel, 02).
    pub host: String,
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
/// (`pkg/api/models/synchronization/endpoint.go`: `path`, `user`, `host`,
/// `connected`, `scanProblems`, `transitionProblems`, `files`...).
fn endpoint_de(v: &Value) -> EndpointStatus {
    // O mutagen serializa usuário e servidor em campos SEPARADOS (`user` e
    // `host`, ambos omitempty — o `user@host` do URL SCP só existe na
    // ENTRADA, nunca no JSON). Recombina aqui no formato que o consumidor
    // espera: `spec_da_sessao` (CLI) separa de novo em '@' para reconstruir
    // a `Beta` de uma sessão viva — sem isso o rescan do painel quebraria a
    // cada tick na topologia real (beta SSH).
    let usuario = v["user"].as_str().unwrap_or_default().trim();
    let servidor = v["host"].as_str().unwrap_or_default().trim();
    let host = if usuario.is_empty() {
        servidor.to_owned()
    } else {
        format!("{usuario}@{servidor}")
    };
    EndpointStatus {
        path: v["path"].as_str().unwrap_or_default().to_owned(),
        host,
        remote: !servidor.is_empty(),
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

// ------------------------------------------------- estabilidade de árvore (02)

/// Teto de espera da estabilidade, em rodadas da janela (02: "ex.: 3x a
/// janela"). Três hashes consecutivos separados pela janela sem mudança
/// nenhuma — estourou, algo está escrevendo sem parar e o erro é gritado.
pub const TETO_RODADAS_ESTABILIDADE: u32 = 3;

/// Lista os arquivos rastreáveis do projeto: `git ls-files --cached --others
/// --exclude-standard` no diretório (rastreados + não-ignorados, a MESMA
/// visão do `.gitignore` que a sync replica), recortada pelo `subcaminho`
/// quando dado (ex.: `database/migrations`), com os GLOBAL_IGNORES + os
/// extras do projeto filtrados fora da lista. Saída com `-z` (paths crus,
/// sem o escape de aspas do git). Erro com origem — repo ausente NUNCA vira
/// lista vazia silenciosa.
pub fn listar_arquivos_rastreaveis(
    dir: &Path,
    extra_ignore: &[String],
    subcaminho: Option<&Path>,
) -> Result<Vec<String>, String> {
    let mut args: Vec<String> = vec![
        "-C".into(),
        dir.display().to_string(),
        "ls-files".into(),
        "-z".into(),
        "--cached".into(),
        "--others".into(),
        "--exclude-standard".into(),
    ];
    if let Some(sub) = subcaminho {
        args.push("--".into());
        args.push(sub.display().to_string().replace('\\', "/"));
    }
    let saida = Command::new("git")
        .args(&args)
        .output()
        .map_err(|e| format!("lançando git ls-files em {}: {e}", dir.display()))?;
    if !saida.status.success() {
        return Err(format!(
            "git ls-files falhou em {}: {}",
            dir.display(),
            String::from_utf8_lossy(&saida.stderr).trim()
        ));
    }
    let mut ignora: Vec<String> = GLOBAL_IGNORES.iter().map(|s| s.to_string()).collect();
    ignora.extend(
        extra_ignore
            .iter()
            .filter(|i| !i.trim().is_empty())
            .cloned(),
    );
    Ok(String::from_utf8_lossy(&saida.stdout)
        .split('\0')
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .filter(|l| !caminho_ignorado(l, &ignora))
        .map(ToOwned::to_owned)
        .collect())
}

/// Um caminho relativo é ignorado por ALGUM padrão? Semântica gitignore no
/// subconjunto que a plataforma usa (02): padrão com `/` é ancorado na raiz
/// (`public/hot` só casa na raiz), padrão solto casa qualquer segmento
/// (`.env`, `*.log`) e padrão terminado em `/` casa o diretório em qualquer
/// nível (`vendor/`).
pub(crate) fn caminho_ignorado(rel: &str, padroes: &[String]) -> bool {
    let rel = rel.replace('\\', "/");
    padroes.iter().any(|p| padrao_casa(&rel, p))
}

fn padrao_casa(rel: &str, padrao: &str) -> bool {
    if let Some(dir) = padrao.strip_suffix('/') {
        return rel.split('/').any(|seg| seg == dir);
    }
    if padrao.contains('/') {
        return glob_casa(rel, padrao.trim_start_matches('/'));
    }
    rel.split('/').any(|seg| glob_casa(seg, padrao))
}

/// Glob mínimo: só `*` (o suficiente para os ignores da plataforma). Sem `*`
/// é igualdade exata; com, o prefixo e o sufixo ancoram e o meio casa em
/// ordem.
fn glob_casa(texto: &str, padrao: &str) -> bool {
    let partes: Vec<&str> = padrao.split('*').collect();
    if partes.len() == 1 {
        return texto == padrao;
    }
    let mut resto = texto;
    for (i, parte) in partes.iter().enumerate() {
        if i == 0 {
            if !resto.starts_with(parte) {
                return false;
            }
            resto = &resto[parte.len()..];
        } else if i == partes.len() - 1 {
            if !resto.ends_with(parte) {
                return false;
            }
        } else {
            match resto.find(parte) {
                Some(p) => resto = &resto[p + parte.len()..],
                None => return false,
            }
        }
    }
    true
}

/// Hash de estabilidade da árvore: sha256 sobre (caminho relativo + tamanho +
/// mtime) de cada arquivo, ordenado — quietude SEM ler conteúdo (vendor com
/// GB seria desperdício; tamanho+mtime detectam escrita). `None` = nenhum
/// arquivo rastreável (subárvore vazia/ausente é um estado legítimo e estável).
pub fn hash_estabilidade(
    dir: &Path,
    extra_ignore: &[String],
    subcaminho: Option<&Path>,
) -> Result<Option<String>, String> {
    let arquivos = listar_arquivos_rastreaveis(dir, extra_ignore, subcaminho)?;
    if arquivos.is_empty() {
        return Ok(None);
    }
    let mut h = Sha256::new();
    for rel in &arquivos {
        let meta = std::fs::metadata(dir.join(rel))
            .map_err(|e| format!("stat de {rel} (árvore de {}): {e}", dir.display()))?;
        let desde_epoca = meta
            .modified()
            .map_err(|e| format!("mtime de {rel}: {e}"))?
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| format!("mtime de {rel} antes da época Unix: {e}"))?;
        h.update(rel.as_bytes());
        h.update([0]);
        h.update(meta.len().to_le_bytes());
        h.update(desde_epoca.as_secs().to_le_bytes());
        h.update(desde_epoca.subsec_nanos().to_le_bytes());
    }
    Ok(Some(hex_de(&h.finalize())))
}

fn hex_de(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Espera a árvore estabilizar (02, janela de silêncio): hash → espera a
/// janela → hash de novo; iguais = estável. Teto de
/// [`TETO_RODADAS_ESTABILIDADE`] rodadas — estourou, `Err` com o motivo
/// (build de asset ou plano de migrations NUNCA rodam sobre árvore em
/// mudança sem o operador saber). `progresso` recebe as mensagens de cada
/// etapa (o CLI imprime; os testes coletam) — a função em si não fala com
/// rede nem com o mutagen.
pub fn esperar_estabilidade(
    dir: &Path,
    extra_ignore: &[String],
    subcaminho: Option<&Path>,
    janela: std::time::Duration,
    mut progresso: impl FnMut(&str),
) -> Result<(), String> {
    let Some(mut anterior) = hash_estabilidade(dir, extra_ignore, subcaminho)? else {
        progresso("nenhum arquivo rastreável na subárvore — considerada estável");
        return Ok(());
    };
    for rodada in 1..=TETO_RODADAS_ESTABILIDADE {
        progresso(&format!(
            "janela de silêncio: {} s (rodada {rodada}/{TETO_RODADAS_ESTABILIDADE})…",
            janela.as_secs()
        ));
        std::thread::sleep(janela);
        let Some(atual) = hash_estabilidade(dir, extra_ignore, subcaminho)? else {
            // A árvore ESVZIOU no meio da espera: sem arquivo, nada muda —
            // estado estável por definição (a sync não tem o que replicar).
            progresso("subárvore ficou vazia durante a espera — considerada estável");
            return Ok(());
        };
        if atual == anterior {
            progresso("árvore estável — pode seguir");
            return Ok(());
        }
        progresso("árvore mudou durante a janela — reiniciando a espera");
        anterior = atual;
    }
    Err(format!(
        "árvore de {} não estabilizou em {TETO_RODADAS_ESTABILIDADE} rodadas de {} s — algo está escrevendo sem parar (watcher local? build em curso?)",
        dir.display(),
        janela.as_secs()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Toggle OFF: só `extra_ignore`, `.dockerignore` nem é lido.
    #[test]
    fn ignores_do_projeto_sem_toggle_so_extras() {
        let dir = std::env::temp_dir();
        let sync = SyncSection {
            extra_ignore: Some(vec!["resources/svg/**".into()]),
            use_dockerignore: Some(false),
            dockerignore_path: None,
        };
        let got = ignores_do_projeto(&dir, Some(&sync)).unwrap();
        assert_eq!(got, vec!["resources/svg/**".to_string()]);
        // Seção ausente: nada a adicionar.
        assert!(ignores_do_projeto(&dir, None).unwrap().is_empty());
    }

    /// Toggle ON: comentários e vazios caem fora, padrões passam verbatim
    /// (inclusive exceção `!`), extras vêm primeiro, duplicata some.
    #[test]
    fn ignores_do_projeto_soma_padroes_do_dockerignore() {
        let base = std::env::temp_dir().join(format!("flux-dockerignore-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(
            base.join(".dockerignore"),
            "# dependências\n\
             \n\
             node_modules\n\
             **/dist\n\
             tests/\n\
             !tests/keep.txt\n",
        )
        .unwrap();
        let sync = SyncSection {
            extra_ignore: Some(vec!["resources/svg/**".into(), "**/dist".into()]),
            use_dockerignore: Some(true),
            dockerignore_path: None,
        };
        let got = ignores_do_projeto(&base, Some(&sync)).unwrap();
        assert_eq!(
            got,
            vec![
                "resources/svg/**".to_string(),
                "**/dist".to_string(),
                "node_modules".to_string(),
                "tests/".to_string(),
                "!tests/keep.txt".to_string(),
            ]
        );
        std::fs::remove_dir_all(&base).ok();
    }

    /// Caminho custom (`dockerignore_path`) resolve relativo à raiz.
    #[test]
    fn ignores_do_projeto_respeita_dockerignore_path() {
        let base =
            std::env::temp_dir().join(format!("flux-dockerignore-path-{}", std::process::id()));
        std::fs::create_dir_all(base.join("config")).unwrap();
        std::fs::write(base.join("config").join("ctx.dockerignore"), b"docs/\n").unwrap();
        let sync = SyncSection {
            extra_ignore: None,
            use_dockerignore: Some(true),
            dockerignore_path: Some("config/ctx.dockerignore".into()),
        };
        let got = ignores_do_projeto(&base, Some(&sync)).unwrap();
        assert_eq!(got, vec!["docs/".to_string()]);
        std::fs::remove_dir_all(&base).ok();
    }

    /// Toggle ON e arquivo ausente é ERRO com o caminho na mensagem — config
    /// declarada nunca vira fallback silencioso.
    #[test]
    fn ignores_do_projeto_com_toggle_e_sem_arquivo_e_erro() {
        let base =
            std::env::temp_dir().join(format!("flux-dockerignore-ausente-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let sync = SyncSection {
            extra_ignore: None,
            use_dockerignore: Some(true),
            dockerignore_path: None,
        };
        let err = ignores_do_projeto(&base, Some(&sync)).unwrap_err();
        assert!(
            err.contains("use_dockerignore") && err.contains(".dockerignore"),
            "erro cita toggle e arquivo: {err}"
        );
        std::fs::remove_dir_all(&base).ok();
    }

    /// `for_project` integra os padrões do `.dockerignore` no spec da sessão.
    #[test]
    fn for_project_carrega_dockerignore_no_spec() {
        if !git_disponivel() {
            eprintln!("git indisponível — teste pulado");
            return;
        }
        let base = repo_temp("dockerignore-spec");
        std::fs::write(base.join("flux.toml"), b"[sync]\nuse_dockerignore = true\n").unwrap();
        std::fs::write(base.join(".dockerignore"), b"docs/\n**/target\n").unwrap();
        let spec = SyncSpec::for_project(&base, Beta::Local(base.join("replica")), None).unwrap();
        let ig = spec.ignores();
        for esperado in ["docs/", "**/target"] {
            assert!(
                ig.iter().any(|g| g == esperado),
                "{esperado} tem de entrar no spec: {ig:?}"
            );
        }
        // Globais OBRIGATÓRIOS continuam na frente, intocados.
        assert_eq!(ig.first(), Some(&".env".to_string()));
        std::fs::remove_dir_all(&base).ok();
    }

    /// Git no PATH? Os testes de árvore precisam de `git ls-files`; sem git,
    /// passam vazios (mesma convenção dos testes gated por `MUTAGEN_BIN`).
    fn git_disponivel() -> bool {
        Command::new("git")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Repo git vazio em tempdir (estilo dos testes do main.rs: pasta única
    /// por processo, removida no fim).
    fn repo_temp(nome: &str) -> std::path::PathBuf {
        let base = std::env::temp_dir().join(format!("flux-est-{nome}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let st = Command::new("git")
            .args(["-C", &base.display().to_string(), "init", "-q"])
            .status()
            .expect("git deveria ter sido lançado (checado por git_disponivel)");
        assert!(st.success(), "git init falhou em {}", base.display());
        base
    }

    /// O JSON REAL do mutagen serializa `user` e `host` em campos separados
    /// (`endpoint.go`) — sem '@' em nenhum. A recombinação `user@host` é a
    /// que `spec_da_sessao` consome para reconstruir a `Beta` no rescan do
    /// painel; sem ela, o split em '@' falha a cada tick na topologia real
    /// (beta SSH).
    #[test]
    fn endpoint_ssh_recombina_user_e_host_do_json_do_mutagen() {
        let v: Value = serde_json::json!({
            "identifier": "sync_abc123",
            "name": "flux-vipa",
            "paused": false,
            "status": 6,
            "alpha": {"path": "C:/proj/vipa", "connected": true},
            "beta": {
                "protocol": 2,
                "user": "flux",
                "host": "192.168.140.2",
                "path": "/srv/flux/projects/vipa",
                "connected": true
            }
        });
        let s = session_de(&v).expect("linha válida do sync list");
        assert_eq!(s.beta.host, "flux@192.168.140.2", "user@host recombinado");
        assert!(s.beta.remote, "host presente = remoto");
        assert_eq!(s.beta.path, "/srv/flux/projects/vipa");
        // O consumo do rescan (split em '@') fecha com o formato recombinado.
        let (user, host) = s
            .beta
            .host
            .split_once('@')
            .expect("host recombinado é user@host");
        assert_eq!((user, host), ("flux", "192.168.140.2"));
        // Alpha local: sem user/host, remote falso e host vazio.
        assert!(!s.alpha.remote);
        assert!(s.alpha.host.is_empty());
    }

    #[test]
    fn padrao_de_ignore_cobre_o_subconjunto_da_plataforma() {
        let p: Vec<String> = [
            ".env",
            ".env.*",
            "public/hot",
            "vendor/",
            "node_modules/",
            "*.log",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert!(caminho_ignorado(".env", &p));
        assert!(caminho_ignorado(".env.local", &p), ".env.* casa o sufixo");
        assert!(
            caminho_ignorado("app/.env.local", &p),
            "padrão solto casa qualquer segmento"
        );
        assert!(
            caminho_ignorado("public/hot", &p),
            "com / é ancorado na raiz"
        );
        assert!(
            !caminho_ignorado("a/public/hot", &p),
            "ancorado NÃO casa em subnível"
        );
        assert!(
            caminho_ignorado("vendor/autoload.php", &p),
            "diretório com / final casa em qualquer nível"
        );
        assert!(caminho_ignorado("x/vendor/y.php", &p));
        assert!(caminho_ignorado("debug.log", &p), "glob *.log");
        assert!(
            !caminho_ignorado("debug.log.bak", &p),
            "sufixo não é sufixo aqui"
        );
        assert!(!caminho_ignorado("log.txt", &p));
        assert!(
            !caminho_ignorado("app/Http/Controllers/Controller.php", &p),
            "código do projeto nunca é ignorado por esses padrões"
        );
        // Windows: barra invertida normalizada antes de casar.
        assert!(caminho_ignorado("vendor\\autoload.php", &p));
    }

    #[test]
    fn dir_sem_git_e_erro_com_origem() {
        if !git_disponivel() {
            eprintln!("git indisponível — teste pulado");
            return;
        }
        let base = std::env::temp_dir().join(format!("flux-est-nogit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(base.join("a.txt"), b"x").unwrap();
        let err = hash_estabilidade(&base, &[], None).unwrap_err();
        assert!(
            err.contains("git ls-files"),
            "origem do erro tem de citar o git: {err}"
        );
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn hash_da_arvore_ignora_globais_e_muda_com_conteudo() {
        if !git_disponivel() {
            eprintln!("git indisponível — teste pulado");
            return;
        }
        let base = repo_temp("ignora");
        std::fs::write(base.join("rotas.php"), b"um").unwrap();
        std::fs::write(base.join(".env"), b"segredo").unwrap();
        std::fs::create_dir_all(base.join("vendor/x")).unwrap();
        std::fs::write(base.join("vendor/x/autoload.php"), b"v").unwrap();

        let h0 = hash_estabilidade(&base, &[], None).unwrap().unwrap();

        // GLOBAL_IGNORES fora do hash: mudança em .env e vendor/ NÃO move o
        // hash — é exatamente o que impede o build de disparar por causa deles.
        std::fs::write(base.join(".env"), b"outro-segredo").unwrap();
        std::fs::write(base.join("vendor/x/autoload.php"), b"v2").unwrap();
        let h1 = hash_estabilidade(&base, &[], None).unwrap().unwrap();
        assert_eq!(h0, h1, "vendor/ e .env ficam fora do hash");

        // Conteúdo de arquivo de verdade muda → hash muda.
        std::fs::write(base.join("rotas.php"), b"dois").unwrap();
        let h2 = hash_estabilidade(&base, &[], None).unwrap().unwrap();
        assert_ne!(h0, h2, "mudança rastreada tem de aparecer no hash");

        // extra_ignore do projeto entra no filtro: com tudo ignorado, a
        // subárvore vira None (estado legítimo e estável).
        let vazio = hash_estabilidade(&base, &["rotas.php".to_owned()], None).unwrap();
        assert_eq!(vazio, None, "sem arquivo rastreável: None, não erro");

        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn subcaminho_recorta_o_escopo_do_hash() {
        if !git_disponivel() {
            eprintln!("git indisponível — teste pulado");
            return;
        }
        let base = repo_temp("sub");
        std::fs::create_dir_all(base.join("database/migrations")).unwrap();
        std::fs::create_dir_all(base.join("app")).unwrap();
        std::fs::write(base.join("database/migrations/0001_cria.php"), b"m").unwrap();
        std::fs::write(base.join("app/outro.php"), b"o").unwrap();
        let sub = Path::new("database/migrations");

        let h0 = hash_estabilidade(&base, &[], Some(sub)).unwrap().unwrap();
        // Mudança FORA do escopo não move o hash da subárvore.
        std::fs::write(base.join("app/outro.php"), b"o2").unwrap();
        let h1 = hash_estabilidade(&base, &[], Some(sub)).unwrap().unwrap();
        assert_eq!(h0, h1, "fora do subcaminho não conta");
        // Mudança DENTRO move.
        std::fs::write(base.join("database/migrations/0001_cria.php"), b"m2").unwrap();
        let h2 = hash_estabilidade(&base, &[], Some(sub)).unwrap().unwrap();
        assert_ne!(h0, h2, "migration mutada tem de mover o hash");

        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn espera_estabilidade_passa_quando_nada_muda() {
        if !git_disponivel() {
            eprintln!("git indisponível — teste pulado");
            return;
        }
        let base = repo_temp("estavel");
        std::fs::write(base.join("a.php"), b"a").unwrap();
        let mut msgs: Vec<String> = Vec::new();
        esperar_estabilidade(&base, &[], None, Duration::from_millis(30), |m| {
            msgs.push(m.to_owned())
        })
        .expect("árvore parada estabiliza na primeira rodada");
        assert!(
            msgs.iter().any(|m| m.contains("estável")),
            "progresso anunciou a estabilidade: {msgs:?}"
        );
        std::fs::remove_dir_all(&base).ok();
    }

    /// Escritor em loop + janela de 100 ms: entre DOIS hashes consecutivos
    /// sempre cai ao menos uma escrita (25 ms de período), então o teto de
    /// 3 rodadas estoura com motivo — o caminho de erro da 02.
    #[test]
    fn espera_estabilidade_erra_quando_escrita_nao_para() {
        if !git_disponivel() {
            eprintln!("git indisponível — teste pulado");
            return;
        }
        let base = repo_temp("instavel");
        let alvo = base.join("a.php");
        std::fs::write(&alvo, b"00000000000000000000").unwrap();
        let escritor = {
            let alvo = alvo.clone();
            std::thread::spawn(move || {
                for i in 0..60u32 {
                    std::thread::sleep(Duration::from_millis(25));
                    let corpo = format!("{i:020}");
                    if std::fs::write(&alvo, corpo.as_bytes()).is_err() {
                        break;
                    }
                }
            })
        };
        let err = esperar_estabilidade(&base, &[], None, Duration::from_millis(100), |_| {})
            .expect_err("escrita contínua NÃO estabiliza");
        escritor.join().unwrap();
        assert!(
            err.contains("não estabilizou"),
            "motivo explícito no erro: {err}"
        );
        std::fs::remove_dir_all(&base).ok();
    }
}
