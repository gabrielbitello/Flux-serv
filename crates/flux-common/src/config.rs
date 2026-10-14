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
    pub image_guard: Option<bool>,
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
    pub image_guard: Option<ImageGuardSection>,
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
    }

    #[test]
    fn flux_toml_vazio_usa_defaults() {
        let t = ProjectToml::parse_str("").unwrap();
        assert_eq!(t.effective_name("pedi"), "pedi");
        assert_eq!(t.health_path(), "/up");
        assert_eq!(t.framework(), Framework::Laravel);
    }

    #[test]
    fn flux_toml_chave_desconhecida_e_erro() {
        let err = ProjectToml::parse_str("naame = \"vipers\"").unwrap_err();
        assert!(err.to_string().contains("naame"), "mensagem: {err}");
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
        let c = GlobalConfig::parse_str("").unwrap();
        let fallback = ImageGuardSection::default();
        let block = c.image_guard.as_ref().unwrap_or(&fallback).block_list();
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
