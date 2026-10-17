//! Estado de sync (mutagen) no fluxd — hub entre o `flux watch` no HOST
//! Windows e o painel web. O watch reporta o estado das sessões (POST
//! /sync/report → `upsert_report`), o painel lê (GET /sync/status →
//! `sync_states`) e pede rescan (POST /sync/rescan → `request_rescan`); o
//! watch consome o pedido e o limpa com `rescan_applied` no relato seguinte.
//! Persistência na tabela `sync_status` (state.rs, schema v3). Erros de
//! escrita propagam com origem "sync:" — nada silencioso.

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::events;

/// Uma linha de `sync_status` pronta para o painel — campos EXATAMENTE os do
/// contrato do GET /sync/status (o painel consome o JSON direto).
#[derive(Debug, Clone, Serialize)]
pub struct SyncRow {
    pub project: String,
    pub status: String,
    pub paused: bool,
    pub files: Option<u64>,
    pub problems: u64,
    pub last_error: Option<String>,
    /// RFC3339 UTC, relógio do banco (`strftime('%Y-%m-%dT%H:%M:%SZ','now')`
    /// — o mesmo padrão de relógio do resto do state.rs).
    pub updated_at: String,
    pub rescan_requested: bool,
}

/// Corpo do POST /sync/report — o relato de UMA sessão do watch. No conflito
/// de chave (mesmo projeto) o upsert substitui o relato anterior; campos
/// ausentes no fio assumem neutro (`#[serde(default)]`).
#[derive(Debug, Deserialize)]
pub struct SyncReport {
    pub project: String,
    pub session: String,
    pub status: String,
    #[serde(default)]
    pub paused: bool,
    #[serde(default)]
    pub files: Option<u64>,
    #[serde(default)]
    pub problems: u64,
    #[serde(default)]
    pub last_error: Option<String>,
    /// `true` = o watch APLICOU o rescan pendente → limpa a flag do projeto.
    #[serde(default)]
    pub rescan_applied: bool,
}

/// Grava (upsert) o relato do watch. `rescan_applied` limpa a flag de rescan
/// do projeto; sem applied, a flag é PRESERVADA — o pedido do painel
/// sobrevive aos relatos seguintes do watch. Linha nova nasce com flag 0:
/// rescan só existe depois de um `request_rescan`.
pub fn upsert_report(state: &AppState, report: &SyncReport) -> Result<(), String> {
    // Flag ANTES do upsert: só vale evento quando o applied de fato limpou
    // um pedido pendente (applied sem pedido não é transição).
    let flag_antes = state
        .state
        .with(|c| {
            c.query_row(
                "SELECT rescan_requested FROM sync_status WHERE project = ?1",
                params![report.project],
                |row| row.get::<_, i64>(0),
            )
            .optional()
        })
        .map_err(|e| {
            format!(
                "sync: falha lendo flag de rescan de {:?}: {e}",
                report.project
            )
        })?
        .map(|v| v != 0)
        .unwrap_or(false);
    state
        .state
        .with(|c| {
            c.execute(
                "INSERT INTO sync_status(
                     project, session, status, paused, files, problems,
                     last_error, rescan_requested, updated_at
                 )
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0,
                         strftime('%Y-%m-%dT%H:%M:%SZ','now'))
                 ON CONFLICT(project) DO UPDATE SET
                    session = excluded.session,
                    status = excluded.status,
                    paused = excluded.paused,
                    files = excluded.files,
                    problems = excluded.problems,
                    last_error = excluded.last_error,
                    rescan_requested = CASE WHEN ?8 = 1 THEN 0
                                       ELSE sync_status.rescan_requested END,
                    updated_at = strftime('%Y-%m-%dT%H:%M:%SZ','now')",
                params![
                    report.project,
                    report.session,
                    report.status,
                    report.paused as i64,
                    report.files.and_then(|f| i64::try_from(f).ok()),
                    i64::try_from(report.problems).unwrap_or(i64::MAX),
                    report.last_error,
                    report.rescan_applied as i64,
                ],
            )?;
            Ok(())
        })
        .map_err(|e| {
            format!(
                "sync: falha gravando relato de sync de {:?}: {e}",
                report.project
            )
        })?;
    if report.rescan_applied && flag_antes {
        events::event(
            &state.state,
            "info",
            "sync",
            &format!(
                "rescan de {:?} aplicado pelo watch (sessão {})",
                report.project, report.session
            ),
        );
    }
    Ok(())
}

/// Linhas de sync para o painel (`GET /sync/status` e render direto). A
/// assinatura é do contrato fixo (sem `Result`): falha de leitura sai no log
/// com origem e devolve lista vazia — NUNCA estado inventado.
pub fn sync_states(state: &AppState) -> Vec<SyncRow> {
    match state.state.with(|c| {
        let mut stmt = c.prepare(
            "SELECT project, status, paused, files, problems, last_error,
                    rescan_requested, updated_at
             FROM sync_status ORDER BY project",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(SyncRow {
                project: row.get(0)?,
                status: row.get(1)?,
                paused: row.get::<_, i64>(2)? != 0,
                files: row
                    .get::<_, Option<i64>>(3)?
                    .and_then(|f| u64::try_from(f).ok()),
                problems: u64::try_from(row.get::<_, i64>(4)?).unwrap_or(0),
                last_error: row.get(5)?,
                rescan_requested: row.get::<_, i64>(6)? != 0,
                updated_at: row.get(7)?,
            })
        })?;
        rows.collect()
    }) {
        Ok(rows) => rows,
        Err(e) => {
            tracing::error!("sync: falha lendo estados de sync do estado: {e}");
            Vec::new()
        }
    }
}

/// Seta `rescan_requested` do projeto (painel → watch). O projeto precisa
/// estar registrado no fluxd (o 404 do endpoint nasce daqui): flag para
/// fantasma seria pedido que o watch nunca consumiria. Sem relato ainda, a
/// linha nasce vazia ('unknown') só com a flag — e a flag sobrevive a
/// relatos sem `rescan_applied` e a reinícios do watch.
pub fn request_rescan(state: &AppState, project: &str) -> Result<(), String> {
    let registrado: Option<i64> = state
        .state
        .with(|c| {
            c.query_row(
                "SELECT 1 FROM projects WHERE name = ?1",
                params![project],
                |row| row.get(0),
            )
            .optional()
        })
        .map_err(|e| format!("sync: falha consultando projeto {project:?}: {e}"))?;
    if registrado.is_none() {
        return Err(format!(
            "projeto {project:?} não registrado no fluxd — rode `flux up` no repo dele"
        ));
    }
    state
        .state
        .with(|c| {
            c.execute(
                "INSERT INTO sync_status(project, rescan_requested, updated_at)
                 VALUES (?1, 1, strftime('%Y-%m-%dT%H:%M:%SZ','now'))
                 ON CONFLICT(project) DO UPDATE SET
                    rescan_requested = 1,
                    updated_at = strftime('%Y-%m-%dT%H:%M:%SZ','now')",
                params![project],
            )?;
            Ok(())
        })
        .map_err(|e| format!("sync: falha gravando flag de rescan de {project:?}: {e}"))?;
    events::event(
        &state.state,
        "info",
        "sync",
        &format!("rescan de sync solicitado para {project:?}"),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::State as DbState;
    use flux_common::config::GlobalConfig;

    fn app() -> AppState {
        AppState::new(
            DbState::in_memory().unwrap(),
            &GlobalConfig::default(),
            None,
        )
    }

    fn relato(project: &str, status: &str) -> SyncReport {
        SyncReport {
            project: project.to_owned(),
            session: format!("flux-{project}"),
            status: status.to_owned(),
            paused: false,
            files: Some(123),
            problems: 0,
            last_error: None,
            rescan_applied: false,
        }
    }

    #[test]
    fn upsert_e_sync_states_roundtrip_com_flag_preservada() {
        let app = app();
        // Rescan exige projeto registrado (contrato) — registra antes.
        app.state.register_project("vipa", "", "h").unwrap();
        upsert_report(&app, &relato("vipa", "watching")).unwrap();
        let rows = sync_states(&app);
        assert_eq!(rows.len(), 1, "um relato = uma linha (upsert)");
        let r = &rows[0];
        assert_eq!(r.project, "vipa");
        assert_eq!(r.status, "watching");
        assert!(!r.paused);
        assert_eq!(r.files, Some(123));
        assert_eq!(r.problems, 0);
        assert_eq!(r.last_error, None);
        assert!(!r.rescan_requested);
        assert!(
            r.updated_at.contains('T') && r.updated_at.ends_with('Z'),
            "updated_at em RFC3339: {}",
            r.updated_at
        );

        // Segundo relato atualiza a MESMA linha e preserva a flag pedida.
        request_rescan(&app, "vipa").unwrap();
        let mut segundo = relato("vipa", "watching");
        segundo.files = Some(124);
        segundo.last_error = Some("boom".into());
        segundo.paused = true;
        upsert_report(&app, &segundo).unwrap();
        let rows = sync_states(&app);
        assert_eq!(rows.len(), 1, "upsert não duplica");
        assert_eq!(rows[0].files, Some(124));
        assert_eq!(rows[0].last_error.as_deref(), Some("boom"));
        assert!(rows[0].paused);
        assert!(
            rows[0].rescan_requested,
            "relato sem applied NUNCA limpa a flag"
        );

        // Applied limpa a flag.
        segundo.rescan_applied = true;
        upsert_report(&app, &segundo).unwrap();
        assert!(!sync_states(&app)[0].rescan_requested);
    }

    #[test]
    fn rescan_em_projeto_desconhecido_recusa_com_causa() {
        let app = app();
        let err = request_rescan(&app, "fantasma").unwrap_err();
        assert!(err.contains("não registrado"), "erro: {err}");
        assert!(err.contains("fantasma"), "erro nomeia o projeto: {err}");
        assert!(sync_states(&app).is_empty(), "fantasma não cria linha");
    }

    #[test]
    fn rescan_sem_relato_cria_linha_somente_com_a_flag() {
        let app = app();
        app.state.register_project("vipa", "", "h").unwrap();
        request_rescan(&app, "vipa").unwrap();
        let rows = sync_states(&app);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].rescan_requested);
        assert_eq!(
            rows[0].status, "unknown",
            "linha só com a flag: relato nenhum ainda"
        );
        assert_eq!(rows[0].files, None);
        assert_eq!(rows[0].problems, 0);
    }
}
