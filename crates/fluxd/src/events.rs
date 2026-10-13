//! Escrita de eventos no SQLite (07): log de acesso estruturado (host, método,
//! caminho, status, duração — sem corpo) + trilha geral para o painel (08).
//! Falha ao gravar é logada em nível erro, nunca engolida.

use crate::state::State;

pub fn access(st: &State, host: &str, method: &str, path: &str, status: u16, dur_ms: u128) {
    let message = format!("{method} {path} {status} {dur_ms}ms host={host}");
    event(st, "info", "access", &message);
}

pub fn event(st: &State, level: &str, scope: &str, message: &str) {
    let r = st.with(|c| {
        c.execute(
            "INSERT INTO events(level, scope, message) VALUES (?1, ?2, ?3)",
            rusqlite::params![level, scope, message],
        )
    });
    if let Err(e) = r {
        tracing::error!("falha ao gravar evento no estado ({scope}): {e}");
    }
}
