//! Doctor (27/10): validação READ-ONLY da fábrica, num lugar só — o endpoint
//! `GET /doctor`, a tool MCP `flux_doctor` e o `flux doctor` (que acrescenta
//! as checagens que só existem no Windows: sessão mutagen local e resolução
//! dos hosts) falam com ESTE módulo. Cada item devolve verde/vermelho com a
//! causa; falha de leitura é item vermelho com o erro real, nunca pânico.
//!
//! Regra de ouro do doctor: ele NUNCA acorda nada. Sem `ensure_awake`, sem
//! `banco::acordar`, sem grant de lease — diagnosticar não pode virar efeito
//! colateral. "Dormindo" é design da plataforma, não defeito: vermelho aqui
//! significa estado INCOERENTE (app de pé sem banco, sync com problems,
//! container parcial, Laravel sem build) ou quebrado de verdade.

use serde::Serialize;
use serde_json::{Value, json};

use flux_common::protocol::Env;

use crate::AppState;
use crate::docker;
use crate::sync;

/// Uma checagem com veredito. `ok` = verde; `estado` é o rótulo curto
/// (o mesmo vocabulário do `flux ps`/painel) e `detalhe` carrega a causa —
/// o que a IA (ou o Gabriel) precisa para agir sem abrir outro comando.
#[derive(Debug, Clone, Serialize)]
pub struct Item {
    pub ok: bool,
    pub estado: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detalhe: Option<String>,
}

fn item(ok: bool, estado: impl Into<String>) -> Item {
    Item {
        ok,
        estado: estado.into(),
        detalhe: None,
    }
}

impl Item {
    fn com_detalhe(mut self, detalhe: impl Into<String>) -> Item {
        self.detalhe = Some(detalhe.into());
        self
    }
}

/// Veredito de UM projeto. `hosts` e `sono` são informativos (não entram no
/// ok agregado): resolução de DNS é verdade do Windows (checada pelo CLI) e
/// dormir é normal. Os hosts vão estruturados (`dev_host`/`prod_host`) — o
/// CLI consome sem parsear texto.
#[derive(Debug, Serialize)]
pub struct Veredito {
    pub projeto: String,
    pub dev_host: String,
    pub prod_host: String,
    pub containers: Item,
    pub sync: Item,
    pub banco: Item,
    pub assets: Item,
    pub hosts: Item,
    pub sono: Item,
}

impl Veredito {
    /// Verde agregado: só o que indica problema de verdade conta.
    pub fn ok(&self) -> bool {
        self.containers.ok && self.sync.ok && self.banco.ok && self.assets.ok
    }
}

/// Relatório completo: a saúde do próprio fluxd + um veredito por projeto.
#[derive(Debug, Serialize)]
pub struct Relatorio {
    pub fluxd: Item,
    pub projetos: Vec<Veredito>,
}

/// Veredito de TODOS os projetos registrados. Erro de listagem é `Err` com a
/// causa (o handler vira 500) — lista vazia por engano mentiria "fábrica sã".
pub async fn checar_todos(app: &AppState) -> Result<Relatorio, String> {
    let nomes = app
        .state
        .list_projects()
        .map_err(|e| format!("listando projetos: {e}"))?;
    let mut projetos = Vec::with_capacity(nomes.len());
    for nome in nomes {
        projetos.push(checar_projeto(app, &nome).await);
    }
    Ok(Relatorio {
        fluxd: item(true, format!("no ar (v{})", env!("CARGO_PKG_VERSION"))),
        projetos,
    })
}

/// Veredito de um projeto. Cada checagem é independente: uma que falhou em
/// LER não esconde as outras.
pub async fn checar_projeto(app: &AppState, name: &str) -> Veredito {
    let toml_src = app.state.project_toml(name).ok().flatten();
    let containers = checar_containers(app, name).await;
    // O banco só é vermelho quando parado com o app DE PÉ (incoerência);
    // dormindo junto com o projeto é o design ("dorme e acorda junto", 21/10).
    let acordado = matches!(containers.estado.as_str(), "ACORDADO" | "PARCIAL");
    let (dev_host, prod_host) = hosts_de(app, name);
    Veredito {
        projeto: name.to_owned(),
        dev_host,
        prod_host,
        containers,
        sync: checar_sync(app, name),
        banco: checar_banco(app, name, toml_src.as_deref(), acordado).await,
        assets: checar_assets(name, toml_src.as_deref()),
        hosts: item(true, "registrados"),
        sono: checar_sono(app, name),
    }
}

/// Containers DEV do projeto (`flux-{name}-dev`), agregados pela MESMA
/// semântica do `flux ps` (`ps_agrupar` é o dono da regra ACORDADO/PARCIAL/
/// DORMINDO — o doctor não replica agregação). PARCIAL = vermelho: services
/// do mesmo compose com vidas diferentes é o sintoma clássico de subida que
/// quebrou no meio.
async fn checar_containers(app: &AppState, name: &str) -> Item {
    let Some(d) = &app.docker else {
        return item(true, "desconhecido").com_detalhe("sem engine Docker — nada a checar aqui");
    };
    let projeto = docker::compose_project(name, Env::Dev);
    let lista = match docker::listar_label(d, Some(&projeto)).await {
        Ok(l) => l,
        Err(e) => return item(false, "erro").com_detalhe(e),
    };
    if lista.is_empty() {
        return item(true, "nunca-subiu").com_detalhe("nenhum container dev — rode `flux up`");
    }
    let linhas: Vec<Value> = lista
        .iter()
        .map(|c| {
            json!({
                "compose_project": projeto,
                "state": c.state.as_ref().map(|s| s.to_string()).unwrap_or_else(|| "unknown".into()),
            })
        })
        .collect();
    let agrupado = flux_common::mcp::ps_agrupar(&linhas, None);
    let estado = agrupado["projetos"][0]["estado"]
        .as_str()
        .unwrap_or("unknown")
        .to_owned();
    if estado == "PARCIAL" {
        let caidos: Vec<String> = lista
            .iter()
            .filter(|c| {
                !c.state.as_ref().is_some_and(|s| {
                    matches!(s, bollard::models::ContainerSummaryStateEnum::RUNNING)
                })
            })
            .filter_map(|c| {
                c.labels
                    .as_ref()
                    .and_then(|l| l.get("com.docker.compose.service").cloned())
            })
            .collect();
        return item(false, estado).com_detalhe(format!("parados: {}", caidos.join(", ")));
    }
    item(true, estado)
}

/// Sync pela visão do fluxd (relatos do `flux watch` no HOST). Mesma regra do
/// `badge_sync` do painel (panel/formato.rs): watching + sem problems + sem
/// erro = verde. SEM relato é vermelho: o watch é quem mantém a linha viva —
/// ausência dele significa sync parada de verdade (a sessão mutagen local só
/// o Windows vê; o CLI acrescenta a checagem do lado de lá).
fn checar_sync(app: &AppState, name: &str) -> Item {
    let Some(r) = sync::sync_states(app)
        .into_iter()
        .find(|r| r.project == name)
    else {
        return item(false, "sem-relato").com_detalhe(
            "nenhum relato do flux watch — sync não monitorada (watch rodando no Windows?)",
        );
    };
    if r.paused {
        return item(false, "pausada").com_detalhe("sync pausada — `flux sync resume`");
    }
    if let Some(e) = &r.last_error {
        return item(false, "erro").com_detalhe(format!("status {}: {e}", r.status));
    }
    if r.problems > 0 {
        return item(false, &r.status)
            .com_detalhe(format!("{} problema(s) pendente(s)", r.problems));
    }
    if r.rescan_requested {
        return item(false, "rescan pendente").com_detalhe("rescan pedido e não aplicado ainda");
    }
    if r.status.contains("watching") {
        item(true, "sincronizando")
    } else if r.status == "unknown" {
        item(false, "sem-relato").com_detalhe("linha existe mas nenhum relato chegou")
    } else {
        item(true, &r.status)
    }
}

/// Banco próprio: inspetura do container do grupo (caminho read-only do
/// `db_status`, api.rs) + coerência com o app. `toml_src` já lido pelo
/// chamador evita reler o TOML duas vezes.
async fn checar_banco(app: &AppState, name: &str, toml_src: Option<&str>, acordado: bool) -> Item {
    let Some(src) = toml_src else {
        return item(false, "sem-toml").com_detalhe("projeto sem flux.toml no state");
    };
    let cfg = match crate::banco::config_de(src, name) {
        Some(Ok(c)) => c,
        Some(Err(e)) => return item(false, "declaração quebrada").com_detalhe(e),
        None => return item(true, "n/a").com_detalhe("sem [db] own — usa o flux-db global"),
    };
    let container = crate::banco::container_name(&cfg.grupo);
    let Some(d) = &app.docker else {
        return item(true, "desconhecido").com_detalhe("sem engine Docker — nada a checar aqui");
    };
    let Ok(insp) = d.inspect_container(&container, None).await else {
        return item(false, "ausente")
            .com_detalhe(format!("{container} não existe — `flux up` cria"));
    };
    let st = insp.state.as_ref();
    let running = st.and_then(|s| s.running).unwrap_or(false);
    let paused = st.and_then(|s| s.paused).unwrap_or(false);
    let saude = st
        .and_then(|s| s.health.as_ref())
        .and_then(|h| h.status.as_ref())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "n/a".into());
    if paused {
        return item(false, "paused")
            .com_detalhe(format!("{container} pausado — unpause antes de usar"));
    }
    if running {
        if saude == "unhealthy" {
            return item(false, "running · unhealthy")
                .com_detalhe(format!("{container} vivo mas o healthcheck falha"));
        }
        return item(true, "running").com_detalhe(format!("health={saude}"));
    }
    if acordado {
        item(false, "parado").com_detalhe(format!(
            "app de pé mas {container} não roda — `flux up` religa o banco antes do app"
        ))
    } else {
        item(true, "parado").com_detalhe("dormindo junto com o projeto (design)")
    }
}

/// Assets do build de frontend (Laravel só, v1): `public/build/manifest.json`
/// presente e não-vazio. Leve de propósito — o doctor NÃO builda (isso é
/// `flux build`, com flock e janela de silêncio).
fn checar_assets(name: &str, toml_src: Option<&str>) -> Item {
    let framework = toml_src
        .and_then(|s| flux_common::config::ProjectToml::parse_str(s).ok())
        .map(|t| t.framework().to_string());
    let Some(fw) = framework else {
        return item(true, "desconhecido")
            .com_detalhe("flux.toml não parseia — registro manda a verdade");
    };
    if fw != "laravel" {
        return item(true, "n/a").com_detalhe(format!("checagem de assets é do laravel ({fw})"));
    }
    let manifest = std::path::Path::new(crate::paths::PROJECTS_ROOT_PADRAO)
        .join(name)
        .join("public/build/manifest.json");
    match std::fs::metadata(&manifest) {
        Err(_) => item(false, "sem-build")
            .com_detalhe("public/build/manifest.json ausente — rode `flux build`"),
        Ok(m) if m.len() == 0 => {
            item(false, "build-vazio").com_detalhe("manifest.json vazio — rode `flux build`")
        }
        Ok(_) => item(true, "buildado"),
    }
}

/// Hosts efetivos ([domains] override + derivação) — estruturados para o CLI
/// checar a resolução de DNS no lado Windows.
fn hosts_de(app: &AppState, name: &str) -> (String, String) {
    let (prod, dev) = app.state.project_hosts(name).unwrap_or((None, None));
    (
        dev.unwrap_or_else(|| app.domain.dev_host(name)),
        prod.unwrap_or_else(|| app.domain.prod_host(name)),
    )
}

/// Sono do dev — informativo (dormir é normal; o doctor NUNCA acorda nada).
fn checar_sono(app: &AppState, name: &str) -> Item {
    match app.state.sleep_row(name, Env::Dev) {
        Err(e) => item(true, "desconhecido").com_detalhe(format!("lendo sono: {e}")),
        Ok((_last, lease, _cooldown, mode)) => {
            let agora = crate::sleep::now_epoch();
            let estado = if lease.is_some_and(|l| l > agora) {
                "acordado (lease ativo)".to_owned()
            } else {
                mode.unwrap_or_else(|| "sem sinal".into())
            };
            item(true, estado).com_detalhe("informativo — acordar é trabalho do request/lease/up")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::State as DbState;
    use flux_common::config::GlobalConfig;

    const TOML_LARAVEL: &str = "name = \"vipa\"\nframework = \"laravel\"\n";
    const TOML_OWN: &str = "name = \"vipa\"\nframework = \"laravel\"\n[db]\nown = true\n";

    fn app() -> AppState {
        AppState::new(
            DbState::in_memory().unwrap(),
            &GlobalConfig::default(),
            None, // sem engine: vereditos "desconhecido" honestos
        )
    }

    #[test]
    fn item_sem_detalhe_nao_serializa_campo() {
        let i = item(true, "running");
        let v = serde_json::to_value(&i).unwrap();
        assert_eq!(v["ok"], json!(true));
        assert!(v.get("detalhe").is_none(), "detalhe ausente não vira null");
        let c = i.com_detalhe("health=healthy");
        assert_eq!(
            serde_json::to_value(&c).unwrap()["detalhe"],
            json!("health=healthy")
        );
    }

    #[tokio::test]
    async fn veredito_sem_engine_e_honesto_por_item() {
        let app = app();
        app.state
            .register_project("vipa", TOML_LARAVEL, "hash")
            .unwrap();
        let v = checar_projeto(&app, "vipa").await;
        // Containers: sem engine é "desconhecido" e NÃO é vermelho (falso
        // alarme em máquina de dev esconderia o sinal real).
        assert!(v.containers.ok);
        assert_eq!(v.containers.estado, "desconhecido");
        // Sync sem relato do watch é vermelho de verdade.
        assert!(!v.sync.ok);
        assert_eq!(v.sync.estado, "sem-relato");
        // Laravel sem [db] own → banco n/a (verde).
        assert!(v.banco.ok);
        assert_eq!(v.banco.estado, "n/a");
        // /srv/flux/projects não existe no Windows de teste → sem-build.
        assert!(!v.assets.ok);
        assert_eq!(v.assets.estado, "sem-build");
        // Informativos não falham.
        assert!(v.hosts.ok && v.sono.ok);
        assert_eq!(v.dev_host, "vipa-d.bitello.xyz", "host estruturado pro CLI");
        assert_eq!(v.prod_host, "vipa.bitello.xyz");
        // Agregado: sync e assets puxam o vermelho.
        assert!(!v.ok());
    }

    #[tokio::test]
    async fn sync_relato_limpo_e_verde_e_problema_e_vermelho() {
        let app = app();
        app.state
            .register_project("vipa", TOML_LARAVEL, "hash")
            .unwrap();
        sync::upsert_report(
            &app,
            &sync::SyncReport {
                project: "vipa".into(),
                session: "flux-vipa".into(),
                status: "watching".into(),
                paused: false,
                files: Some(10),
                problems: 0,
                last_error: None,
                rescan_applied: false,
            },
        )
        .unwrap();
        assert!(checar_sync(&app, "vipa").ok);

        sync::upsert_report(
            &app,
            &sync::SyncReport {
                project: "vipa".into(),
                session: "flux-vipa".into(),
                status: "watching".into(),
                paused: false,
                files: Some(11),
                problems: 2,
                last_error: Some("boom".into()),
                rescan_applied: false,
            },
        )
        .unwrap();
        let ruim = checar_sync(&app, "vipa");
        assert!(!ruim.ok);
        assert!(ruim.detalhe.unwrap_or_default().contains("boom"));
    }

    #[tokio::test]
    async fn banco_own_sem_engine_e_desconhecido_nao_vermelho() {
        let app = app();
        app.state
            .register_project("vipa", TOML_OWN, "hash")
            .unwrap();
        let src = app.state.project_toml("vipa").unwrap().unwrap();
        let item = checar_banco(&app, "vipa", Some(&src), false).await;
        assert!(item.ok, "sem engine não é defeito: {:?}", item);
        assert_eq!(item.estado, "desconhecido");
    }

    #[tokio::test]
    async fn banco_declaracao_quebrada_e_vermelho_com_causa() {
        let app = app();
        app.state
            .register_project("vipa", TOML_OWN, "hash")
            .unwrap();
        let item = checar_banco(
            &app,
            "vipa",
            Some("[db]\nown = true\ngrupo = \"GRUPO_RUIM!\"\n"),
            false,
        )
        .await;
        assert!(!item.ok);
        assert_eq!(item.estado, "declaração quebrada");
        assert!(item.detalhe.unwrap_or_default().contains("GRUPO_RUIM"));
    }

    #[tokio::test]
    async fn checar_todos_lista_todos_os_registrados() {
        let app = app();
        app.state
            .register_project("vipa", TOML_LARAVEL, "h1")
            .unwrap();
        app.state
            .register_project("djm", TOML_LARAVEL, "h2")
            .unwrap();
        let r = checar_todos(&app).await.unwrap();
        assert!(r.fluxd.ok);
        let mut nomes: Vec<&str> = r.projetos.iter().map(|v| v.projeto.as_str()).collect();
        nomes.sort();
        assert_eq!(nomes, vec!["djm", "vipa"]);
        assert!(
            r.projetos.iter().all(|v| !v.ok()),
            "sem watch e sem build: ninguém verde"
        );
    }
}
