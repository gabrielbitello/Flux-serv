//! Schemas de configuração: `flux.toml` do projeto (versionado, SEM segredo —
//! 09) e config global (`%USERPROFILE%\.flux\config.toml` no Windows — 09).
//! Validação estrita dos dois lados: chave desconhecida é ERRO (06 — "nada de
//! typo silencioso").

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::OnceLock;

use serde::Deserialize;

use crate::error::FluxError;
use crate::framework::Framework;

// ---------------------------------------------------------------- flux.toml

/// `flux.toml` na raiz do projeto (06). Seções ausentes herdam o default da
/// plataforma; segredo NUNCA entra aqui.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectToml {
    pub name: Option<String>,
    pub framework: Option<Framework>,
    pub domains: Option<DomainsSection>,
    pub db: Option<DbSection>,
    pub build: Option<BuildSection>,
    pub sleep: Option<SleepSection>,
    pub resources: Option<ResourcesSection>,
    pub sync: Option<SyncSection>,
    pub temporary: Option<TemporarySection>,
    pub provision: Option<ProvisionSection>,
    pub branding: Option<BrandingSection>,
    pub shield: Option<ShieldSection>,
    pub health: Option<HealthSection>,
    pub dev: Option<DevSection>,
    pub limits: Option<ProjectLimitsSection>,
}

impl ProjectToml {
    /// Parse estrito do conteúdo do `flux.toml`.
    pub fn parse_str(src: &str) -> Result<Self, FluxError> {
        toml::from_str(src).map_err(|e| FluxError::Config {
            file: "flux.toml".into(),
            source: e,
        })
    }

    pub fn load(path: &Path) -> Result<Self, FluxError> {
        let src = std::fs::read_to_string(path).map_err(|e| FluxError::ConfigIo {
            file: path.display().to_string(),
            source: e,
        })?;
        toml::from_str(&src).map_err(|e| FluxError::Config {
            file: path.display().to_string(),
            source: e,
        })
    }

    /// Nome efetivo: explícito no toml vence; senão o nome da pasta (06).
    pub fn effective_name<'a>(&'a self, folder_fallback: &'a str) -> &'a str {
        self.name.as_deref().unwrap_or(folder_fallback)
    }

    /// Path de health. Default `/up` (laravel); node/rust DEVEM declarar
    /// `[health] path` — a exigência por stack é validada no registro (06).
    pub fn health_path(&self) -> &str {
        self.health
            .as_ref()
            .and_then(|h| h.path.as_deref())
            .unwrap_or("/up")
    }

    /// Janela de silêncio do `flux build` (02). Default 180 s.
    pub fn build_silence_window_secs(&self) -> u64 {
        self.build
            .as_ref()
            .map(|b| b.silence_window_secs())
            .unwrap_or(180)
    }

    pub fn framework(&self) -> Framework {
        self.framework.unwrap_or_default()
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DomainsSection {
    pub prod: Option<String>,
    pub dev: Option<String>,
}

/// Política de rebuild do banco em dev (05). Prod NUNCA reconstrói — não
/// existe flag para isso, por design.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RebuildPolicy {
    #[default]
    Auto,
    Always,
    Never,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DbSection {
    pub rebuild_policy: Option<RebuildPolicy>,
    pub seed_on_rebuild: Option<bool>,
    /// node/rust: comando de schema dentro do container (06); vazio = sem
    /// pipeline automático (fluxd avisa, nunca silêncio).
    pub migrate_cmd: Option<String>,
    /// Janela de silêncio da árvore ANTES da coleta de migrations no `flux up`
    /// (02): a coleta lê `database/migrations` do disco local e o hash vai
    /// para o plano de rebuild — coletar no meio de uma escrita diverge do
    /// que a sync replica. Default 5 s.
    pub silence_window_secs: Option<u64>,
    /// Banco PRÓPRIO do projeto (21/10, pedido do vippers): o compose do
    /// projeto declara o service de banco (mysql/mariadb) e o fluxd
    /// materializa UM container dedicado por grupo — `flux-db-{grupo}` — em
    /// vez de apontar para o flux-db global. Dorme e acorda junto com o
    /// projeto (última instância parou → container para; qualquer subida →
    /// acorda antes do app).
    pub own: Option<bool>,
    /// Grupo de compartilhamento do banco próprio: projetos com o mesmo
    /// `grupo` usam o MESMO container, cada instância com seu database
    /// (`{name}_{env}`). Default: o próprio `name` (worktree registrado como
    /// projeto declara o grupo do projeto principal).
    pub grupo: Option<String>,
    /// TTL dos databases de DEV inativos, em dias (default 7 = "1 semana").
    /// Antes de apagar, o fluxd grava dump em `/srv/flux/state/db-backups/`.
    /// Databases de PROD nunca expiram, sem exceção.
    pub ttl_days: Option<u64>,
    /// Imagem do banco quando o COMPOSE NÃO declara service de banco
    /// (21/10, pedido do Gabriel: "não é para ter no compose, ele baixa
    /// sozinho") — ex.: `image = "mysql:9.3"`. Ausente/vazia = default da
    /// plataforma (`banco::IMAGEM_PADRAO`). Se o compose TEM o service, a
    /// captura dele vence (imagem + credenciais do compose).
    pub image: Option<String>,
}

impl DbSection {
    /// Janela de silêncio das migrations (02). Default 5 s — migration escrita
    /// por editor/grupo de trabalho converge em segundos.
    pub fn silence_window_secs(&self) -> u64 {
        self.silence_window_secs.unwrap_or(5)
    }

    /// Política de rebuild do banco DEV (05). Default `auto` — quem nunca
    /// declarou a chave mantém o comportamento de sempre.
    pub fn rebuild_policy(&self) -> RebuildPolicy {
        self.rebuild_policy.unwrap_or_default()
    }

    /// Banco próprio ligado? Default `false` — quem nunca declarou segue no
    /// flux-db global (mesmo idioma de `[temporary] enabled`).
    pub fn own(&self) -> bool {
        self.own.unwrap_or(false)
    }

    /// Grupo efetivo do banco próprio: declarado vence; default = nome do
    /// projeto. Erro quando o declarado não é nome de projeto válido — viraria
    /// container/rede com nome fora do contrato (nada de chute silencioso).
    pub fn db_grupo(&self, name: &str) -> Result<String, String> {
        match self.grupo.as_deref() {
            None => Ok(name.to_owned()),
            Some(g) if crate::nomes::projeto_valido(g) => Ok(g.to_owned()),
            Some(g) => Err(format!(
                "db.grupo '{g}' inválido — use minúsculas, dígitos e '-' (vira container flux-db-{g})"
            )),
        }
    }

    /// TTL dos databases de dev em dias para exibição; mínimo 1 (0 = typo,
    /// não "apaga já").
    pub fn ttl_days_efetivo(&self) -> u64 {
        self.ttl_days.unwrap_or(7).max(1)
    }

    /// TTL dos databases de dev em segundos (satura em vez de estourar i64).
    pub fn ttl_secs(&self) -> i64 {
        i64::try_from(self.ttl_days_efetivo().saturating_mul(86_400)).unwrap_or(i64::MAX)
    }

    /// Imagem declarada para o banco próprio quando o compose não tem o
    /// service. `None` = default da plataforma (banco::IMAGEM_PADRAO).
    /// Declarada vazia é typo e vira None — o evento do up mostra o que
    /// valer, nunca silêncio.
    pub fn own_image(&self) -> Option<&str> {
        self.image
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
    }
}

/// Build de assets do projeto (02, `flux build`): comando rodado DENTRO do
/// container de dev na VM, precedido da janela de silêncio da árvore no
/// Windows (a sync precisa convergir antes) e do lock global
/// `/srv/flux/build.lock` na VM (um build por vez, no fluxd inteiro).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildSection {
    /// Comando de build de assets (ex.: `npm run build`). Só `laravel` tem
    /// default; as demais stacks precisam declarar (nada de chute).
    pub command: Option<String>,
    /// Service do compose que roda o comando — convenção da plataforma é o
    /// container `app` (06).
    pub service: Option<String>,
    /// Janela de silêncio da árvore no Windows antes do build (02). Default
    /// 180 s — assets mudam em rajada (vários saves seguidos), bem mais que
    /// a janela de migrations.
    pub silence_window_secs: Option<u64>,
}

impl BuildSection {
    /// Service efetivo do build (convenção da plataforma: container `app`).
    pub fn service(&self) -> &str {
        self.service.as_deref().unwrap_or("app")
    }

    /// Janela de silêncio do build. Default 180 s (02).
    pub fn silence_window_secs(&self) -> u64 {
        self.silence_window_secs.unwrap_or(180)
    }
}

/// Hibernação de prod (11): `pause` acorda em < 1 s; worker segue 24/7.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProdSleepMode {
    #[default]
    Pause,
    Stop,
    Off,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SleepSection {
    pub dev_enabled: Option<bool>,
    pub idle_minutes: Option<u32>,
    pub prod_mode: Option<ProdSleepMode>,
    pub prod_idle_minutes: Option<u32>,
    pub cpu_percent_teto: Option<f64>,
    /// Hold de API (18/10, pedido do Gabriel p/ zcode-api): com `true`, request
    /// de API para projeto dormindo/subindo é SEGURA enquanto o container
    /// acorda — nunca 503 de volta (IDEs quebram com recusa, toleram espera).
    /// Default `false` = comportamento atual (503 + Retry-After, 12).
    pub api_hold: Option<bool>,
}

/// Limites INDIVIDUAIS de prod (11).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourcesSection {
    pub http_memory_mb: Option<u32>,
    pub http_cpus: Option<f64>,
    pub worker_memory_mb: Option<u32>,
    pub worker_cpus: Option<f64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncSection {
    /// Só ADICIONA ao ignore global (02) — nunca restringe o ignore de fábrica
    /// (`.env`, `vendor/`, `node_modules/`, `storage/`).
    pub extra_ignore: Option<Vec<String>>,
    /// Soma os padrões do `.dockerignore` aos ignores da sessão (02). Default
    /// false — quem nunca ouviu falar no `.dockerignore` não pode ter o sync
    /// mudado por baixo.
    pub use_dockerignore: Option<bool>,
    /// Onde está o arquivo, relativo à raiz do projeto. Só tem efeito com
    /// `use_dockerignore = true`.
    pub dockerignore_path: Option<String>,
}

impl SyncSection {
    /// Toggle desligado por default (mesma filosofia do `[temporary]`).
    pub fn use_dockerignore(&self) -> bool {
        self.use_dockerignore.unwrap_or(false)
    }

    /// Convenção do Docker: arquivo na raiz do contexto de build.
    pub fn dockerignore_path(&self) -> &str {
        self.dockerignore_path.as_deref().unwrap_or(".dockerignore")
    }
}

/// Modo dev temporário (14): apaga o projeto SOZINHO da VM após TTL sem
/// interação. Repo no Windows intocado (sync é one-way-replica VM←Windows).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TemporarySection {
    /// Precisa ser dito — default false (nada se autodestrói por acidente).
    pub enabled: Option<bool>,
    /// Horas SEM INTERAÇÃO NENHUMA antes do teardown. Default 6 (14).
    pub ttl_hours: Option<u64>,
}

impl TemporarySection {
    pub fn enabled(&self) -> bool {
        self.enabled.unwrap_or(false)
    }

    pub fn ttl_hours(&self) -> u64 {
        self.ttl_hours.unwrap_or(6)
    }

    /// TTL em segundos para o sweep do fluxd (satura em vez de estourar i64).
    pub fn ttl_secs(&self) -> i64 {
        i64::try_from(self.ttl_hours().saturating_mul(3600)).unwrap_or(i64::MAX)
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProvisionSection {
    pub on_wake: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrandingSection {
    /// Logo do cliente na página de manutenção (15).
    pub logo_url: Option<String>,
}

impl BrandingSection {
    /// URL declarada e não vazia; `Some("")` é typo e vira None com aviso do
    /// chamador — nunca `<img src="">` na página.
    pub fn logo_url(&self) -> Option<&str> {
        self.logo_url
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShieldSection {
    /// Paths de auth para o anti-brute-force; default detecta login padrão (16).
    pub auth_paths: Option<Vec<String>>,
    pub tarpit: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HealthSection {
    pub path: Option<String>,
}

/// Live server de dev (seção `[dev]`): comando de hot reload rodado DENTRO do
/// container `app` logo após um `compose up` com SUCESSO em env=dev —
/// `docker compose -p {projeto} exec -d app sh -c "cd /var/www && {live}"`.
/// Ex.: `npm run dev -- --host 0.0.0.0`. Nunca roda em prod (o gatilho é
/// `env == "dev"` no `subir_projeto` do compose.rs); falha do live NUNCA
/// derruba o up — vira warn no painel com o erro real.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DevSection {
    /// Comando de hot reload, executado em `/var/www` do container `app`.
    pub live: Option<String>,
    /// Overlays de arquivos (02/06): quando `true`, o CLI aplica
    /// `.flux/overlays/**` por cima do repo NA VM a cada `up` — versões
    /// locais (gitignored) de arquivos que o dev precisa diferentes do que
    /// está commitado (ex.: SQL de migration por dialeto de banco). Só faz
    /// efeito com o dir `.flux/overlays/` presente na pasta do projeto.
    pub overlays: Option<bool>,
}

/// Override por projeto da borda (07): rate limit por host.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectLimitsSection {
    pub rate: Option<u32>,
}

// ------------------------------------------------------------ config global

/// Config global (09). No Windows: `%USERPROFILE%\.flux\config.toml` com ACL
/// do usuário. Na VM, o fluxd lê o mesmo schema por `--config`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct GlobalConfig {
    pub vm: Option<VmSection>,
    pub domain: DomainConfig,
    pub api: Option<ApiSection>,
    pub defaults: Option<DefaultsSection>,
    pub limits: Option<GlobalLimitsSection>,
    pub queue: Option<QueueSection>,
    pub logs: Option<LogsSection>,
    pub alerts: Option<AlertsSection>,
    pub shield: Option<GlobalShieldSection>,
    pub status: Option<StatusSection>,
    pub tools: Option<ToolsSection>,
    pub deploy: Option<DeploySection>,
}

impl GlobalConfig {
    pub fn parse_str(src: &str) -> Result<Self, FluxError> {
        toml::from_str(src).map_err(|e| FluxError::Config {
            file: "config.toml".into(),
            source: e,
        })
    }

    pub fn load(path: &Path) -> Result<Self, FluxError> {
        let src = std::fs::read_to_string(path).map_err(|e| FluxError::ConfigIo {
            file: path.display().to_string(),
            source: e,
        })?;
        toml::from_str(&src).map_err(|e| FluxError::Config {
            file: path.display().to_string(),
            source: e,
        })
    }

    pub fn idle_minutes(&self) -> u32 {
        self.defaults
            .as_ref()
            .and_then(|d| d.idle_minutes)
            .unwrap_or(15)
    }

    /// Concorrência da fila de builds (11/R27): UM build por vez por default,
    /// prioridade prod > provision > dev.
    pub fn build_concurrency(&self) -> u32 {
        self.queue
            .as_ref()
            .and_then(|q| q.build_concurrency)
            .unwrap_or(1)
    }

    /// Teto de body na borda do proxy (07/R30): 100 MB.
    pub fn max_body_bytes(&self) -> u64 {
        self.limits
            .as_ref()
            .and_then(|l| l.max_body_bytes)
            .unwrap_or(104_857_600)
    }

    /// Rota pública de deploy ligada? (`[deploy] public_route`, 10/F4.)
    /// Default FALSE: o fluxd nasce fechado — nenhuma superfície nova fica
    /// exposta sem decisão explícita no config (mesmo idioma de
    /// `[temporary] enabled`, que também só existe declarado).
    pub fn deploy_public(&self) -> bool {
        self.deploy
            .as_ref()
            .and_then(|d| d.public_route)
            .unwrap_or(false)
    }
}

/// Rota pública de deploy (10/F4): `POST /api/projects/{name}/deploy` no host
/// do painel, servida pelo router :8080 do fluxd. A API de controle :7420
/// NÃO é afetada — continua 127.0.0.1.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeploySection {
    /// `true` = rota responde (com auth por projeto + rate limit próprio);
    /// ausente/`false` = o path não existe para o router (404 brandiado).
    pub public_route: Option<bool>,
}

/// Domínios gerenciados (09 [domain]): `{name}.bitello.xyz` prod,
/// `{name}-d.bitello.xyz` dev, painel `flux.bitello.xyz`.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DomainConfig {
    pub zone: String,
    pub dev_suffix: String,
    pub panel_host: String,
}

impl Default for DomainConfig {
    fn default() -> Self {
        Self {
            zone: "bitello.xyz".into(),
            dev_suffix: "-d".into(),
            panel_host: "flux.bitello.xyz".into(),
        }
    }
}

impl DomainConfig {
    pub fn prod_host(&self, name: &str) -> String {
        format!("{name}.{}", self.zone)
    }

    pub fn dev_host(&self, name: &str) -> String {
        format!("{name}{}.{}", self.dev_suffix, self.zone)
    }
}

impl ProjectToml {
    /// Hosts custom de `[domains]` (09), validados e em minúsculas:
    /// `(prod, dev)`. Regras — o host PRECISA terminar com `.{zone}` (o túnel
    /// cobre `*.{zone}`; host fora da zona nunca rotearia, e aí a falha seria
    /// silenciosa na borda do Cloudflare), não pode ser o painel, não pode
    /// começar com `vite-` (prefixo reservado do dev server) nem o rótulo
    /// terminar com o `dev_suffix` (colide com a derivação de dev). Charset
    /// de host legal. Erro SEMPRE cita a chave. Os hosts derivados continuam
    /// `[domains]` (09): valida e normaliza os hosts custom do projeto →
    /// `(prod, dev)`. DELEGAÇÃO de 1 linha — a REGRA vive em `host.rs`
    /// (L2.3): quem valida na escrita e quem interpreta na leitura (`vite-`,
    /// `dev_suffix`) precisam ser o mesmo arquivo, senão nasce host que
    /// valida no registro mas não roteia na borda (ou vice-versa).
    pub fn hosts_custom(
        &self,
        domain: &DomainConfig,
    ) -> Result<(Option<String>, Option<String>), String> {
        crate::host::validar_hosts_custom(self.domains.as_ref(), domain)
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VmSection {
    pub switch: Option<String>,
    pub host_ip: Option<String>,
    pub vm_ip: Option<String>,
    pub user: Option<String>,
    pub key: Option<String>,
    pub cpus: Option<u32>,
    pub memory_min_gb: Option<u32>,
    pub memory_max_gb: Option<u32>,
    pub disk_gb: Option<u32>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiSection {
    /// Token da API do fluxd (09): vive AQUI (arquivo do usuário), nunca no
    /// flux.toml; no fluxd só o hash, no SQLite.
    pub token: Option<String>,
    pub base_url: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DefaultsSection {
    pub idle_minutes: Option<u32>,
    pub cpu_percent_teto: Option<f64>,
    pub sync_debounce_secs: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageGuardSection {
    /// Imagens de dados/serviço proibidas nos Dockerfile/compose dos projetos
    /// (05). Default cobre bancos E cache em memória — decisão 13/10/2026
    /// (pergunta 3 da 00): Redis/valkey/memcached bloqueados por padrão, pois
    /// nenhum projeto usa; cache/fila Laravel rodam no driver `database`.
    pub block: Option<Vec<String>>,
}

impl ImageGuardSection {
    pub fn block_list(&self) -> &[String] {
        match &self.block {
            Some(v) => v,
            None => default_block_list(),
        }
    }
}

fn default_block_list() -> &'static [String] {
    static DEFAULT: OnceLock<Vec<String>> = OnceLock::new();
    DEFAULT.get_or_init(|| {
        [
            "mysql",
            "mariadb",
            "postgres",
            "mongo",
            "redis",
            "valkey",
            "memcached",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    })
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GlobalLimitsSection {
    pub max_body_bytes: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueueSection {
    pub build_concurrency: Option<u32>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogsSection {
    pub retention_days: Option<u32>,
}

/// Canal de alerta outbound (13): telegram | discord | email.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AlertChannel {
    #[default]
    Telegram,
    Discord,
    Email,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AlertsSection {
    pub channel: Option<AlertChannel>,
    pub cooldown_minutes: Option<u32>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GlobalShieldSection {
    /// Dias em shadow mode (só log) antes de enforce (16) — falso positivo
    /// nunca derruba cliente.
    pub shadow_days: Option<u32>,
    /// IPs nunca bloqueados (IP do host, do Gabriel).
    pub allowlist: Option<Vec<String>>,
    /// Regra do anti-brute-force em texto (16); parse numérico é da fase F19.
    pub brute_force: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatusSection {
    /// Testemunha externa do healthz (17): monitor gratuito ou sonda no
    /// notebook; NUNCA usada para acordar nada.
    pub external_witness: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolsSection {
    /// Imagens oficiais pinadas para `flux tool` (06): node → node:22, etc.
    pub images: Option<BTreeMap<String, String>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flux_toml_completo_parseia() {
        let src = r#"
name = "vippers"
framework = "laravel"

[db]
rebuild_policy = "auto"
seed_on_rebuild = true

[sleep]
dev_enabled = true
idle_minutes = 15
prod_mode = "pause"
prod_idle_minutes = 30
cpu_percent_teto = 5.0

[resources]
http_memory_mb = 512
http_cpus = 1.0
worker_memory_mb = 256
worker_cpus = 0.5

[sync]
extra_ignore = ["resources/svg/**"]
use_dockerignore = true
dockerignore_path = "config/.dockerignore"

[provision]
on_wake = "bash /provision.sh"

[branding]
logo_url = "https://vippers.bitello.xyz/logo.png"

[shield]
auth_paths = ["/login"]
tarpit = false

[health]
path = "/up"

[limits]
rate = 500
"#;
        let t = ProjectToml::parse_str(src).unwrap();
        assert_eq!(t.effective_name("outra-pasta"), "vippers");
        assert_eq!(t.framework(), Framework::Laravel);
        assert_eq!(t.health_path(), "/up");
        assert_eq!(t.limits.as_ref().unwrap().rate, Some(500));
        assert_eq!(
            t.db.as_ref().unwrap().rebuild_policy,
            Some(RebuildPolicy::Auto)
        );
        let sync = t.sync.as_ref().unwrap();
        assert!(sync.use_dockerignore());
        assert_eq!(sync.dockerignore_path(), "config/.dockerignore");
    }

    #[test]
    fn flux_toml_vazio_usa_defaults() {
        let t = ProjectToml::parse_str("").unwrap();
        assert_eq!(t.effective_name("pedi"), "pedi");
        assert_eq!(t.health_path(), "/up");
        assert_eq!(t.framework(), Framework::Laravel);
        // [sync] ausente: toggle do .dockerignore desligado, caminho na
        // convenção do Docker — o sync de quem nunca declarou a chave não
        // pode mudar de comportamento.
        assert!(!SyncSection::default().use_dockerignore());
        assert_eq!(SyncSection::default().dockerignore_path(), ".dockerignore");
    }

    #[test]
    fn flux_toml_chave_desconhecida_e_erro() {
        let err = ProjectToml::parse_str("naame = \"vipers\"").unwrap_err();
        assert!(err.to_string().contains("naame"), "mensagem: {err}");
    }

    /// Hold de API (18/10): `[sleep].api_hold` parseia; ausente = None
    /// (default OFF — nenhum projeto muda de comportamento sem a flag).
    #[test]
    fn sleep_api_hold_parseia_e_default_e_off() {
        let t = ProjectToml::parse_str("[sleep]\napi_hold = true\nidle_minutes = 10\n").unwrap();
        assert_eq!(t.sleep.as_ref().and_then(|s| s.api_hold), Some(true));
        let vazio = ProjectToml::parse_str("").unwrap();
        assert_eq!(vazio.sleep.and_then(|s| s.api_hold), None);
    }

    /// Chaves novas da 02: `[db].silence_window_secs` (default 5) e a seção
    /// `[build]` inteira (default 180, service `app`).
    #[test]
    fn janelas_de_silencio_parseiam_e_tem_defaults() {
        let t = ProjectToml::parse_str(
            "[db]\nsilence_window_secs = 7\n\n[build]\ncommand = \"npm run build\"\nservice = \"web\"\nsilence_window_secs = 60\n",
        )
        .unwrap();
        assert_eq!(
            t.db.as_ref().unwrap().silence_window_secs(),
            7,
            "valor explícito vence o default de 5"
        );
        let b = t.build.as_ref().unwrap();
        assert_eq!(b.command.as_deref(), Some("npm run build"));
        assert_eq!(b.service(), "web");
        assert_eq!(t.build_silence_window_secs(), 60);
    }

    #[test]
    fn sem_db_e_sem_build_os_defaults_da_plataforma_valem() {
        let t = ProjectToml::parse_str("").unwrap();
        assert_eq!(
            t.db.as_ref().map(|d| d.silence_window_secs()),
            None,
            "sem seção [db] não há override"
        );
        assert!(t.build.is_none(), "sem seção [build]");
        assert_eq!(
            t.build_silence_window_secs(),
            180,
            "default do build: 180 s (02)"
        );
        // Service efetivo com seção presente mas sem service:
        let b = ProjectToml::parse_str("[build]\ncommand = \"make\"\n")
            .unwrap()
            .build
            .unwrap();
        assert_eq!(b.service(), "app", "convenção da plataforma: container app");
    }

    #[test]
    fn build_section_chave_desconhecida_e_erro() {
        let err = ProjectToml::parse_str("[build]\ncomand = \"npm run build\"\n").unwrap_err();
        assert!(
            err.to_string().contains("comand"),
            "typo em [build] é erro, nunca ignorado: {err}"
        );
    }

    /// Banco próprio (21/10): chaves novas parseiam, defaults batem com o
    /// pedido (TTL = 1 semana) e grupo inválido é ERRO — vira nome de
    /// container, não pode passar batido.
    #[test]
    fn db_own_grupo_e_ttl_parseiam_com_defaults() {
        let t = ProjectToml::parse_str("[db]\nown = true\nttl_days = 7\n").unwrap();
        let db = t.db.as_ref().expect("seção [db]");
        assert!(db.own(), "own declarado liga o banco próprio");
        assert_eq!(
            db.db_grupo("vippers").unwrap(),
            "vippers",
            "grupo default = nome do projeto"
        );
        assert_eq!(db.ttl_days_efetivo(), 7, "TTL default = 1 semana");
        assert_eq!(db.ttl_secs(), 7 * 86_400);

        // Grupo declarado vence (worktree aponta pro banco do principal).
        let wt = ProjectToml::parse_str("[db]\nown = true\ngrupo = \"vippers\"\n")
            .unwrap()
            .db
            .unwrap();
        assert_eq!(wt.db_grupo("vippers-agenda").unwrap(), "vippers");
        assert!(wt.own(), "own declarado junto com grupo");

        // TTL 0 é typo: vira 1 dia, nunca "apaga já".
        let zero = ProjectToml::parse_str("[db]\nown = true\nttl_days = 0\n")
            .unwrap()
            .db
            .unwrap();
        assert_eq!(zero.ttl_days_efetivo(), 1, "ttl_days = 0 vira 1 (piso)");

        // Grupo com caractere fora do contrato é ERRO com a chave citada.
        let ruim = ProjectToml::parse_str("[db]\nown = true\ngrupo = \"Vippers X\"\n")
            .unwrap()
            .db
            .unwrap();
        let err = ruim.db_grupo("vippers").unwrap_err();
        assert!(err.contains("db.grupo"), "erro cita a chave: {err}");

        // Sem own, comportamento de sempre (flux-db global) — grupo/ttl mortos.
        let antigo = ProjectToml::parse_str("[db]\nrebuild_policy = \"auto\"\n")
            .unwrap()
            .db
            .unwrap();
        assert!(
            !antigo.own(),
            "default false — nada muda pra quem nunca declarou"
        );

        // Chave desconhecida em [db] continua erro estrito.
        let err = ProjectToml::parse_str("[db]\nowm = true\n").unwrap_err();
        assert!(
            err.to_string().contains("owm"),
            "typo em [db] é erro: {err}"
        );
    }

    /// `[db] image` (21/10 v2): compose SEM service de banco — o flux baixa
    /// sozinho. Declarada vence; vazia é typo (None → default da
    /// plataforma); ausente = None.
    #[test]
    fn db_own_image_cobre_declarada_vazia_e_ausente() {
        let t = ProjectToml::parse_str("[db]\nown = true\nimage = \"mysql:9.3\"\n").unwrap();
        assert_eq!(
            t.db.as_ref().unwrap().own_image(),
            Some("mysql:9.3"),
            "imagem declarada para o flux baixar sozinho"
        );
        let vazia = ProjectToml::parse_str("[db]\nown = true\nimage = \"  \"\n")
            .unwrap()
            .db
            .unwrap();
        assert_eq!(vazia.own_image(), None, "vazia = typo, cai no default");
        let ausente = ProjectToml::parse_str("[db]\nown = true\n")
            .unwrap()
            .db
            .unwrap();
        assert_eq!(ausente.own_image(), None, "ausente = default da plataforma");
    }

    /// O `flux.toml.example` da RAIZ do repo é o contrato user-facing do
    /// struct: descomentado, TEM de parsear sem erro (deny_unknown_fields
    /// pega chave a mais) e cobrir as chaves novas da 02. Template e struct
    /// divergirem = documentation mentindo.
    #[test]
    fn flux_toml_example_da_raiz_parseia_inteiro() {
        let caminho = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../flux.toml.example");
        let src = std::fs::read_to_string(&caminho)
            .unwrap_or_else(|e| panic!("lendo {}: {e}", caminho.display()));
        // Descomenta linha a linha: `#key = v` e `#[section]` viram código;
        // separadores e prosa ficam de fora (chave candidata TEM de ser uma
        // chave TOML bare antes do `=` — prosa com "=" não passa).
        let descomentado = src
            .lines()
            .filter_map(|l| {
                let resto = l.trim().strip_prefix('#')?.trim_start();
                let codigo = if resto.starts_with('[') {
                    true
                } else {
                    match resto.split_once('=') {
                        Some((chave, _)) => {
                            let chave = chave.trim();
                            !chave.is_empty()
                                && chave.chars().all(|c| {
                                    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '"')
                                })
                        }
                        None => false,
                    }
                };
                codigo.then(|| resto.to_owned())
            })
            .collect::<Vec<_>>()
            .join("\n");
        let t = ProjectToml::parse_str(&descomentado)
            .unwrap_or_else(|e| panic!("template da raiz divergiu do struct: {e}"));
        assert_eq!(t.name.as_deref(), Some("vippers"));
        assert_eq!(t.framework(), Framework::Laravel);
        let db = t.db.as_ref().expect("template documenta [db]");
        assert_eq!(
            db.silence_window_secs(),
            5,
            "[db] silence_window_secs na 02"
        );
        let build = t.build.as_ref().expect("template documenta [build]");
        assert_eq!(build.command.as_deref(), Some("npm run build"));
        assert_eq!(build.service(), "app");
        assert_eq!(
            t.build_silence_window_secs(),
            180,
            "[build] silence_window_secs na 02"
        );
        assert!(t.sync.as_ref().unwrap().extra_ignore.is_some());
        assert!(t.dev.as_ref().unwrap().live.is_some());
    }

    #[test]
    fn flux_toml_dev_live_parseia() {
        let src = "\n[dev]\nlive = \"npm run dev -- --host 0.0.0.0\"\n";
        let t = ProjectToml::parse_str(src).unwrap();
        assert_eq!(
            t.dev.as_ref().and_then(|d| d.live.as_deref()),
            Some("npm run dev -- --host 0.0.0.0"),
            "[dev] live presente deve parsear verbatim"
        );
    }

    #[test]
    fn flux_toml_sem_dev_fica_none() {
        let t = ProjectToml::parse_str("name = \"pedi\"\n").unwrap();
        assert_eq!(
            t.dev.and_then(|d| d.live),
            None,
            "sem seção [dev] não existe live"
        );
    }

    #[test]
    fn flux_toml_dev_chave_desconhecida_e_erro() {
        let err = ProjectToml::parse_str("[dev]\nlvie = \"npm run dev\"\n").unwrap_err();
        assert!(err.to_string().contains("lvie"), "mensagem: {err}");
    }

    #[test]
    fn config_global_parseia_com_secoes_das_fases_futuras() {
        let src = r#"
[vm]
switch = "FluxSwitch"
vm_ip = "192.168.140.2"
cpus = 8

[domain]
zone = "bitello.xyz"
dev_suffix = "-d"
panel_host = "flux.bitello.xyz"

[limits]
max_body_bytes = 104857600

[queue]
build_concurrency = 1

[shield]
shadow_days = 7
allowlist = ["192.168.140.1"]
brute_force = "10 falhas / 10 min -> 15 min de ban"

[status]
external_witness = "uptimerobot"

[tools.images]
node = "node:22"
"#;
        let c = GlobalConfig::parse_str(src).unwrap();
        assert_eq!(c.domain.prod_host("vippers"), "vippers.bitello.xyz");
        assert_eq!(c.domain.dev_host("vippers"), "vippers-d.bitello.xyz");
        assert_eq!(c.build_concurrency(), 1);
        assert_eq!(c.max_body_bytes(), 104_857_600);
        assert_eq!(
            c.tools.as_ref().unwrap().images.as_ref().unwrap()["node"],
            "node:22"
        );
    }

    #[test]
    fn config_global_vazia_tem_defaults_de_fabrica() {
        let c = GlobalConfig::parse_str("").unwrap();
        assert_eq!(c.idle_minutes(), 15);
        assert_eq!(c.domain.zone, "bitello.xyz");
        assert_eq!(c.domain.panel_host, "flux.bitello.xyz");
        assert!(
            !c.deploy_public(),
            "sem [deploy] a rota pública nasce FECHADA (default false)"
        );
    }

    #[test]
    fn deploy_public_route_somente_declarada_liga_a_rota() {
        let fechado = GlobalConfig::parse_str("[deploy]\npublic_route = false\n").unwrap();
        assert!(!fechado.deploy_public(), "explicit false fecha");
        let aberto = GlobalConfig::parse_str("[deploy]\npublic_route = true\n").unwrap();
        assert!(aberto.deploy_public(), "explicit true abre");
        // Chave desconhecida na seção continua ERRO (validação estrita, 06).
        let err = GlobalConfig::parse_str("[deploy]\npublic_rout = true\n").unwrap_err();
        assert!(err.to_string().contains("public_rout"), "erro: {err}");
    }

    #[test]
    fn guard_bloqueia_redis_por_decisao_da_pergunta_3() {
        let fallback = ImageGuardSection::default();
        let block = fallback.block_list();
        for img in [
            "mysql",
            "mariadb",
            "postgres",
            "redis",
            "valkey",
            "memcached",
        ] {
            assert!(
                block.iter().any(|b| b == img),
                "{img} deveria estar bloqueado"
            );
        }
    }
}
