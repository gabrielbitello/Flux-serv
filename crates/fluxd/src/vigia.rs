//! Vigia de anomalia (24/10 — lição dos incidentes de 22 e 24/10): o access
//! gravava TUDO e entendia NADA — o loop de reload rodou duas vezes por horas
//! sem ninguém saber, porque o padrão não era um request estranho, era um
//! padrão normal demais repetido (IP confiável, tudo 200). Este módulo roda no
//! ciclo do sampler (60 s), olha a janela recente de `events` e dispara UM
//! evento `warn` escopo `anomalia` quando um dos sinais casa. NUNCA bloqueia:
//! ontem o "atacante" era o próprio operador — o vigia acorda, quem decide é
//! o Gabriel (o pânico segue sendo o redroom manual).
//!
//! Sinais (limiares nascidos das medições dos incidentes; ajustáveis com uso):
//! 1. **Recarga em loop** — mesmo IP carregando o MESMO documento GET 200 mais
//!    de [`RELOAD_POR_MIN`]×/min. Página viva não faz isso.
//! 2. **Martelada** — mesmo IP acima de [`MARTELADA_EM_3MIN`] requests em 3 min.
//! 3. **Dança do websocket** — mais de [`WS_POR_MIN`] upgrades `/@vite/ws` por
//!    minuto (o HMR reconecta raro; em 22/10 foram 3.094 tentativas).
//!
//! O vigia olha TODO MUNDO, inclusive IP confiável do gate — decisão do
//! Gabriel: "os piores problemas vieram do dev". Anti-spam: UMA anomalia por
//! métrica+IP a cada [`COOLDOWN_SECS`] s (tabela `vigia_alerta`, schema v6).

use crate::events;
use crate::state::State;
use rusqlite::OptionalExtension;

pub const RELOAD_POR_MIN: i64 = 8;
pub const MARTELADA_EM_3MIN: i64 = 180;
pub const WS_POR_MIN: i64 = 6;
pub const COOLDOWN_SECS: i64 = 900;

/// Um ciclo do vigia (chamado do sampler de 60 s): varre os 3 sinais sobre a
/// janela recente e grava `settings.vigia_ultimo_ciclo` — o painel mostra o
/// último ciclo pra provar que o vigia está VIVO (detector morto em silêncio
/// é pior que detector ausente).
pub fn ciclo(st: &State) {
    let agora = match st.with(|c| {
        c.query_row("SELECT CAST(strftime('%s','now') AS INTEGER)", [], |r| {
            r.get(0)
        })
    }) {
        Ok(a) => a,
        Err(e) => {
            tracing::error!("vigia: sem relógio do estado: {e}");
            return;
        }
    };

    // 1) Recarga em loop: mesmo IP + mesmo documento, GET 200, na janela de 60 s.
    let recargas = st.with(|c| {
        let mut q = c.prepare(
            "SELECT ip, substr(message, instr(message,' ')+1, \
                    instr(substr(message, instr(message,' ')+1), ' ')-1) AS p, \
                    COUNT(*) AS n \
             FROM events \
             WHERE scope='access' AND ip != '' \
               AND created_at >= datetime('now','-60 seconds') \
               AND message LIKE 'GET % 200 %' \
               AND message NOT LIKE '%/@vite%' \
               AND message NOT LIKE '%/livewire%' \
             GROUP BY ip, p HAVING n > ?1 ORDER BY n DESC LIMIT 5",
        )?;
        let rows = q.query_map([RELOAD_POR_MIN], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?;
        rows.collect::<Result<Vec<_>, _>>()
    });
    match recargas {
        Ok(lista) => {
            for (ip, path, n) in lista {
                alerta(
                    st,
                    agora,
                    "recarga",
                    &ip,
                    format!(
                        "mesma página recarregada {n}x/min (limiar {RELOAD_POR_MIN}) — caminho `{path}`"
                    ),
                );
            }
        }
        Err(e) => tracing::error!("vigia: consulta de recarga falhou: {e}"),
    }

    // 2) Martelada: 1 IP acima do teto em 3 minutos (navegação real fica em
    //    15–30/min; os incidentes de 22 e 24/10 passaram de 75/min por horas).
    let martelada = st.with(|c| {
        let mut q = c.prepare(
            "SELECT ip, COUNT(*) AS n \
             FROM events \
             WHERE scope='access' AND ip != '' \
               AND created_at >= datetime('now','-180 seconds') \
             GROUP BY ip HAVING n > ?1 ORDER BY n DESC LIMIT 5",
        )?;
        let rows = q.query_map([MARTELADA_EM_3MIN], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })?;
        rows.collect::<Result<Vec<_>, _>>()
    });
    match martelada {
        Ok(lista) => {
            for (ip, n) in lista {
                let top = top_caminhos(st, &ip);
                alerta(
                    st,
                    agora,
                    "martelada",
                    &ip,
                    format!("{n} requests em 3 min (limiar {MARTELADA_EM_3MIN}) — top: {top}"),
                );
            }
        }
        Err(e) => tracing::error!("vigia: consulta de martelada falhou: {e}"),
    }

    // 3) Dança do websocket: upgrades /@vite/ws em excesso = HMR brigando
    //    (proxy, WARP, VPN ou loop de reload do outro lado).
    let danca = st.with(|c| {
        let mut q = c.prepare(
            "SELECT ip, COUNT(*) AS n \
             FROM events \
             WHERE scope='access' AND ip != '' \
               AND created_at >= datetime('now','-60 seconds') \
               AND message LIKE '%/@vite/ws% 101 %' \
             GROUP BY ip HAVING n > ?1 ORDER BY n DESC LIMIT 5",
        )?;
        let rows = q.query_map([WS_POR_MIN], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })?;
        rows.collect::<Result<Vec<_>, _>>()
    });
    match danca {
        Ok(lista) => {
            for (ip, n) in lista {
                alerta(
                    st,
                    agora,
                    "ws",
                    &ip,
                    format!(
                        "{n} upgrades de websocket HMR/min (limiar {WS_POR_MIN}) — HMR reconectando em loop"
                    ),
                );
            }
        }
        Err(e) => tracing::error!("vigia: consulta de ws falhou: {e}"),
    }

    let _ = st.with(|c| {
        c.execute(
            "INSERT INTO settings(k, v) VALUES ('vigia_ultimo_ciclo', ?1) \
             ON CONFLICT(k) DO UPDATE SET v = ?1",
            [agora.to_string()],
        )
    });
}

/// Os 3 caminhos mais batidos do IP na última hora — o evento precisa deixar a
/// primeira pista colada no alerta (não fazer o Gabriel caçar de novo).
fn top_caminhos(st: &State, ip: &str) -> String {
    st.with(|c| {
        let mut q = c.prepare(
            "SELECT substr(message, instr(message,' ')+1, \
                    instr(substr(message, instr(message,' ')+1), ' ')-1) AS p, \
                    COUNT(*) AS n \
             FROM events \
             WHERE scope='access' AND ip = ?1 \
               AND created_at >= datetime('now','-60 minutes') \
             GROUP BY p ORDER BY n DESC LIMIT 3",
        )?;
        let rows = q.query_map([ip], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        let lista: Vec<String> = rows
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(|(p, n)| format!("{p} x{n}"))
            .collect();
        Ok(if lista.is_empty() {
            "—".to_owned()
        } else {
            lista.join(", ")
        })
    })
    .unwrap_or_else(|e| {
        tracing::error!("vigia: top caminhos falhou: {e}");
        "—".to_owned()
    })
}

/// Dispara a anomalia respeitando o cooldown por métrica+IP: tempestade não
/// enche a tabela, e o evento carrega o IP na coluna (agregável por SQL).
fn alerta(st: &State, agora: i64, metrica: &str, ip: &str, detalhe: String) {
    let chave = format!("{metrica}|{ip}");
    let recente: Option<i64> = st
        .with(|c| {
            c.query_row(
                "SELECT ultima_em FROM vigia_alerta WHERE chave = ?1",
                [&chave],
                |r| r.get(0),
            )
            .optional()
        })
        .ok()
        .flatten();
    if let Some(ultima) = recente
        && agora - ultima < COOLDOWN_SECS
    {
        return;
    }
    let _ = st.with(|c| {
        c.execute(
            "INSERT INTO vigia_alerta(chave, ultima_em) VALUES (?1, ?2) \
             ON CONFLICT(chave) DO UPDATE SET ultima_em = ?2",
            rusqlite::params![chave, agora],
        )
    });
    events::event_com_ip(
        st,
        "warn",
        "anomalia",
        &format!("vigia: {metrica} — {detalhe}"),
        ip,
    );
    tracing::warn!(target: "vigia", metrica, ip, "vigia: anomalia detectada");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AppState;
    use flux_common::config::GlobalConfig;
    use std::sync::Arc;

    fn app() -> Arc<crate::AppState> {
        Arc::new(crate::AppState::new(
            crate::state::State::in_memory().unwrap(),
            &GlobalConfig::default(),
            None,
        ))
    }

    fn state(app: &Arc<AppState>) -> &State {
        &app.state
    }

    /// Insere um access sintético com IP e created_at controlados.
    fn acessar(st: &State, ip: &str, method: &str, path: &str, status: u16, ha: &str) {
        let msg = format!("{method} {path} {status} 100ms host=teste ip={ip} xff={ip}");
        st.with(|c| {
            c.execute(
                "INSERT INTO events(level, scope, message, ip, created_at) \
                 VALUES ('info', 'access', ?1, ?2, datetime('now', ?3))",
                rusqlite::params![msg, ip, ha],
            )
        })
        .unwrap();
    }

    fn anomalias(st: &State) -> i64 {
        st.with(|c| {
            c.query_row(
                "SELECT COUNT(*) FROM events WHERE scope='anomalia'",
                [],
                |r| r.get(0),
            )
        })
        .unwrap()
    }

    #[test]
    fn recarga_em_loop_dispara_e_cooldown_segura() {
        let app = app();
        for _ in 0..10 {
            acessar(
                state(&app),
                "203.0.113.9",
                "GET",
                "/cms",
                200,
                "-30 seconds",
            );
        }
        ciclo(state(&app));
        assert_eq!(anomalias(state(&app)), 1, "recarga em loop = 1 anomalia");
        // Cooldown: ciclo imediatamente seguinte NÃO duplica.
        ciclo(state(&app));
        assert_eq!(anomalias(state(&app)), 1, "cooldown de 15 min segura");
    }

    #[test]
    fn navegacao_normal_nao_dispara_nada() {
        let app = app();
        for i in 0..6 {
            acessar(
                state(&app),
                "203.0.113.9",
                "GET",
                &format!("/pagina-{i}"),
                200,
                "-30 seconds",
            );
        }
        ciclo(state(&app));
        assert_eq!(anomalias(state(&app)), 0, "paths distintos = navegação");
    }

    #[test]
    fn martelada_acima_do_teto_dispara() {
        let app = app();
        for _ in 0..(MARTELADA_EM_3MIN + 10) {
            acessar(
                state(&app),
                "198.51.100.5",
                "POST",
                "/livewire/update",
                200,
                "-100 seconds",
            );
        }
        ciclo(state(&app));
        assert!(anomalias(state(&app)) >= 1, "martelada dispara");
        let msg: String = state(&app)
            .with(|c| {
                c.query_row(
                    "SELECT message FROM events WHERE scope='anomalia' LIMIT 1",
                    [],
                    |r| r.get(0),
                )
            })
            .unwrap();
        assert!(msg.contains("martelada"), "mensagem: {msg}");
        assert!(msg.contains("top:"), "evento carrega top caminhos: {msg}");
    }

    #[test]
    fn danca_de_websocket_dispara() {
        let app = app();
        for _ in 0..(WS_POR_MIN + 2) {
            acessar(
                state(&app),
                "198.51.100.7",
                "GET",
                "/@vite/ws?token=x",
                101,
                "-20 seconds",
            );
        }
        ciclo(state(&app));
        assert!(anomalias(state(&app)) >= 1, "dança de ws dispara");
    }

    #[test]
    fn ip_vazio_nunca_alerta() {
        let app = app();
        for _ in 0..50 {
            acessar(state(&app), "", "GET", "/cms", 200, "-30 seconds");
        }
        ciclo(state(&app));
        assert_eq!(anomalias(state(&app)), 0, "linha sem ip não entra na régua");
    }

    #[test]
    fn ciclo_deixa_prova_de_vida() {
        let app = app();
        ciclo(state(&app));
        let marca: Option<String> = state(&app)
            .with(|c| {
                c.query_row(
                    "SELECT v FROM settings WHERE k = 'vigia_ultimo_ciclo'",
                    [],
                    |r| r.get(0),
                )
                .optional()
            })
            .unwrap();
        assert!(marca.is_some(), "vigia vivo deixa marca do último ciclo");
    }
}
