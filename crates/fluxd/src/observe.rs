//! Observabilidade de recursos (spec 13 + pedido novo do Gabriel): o sampler
//! embutido mede CPU/RAM/rede de CADA container rodando com label
//! `com.docker.compose.project` começando em `flux-`, a cada 30 s, via bollard
//! stats one-shot (mesmo padrão do `sleep.rs`: `stats(id, None)` + primeiro
//! frame; CPU = cpu_delta/system_delta × online × 100). Rollup de 1 minuto em
//! SQLite (`metrics_1min`, bucket ts = epoch/60*60, UPSERT — a última amostra
//! do minuto vence); retenção de 24 h (DELETE a cada ciclo).
//!
//! PERÍCIA DE PICO: cpu_percent > 80.0 OU mem_used_mb > 0.85 × mem_limit_mb
//! (só quando limit > 0) grava uma linha em `peaks` com SNAPSHOT DO MOMENTO —
//! JSON com a imagem do container e TODOS os irmãos do mesmo compose project
//! coletados no mesmo ciclo (o próprio container pico entra entre os irmãos:
//! é parte do projeto). Anti-spam: máx. 1 peak por container a cada 300 s
//! (checa o último `MAX(ts)` gravado). Quando os dois critérios estouram no
//! mesmo ciclo, grava UM peak só (cpu avaliada primeiro).
//!
//! Erros NUNCA silenciosos: falha de stats de um container loga e o ciclo
//! segue (o sampler nunca morre); falha de banco loga com causa. Consultas
//! devolvem `Vec`/`Option` (assinaturas pedidas pelo painel) — erro de leitura
//! loga e devolve vazio/`None`, nunca pânico.
//!
//! Integração: o `serve` (lib.rs) chama `observe::iniciar(Arc::clone(&shared))`
//! uma vez — a função garante o schema (CREATE TABLE IF NOT EXISTS, idempotente)
//! e faz o `tokio::spawn` do loop de 30 s. Sem engine de Docker o sampler nem
//! inicia (warn explícito), mas o schema continua sendo criado para o painel.
//!
//! Unidades: MB = 1024² bytes, KB = 1024 bytes (convenção Docker/desktop).
//! Campos ausentes no stats do engine (ex.: `memory_stats.usage` em Windows)
//! viram 0.0 com log debug — coluna NOT NULL, e pico de memória só dispara com
//! limit > 0, então ausência nunca dispara falso pico.

use std::collections::BTreeMap;
use std::sync::Arc;

use bollard::Docker;
use bollard::models::ContainerSummaryStateEnum;
use futures_util::StreamExt;
use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, OptionalExtension, params, params_from_iter};
use serde_json::json;

use crate::events;
use crate::sleep::now_epoch;
use crate::state::State;

/// Intervalo do sampler (spec 13: amostra a cada 30 s).
pub const INTERVALO_SECS: u64 = 30;

/// Retenção do rollup (pedido do Gabriel): histórico de 24 h.
pub const RETENCAO_SECS: i64 = 24 * 3600;

/// Gatilho de pico de CPU (pedido novo): cpu_percent > 80.0.
pub const CPU_PEAK_PCT: f64 = 80.0;

/// Gatilho de pico de memória (pedido novo): mem_used_mb > 0.85 × mem_limit_mb.
pub const MEM_PEAK_FRAC: f64 = 0.85;

/// Anti-spam de perícia: máx. 1 peak por container a cada 300 s.
pub const PEAK_COOLDOWN_SECS: i64 = 300;

const MB: f64 = 1024.0 * 1024.0;
const KB: f64 = 1024.0;

/// Prefixo dos compose projects monitorados (convenção da spec 06).
const PREFIXO_PROJECT: &str = "flux-";

const SCHEMA_OBSERVE: &str = r#"
CREATE TABLE IF NOT EXISTS metrics_1min(
    ts INTEGER NOT NULL,
    compose_project TEXT NOT NULL,
    service TEXT NOT NULL,
    container TEXT NOT NULL,
    image TEXT NOT NULL,
    cpu_percent REAL NOT NULL,
    mem_used_mb REAL NOT NULL,
    mem_limit_mb REAL NOT NULL,
    net_rx_kb REAL NOT NULL,
    net_tx_kb REAL NOT NULL,
    PRIMARY KEY(ts, container)
);
CREATE TABLE IF NOT EXISTS peaks(
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    ts INTEGER,
    container TEXT,
    compose_project TEXT,
    service TEXT,
    image TEXT,
    metric TEXT,
    valor REAL,
    threshold REAL,
    snapshot TEXT
);
CREATE INDEX IF NOT EXISTS idx_peaks_ts ON peaks(ts);
CREATE INDEX IF NOT EXISTS idx_peaks_container_ts ON peaks(container, ts);
"#;

/// Métrica consultável pelo painel — cada variante é uma coluna do rollup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Metrica {
    Cpu,
    Mem,
    NetRx,
    NetTx,
}

impl Metrica {
    /// Coluna SQL correspondente (nomes fixos — sem SQL montado por input).
    pub fn coluna(self) -> &'static str {
        match self {
            Metrica::Cpu => "cpu_percent",
            Metrica::Mem => "mem_used_mb",
            Metrica::NetRx => "net_rx_kb",
            Metrica::NetTx => "net_tx_kb",
        }
    }
}

/// Uma coleta de um container num ciclo do sampler (antes do UPSERT).
#[derive(Debug, Clone)]
pub struct Amostra {
    pub ts: i64,
    pub compose_project: String,
    pub service: String,
    pub container: String,
    pub image: String,
    pub cpu_percent: f64,
    pub mem_used_mb: f64,
    pub mem_limit_mb: f64,
    pub net_rx_kb: f64,
    pub net_tx_kb: f64,
    /// Estado do container no Docker no momento da coleta ("running", "paused"...).
    pub status: String,
}

/// Linha de perícia lida do banco (para a página do painel). `snapshot` não
/// vem aqui — vai buscado à parte por [`peak_snapshot`] (pode ser grande).
#[derive(Debug, Clone, PartialEq)]
pub struct PeakRow {
    pub id: i64,
    pub ts: i64,
    pub container: String,
    pub compose_project: String,
    pub service: String,
    pub image: String,
    pub metric: String,
    pub valor: f64,
    pub threshold: f64,
}

/// Cria as tabelas de métricas/perícia — idempotente, roda no arranque
/// (dentro de [`iniciar`]) e nos testes. Mesmo padrão das migrations do
/// `state.rs`: CREATE IF NOT EXISTS, nunca destrói dado.
pub fn garantir_schema(st: &State) -> Result<(), crate::state::StateError> {
    st.with(|c| c.execute_batch(SCHEMA_OBSERVE))
}

/// Bucket de 1 minuto: ts = epoch/60*60 (divisão inteira, alinhado ao minuto).
pub fn bucket_ts(epoch: i64) -> i64 {
    (epoch / 60) * 60
}

/// Escapa os wildcards do LIKE no valor do filtro (`%`, `_` e o próprio `\`)
/// — o `?projeto=` vem da query string do painel e NÃO pode alargar o recorte
/// (`%25` decodifica `%`, que casaria todos os projetos). Use com
/// `ESCAPE '\'` no SQL. `%`/`_` não ocorrem em nome de projeto válido
/// ([a-z0-9-]), então escapar nunca muda filtro legítimo.
fn like_escape(v: &str) -> String {
    let mut s = String::with_capacity(v.len());
    for c in v.chars() {
        if matches!(c, '%' | '_' | '\\') {
            s.push('\\');
        }
        s.push(c);
    }
    s
}

/// UPSERT da amostra no bucket do minuto. Duas coletas do mesmo minuto para o
/// mesmo container = 1 linha (a última vence — PK(ts, container)).
pub fn upsert(st: &State, a: &Amostra) -> Result<(), crate::state::StateError> {
    st.with(|c| upsert_conn(c, a).map(|_| ()))
}

/// SQL do UPSERT, reusado pela gravação em transação do ciclo.
fn upsert_conn(c: &Connection, a: &Amostra) -> rusqlite::Result<usize> {
    c.execute(
        "INSERT INTO metrics_1min(ts, compose_project, service, container, image,
            cpu_percent, mem_used_mb, mem_limit_mb, net_rx_kb, net_tx_kb)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(ts, container) DO UPDATE SET
            compose_project = excluded.compose_project,
            service = excluded.service,
            image = excluded.image,
            cpu_percent = excluded.cpu_percent,
            mem_used_mb = excluded.mem_used_mb,
            mem_limit_mb = excluded.mem_limit_mb,
            net_rx_kb = excluded.net_rx_kb,
            net_tx_kb = excluded.net_tx_kb",
        params![
            a.ts,
            a.compose_project,
            a.service,
            a.container,
            a.image,
            a.cpu_percent,
            a.mem_used_mb,
            a.mem_limit_mb,
            a.net_rx_kb,
            a.net_tx_kb,
        ],
    )
}

/// Grava o ciclo inteiro numa trava só, em transação: ou o ciclo entra
/// inteiro ou erro claro — metade de bucket silenciosa não existe.
fn gravar_ciclo(st: &State, amostras: &[Amostra]) -> Result<(), crate::state::StateError> {
    if amostras.is_empty() {
        return Ok(());
    }
    st.with(|c| {
        let tx = c.unchecked_transaction()?;
        for a in amostras {
            upsert_conn(&tx, a)?;
        }
        tx.commit()?;
        Ok(())
    })
}

/// Retenção (spec 13 + pedido: 24 h): apaga buckets do rollup E perícia de
/// pico (`peaks` também cresce sem limite — anti-spam de 300 s ainda deixa
/// ~576 linhas/dia por container em pico contínuo). Retorna quantas linhas
/// saíram (as duas tabelas somadas). Roda a cada ciclo do sampler.
pub fn podar(st: &State, agora: i64) -> Result<usize, crate::state::StateError> {
    let limite = bucket_ts(agora - RETENCAO_SECS);
    st.with(|c| {
        Ok(
            c.execute("DELETE FROM metrics_1min WHERE ts < ?1", params![limite])?
                + c.execute("DELETE FROM peaks WHERE ts < ?1", params![limite])?,
        )
    })
}

/// Decisão pura de pico — testável sem Docker. Ordem: cpu primeiro; memória só
/// com limit > 0. Estrito (`>`): 80.0 exato NÃO é pico. Retorna
/// `(metric, valor, threshold)` — os nomes de `metric` são as colunas do
/// rollup (mesmo vocabulário de [`Metrica::coluna`]).
pub fn pico_de(a: &Amostra) -> Option<(&'static str, f64, f64)> {
    if a.cpu_percent > CPU_PEAK_PCT {
        return Some(("cpu_percent", a.cpu_percent, CPU_PEAK_PCT));
    }
    if a.mem_limit_mb > 0.0 && a.mem_used_mb > MEM_PEAK_FRAC * a.mem_limit_mb {
        let limiar = MEM_PEAK_FRAC * a.mem_limit_mb;
        return Some(("mem_used_mb", a.mem_used_mb, limiar));
    }
    None
}

/// Snapshot do momento em JSON: imagem do container pico + TODOS os irmãos do
/// mesmo compose project coletados no MESMO ciclo (incluindo o próprio pico).
pub fn snapshot_json(pico: &Amostra, irmaos: &[Amostra]) -> String {
    let siblings: Vec<_> = irmaos
        .iter()
        .map(|a| {
            json!({
                "container": a.container,
                "service": a.service,
                "image": a.image,
                "cpu_percent": a.cpu_percent,
                "mem_used_mb": a.mem_used_mb,
                "status": a.status,
            })
        })
        .collect();
    json!({ "image": pico.image, "siblings": siblings }).to_string()
}

/// Anti-spam: o container pode ter novo peak? Checa o ÚLTIMO gravado —
/// intervalo >= [`PEAK_COOLDOWN_SECS`] (ou nunca houve peak) autoriza.
pub fn peak_autorizado(
    st: &State,
    container: &str,
    agora: i64,
) -> Result<bool, crate::state::StateError> {
    st.with(|c| {
        let ultimo: Option<i64> = c.query_row(
            "SELECT MAX(ts) FROM peaks WHERE container = ?1",
            params![container],
            |r| r.get(0),
        )?;
        Ok(match ultimo {
            None => true,
            Some(ts) => agora - ts >= PEAK_COOLDOWN_SECS,
        })
    })
}

/// Grava o peak com o snapshot do momento; devolve o `id` criado.
pub fn registrar_peak(
    st: &State,
    a: &Amostra,
    metric: &str,
    valor: f64,
    threshold: f64,
    snapshot: &str,
) -> Result<i64, crate::state::StateError> {
    st.with(|c| {
        c.execute(
            "INSERT INTO peaks(ts, container, compose_project, service, image, metric,
                valor, threshold, snapshot)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                a.ts,
                a.container,
                a.compose_project,
                a.service,
                a.image,
                metric,
                valor,
                threshold,
                snapshot,
            ],
        )?;
        Ok(c.last_insert_rowid())
    })
}

/// Série temporal do rollup para o painel: janela de `minutos` até agora.
/// Filtros: `projeto` casa `LIKE 'flux-{projeto}-%'`; `container` é igualdade
/// exata. Com filtro só de projeto (ou sem filtro), o valor por minuto é a
/// MÉDIA entre os containers do minuto (GROUP BY ts). Erro de leitura: loga
/// com causa e devolve vazio — painel mostra vazio, nunca pânico.
pub fn serie(
    st: &State,
    projeto: Option<&str>,
    container: Option<&str>,
    metrica: Metrica,
    minutos: i64,
) -> Vec<(i64, f64)> {
    let desde = bucket_ts(now_epoch()) - minutos * 60;
    let col = metrica.coluna();
    // Placeholders ANÔNIMOS sequenciais: filtros opcionais são anexados em
    // ordem — numerados (?1/?2/?3) deixaria buraco de índice quando `projeto`
    // vem None e o `?3` ficava NULL sem erro.
    let mut sql = format!("SELECT ts, AVG({col}) FROM metrics_1min WHERE ts >= ?");
    let mut args: Vec<SqlValue> = vec![SqlValue::Integer(desde)];
    if let Some(p) = projeto {
        sql.push_str(" AND compose_project LIKE ? ESCAPE '\\'");
        args.push(SqlValue::Text(format!(
            "{PREFIXO_PROJECT}{}-%",
            like_escape(p)
        )));
    }
    if let Some(ct) = container {
        sql.push_str(" AND container = ?");
        args.push(SqlValue::Text(ct.to_owned()));
    }
    sql.push_str(" GROUP BY ts ORDER BY ts");
    let r = st.with(|c| {
        let mut stmt = c.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(args.iter()), |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?))
        })?;
        rows.collect()
    });
    match r {
        Ok(v) => v,
        Err(e) => {
            tracing::error!("observe: série {col} falhou: {e}");
            Vec::new()
        }
    }
}

/// Drill de projeto para containers: série de cada container que já apareceu
/// no rollup de `flux-{projeto}-%` na janela. Ordenação determinística por
/// nome de container (BTreeMap). Erro: loga e devolve vazio.
pub fn series_por_container(
    st: &State,
    projeto: &str,
    metrica: Metrica,
    minutos: i64,
) -> Vec<(String, Vec<(i64, f64)>)> {
    let desde = bucket_ts(now_epoch()) - minutos * 60;
    let col = metrica.coluna();
    let r: Result<BTreeMap<String, Vec<(i64, f64)>>, _> = st.with(|c| {
        let mut stmt = c.prepare(&format!(
            "SELECT container, ts, {col} FROM metrics_1min
             WHERE compose_project LIKE ?1 ESCAPE '\\' AND ts >= ?2
             ORDER BY container, ts"
        ))?;
        let rows = stmt.query_map(
            params![
                format!("{PREFIXO_PROJECT}{}-%", like_escape(projeto)),
                desde
            ],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, f64>(2)?,
                ))
            },
        )?;
        let mut mapa: BTreeMap<String, Vec<(i64, f64)>> = BTreeMap::new();
        for row in rows {
            let (container, ts, v) = row?;
            mapa.entry(container).or_default().push((ts, v));
        }
        Ok(mapa)
    });
    match r {
        Ok(mapa) => mapa.into_iter().collect(),
        Err(e) => {
            tracing::error!("observe: séries por container falharam: {e}");
            Vec::new()
        }
    }
}

/// Perícia registrada na janela (`minutos`), mais recente primeiro. Filtro de
/// projeto casa `LIKE 'flux-{projeto}-%'`. NULL de colunas (a DDL permite) é
/// tolerado defensivamente como vazio/0 — os inserts deste módulo sempre
/// preenchem tudo.
pub fn peaks(st: &State, minutos: i64, projeto: Option<&str>) -> Vec<PeakRow> {
    let desde = now_epoch() - minutos * 60;
    let mut sql = String::from(
        "SELECT id, ts, container, compose_project, service, image, metric, valor, threshold
         FROM peaks WHERE ts >= ?1",
    );
    let mut args: Vec<SqlValue> = vec![SqlValue::Integer(desde)];
    if let Some(p) = projeto {
        sql.push_str(" AND compose_project LIKE ?2 ESCAPE '\\'");
        args.push(SqlValue::Text(format!(
            "{PREFIXO_PROJECT}{}-%",
            like_escape(p)
        )));
    }
    sql.push_str(" ORDER BY ts DESC");
    let r: Result<Vec<PeakRow>, _> = st.with(|c| {
        let mut stmt = c.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(args.iter()), |r| {
            Ok(PeakRow {
                id: r.get(0)?,
                ts: r.get(1)?,
                container: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                compose_project: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                service: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
                image: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
                metric: r.get::<_, Option<String>>(6)?.unwrap_or_default(),
                valor: r.get::<_, Option<f64>>(7)?.unwrap_or(0.0),
                threshold: r.get::<_, Option<f64>>(8)?.unwrap_or(0.0),
            })
        })?;
        rows.collect()
    });
    match r {
        Ok(v) => v,
        Err(e) => {
            tracing::error!("observe: consulta de peaks falhou: {e}");
            Vec::new()
        }
    }
}

/// Snapshot JSON do peak (corpo da perícia), buscado à parte. `None` = peak
/// inexistente ou sem snapshot gravado. Erro: loga e devolve `None`.
pub fn peak_snapshot(st: &State, id: i64) -> Option<String> {
    let r: Result<Option<Option<String>>, _> = st.with(|c| {
        c.query_row(
            "SELECT snapshot FROM peaks WHERE id = ?1",
            params![id],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()
    });
    match r {
        // Linha existe: Some(snapshot), onde snapshot pode ser NULL → None.
        Ok(Some(snapshot)) => snapshot,
        // Peak inexistente.
        Ok(None) => None,
        Err(e) => {
            tracing::error!("observe: snapshot do peak {id} falhou: {e}");
            None
        }
    }
}

/// Arranque do sampler: garante schema (idempotente), valida engine de Docker
/// e faz o `tokio::spawn` do loop de 30 s. O integrador (`serve`, lib.rs)
/// chama isto UMA vez. Erro de schema é logado com causa e o sampler não
/// inicia — métrica sem tabela é silêncio disfarçado.
pub async fn iniciar(app: Arc<crate::AppState>) {
    if let Err(e) = garantir_schema(&app.state) {
        tracing::error!("observe: schema de métricas/perícia falhou: {e} — sampler não inicia");
        return;
    }
    let Some(docker) = app.docker.clone() else {
        tracing::warn!(
            "observe: sem engine de Docker — sampler de recursos não inicia \
             (métricas e perícia de pico ficam sem coleta)"
        );
        return;
    };
    tracing::info!(
        "observe: sampler de recursos no ar (intervalo {INTERVALO_SECS}s, retenção {}h, \
         pico cpu>{CPU_PEAK_PCT}% / mem>{MEM_PEAK_FRAC:.0}×limite)",
        RETENCAO_SECS / 3600
    );
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(INTERVALO_SECS));
        loop {
            tick.tick().await; // 1º tick dispara já — primeira coleta no arranque
            if let Err(e) = ciclo(&docker, &app.state).await {
                tracing::error!("observe: ciclo de coleta falhou: {e} — sampler segue vivo");
            }
        }
    });
}

/// Um ciclo do sampler: coleta → grava (1 trava, em transação) → retenção →
/// perícia. Erro de banco sobe para o loop logar com causa e o sampler segue.
async fn ciclo(docker: &Docker, st: &State) -> Result<(), crate::state::StateError> {
    let amostras = coleta(docker).await;
    gravar_ciclo(st, &amostras)?;
    let agora = now_epoch();
    match podar(st, agora) {
        Ok(0) => {}
        Ok(n) => tracing::debug!("observe: retenção apagou {n} buckets > 24h"),
        Err(e) => tracing::error!("observe: retenção falhou: {e}"),
    }
    pericia(st, &amostras, agora);
    Ok(())
}

/// Coleta TODOS os containers rodando com project `flux-*` (label filter de
/// existência no engine, prefixo conferido aqui). Falha de stats de um
/// container: warn com causa e segue. Sem containers: ciclo vazio, sem erro.
async fn coleta(docker: &Docker) -> Vec<Amostra> {
    let mut filters = std::collections::HashMap::new();
    // Filtro `label` SEM valor = "tem a label" (API do Docker).
    filters.insert(
        "label".to_owned(),
        vec!["com.docker.compose.project".to_owned()],
    );
    let options = bollard::query_parameters::ListContainersOptionsBuilder::default()
        .filters(&filters)
        .build();
    let lista = match docker.list_containers(Some(options)).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!("observe: listagem de containers falhou: {e}");
            return Vec::new();
        }
    };

    let ts = bucket_ts(now_epoch());
    // Agrupa por compose project para o snapshot de irmãos da perícia; BTreeMap
    // = ordem determinística de projeto e de snapshot.
    let mut por_projeto: BTreeMap<String, Vec<Amostra>> = BTreeMap::new();

    for c in lista {
        let labels = c.labels.clone().unwrap_or_default();
        let Some(projeto) = labels.get("com.docker.compose.project") else {
            continue;
        };
        if !projeto.starts_with(PREFIXO_PROJECT) {
            continue;
        }
        if c.state.as_ref() != Some(&ContainerSummaryStateEnum::RUNNING) {
            continue; // só container RODANDO entra no rollup
        }
        let Some(id) = c.id.clone() else {
            tracing::warn!("observe: container de {projeto} sem id — pulado neste ciclo");
            continue;
        };
        // Nome curto (API devolve "/nome"); sem nome, o id serve de chave.
        let nome = c
            .names
            .as_ref()
            .and_then(|ns| ns.first())
            .map(|n| n.trim_start_matches('/').to_owned())
            .unwrap_or_else(|| id.clone());
        let service = labels
            .get("com.docker.compose.service")
            .cloned()
            .unwrap_or_default();
        let image = c.image.clone().unwrap_or_default();
        let status = c
            .state
            .map(|s| s.to_string())
            .unwrap_or_else(|| "unknown".to_owned());

        let mut stream = docker.stats(&id, None); // padrão do sleep.rs: 1º frame
        let amostra = match stream.next().await {
            Some(Ok(stats)) => {
                let cpu_percent = crate::sleep::cpu_percent(&stats);
                let (mem_used_mb, mem_limit_mb) = match stats.memory_stats.as_ref() {
                    Some(m) => (
                        m.usage.map(|u| u as f64 / MB).unwrap_or_else(|| {
                            tracing::debug!("observe: {nome} sem memory_stats.usage — 0.0");
                            0.0
                        }),
                        m.limit.map(|l| l as f64 / MB).unwrap_or_else(|| {
                            tracing::debug!("observe: {nome} sem memory_stats.limit — 0.0");
                            0.0
                        }),
                    ),
                    None => {
                        tracing::debug!("observe: {nome} sem memory_stats — 0.0");
                        (0.0, 0.0)
                    }
                };
                let (mut rx, mut tx) = (0.0, 0.0);
                if let Some(nets) = stats.networks.as_ref() {
                    for n in nets.values() {
                        rx += n.rx_bytes.unwrap_or(0) as f64;
                        tx += n.tx_bytes.unwrap_or(0) as f64;
                    }
                }
                Amostra {
                    ts,
                    compose_project: projeto.clone(),
                    service,
                    container: nome,
                    image,
                    cpu_percent,
                    mem_used_mb,
                    mem_limit_mb,
                    net_rx_kb: rx / KB,
                    net_tx_kb: tx / KB,
                    status,
                }
            }
            Some(Err(e)) => {
                tracing::warn!("observe: stats de {nome} ({projeto}) falhou: {e} — segue");
                continue;
            }
            None => {
                tracing::warn!("observe: stats de {nome} ({projeto}) veio vazio — segue");
                continue;
            }
        };
        por_projeto
            .entry(projeto.clone())
            .or_default()
            .push(amostra);
    }

    // Sort por container: snapshots de perícia comparáveis entre ciclos.
    let mut todas = Vec::new();
    for (_, mut amostras) in por_projeto {
        amostras.sort_by(|a, b| a.container.cmp(&b.container));
        todas.extend(amostras);
    }
    todas
}

/// Perícia de pico: para cada amostra que estoura threshold, grava peak com
/// snapshot dos irmãos do MESMO project coletados neste ciclo, respeitando o
/// anti-spam de 300 s por container. Falha de um peak loga e segue.
fn pericia(st: &State, amostras: &[Amostra], agora: i64) {
    for a in amostras {
        let Some((metric, valor, threshold)) = pico_de(a) else {
            continue;
        };
        let autorizado = match peak_autorizado(st, &a.container, agora) {
            Ok(v) => v,
            Err(e) => {
                tracing::error!(
                    "observe: anti-spam de peak de {} falhou: {e} — peak não gravado",
                    a.container
                );
                continue;
            }
        };
        if !autorizado {
            tracing::debug!(
                "observe: peak de {} em anti-spam (< {PEAK_COOLDOWN_SECS}s) — ignorado",
                a.container
            );
            continue;
        }
        let irmaos: Vec<Amostra> = amostras
            .iter()
            .filter(|i| i.compose_project == a.compose_project)
            .cloned()
            .collect();
        let snapshot = snapshot_json(a, &irmaos);
        match registrar_peak(st, a, metric, valor, threshold, &snapshot) {
            Ok(id) => {
                let msg = format!(
                    "pico de {metric} em {}: {valor:.1} (limiar {threshold:.1}) — snapshot #{id}",
                    a.container
                );
                tracing::warn!("observe: {msg}");
                events::event(st, "warn", "observe", &msg);
            }
            Err(e) => {
                tracing::error!("observe: peak de {} não gravou: {e}", a.container);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn amostra(
        ts: i64,
        projeto: &str,
        container: &str,
        cpu: f64,
        mem: f64,
        limite: f64,
    ) -> Amostra {
        Amostra {
            ts,
            compose_project: projeto.to_owned(),
            service: "app".to_owned(),
            container: container.to_owned(),
            image: "laravel:teste".to_owned(),
            cpu_percent: cpu,
            mem_used_mb: mem,
            mem_limit_mb: limite,
            net_rx_kb: 1.0,
            net_tx_kb: 2.0,
            status: "running".to_owned(),
        }
    }

    // ------------------------------------------------- bucket / upsert

    #[test]
    fn bucket_alinha_no_minuto() {
        assert_eq!(bucket_ts(1_727_000_061), 1_727_000_040);
        assert_eq!(bucket_ts(120), 120);
        assert_eq!(bucket_ts(59), 0);
    }

    #[test]
    fn upsert_duas_coletas_no_mesmo_minuto_e_uma_linha() {
        let st = State::in_memory().unwrap();
        garantir_schema(&st).unwrap();
        // Schema idempotente: segunda chamada não pode falhar.
        garantir_schema(&st).unwrap();

        // No sampler real, Amostra.ts JÁ vem bucketado (bucket_ts de agora) —
        // duas coletas do mesmo minuto chegam com o MESMO ts.
        let t1 = amostra(1200, "flux-vipers-dev", "c-app-1", 10.0, 100.0, 512.0);
        upsert(&st, &t1).unwrap();
        let t2 = amostra(1200, "flux-vipers-dev", "c-app-1", 99.5, 200.0, 512.0);
        upsert(&st, &t2).unwrap();

        let (n, cpu, mem): (i64, f64, f64) = st
            .with(|c| {
                c.query_row(
                    "SELECT COUNT(*), cpu_percent, mem_used_mb FROM metrics_1min",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
            })
            .unwrap();
        assert_eq!(n, 1, "duas coletas no mesmo minuto = 1 linha");
        assert_eq!(cpu, 99.5, "a última coleta do minuto vence");
        assert_eq!(mem, 200.0);

        // Mesmo bucket, container DIFERENTE: linha nova.
        upsert(
            &st,
            &amostra(1200, "flux-vipers-dev", "c-worker-1", 5.0, 50.0, 512.0),
        )
        .unwrap();
        let n: i64 = st
            .with(|c| c.query_row("SELECT COUNT(*) FROM metrics_1min", [], |r| r.get(0)))
            .unwrap();
        assert_eq!(
            n, 2,
            "PK(ts, container): container distinto = linha própria"
        );
    }

    // ------------------------------------------------- retenção

    #[test]
    fn retencao_apaga_so_acima_de_24h() {
        let st = State::in_memory().unwrap();
        garantir_schema(&st).unwrap();
        let agora = 2_000_000_000;
        let limite = bucket_ts(agora - RETENCAO_SECS);
        upsert(
            &st,
            &amostra(limite - 120, "flux-vipers-dev", "c-velho", 1.0, 1.0, 512.0),
        )
        .unwrap();
        upsert(
            &st,
            &amostra(limite, "flux-vipers-dev", "c-na-borda", 1.0, 1.0, 512.0),
        )
        .unwrap();
        upsert(
            &st,
            &amostra(
                bucket_ts(agora),
                "flux-vipers-dev",
                "c-fresco",
                1.0,
                1.0,
                512.0,
            ),
        )
        .unwrap();

        let apagados = podar(&st, agora).unwrap();
        assert_eq!(apagados, 1, "só o bucket > 24h sai");

        let nomes: Vec<String> = st
            .with(|c| {
                let mut stmt =
                    c.prepare("SELECT container FROM metrics_1min ORDER BY container")?;
                let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
                rows.collect()
            })
            .unwrap();
        assert_eq!(nomes, vec!["c-fresco", "c-na-borda"], "borda (== 24h) fica");
    }

    /// Revisão: `peaks` TAMBÉM tem retenção — sem poda, pico contínuo gerava
    /// ~576 linhas/dia por container para sempre no SQLite do fluxd.
    #[test]
    fn retencao_apaga_peaks_acima_de_24h_tambem() {
        let st = State::in_memory().unwrap();
        garantir_schema(&st).unwrap();
        let agora = 2_000_000_000;
        let velho = amostra(
            agora - RETENCAO_SECS - 120,
            "flux-vipers-dev",
            "c-velho",
            90.0,
            10.0,
            512.0,
        );
        let fresco = amostra(agora - 30, "flux-vipers-dev", "c-fresco", 90.0, 10.0, 512.0);
        registrar_peak(&st, &velho, "cpu_percent", 90.0, CPU_PEAK_PCT, "{}").unwrap();
        registrar_peak(&st, &fresco, "cpu_percent", 90.0, CPU_PEAK_PCT, "{}").unwrap();

        let apagados = podar(&st, agora).unwrap();
        assert_eq!(apagados, 1, "só o peak > 24h sai");
        let restantes = peaks(&st, 60 * 24 * 7, None);
        assert_eq!(restantes.len(), 1, "peak fresco fica");
        assert_eq!(restantes[0].container, "c-fresco");
    }

    // ------------------------------------------------- perícia de pico

    #[test]
    fn pico_dispara_nos_dois_criterios_e_nao_abaixo() {
        // CPU acima do limiar.
        let (m, v, t) = pico_de(&amostra(0, "p", "c", 81.0, 10.0, 512.0)).unwrap();
        assert_eq!((m, v, t), ("cpu_percent", 81.0, CPU_PEAK_PCT));
        // Memória acima de 85% do limite (com limite > 0).
        let (m, v, t) = pico_de(&amostra(0, "p", "c", 1.0, 440.0, 512.0)).unwrap();
        assert_eq!((m, v, t), ("mem_used_mb", 440.0, MEM_PEAK_FRAC * 512.0));
        // Abaixo dos dois: nada.
        assert_eq!(pico_de(&amostra(0, "p", "c", 50.0, 100.0, 512.0)), None);
        // Borda exata: 80.0 e exatamente 85% NÃO disparam (estrito >).
        assert_eq!(
            pico_de(&amostra(0, "p", "c", CPU_PEAK_PCT, 10.0, 512.0)),
            None
        );
        assert_eq!(
            pico_de(&amostra(0, "p", "c", 1.0, MEM_PEAK_FRAC * 512.0, 512.0)),
            None
        );
        // Sem limite (0.0): memória NUNCA dispara, mesmo com uso alto.
        assert_eq!(pico_de(&amostra(0, "p", "c", 1.0, 10_000.0, 0.0)), None);
        // Os dois estourados: cpu vence, UM peak só.
        let (m, _, _) = pico_de(&amostra(0, "p", "c", 99.0, 500.0, 512.0)).unwrap();
        assert_eq!(m, "cpu_percent");
    }

    #[test]
    fn anti_spam_300s_bloqueia_e_libera() {
        let st = State::in_memory().unwrap();
        garantir_schema(&st).unwrap();
        let agora = 1_000_000;

        // Nunca houve peak: autorizado.
        assert!(peak_autorizado(&st, "c-app-1", agora).unwrap());

        // Peak há 299 s: bloqueado. Há exatos 300 s: liberado (>= 300).
        let a = amostra(agora - 299, "flux-vipers-dev", "c-app-1", 90.0, 10.0, 512.0);
        registrar_peak(&st, &a, "cpu_percent", 90.0, CPU_PEAK_PCT, "{}").unwrap();
        assert!(
            !peak_autorizado(&st, "c-app-1", agora).unwrap(),
            "299s < 300s bloqueia"
        );
        assert!(
            peak_autorizado(&st, "c-app-1", agora + 1).unwrap(),
            "300s libera"
        );

        // Anti-spam é POR container: outro container não herda o bloqueio.
        assert!(peak_autorizado(&st, "c-worker-1", agora).unwrap());
    }

    #[test]
    fn snapshot_tem_image_e_irmaos_do_momento() {
        let pico = amostra(1200, "flux-vipers-dev", "c-app-1", 95.0, 480.0, 512.0);
        let irmaos = [
            amostra(1200, "flux-vipers-dev", "c-app-1", 95.0, 480.0, 512.0),
            amostra(1200, "flux-vipers-dev", "c-worker-1", 12.0, 80.0, 256.0),
            amostra(1200, "flux-outro-prod", "c-fora", 1.0, 1.0, 64.0), // NÃO é irmão
        ];
        let so_irmaos: Vec<Amostra> = irmaos
            .iter()
            .filter(|a| a.compose_project == pico.compose_project)
            .cloned()
            .collect();
        let s = snapshot_json(&pico, &so_irmaos);
        let v: Value = serde_json::from_str(&s).unwrap();
        assert_eq!(
            v["image"], "laravel:teste",
            "snapshot traz a imagem do pico"
        );
        let sib = v["siblings"].as_array().unwrap();
        assert_eq!(sib.len(), 2, "só irmãos do MESMO compose project");
        assert_eq!(sib[0]["container"], "c-app-1");
        assert_eq!(sib[0]["service"], "app");
        assert_eq!(sib[0]["cpu_percent"], 95.0);
        assert_eq!(sib[0]["mem_used_mb"], 480.0);
        assert_eq!(sib[0]["status"], "running");
        assert_eq!(sib[1]["container"], "c-worker-1");
    }

    // ------------------------------------------------- consultas / filtros

    #[test]
    fn filtros_de_serie_serie_projeto_e_container() {
        let st = State::in_memory().unwrap();
        garantir_schema(&st).unwrap();
        // ts relativos ao RELÓGIO REAL: as consultas ancoram a janela em now.
        let b0 = bucket_ts(now_epoch());
        let b_antigo = b0 - 120; // dois buckets atrás
        upsert(
            &st,
            &amostra(b0, "flux-vipers-dev", "c-app-1", 10.0, 100.0, 512.0),
        )
        .unwrap();
        upsert(
            &st,
            &amostra(b0, "flux-vipers-dev", "c-worker-1", 30.0, 300.0, 512.0),
        )
        .unwrap();
        upsert(
            &st,
            &amostra(b_antigo, "flux-vipers-dev", "c-app-1", 20.0, 200.0, 512.0),
        )
        .unwrap();
        // Container do outro projeto com nome PRÓPRIO: no Docker nomes são
        // globalmente únicos, e a PK(ts, container) não inclui projeto.
        upsert(
            &st,
            &amostra(b0, "flux-outra-dev", "z-app-1", 99.0, 999.0, 512.0),
        )
        .unwrap();

        // Por container: só as linhas dele, em ordem de ts.
        let serie_c = serie(&st, None, Some("c-app-1"), Metrica::Cpu, 10);
        assert_eq!(serie_c, vec![(b_antigo, 20.0), (b0, 10.0)]);

        // Por projeto: média entre os containers do projeto, outro projeto fora.
        let serie_p = serie(&st, Some("vipers"), None, Metrica::Cpu, 10);
        assert_eq!(serie_p, vec![(b_antigo, 20.0), (b0, 20.0)]); // (10+30)/2

        // LIKE é prefixo: "outra" casa flux-outra-dev, e SÓ ele.
        let outra = serie(&st, Some("outra"), None, Metrica::Cpu, 10);
        assert_eq!(outra, vec![(b0, 99.0)]);

        // Janela: 1 minuto cobre desde b0-60 — o bucket b_antigo (b0-120) fora.
        let janela = serie(&st, None, None, Metrica::Cpu, 1);
        assert_eq!(janela, vec![(b0, (10.0 + 30.0 + 99.0) / 3.0)]);

        // Métrica troca a coluna.
        let mem = serie(&st, Some("vipers"), Some("c-worker-1"), Metrica::Mem, 10);
        assert_eq!(mem, vec![(b0, 300.0)]);

        // Wildcard do LIKE no VALOR do filtro não alarga o recorte (revisão):
        // "%25" decodifica "%", que sem escape viraria LIKE 'flux-%-%' e
        // casaria TODOS os projetos.
        let vazia = serie(&st, Some("%"), None, Metrica::Cpu, 10);
        assert!(vazia.is_empty(), "% no filtro não casa nada: {vazia:?}");
        let sublinhado = serie(&st, Some("_"), None, Metrica::Cpu, 10);
        assert!(sublinhado.is_empty(), "_ no filtro não casa nada");
    }

    #[test]
    fn series_por_container_drill_de_projeto() {
        let st = State::in_memory().unwrap();
        garantir_schema(&st).unwrap();
        let b0 = bucket_ts(now_epoch());
        let b_antigo = b0 - 120;
        upsert(
            &st,
            &amostra(b0, "flux-vipers-dev", "c-worker-1", 30.0, 1.0, 512.0),
        )
        .unwrap();
        upsert(
            &st,
            &amostra(b0, "flux-vipers-dev", "c-app-1", 10.0, 1.0, 512.0),
        )
        .unwrap();
        upsert(
            &st,
            &amostra(b_antigo, "flux-vipers-dev", "c-app-1", 20.0, 1.0, 512.0),
        )
        .unwrap();
        upsert(
            &st,
            &amostra(b0, "flux-outra-dev", "z-app-1", 50.0, 1.0, 512.0),
        )
        .unwrap();

        let drill = series_por_container(&st, "vipers", Metrica::Cpu, 10);
        assert_eq!(
            drill,
            vec![
                ("c-app-1".to_owned(), vec![(b_antigo, 20.0), (b0, 10.0)]),
                ("c-worker-1".to_owned(), vec![(b0, 30.0)]),
            ],
            "um vetor por container, ordem alfabética, sem contaminação de outro projeto"
        );
    }

    #[test]
    fn peaks_consulta_filtro_projeto_e_snapshot_por_id() {
        let st = State::in_memory().unwrap();
        garantir_schema(&st).unwrap();
        let agora = now_epoch();

        let p1 = amostra(agora - 30, "flux-vipers-dev", "c-app-1", 95.0, 480.0, 512.0);
        let snapshot = snapshot_json(&p1, std::slice::from_ref(&p1));
        let id1 = registrar_peak(&st, &p1, "cpu_percent", 95.0, CPU_PEAK_PCT, &snapshot).unwrap();

        let p2 = amostra(agora - 90, "flux-outra-dev", "c-fora", 10.0, 500.0, 512.0);
        let id2 =
            registrar_peak(&st, &p2, "mem_used_mb", 500.0, MEM_PEAK_FRAC * 512.0, "{}").unwrap();

        // Sem filtro: os dois, mais recente primeiro.
        let todos = peaks(&st, 60, None);
        assert_eq!(todos.len(), 2);
        assert_eq!(todos[0].container, "c-app-1");
        assert_eq!(todos[0].metric, "cpu_percent");
        assert_eq!(todos[0].valor, 95.0);
        assert_eq!(todos[0].threshold, CPU_PEAK_PCT);
        assert_eq!(todos[1].container, "c-fora");

        // Filtro de projeto: LIKE flux-vipers-%.
        let so_vipers = peaks(&st, 60, Some("vipers"));
        assert_eq!(so_vipers.len(), 1);
        assert_eq!(so_vipers[0].id, id1);

        // Janela: 1 minuto — p1 (agora-30) dentro, p2 (agora-90) fora. Margem
        // de 30 s dos inserts para o teste não flutuar com o relógio correndo.
        let janela = peaks(&st, 1, None);
        assert_eq!(janela.len(), 1);
        assert_eq!(janela[0].container, "c-app-1");

        // Snapshot por id: corpo JSON, inexistente = None.
        let s = peak_snapshot(&st, id1).unwrap();
        let v: Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["image"], "laravel:teste");
        assert!(peak_snapshot(&st, id2).is_some());
        assert!(
            peak_snapshot(&st, 999_999).is_none(),
            "id inexistente = None"
        );
    }

    /// GATED: exige engine de Docker (`FLUX_DOCKER_URL`, convenção do repo —
    /// NUNCA definida aqui). Um ciclo inteiro contra o engine real: não pode
    /// errar e, havendo container `flux-*` rodando, o rollup ganha linhas.
    #[tokio::test(flavor = "multi_thread")]
    async fn ciclo_com_engine_real_coleta_e_grava() {
        if std::env::var("FLUX_DOCKER_URL").is_err() {
            eprintln!("pulado: FLUX_DOCKER_URL ausente — coleta real não testada");
            return;
        }
        let Some(d) = crate::docker::DockerSource::connect() else {
            eprintln!("pulado: conexão com o Docker falhou mesmo com FLUX_DOCKER_URL");
            return;
        };
        let st = State::in_memory().unwrap();
        garantir_schema(&st).unwrap();
        ciclo(&d, &st)
            .await
            .unwrap_or_else(|e| panic!("ciclo real falhou: {e}"));
        let n: i64 = st
            .with(|c| c.query_row("SELECT COUNT(*) FROM metrics_1min", [], |r| r.get(0)))
            .unwrap();
        println!("observe: {n} buckets gravados no ciclo real");
    }
}
