//! Leitura/consulta do painel (L6.3): TODA SQL da interface vive aqui —
//! sem desenho de HTML (apresentação é paginas.rs).

use flux_common::protocol::{ContainerState, Env};
use rusqlite::params;

use crate::AppState;
use crate::decision::HostSource as _;
use crate::docker;
use crate::sqlite_source;
use crate::state::SleepRow;

pub(super) const EVENTOS_DASHBOARD: i64 = 15;
pub(super) const EVENTOS_POR_PAGINA: i64 = 50;
const CURVA_MINUTOS: usize = 24 * 60;

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
    pub(super) env: Env,
    pub(super) estado: ContainerState,
    pub(super) fonte: &'static str,
    pub(super) row: Result<SleepRow, String>,
    pub(super) aplicadas: Vec<(String, String)>,
}

pub(super) fn ambiente(app: &AppState, name: &str, env: Env) -> PainelEnv {
    let (estado, fonte) = verdade_ambiente(app, name, env);
    PainelEnv {
        env,
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

pub(super) struct EventoRow {
    pub(super) level: String,
    pub(super) scope: String,
    pub(super) message: String,
    pub(super) created_at: String,
}

const COLUNAS_EVENTO: &str = "level, scope, message, created_at";

pub(super) fn row_evento(r: &rusqlite::Row<'_>) -> rusqlite::Result<EventoRow> {
    Ok(EventoRow {
        level: r.get(0)?,
        scope: r.get(1)?,
        message: r.get(2)?,
        created_at: r.get(3)?,
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

/// Página de eventos — a página é CLAMPADA ao total ANTES do fetch (pedir
/// página 99 sem clampa devolveria "página 3" com zero linhas: desconexo).
pub(super) fn eventos_pagina(
    app: &AppState,
    page: i64,
    size: i64,
) -> Result<(i64, Vec<EventoRow>, i64), String> {
    let total: i64 = app
        .state
        .with(|c| c.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0)))
        .map_err(|e| e.to_string())?;
    let paginas = ((total + size - 1) / size).max(1);
    let page = page.min(paginas);
    let offset = (page - 1) * size;
    let rows = app
        .state
        .with(|c| {
            let mut stmt = c.prepare(&format!(
                "SELECT {COLUNAS_EVENTO} FROM events ORDER BY id DESC LIMIT ?1 OFFSET ?2"
            ))?;
            let rows = stmt.query_map(params![size, offset], row_evento)?;
            rows.collect()
        })
        .map_err(|e| e.to_string())?;
    Ok((page, rows, total))
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

pub(super) fn curva_contagem(app: &AppState) -> Result<Option<(Vec<u64>, u64, u64)>, String> {
    let agora = agora_epoch(app)?;
    let buckets: Vec<(i64, i64)> = app
        .state
        .with(|c| {
            let mut stmt = c.prepare(
                "SELECT CAST(strftime('%s', created_at) AS INTEGER) / 60 AS minuto, COUNT(*)
                 FROM events
                 WHERE created_at >= datetime('now', '-24 hours')
                 GROUP BY minuto",
            )?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?;
            rows.collect()
        })
        .map_err(|e| e.to_string())?;

    // 1440 buckets (um por minuto). Timestamps no futuro (relógio adiantado)
    // não entram — dado inventado não.
    let minuto_atual = agora / 60;
    let mut conts = vec![0u64; CURVA_MINUTOS];
    for (minuto, n) in buckets {
        if minuto > minuto_atual {
            continue;
        }
        let base = minuto_atual - (CURVA_MINUTOS as i64 - 1);
        if let Ok(idx) = usize::try_from(minuto - base)
            && idx < CURVA_MINUTOS
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
