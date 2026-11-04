//! Leitura/consulta do painel (L6.3): TODA SQL da interface vive aqui —
//! sem desenho de HTML (apresentação é paginas.rs).

use flux_common::config::ProjectToml;
use flux_common::protocol::{ContainerState, Env};
use rusqlite::OptionalExtension;
use rusqlite::params;
use rusqlite::types::Value;

use super::formato;
use crate::AppState;
use crate::decision::HostSource as _;
use crate::docker;
use crate::sqlite_source;
use crate::state::SleepRow;

pub(super) const EVENTOS_DASHBOARD: i64 = 15;
pub(super) const EVENTOS_POR_PAGINA: i64 = 50;
/// Teto da janela da curva (buckets de 1 min) — o cliente pede menos
/// (`?minutos=15|30|60|360|720|1440`, realtime = 10); teto = 24 h.
pub(super) const CURVA_TETO_MINUTOS: usize = 24 * 60;
/// Card "últimos 15 min": quantos warns a lista mostra (o total vai à parte).
pub(super) const ULTIMOS15_LIMITE: i64 = 10;
pub(super) const ULTIMOS15_JANELA: &str = "-15 minutes";

// ------------------------------------------------------------------ leitura

/// Verdade do estado de um ambiente, com a FONTE declarada para o leitor:
/// Docker quando existe (07 — estado interno nunca é verdade absoluta),
/// senão a linha gravada pelo sweep do sleep manager (04, ciclo de 60 s).
pub(super) fn verdade_ambiente(
    app: &AppState,
    name: &str,
    env: Env,
) -> (ContainerState, &'static str) {
    match &app.docker {
        Some(d) => (
            docker::DockerSource::new(d.clone(), &app.state).env_state(name, env),
            "docker (verdade)",
        ),
        None => (
            sqlite_source::SqliteSource::new(&app.state).env_state(name, env),
            "estado (sweep 60 s)",
        ),
    }
}

/// Um ambiente de um projeto pronto para renderizar. Erro de leitura vira
/// `Err(texto)` — o card mostra a mensagem real, nunca silêncio.
pub(super) struct PainelEnv {
    pub(super) estado: ContainerState,
    pub(super) fonte: &'static str,
    pub(super) row: Result<SleepRow, String>,
    pub(super) aplicadas: Vec<(String, String)>,
}

pub(super) fn ambiente(app: &AppState, name: &str, env: Env) -> PainelEnv {
    let (estado, fonte) = verdade_ambiente(app, name, env);
    let _ = env; // rotulado no JSON pelo chamador (detalhe_env/dashboard_env)
    PainelEnv {
        estado,
        fonte,
        row: app
            .state
            .sleep_row(name, env)
            .map_err(|e| format!("falha lendo sleep_state: {e}")),
        aplicadas: app.state.migration_rows(name, env).unwrap_or_else(|e| {
            tracing::warn!("painel: migrations de {name}/{env} ilegíveis: {e}");
            Vec::new()
        }),
    }
}

/// `(name, flux.toml bruto)` de todos os projetos registrados.
pub(super) fn projetos(app: &AppState) -> Result<Vec<(String, String)>, String> {
    app.state
        .with(|c| {
            let mut stmt = c.prepare("SELECT name, toml FROM projects ORDER BY name")?;
            let rows =
                stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
            rows.collect()
        })
        .map_err(|e| e.to_string())
}

/// Serializável: o snapshot do dashboard carrega eventos direto (a página
/// HTML antiga escapava à mão; o JSON é a via nova). Debug pros testes.
/// `ip`+`host` (03/11): o painel marca "é meu" (ip = origem do navegador) e
/// "veio da infra" (ip do host/VM) sem parsear a mensagem.
#[derive(serde::Serialize, std::fmt::Debug)]
pub(super) struct EventoRow {
    pub(super) id: i64,
    pub(super) level: String,
    pub(super) scope: String,
    pub(super) message: String,
    pub(super) created_at: String,
    pub(super) ip: String,
    pub(super) host: String,
    /// Emissor (schema v8): projeto/env dos access do router; vazio nos
    /// internos e nas linhas antigas (pré-v8).
    pub(super) projeto: String,
    pub(super) env: String,
    /// Código HTTP de resposta (v8) — access e waf; `None` nos internos.
    pub(super) st: Option<i64>,
}

/// O `id` é a chave estável do evento: a API pública e o append do /events
/// novo usam pra preservar posição/dedupe entre fetches.
pub(super) const COLUNAS_EVENTO: &str =
    "id, level, scope, message, created_at, ip, host, projeto, env, st";

pub(super) fn row_evento(r: &rusqlite::Row<'_>) -> rusqlite::Result<EventoRow> {
    Ok(EventoRow {
        id: r.get(0)?,
        level: r.get(1)?,
        scope: r.get(2)?,
        message: r.get(3)?,
        created_at: r.get(4)?,
        ip: r.get(5)?,
        host: r.get(6)?,
        projeto: r.get(7)?,
        env: r.get(8)?,
        st: r.get(9)?,
    })
}

/// Últimos N eventos, mais novo primeiro (dashboard).
pub(super) fn eventos_recentes(app: &AppState, n: i64) -> Result<Vec<EventoRow>, String> {
    app.state
        .with(|c| {
            let mut stmt = c.prepare(&format!(
                "SELECT {COLUNAS_EVENTO} FROM events ORDER BY id DESC LIMIT ?1"
            ))?;
            let rows = stmt.query_map(params![n], row_evento)?;
            rows.collect()
        })
        .map_err(|e| e.to_string())
}

/// Últimas anomalias do vigia (mais novas primeiro) — card do dashboard.
pub(super) fn anomalias_recentes(app: &AppState, n: i64) -> Result<Vec<EventoRow>, String> {
    app.state
        .with(|c| {
            let mut stmt = c.prepare(&format!(
                "SELECT {COLUNAS_EVENTO} FROM events WHERE scope='anomalia'                  ORDER BY id DESC LIMIT ?1"
            ))?;
            let rows = stmt.query_map(params![n], row_evento)?;
            rows.collect()
        })
        .map_err(|e| e.to_string())
}

/// Época do último ciclo do vigia — a prova de que o detector está VIVO.
pub(super) fn vigia_ultimo_ciclo(app: &AppState) -> Result<Option<String>, String> {
    app.state
        .with(|c| {
            c.query_row(
                "SELECT v FROM settings WHERE k = 'vigia_ultimo_ciclo'",
                [],
                |r| r.get(0),
            )
            .optional()
        })
        .map_err(|e| e.to_string())
}

/// Época atual em segundos, pela MESMA fonte dos timestamps gravados
/// (`datetime('now')` é UTC no SQLite) — relógio único, sem drift de host.
pub(super) fn agora_epoch(app: &AppState) -> Result<i64, String> {
    app.state
        .with(|c| {
            c.query_row("SELECT CAST(strftime('%s','now') AS INTEGER)", [], |r| {
                r.get(0)
            })
        })
        .map_err(|e| e.to_string())
}

/// Curva de eventos/min na janela pedida (`minutos`, 1..=1440): buckets de
/// 1 min, `None` = sem dado nenhum (a frase "sem dados", não gráfico vazio).
pub(super) fn curva_contagem(
    app: &AppState,
    minutos: usize,
) -> Result<Option<(Vec<u64>, u64, u64)>, String> {
    let agora = agora_epoch(app)?;
    let janela = minutos.clamp(1, CURVA_TETO_MINUTOS);
    let buckets: Vec<(i64, i64)> = app
        .state
        .with(|c| {
            let mut stmt = c.prepare(
                "SELECT CAST(strftime('%s', created_at) AS INTEGER) / 60 AS minuto, COUNT(*)
                 FROM events
                 WHERE created_at >= datetime('now', ?1)
                 GROUP BY minuto",
            )?;
            let rows = stmt.query_map(params![format!("-{janela} minutes")], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
            })?;
            rows.collect()
        })
        .map_err(|e| e.to_string())?;

    // Um bucket por minuto. Timestamps no futuro (relógio adiantado) não
    // entram — dado inventado não.
    let minuto_atual = agora / 60;
    let mut conts = vec![0u64; janela];
    for (minuto, n) in buckets {
        if minuto > minuto_atual {
            continue;
        }
        let base = minuto_atual - (janela as i64 - 1);
        if let Ok(idx) = usize::try_from(minuto - base)
            && idx < janela
        {
            conts[idx] += n as u64;
        }
    }
    let total: u64 = conts.iter().sum();
    if total == 0 {
        return Ok(None);
    }
    let max = *conts.iter().max().unwrap_or(&1);
    Ok(Some((conts, total, max)))
}

/// Card "últimos 15 min" (03/11): o que é IMPORTANTE agora — nível warn da
/// janela. Lista (mais novo primeiro, teto curto de DOM) + total real.
pub(super) fn ultimos15(app: &AppState) -> Result<(Vec<EventoRow>, i64), String> {
    let total: i64 = app
        .state
        .with(|c| {
            c.query_row(
                &format!(
                    "SELECT COUNT(*) FROM events WHERE level='warn' \
                     AND created_at >= datetime('now', '{ULTIMOS15_JANELA}')"
                ),
                [],
                |r| r.get(0),
            )
        })
        .map_err(|e| e.to_string())?;
    let rows = app
        .state
        .with(|c| {
            let mut stmt = c.prepare(&format!(
                "SELECT {COLUNAS_EVENTO} FROM events WHERE level='warn' \
                 AND created_at >= datetime('now', '{ULTIMOS15_JANELA}') \
                 ORDER BY id DESC LIMIT ?1"
            ))?;
            let rows = stmt.query_map(params![ULTIMOS15_LIMITE], row_evento)?;
            rows.collect()
        })
        .map_err(|e| e.to_string())?;
    Ok((rows, total))
}

// --------------------------------------------------- eventos com filtro 26/10

/// Filtros da página de eventos. `None` = sem filtro (a SQL não ganha cláusula).
#[derive(Default, Clone, PartialEq)]
pub(super) struct FiltroEventos {
    pub(super) scope: Option<String>,
    pub(super) nivel: Option<String>,
    pub(super) busca: Option<String>,
    /// Host da request (access, schema v7) — "projeto de origem".
    pub(super) host: Option<String>,
    /// "dev" | "prod": derivado do host (`-d.`/`vite-` = dev; vazio = sem
    /// classificação e FORA dos dois filtros).
    pub(super) ambiente: Option<String>,
    /// Nome do projeto emissor (schema v8, 04/11): casa pela COLUNA nos
    /// access novos e, pra linha antiga/interna (coluna vazia), pelo nome
    /// citado na mensagem — o filtro nunca encolhe o histórico.
    pub(super) projeto: Option<String>,
}

/// Parse da query string (`?escopo=access&nivel=warn&q=...`) com valores
/// url-encoded — a busca do operador vem com %XX e `+` do browser.
pub(super) fn filtro_de(query: Option<&str>) -> FiltroEventos {
    let mut f = FiltroEventos::default();
    let Some(q) = query else {
        return f;
    };
    for par in q.split('&') {
        let Some((k, v)) = par.split_once('=') else {
            continue;
        };
        let v = formato::query_decode(v);
        let v = (!v.trim().is_empty()).then_some(v);
        match k {
            "escopo" => f.scope = v,
            "host" => f.host = v,
            "ambiente" => f.ambiente = v,
            "nivel" => f.nivel = v,
            "projeto" => f.projeto = v,
            "q" => f.busca = v,
            _ => {}
        }
    }
    f
}

/// Cláusula WHERE + bindings dos filtros — UMA definição usada pelo COUNT e
/// pelo SELECT (contagem e página nunca divergem).
fn clausula_filtro(f: &FiltroEventos) -> (String, Vec<Value>) {
    let mut onde = String::from("1=1");
    let mut vals: Vec<Value> = Vec::new();
    if let Some(s) = &f.scope {
        vals.push(Value::from(s.clone()));
        onde.push_str(&format!(" AND scope = ?{}", vals.len()));
    }
    if let Some(n) = &f.nivel {
        vals.push(Value::from(n.clone()));
        onde.push_str(&format!(" AND level = ?{}", vals.len()));
    }
    if let Some(q) = &f.busca {
        vals.push(Value::from(format!("%{}%", formato::like_esc(q))));
        onde.push_str(&format!(" AND message LIKE ?{} ESCAPE '\\'", vals.len()));
    }
    if let Some(h) = &f.host {
        vals.push(Value::from(h.clone()));
        onde.push_str(&format!(" AND host = ?{}", vals.len()));
    }
    if let Some(a) = &f.ambiente {
        // dev = host de dev (`-d.` custom/derivado ou `vite-*`); prod = com
        // host e não-dev; vazio = interno (sai dos dois). Ambiente inválido
        // = filtro que NUNCA casa (nunca filtro mentindo).
        let cond = match a.as_str() {
            "dev" => "(host LIKE '%-d.%' OR host LIKE 'vite-%')",
            "prod" => "(host != '' AND host NOT LIKE '%-d.%' AND host NOT LIKE 'vite-%')",
            _ => return (String::from("1=0"), Vec::new()),
        };
        onde.push_str(&format!(" AND {cond}"));
    }
    if let Some(p) = &f.projeto {
        // Coluna primeiro (access v8+); linha antiga/interna (coluna vazia)
        // casa pelo nome citado na mensagem — o filtro cobre o histórico.
        vals.push(Value::from(p.clone()));
        let n = vals.len();
        vals.push(Value::from(format!("%{}%", formato::like_esc(p))));
        onde.push_str(&format!(
            " AND (projeto = ?{n} OR (projeto = '' AND message LIKE ?{} ESCAPE '\\'))",
            n + 1
        ));
    }
    (onde, vals)
}

/// Hosts distintos COM classificação (dev/prod) — os chips de projeto de
/// origem nunca inventam opção. Vazio (internos) sai da lista.
pub(super) fn hosts_distintos(app: &AppState) -> Result<Vec<(String, String)>, String> {
    app.state
        .with(|c| {
            let mut stmt =
                c.prepare("SELECT DISTINCT host FROM events WHERE host != '' ORDER BY host")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            let hosts: Vec<String> = rows.collect::<rusqlite::Result<_>>()?;
            Ok(hosts
                .into_iter()
                .map(|h| {
                    let ambiente = if h.contains("-d.") || h.starts_with("vite-") {
                        "dev"
                    } else {
                        "prod"
                    };
                    (h, ambiente.to_owned())
                })
                .collect())
        })
        .map_err(|e| e.to_string())
}

/// Página de eventos COM filtro — a página é clampada ao total filtrado
/// (mesma promessa da paginação antiga: pedir página 99 não devolve vazia
/// desconexa). Sem filtro (`FiltroEventos::default()`) é o caso geral.
pub(super) fn eventos_filtrados(
    app: &AppState,
    page: i64,
    size: i64,
    f: &FiltroEventos,
) -> Result<(i64, Vec<EventoRow>, i64), String> {
    let (onde, vals) = clausula_filtro(f);
    let total: i64 = app
        .state
        .with(|c| {
            c.query_row(
                &format!("SELECT COUNT(*) FROM events WHERE {onde}"),
                rusqlite::params_from_iter(vals.iter()),
                |r| r.get(0),
            )
        })
        .map_err(|e| e.to_string())?;
    let paginas = ((total + size - 1) / size).max(1);
    let page = page.min(paginas);
    let offset = (page - 1) * size;
    let mut binds = vals.clone();
    binds.push(Value::from(size));
    binds.push(Value::from(offset));
    let rows = app
        .state
        .with(|c| {
            let mut stmt = c.prepare(&format!(
                "SELECT {COLUNAS_EVENTO} FROM events WHERE {onde} \
                 ORDER BY id DESC LIMIT ?{} OFFSET ?{}",
                binds.len() - 1,
                binds.len()
            ))?;
            let rows = stmt.query_map(rusqlite::params_from_iter(binds.iter()), row_evento)?;
            rows.collect()
        })
        .map_err(|e| e.to_string())?;
    Ok((page, rows, total))
}

/// Eventos com id > `desde` (backlog do SSE `/api/events/follow`), MESMO
/// filtro, ordem CRONOLÓGICA — o stream corre ASC; a página é DESC porque
/// mostra o recente primeiro.
pub(super) fn eventos_desde(
    app: &AppState,
    desde: i64,
    f: &FiltroEventos,
) -> Result<Vec<EventoRow>, String> {
    let (onde, vals) = clausula_filtro(f);
    let mut binds = vals;
    binds.push(Value::from(desde));
    app.state
        .with(|c| {
            let mut stmt = c.prepare(&format!(
                "SELECT {COLUNAS_EVENTO} FROM events WHERE {onde} AND id > ?{} ORDER BY id ASC",
                binds.len()
            ))?;
            let rows = stmt.query_map(rusqlite::params_from_iter(binds.iter()), row_evento)?;
            rows.collect()
        })
        .map_err(|e| e.to_string())
}

/// Escopos existentes na trilha — os chips do filtro nunca inventam opção.
pub(super) fn escopos_distintos(app: &AppState) -> Result<Vec<String>, String> {
    app.state
        .with(|c| {
            let mut stmt = c.prepare("SELECT DISTINCT scope FROM events ORDER BY scope")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            rows.collect()
        })
        .map_err(|e| e.to_string())
}

// ------------------------------------------------------ dashboard JSON (fatia 5)

/// Projecção JSON do dashboard — MESMAS consultas do render HTML antigo
/// (projetos + dev/prod + sync + sono + últimos eventos + vigia + curva),
/// sem nada inventado. Saúde NÃO entra aqui: doctor é pesado (DNS/docker
/// por projeto) e mora em /api/saude, puxado pelo cliente a cada minuto.
#[derive(serde::Serialize)]
pub(super) struct Snapshot {
    pub(super) projetos: Vec<DashboardProjeto>,
    pub(super) eventos: Vec<EventoRow>,
    pub(super) vigia_marca: Option<String>,
    pub(super) anomalias: Vec<EventoRow>,
    /// IP real do cliente da request (mesma régua `waf::ip_real` do access):
    /// o painel marca os eventos "é você" (toggle ocultar meus eventos).
    pub(super) meu_ip: String,
    /// Card "últimos 15 min": warns da janela (lista curta + total real).
    pub(super) ultimos15: Vec<EventoRow>,
    pub(super) ultimos15_total: i64,
    /// Curva de eventos/min na janela pedida (`?minutos=`, default 24 h) — o
    /// cliente desenha; `None` = sem dado ainda (a frase exata, sem gráfico
    /// mentindo vida).
    pub(super) curva: Option<CurvaJson>,
}

#[derive(serde::Serialize)]
pub(super) struct CurvaJson {
    pub(super) conts: Vec<u64>,
    pub(super) total: u64,
    pub(super) max: u64,
}

#[derive(serde::Serialize)]
pub(super) struct DashboardProjeto {
    pub(super) nome: String,
    pub(super) framework: String,
    pub(super) framework_erro: Option<String>,
    pub(super) dev_host: String,
    pub(super) prod_host: String,
    pub(super) dev: DashboardEnv,
    pub(super) prod: DashboardEnv,
    pub(super) sync: Option<SyncJson>,
}

/// Um ambiente de projeto em JSON. `estado` é o nome do
/// `ContainerState` em minúsculas — o cliente espelha formato::dot (contrato
/// referenciado; `fonte` é a verdade declarada: "docker" ou "sqlite").
#[derive(serde::Serialize)]
pub(super) struct DashboardEnv {
    pub(super) env: &'static str,
    pub(super) estado: String,
    pub(super) fonte: &'static str,
    pub(super) aplicadas: usize,
    /// Falha lendo sleep_state — a mensagem REAL vai pro card.
    pub(super) sono_erro: Option<String>,
    pub(super) interacao: Option<i64>,
    pub(super) lease: Option<i64>,
    pub(super) cooldown: Option<i64>,
}

#[derive(serde::Serialize)]
pub(super) struct SyncJson {
    pub(super) status: String,
    pub(super) paused: bool,
    /// Flag do request_rescan — o card antigo mostrava "rescan pendente".
    pub(super) rescan_requested: bool,
    pub(super) files: Option<u64>,
    pub(super) problems: u64,
    pub(super) last_error: Option<String>,
    pub(super) atualizado: String,
}

/// Snapshot completo do dashboard. INFALÍVEL por design: falha de leitura
/// vira campo de erro/menos dado — nunca 500 sem contexto (o card mostra a
/// causa; a promessa do painel é "o que não sei, digo que não sei").
pub(super) fn dashboard(app: &AppState, minutos: usize, meu_ip: String) -> Snapshot {
    let mut cards = Vec::new();
    if let Ok(lista) = projetos(app) {
        let syncs = crate::sync::sync_states(app);
        for (name, toml_src) in lista {
            let (framework, framework_erro) = match ProjectToml::parse_str(&toml_src) {
                Ok(t) => (t.framework().to_string(), None),
                Err(e) => ("?".to_owned(), Some(e.to_string())),
            };
            let (ov_prod, ov_dev) = app.state.project_hosts(&name).unwrap_or((None, None));
            cards.push(DashboardProjeto {
                dev_host: ov_dev.unwrap_or_else(|| app.domain.dev_host(&name)),
                prod_host: ov_prod.unwrap_or_else(|| app.domain.prod_host(&name)),
                framework,
                framework_erro,
                dev: dashboard_env(app, &name, Env::Dev),
                prod: dashboard_env(app, &name, Env::Prod),
                sync: syncs.iter().find(|r| r.project == name).map(|r| SyncJson {
                    status: r.status.clone(),
                    paused: r.paused,
                    rescan_requested: r.rescan_requested,
                    files: r.files,
                    problems: r.problems,
                    last_error: r.last_error.clone(),
                    atualizado: r.updated_at.clone(),
                }),
                nome: name,
            });
        }
    }
    let (ultimos15, ultimos15_total) = ultimos15(app).unwrap_or_default();
    Snapshot {
        eventos: eventos_recentes(app, EVENTOS_DASHBOARD).unwrap_or_default(),
        vigia_marca: vigia_ultimo_ciclo(app).unwrap_or(None),
        anomalias: anomalias_recentes(app, 5).unwrap_or_default(),
        meu_ip,
        ultimos15,
        ultimos15_total,
        curva: curva_contagem(app, minutos)
            .unwrap_or(None)
            .map(|(conts, total, max)| CurvaJson { conts, total, max }),
        projetos: cards,
    }
}

fn dashboard_env(app: &AppState, name: &str, env: Env) -> DashboardEnv {
    let p = ambiente(app, name, env);
    let (interacao, lease, cooldown, sono_erro) = match &p.row {
        Ok(r) => (r.0, r.1, r.2, None),
        Err(e) => (None, None, None, Some(e.clone())),
    };
    DashboardEnv {
        env: match env {
            Env::Dev => "dev",
            Env::Prod => "prod",
        },
        estado: format!("{:?}", p.estado).to_lowercase(),
        fonte: p.fonte,
        aplicadas: p.aplicadas.len(),
        sono_erro,
        interacao,
        lease,
        cooldown,
    }
}

// ------------------------------------------------ detalhe JSON (fatia 6a)

/// Snapshot do detalhe de UM projeto — MESMAS consultas do render HTML
/// antigo (ambientes com a LISTA de migrations, sync, hosts) + eventos que
/// mencionam o nome. `None` do outer = projeto não registrado (o handler
/// devolve 404 honesto); erros internos viram campos.
#[derive(serde::Serialize)]
pub(super) struct DetalheSnapshot {
    pub(super) nome: String,
    pub(super) framework: String,
    pub(super) framework_erro: Option<String>,
    pub(super) dev_host: String,
    pub(super) prod_host: String,
    pub(super) dev: DetalheEnv,
    pub(super) prod: DetalheEnv,
    pub(super) sync: Option<SyncJson>,
    /// Eventos cuja mensagem menciona o nome (LIKE, 20 mais novos).
    pub(super) eventos: Vec<EventoRow>,
    /// Status REAL do live dev (decisão do escopo: toggle com status
    /// verdade) — `None` = sem como medir (sem engine/IP).
    pub(super) live: Option<LiveJson>,
}

#[derive(serde::Serialize)]
pub(super) struct LiveJson {
    pub(super) hot: bool,
    pub(super) porta: bool,
}

#[derive(serde::Serialize)]
pub(super) struct DetalheEnv {
    pub(super) env: &'static str,
    pub(super) estado: String,
    pub(super) fonte: &'static str,
    /// Lista COMPLETA (nome, hash) — o detalhe mostra, o dashboard conta.
    pub(super) migracoes: Vec<[String; 2]>,
    pub(super) sono_erro: Option<String>,
    pub(super) interacao: Option<i64>,
    pub(super) lease: Option<i64>,
    pub(super) cooldown: Option<i64>,
}

/// `Ok(Some)` = registrado; `Ok(None)` = não registrado (404 honesto);
/// `Err` = estado ilegível (503 com a causa).
pub(super) fn detalhe(
    app: &AppState,
    name: &str,
    live: Option<(bool, bool)>,
) -> Result<Option<DetalheSnapshot>, String> {
    let lista = projetos(app)?;
    let Some((_, toml_src)) = lista.iter().find(|(n, _)| n == name) else {
        return Ok(None);
    };
    let (framework, framework_erro) = match ProjectToml::parse_str(toml_src) {
        Ok(t) => (t.framework().to_string(), None),
        Err(e) => ("?".to_owned(), Some(e.to_string())),
    };
    let (ov_prod, ov_dev) = app.state.project_hosts(name).unwrap_or((None, None));
    let syncs = crate::sync::sync_states(app);

    // Eventos do projeto (04/11): a COLUNA `projeto` (v8) é a fonte — os
    // access do router já nascem com ela; linha antiga/interna (coluna
    // vazia) casa pelo nome citado na mensagem, mesmo padrão escapado de
    // antes. ESCAPE '\\' em Rust = `'` do SQL espera a BARRA (mesmo
    // contrato do clausula_filtro). O `'\'` que estava aqui era aspa
    // ESCAPADA no Rust — o SQL saía com aspa aberta, o prepare falhava e o
    // unwrap_or_default engolia: a lista de eventos do detalhe voltava
    // vazia SEMPRE (achado 03/11, teste cobre).
    let padrao = formato::like_esc(name);
    let eventos = app
        .state
        .with(|c| {
            let mut stmt = c.prepare(&format!(
                "SELECT {COLUNAS_EVENTO} FROM events \
                 WHERE projeto = ?1 OR (projeto = '' AND message LIKE '%' || ?2 || '%' ESCAPE '\\') \
                 ORDER BY id DESC LIMIT 20"
            ))?;
            let rows = stmt.query_map(params![name, format!("%{padrao}%")], row_evento)?;
            rows.collect()
        })
        .unwrap_or_else(|e| {
            tracing::error!("painel: eventos do detalhe de {name} ilegíveis: {e}");
            Vec::new()
        });

    Ok(Some(DetalheSnapshot {
        nome: name.to_owned(),
        framework,
        framework_erro,
        dev_host: ov_dev.unwrap_or_else(|| app.domain.dev_host(name)),
        prod_host: ov_prod.unwrap_or_else(|| app.domain.prod_host(name)),
        dev: detalhe_env(app, name, Env::Dev),
        prod: detalhe_env(app, name, Env::Prod),
        sync: syncs.iter().find(|r| r.project == name).map(|r| SyncJson {
            status: r.status.clone(),
            paused: r.paused,
            rescan_requested: r.rescan_requested,
            files: r.files,
            problems: r.problems,
            last_error: r.last_error.clone(),
            atualizado: r.updated_at.clone(),
        }),
        eventos,
        live: live.map(|(hot, porta)| LiveJson { hot, porta }),
    }))
}

fn detalhe_env(app: &AppState, name: &str, env: Env) -> DetalheEnv {
    let p = ambiente(app, name, env);
    let (interacao, lease, cooldown, sono_erro) = match &p.row {
        Ok(r) => (r.0, r.1, r.2, None),
        Err(e) => (None, None, None, Some(e.clone())),
    };
    DetalheEnv {
        env: match env {
            Env::Dev => "dev",
            Env::Prod => "prod",
        },
        estado: format!("{:?}", p.estado).to_lowercase(),
        fonte: p.fonte,
        migracoes: p
            .aplicadas
            .into_iter()
            .map(|(nome, hash)| [nome, hash])
            .collect(),
        sono_erro,
        interacao,
        lease,
        cooldown,
    }
}
