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

mod estabilidade;
mod spec;

// Superfície pública preservada: os consumidores continuam `sync::*`
// (invariante L7.4 — nenhum consumidor muda), e os 22 testes de
// tests/sync_tests.rs (compilado standalone via #[path]) acessam tudo pelo
// mesmo caminho. Sem consumidor interno, o `pub use` sobra como "unused" no
// binário — o allow é o contrato da ponte, não concessão.
pub(crate) use estabilidade::caminho_ignorado;
#[allow(unused_imports)]
pub use estabilidade::{
    TETO_RODADAS_ESTABILIDADE, esperar_estabilidade, hash_estabilidade, listar_arquivos_rastreaveis,
};
#[allow(unused_imports)]
pub use spec::{
    Beta, GLOBAL_IGNORES, LABEL, SyncSpec, VM_PROJECTS_ROOT, branch_slug, ignores_do_projeto,
    vm_beta,
};

use std::process::Command;

use serde_json::Value;

/// Env que aponta o binário do mutagen (convenção de testes gated do projeto:
/// sem a env definida, os testes de sync real PASSAM vazios).
pub const ENV_BIN: &str = "MUTAGEN_BIN";

/// Endpoint da VM (spec 01/02): usuário, IP fixo e raiz dos projetos.
/// L2.5: FONTE ÚNICA das constantes da VM. Mora AQUI (e não no tunel.rs)
/// porque a ponte `#[path]` compila este arquivo AVULSO no target de testes —
/// `crate::tunel` não existe nesse contexto (constraint do L1.7); a direção
/// possível é tunel.rs → sync.rs. Redeclarar noutro arquivo nasce drift
/// silencioso (sync aponta VM, túnel do token aponta outra).
pub const VM_USER: &str = "flux";
pub const VM_HOST: &str = "192.168.140.2";

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

// ------------------------------------------------------------------ SyncSpec

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
/// para os testes de integração via ponte `#[path]` (sync_tests.rs);
/// consumo interno em `create`.
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
