//! Vigia de anomalia (24/10 — lição dos incidentes de 22 e 24/10): o access
//! gravava TUDO e entendia NADA — o loop de reload rodou duas vezes por horas
//! sem ninguém saber, porque o padrão não era um request estranho, era um
//! padrão normal demais repetido (IP confiável, tudo 200). Este módulo roda no
//! ciclo do sampler (60 s), olha a janela recente de `events` e dispara UM
//! evento `warn` escopo `anomalia` quando um dos sinais casa. NUNCA bloqueia:
//! ontem o "atacante" era o próprio operador — o vigia acorda, quem decide é
//! o Gabriel (o pânico segue sendo o redroom manual).
//!
//! Sinais (limiares derivados do base; pisos nascidos das medições dos
//! incidentes):
//! 1. **Recarga em loop** — mesmo IP carregando o MESMO documento GET 200
//!    mais de [`Limiares::recarga_por_min`]×/min. Página viva não faz isso;
//!    página que se atualiza SOZINHAS sim (meta-refresh, polling HTMX) — por
//!    isso o base é configurável POR PROJETO (26/10, pedido do Gabriel:
//!    suportar atualização a cada 23x/min): `flux vigia set <projeto> <n>`
//!    grava [`CHAVE_BASE`] no estado; hosts fora de projeto usam o default
//!    [`BASE_RECARGA_POR_MIN`].
//! 2. **Martelada** — mesmo IP acima de [`Limiares::martelada_3min`] requests
//!    em 3 min (navegação real fica em 15–30/min; os incidentes de 22 e 24/10
//!    passaram de 75/min por horas).
//! 3. **Dança do websocket** — mais de [`Limiares::ws_por_min`] upgrades
//!    `/@vite/ws` por minuto (o HMR reconecta raro; em 22/10 foram 3.094
//!    tentativas).
//!
//! O vigia olha TODO MUNDO, inclusive IP confiável do gate — decisão do
//! Gabriel: "os piores problemas vieram do dev". Anti-spam: UMA anomalia por
//! métrica+IP a cada [`COOLDOWN_SECS`] s (tabela `vigia_alerta`, schema v6).

use std::collections::HashMap;

use rusqlite::OptionalExtension;

use crate::events;
use crate::state::State;

/// Base do limiar de recarga — default GLOBAL (hosts fora de projeto e
/// projetos sem override). 26/10: 8 → 24. As medições de 24/10 calibraram 8;
/// o painel do próprio fluxd aberto (meta-refresh de 5s = 12/min) provou que
/// 8 grita com tráfego legítimo — e páginas que se atualizam até 23x/min
/// (pedido do Gabriel) precisam passar sem alerta. Comportamento DECLARADO:
/// reload-loop real de 15–24/min deixa de disparar a régua de recarga (o
/// caso WARP real segue coberto pela dança de ws e pela borda de 120/min).
pub const BASE_RECARGA_POR_MIN: i64 = 24;

/// Piso da martelada: a régua que provou pegar os incidentes (75+/min).
pub const PISO_MARTELADA_3MIN: i64 = 180;

/// Piso da dança de ws: HMR saudável reconecta raro — auto-refresh de página
/// não re-upa websocket, então o ws não acompanha o base.
pub const PISO_WS_POR_MIN: i64 = 6;

pub const COOLDOWN_SECS: i64 = 900;

/// Prefixo da settings que guarda o override do base por projeto
/// (`vigia_base|{projeto}` → recargas/min). Gravado por `definir_base`
/// (API `POST /vigia`, CLI `flux vigia set`).
pub const CHAVE_BASE: &str = "vigia_base|";

/// Limiares efetivos derivados do base de um projeto. Os pisos garantem que
/// base pequeno herde EXATAMENTE a régua de 24/10 — derivar nunca afrouxa
/// abaixo do que já provou pegar incidente.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limiares {
    pub recarga_por_min: i64,
    pub martelada_3min: i64,
    pub ws_por_min: i64,
}

/// `recarga = base`; `martelada = max(piso, base×7)`; `ws = max(piso, base÷4)`.
/// Fator 7: com o default 24 o piso ainda domina (168 < 180) — os três
/// efetivos do default são 24/min, 180/3min e 6/min. Bases altos sobem a
/// martelada proporcionalmente (auto-refresh pesado é tráfego legítimo).
pub fn limiares(base: i64) -> Limiares {
    let base = base.max(1);
    Limiares {
        recarga_por_min: base,
        martelada_3min: (base * 7).max(PISO_MARTELADA_3MIN),
        ws_por_min: (base / 4).max(PISO_WS_POR_MIN),
    }
}

/// Grava o override do base de um projeto (1–600 recargas/min).
pub fn definir_base(st: &State, projeto: &str, base: i64) -> Result<(), String> {
    if !(1..=600).contains(&base) {
        return Err(format!(
            "limite base {base} fora da faixa — use 1 a 600 recargas/min"
        ));
    }
    st.with(|c| {
        c.execute(
            "INSERT INTO settings(k, v) VALUES (?1, ?2) \
             ON CONFLICT(k) DO UPDATE SET v = ?2",
            rusqlite::params![format!("{CHAVE_BASE}{projeto}"), base.to_string()],
        )
    })
    .map(|_| ())
    .map_err(|e| e.to_string())
}

/// Remove o override de um projeto — `true` se existia.
pub fn remover_base(st: &State, projeto: &str) -> Result<bool, String> {
    let n = st
        .with(|c| {
            c.execute(
                "DELETE FROM settings WHERE k = ?1",
                [format!("{CHAVE_BASE}{projeto}")],
            )
        })
        .map_err(|e| e.to_string())?;
    Ok(n > 0)
}

/// Overrides gravados `(projeto, base)` — status da API e card do painel.
pub fn bases(st: &State) -> Result<Vec<(String, i64)>, String> {
    st.with(|c| {
        let mut q = c.prepare("SELECT k, v FROM settings WHERE k LIKE ?1")?;
        let rows = q.query_map([format!("{CHAVE_BASE}%")], |r| {
            Ok((
                r.get::<_, String>(0)?
                    .strip_prefix(CHAVE_BASE)
                    .unwrap_or_default()
                    .to_owned(),
                r.get::<_, String>(1)?
                    .parse::<i64>()
                    .unwrap_or(BASE_RECARGA_POR_MIN),
            ))
        })?;
        rows.collect::<Result<Vec<_>, _>>()
    })
    .map_err(|e| e.to_string())
}

/// Base efetivo de um projeto: override gravado ou o default global. Parse
/// quebrado cai no default (gravação é validada em `definir_base`).
pub fn base_de(st: &State, projeto: &str) -> i64 {
    st.with(|c| {
        c.query_row(
            "SELECT v FROM settings WHERE k = ?1",
            [format!("{CHAVE_BASE}{projeto}")],
            |r| r.get::<_, String>(0),
        )
        .optional()
    })
    .ok()
    .flatten()
    .and_then(|v| v.parse().ok())
    .unwrap_or(BASE_RECARGA_POR_MIN)
}

/// `(caminho, host)` da linha de access — formato canônico de
/// `events::linha_access` (`GET {path} {status} {dur}ms host={host} …`).
/// Linha sem host não entra na régua (agrupamento é por host+caminho).
fn caminho_host(message: &str) -> Option<(&str, &str)> {
    let mut t = message.split(' ');
    t.next()?; // método
    let caminho = t.next()?;
    let host = t.find_map(|p| p.strip_prefix("host="))?;
    Some((caminho, host))
}

/// Um ciclo do vigia (chamado do sampler de 60 s): varre os 3 sinais sobre a
/// janela recente e grava `settings.vigia_ultimo_ciclo` — o painel mostra o
/// último ciclo pra provar que o vigia está VIVO (detector morto em silêncio
/// é pior que detector ausente). A leitura do base é NO ciclo: override novo
/// vale no ciclo seguinte (≤60 s), sem reiniciar o fluxd.
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

    // 1) Recarga em loop: mesmo IP + mesma página (HOST + caminho), GET 200,
    //    na janela de 60 s. O host entrou no agrupamento (26/10): a `/` do
    //    painel e a `/` de um projeto não são a mesma página. O limiar é o do
    //    projeto dono do host, senão o default global.
    match recargas_anomalas(st) {
        Ok(mut lista) => {
            lista.sort_by(|a, b| b.n.cmp(&a.n));
            for r in lista.into_iter().take(5) {
                alerta(
                    st,
                    agora,
                    "recarga",
                    &r.ip,
                    format!(
                        "mesma página recarregada {n}x/min (limiar {limite}) — \
                         caminho `{caminho}` no host `{host}`",
                        n = r.n,
                        limite = r.limite,
                        caminho = r.caminho,
                        host = r.host,
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
        let rows = q.query_map([PISO_MARTELADA_3MIN], |r| {
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
                    format!("{n} requests em 3 min (limiar {PISO_MARTELADA_3MIN}) — top: {top}"),
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
        let rows = q.query_map([PISO_WS_POR_MIN], |r| {
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
                        "{n} upgrades de websocket HMR/min (limiar {PISO_WS_POR_MIN}) — \
                         HMR reconectando em loop"
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

/// Uma recarga anômala da janela: página (host+caminho) que o IP carregou
/// `n` vezes com limiar efetivo `limite` do projeto dono do host.
struct RecargaAnomala {
    ip: String,
    host: String,
    caminho: String,
    n: i64,
    limite: i64,
}

/// Recargas anômalas da janela — agrupamento no Rust sobre as linhas da
/// janela (volume de 60 s é pequeno) porque o limiar depende do PROJETO do
/// host, e isso é mapa do estado, não expressão SQL.
fn recargas_anomalas(st: &State) -> Result<Vec<RecargaAnomala>, String> {
    let overrides = bases(st)?;
    let hosts: HashMap<String, String> = st
        .with(|c| {
            let mut q = c.prepare("SELECT host, project FROM project_hosts")?;
            let rows = q.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
            rows.collect::<Result<HashMap<_, _>, _>>()
        })
        .map_err(|e| e.to_string())?;
    let linhas: Vec<(String, String)> = st
        .with(|c| {
            let mut q = c.prepare(
                "SELECT ip, message FROM events \
                 WHERE scope='access' AND ip != '' \
                   AND created_at >= datetime('now','-60 seconds') \
                   AND message LIKE 'GET % 200 %' \
                   AND message NOT LIKE '%/@vite%' \
                   AND message NOT LIKE '%/livewire%'",
            )?;
            let rows = q.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
            rows.collect()
        })
        .map_err(|e| e.to_string())?;

    let mut contagem: HashMap<(String, String, String), i64> = HashMap::new();
    for (ip, message) in &linhas {
        if let Some((caminho, host)) = caminho_host(message) {
            // API não é recarga de página (03/11): os GETs do próprio painel
            // (/api/* — fetch de snapshot, SSE de follow) bateriam na régua
            // quando vários painéis abertos reconectam junto (4G trocando de
            // torre, fluxd reiniciado). Burst de reconexão não é reload — quem
            // martela /api/* continua coberto pela régua de 3 min.
            if caminho.starts_with("/api/") {
                continue;
            }
            *contagem
                .entry((ip.clone(), host.to_owned(), caminho.to_owned()))
                .or_insert(0) += 1;
        }
    }
    Ok(contagem
        .into_iter()
        .map(|((ip, host, caminho), n)| {
            let base = hosts
                .get(&host)
                .and_then(|proj| overrides.iter().find(|(p, _)| p == proj))
                .map(|(_, b)| *b)
                .unwrap_or(BASE_RECARGA_POR_MIN);
            RecargaAnomala {
                ip,
                host,
                caminho,
                n,
                limite: limiares(base).recarga_por_min,
            }
        })
        .filter(|r| r.n > r.limite)
        .collect())
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

    /// Access sintético com HOST explícito (o agrupamento de recarga é por
    /// host+caminho desde 26/10).
    fn acessar_host(st: &State, ip: &str, host: &str, path: &str, ha: &str) {
        let msg = format!("GET {path} 200 100ms host={host} ip={ip} xff={ip}");
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
    fn limiares_derivam_do_base_com_piso() {
        let l = limiares(BASE_RECARGA_POR_MIN);
        assert_eq!(l.recarga_por_min, 24, "base default é a própria recarga");
        assert_eq!(l.martelada_3min, PISO_MARTELADA_3MIN, "piso domina em 24");
        assert_eq!(l.ws_por_min, PISO_WS_POR_MIN, "piso do ws domina em 24");
        // Base da régua antiga herda a régua antiga inteira.
        let l8 = limiares(8);
        assert_eq!(
            (l8.recarga_por_min, l8.martelada_3min, l8.ws_por_min),
            (8, 180, 6)
        );
        // Base alto sobe a martelada proporcionalmente e nunca abaixa o piso.
        let l100 = limiares(100);
        assert_eq!(l100.martelada_3min, 700);
        assert_eq!(l100.ws_por_min, 25);
        assert_eq!(limiares(0).recarga_por_min, 1, "base nunca é zero");
    }

    #[test]
    fn definir_base_valida_faixa_e_remove_volta_ao_default() {
        let a = app();
        assert!(definir_base(state(&a), "loja", 0).is_err(), "zero recusa");
        assert!(definir_base(state(&a), "loja", 601).is_err(), "601 recusa");
        assert!(definir_base(state(&a), "loja", 40).is_ok());
        assert_eq!(base_de(state(&a), "loja"), 40);
        assert_eq!(base_de(state(&a), "outro"), BASE_RECARGA_POR_MIN);
        assert!(remover_base(state(&a), "loja").unwrap());
        assert_eq!(base_de(state(&a), "loja"), BASE_RECARGA_POR_MIN);
        assert!(
            !remover_base(state(&a), "loja").unwrap(),
            "sem override = false"
        );
    }

    #[test]
    fn caminho_host_extrai_do_formato_canonico() {
        let linha = "GET /cms/rides 200 120ms host=loja-d.bitello.xyz ip=1.2.3.4 xff=x";
        assert_eq!(
            caminho_host(linha),
            Some(("/cms/rides", "loja-d.bitello.xyz"))
        );
        assert_eq!(caminho_host("vigia: anomalia qualquer"), None);
    }

    #[test]
    fn recarga_em_loop_dispara_e_cooldown_segura() {
        let a = app();
        for _ in 0..(BASE_RECARGA_POR_MIN + 1) {
            acessar(state(&a), "203.0.113.9", "GET", "/cms", 200, "-30 seconds");
        }
        ciclo(state(&a));
        assert_eq!(anomalias(state(&a)), 1, "recarga em loop = 1 anomalia");
        // Cooldown: ciclo imediatamente seguinte NÃO duplica.
        ciclo(state(&a));
        assert_eq!(anomalias(state(&a)), 1, "cooldown de 15 min segura");
    }

    /// Painel com vários navegadores abertos reconectando o SSE junto (5
    /// painéis, 2 em realtime, 03/11): burst de GET /api/* NÃO é recarga de
    /// página — a régua de recarga ignora; a de página real segue viva.
    #[test]
    fn burst_da_api_do_painel_nao_e_recarga() {
        let a = app();
        for _ in 0..(BASE_RECARGA_POR_MIN * 3) {
            acessar(
                state(&a),
                "203.0.113.9",
                "GET",
                "/api/dashboard/follow",
                200,
                "-30 seconds",
            );
            acessar(
                state(&a),
                "203.0.113.10",
                "GET",
                "/api/events?page=2",
                200,
                "-30 seconds",
            );
        }
        ciclo(state(&a));
        assert_eq!(
            anomalias(state(&a)),
            0,
            "martelada de /api/* não vira anomalia de recarga"
        );
        // A mesma rajada numa PÁGINA continua disparando (a régua não morreu):
        let b = app();
        for _ in 0..(BASE_RECARGA_POR_MIN + 1) {
            acessar(state(&b), "203.0.113.9", "GET", "/", 200, "-30 seconds");
        }
        ciclo(state(&b));
        assert_eq!(anomalias(state(&b)), 1, "recarga de página segue detectada");
    }

    #[test]
    fn navegacao_normal_nao_dispara_nada() {
        let a = app();
        for i in 0..6 {
            acessar(
                state(&a),
                "203.0.113.9",
                "GET",
                &format!("/pagina-{i}"),
                200,
                "-30 seconds",
            );
        }
        ciclo(state(&a));
        assert_eq!(anomalias(state(&a)), 0, "paths distintos = navegação");
    }

    #[test]
    fn override_do_projeto_muda_o_limiar_na_hora() {
        let a = app();
        a.state
            .register_project("loja", "framework = \"node\"\n", "h")
            .unwrap();
        a.state
            .set_project_hosts("loja", &[("loja-d.bitello.xyz".into(), true)])
            .unwrap();
        definir_base(state(&a), "loja", 40).unwrap();
        // 25 recargas (limiar default) no host do projeto: com base 40, NÃO é anomalia.
        for _ in 0..25 {
            acessar_host(
                state(&a),
                "203.0.113.9",
                "loja-d.bitello.xyz",
                "/",
                "-30 seconds",
            );
        }
        ciclo(state(&a));
        assert_eq!(anomalias(state(&a)), 0, "override de 40 tolera 25/min");
        // Passa de 40: dispara com o limiar do projeto na mensagem.
        for _ in 0..16 {
            acessar_host(
                state(&a),
                "203.0.113.9",
                "loja-d.bitello.xyz",
                "/",
                "-30 seconds",
            );
        }
        ciclo(state(&a));
        assert_eq!(anomalias(state(&a)), 1, "41/min estoura o override de 40");
        let msg: String = state(&a)
            .with(|c| {
                c.query_row(
                    "SELECT message FROM events WHERE scope='anomalia' LIMIT 1",
                    [],
                    |r| r.get(0),
                )
            })
            .unwrap();
        assert!(
            msg.contains("(limiar 40)"),
            "limiar efetivo no texto: {msg}"
        );
    }

    #[test]
    fn hosts_diferentes_nao_somam_na_regua_de_recarga() {
        let a = app();
        // Painel (host sem projeto) e projeto no MESMO IP: cada página com
        // metade do limiar — soma passaria do teto, mas são páginas distintas.
        for _ in 0..(BASE_RECARGA_POR_MIN / 2) {
            acessar_host(
                state(&a),
                "203.0.113.9",
                "flux.bitello.xyz",
                "/",
                "-30 seconds",
            );
            acessar_host(
                state(&a),
                "203.0.113.9",
                "loja-d.bitello.xyz",
                "/",
                "-30 seconds",
            );
        }
        ciclo(state(&a));
        assert_eq!(anomalias(state(&a)), 0, "host separa a régua");
    }

    #[test]
    fn martelada_acima_do_teto_dispara() {
        let a = app();
        for _ in 0..(PISO_MARTELADA_3MIN + 10) {
            acessar(
                state(&a),
                "198.51.100.5",
                "POST",
                "/livewire/update",
                200,
                "-100 seconds",
            );
        }
        ciclo(state(&a));
        assert!(anomalias(state(&a)) >= 1, "martelada dispara");
        let msg: String = state(&a)
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
        let a = app();
        for _ in 0..(PISO_WS_POR_MIN + 2) {
            acessar(
                state(&a),
                "198.51.100.7",
                "GET",
                "/@vite/ws?token=x",
                101,
                "-20 seconds",
            );
        }
        ciclo(state(&a));
        assert!(anomalias(state(&a)) >= 1, "dança de ws dispara");
    }

    #[test]
    fn ip_vazio_nunca_alerta() {
        let a = app();
        for _ in 0..50 {
            acessar(state(&a), "", "GET", "/cms", 200, "-30 seconds");
        }
        ciclo(state(&a));
        assert_eq!(anomalias(state(&a)), 0, "linha sem ip não entra na régua");
    }

    #[test]
    fn ciclo_deixa_prova_de_vida() {
        let a = app();
        ciclo(state(&a));
        let marca: Option<String> = state(&a)
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
