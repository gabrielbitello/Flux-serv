//! Beat do sono (spec 04): `POST /__flux/beat` recebe o sinal do overlay JS
//! injetado pelo proxy (mesmo desenho do porteiro) e zera o timer de
//! ociosidade com `touch_interaction`. A decisão de quem é host gerenciado é
//! a MESMA do router (03): `ManagedHost::parse` com `project_exists` vindo do
//! engine Docker quando existe (`app.docker`), SQLite como fallback (dev e
//! testes sem engine). Projeto registrado (Prod ou Dev) → 204 vazio; painel,
//! fora da zona ou projeto fantasma → 404 SEM tocar em nada (só container de
//! app dorme/acorda, 04). Erro gravando o sinal nunca é silencioso: loga com
//! causa — a batida é fire-and-forget para o navegador (`sendBeacon` ignora a
//! resposta), então 204 segue de pé com o erro registrado.

use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use flux_common::host::ManagedHost;
use flux_common::protocol::Env;

use crate::AppState;
use crate::decision::HostSource;
use crate::docker::DockerSource;
use crate::sqlite_source::SqliteSource;

/// JS do overlay (04, "Sinais que mantêm acordado"): zero UI — só escuta.
/// Eventos idênticos aos do porteiro (`mousemove, mousedown, keydown, wheel,
/// scroll, touchstart`), `navigator.sendBeacon('/__flux/beat')` com throttle
/// de 30 s (a batida sobrevive ao unload da página — decisão da 04 contra
/// websocket dedicado) e beat de intervalo a cada 60 s. O throttle vale para
/// eventos E intervalo: distância efetiva entre batidas fica entre 30 e 60 s.
/// Uma linha, sem dependência — o proxy (`proxy::injeta_script`) costura
/// dentro de `<script>`.
pub const SCRIPT: &str = "(function(){var u=0;function b(){var n=Date.now();if(n-u<30000)return;u=n;try{navigator.sendBeacon('/__flux/beat')}catch(e){}}['mousemove','mousedown','keydown','wheel','scroll','touchstart'].forEach(function(e){window.addEventListener(e,b,{passive:true})});setInterval(b,60000)})();";

/// Handler de `POST /__flux/beat` (rota montada pelo integrador, estado
/// `Arc<AppState>`): host gerenciado → zera o timer e 204 vazio; host não
/// gerenciado → 404 sem tocar em nada.
pub async fn beat(State(app): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let started = std::time::Instant::now();
    let host_raw = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    // Fonte da verdade = a mesma do router (03): engine quando existe,
    // SQLite como fallback. `Docker` é clone barato (Arc interno) — o
    // próprio router faz o mesmo por request.
    let is_project = |n: &str| match &app.docker {
        Some(d) => DockerSource::new(d.clone(), &app.state).project_exists(n),
        None => SqliteSource::new(&app.state).project_exists(n),
    };
    let host_custom = |h: &str| SqliteSource::new(&app.state).host_custom(h);
    match ManagedHost::parse_custom(host_raw, &app.domain, &is_project, &host_custom) {
        ManagedHost::Prod { name } | ManagedHost::Custom { name, dev: false } => {
            tocar(&app.state, &name, Env::Prod)
        }
        // Vite (dev server, 06/live) mantém o MESMO container acordado; host
        // custom de dev ([domains], 09) idem. Gate do dev (24/10): beat de
        // FORA só mantém o dev acordado se o IP for CONFIÁVEL (senha certa
        // no desafio do gate); banido ou sem confiança = 404 sem tocar — é o
        // mesmo corte do router (a rota é REAL, o request nunca chega ao
        // fallback onde fica o gate de lá). ConnectInfo não chega ao handler
        // (extração por extensão é do service) — a régua roda com
        // `connect = None`, e request real SEMPRE chega pelo Caddy com XFF
        // (LAN) ou marcador do túnel (externo). Beat retido vira access 404
        // com dump: request externo é auditável.
        ManagedHost::Dev { name, .. }
        | ManagedHost::Vite { name, .. }
        | ManagedHost::Custom { name, dev: true } => {
            if crate::waf::fonte_externa(&headers, None) {
                let ip = crate::waf::ip_real(&headers, None)
                    .map(|i| i.to_string())
                    .unwrap_or_default();
                if crate::gate::estado_ip(&app.state, &ip) == crate::gate::EstadoIp::Confiavel {
                    return tocar(&app.state, &name, Env::Dev);
                }
                tracing::debug!(target: "gate", host = %host_raw, "dev: beat externo sem confiança — retido");
                crate::events::access(
                    &app.state,
                    host_raw,
                    "POST",
                    "/__flux/beat",
                    404,
                    started.elapsed().as_millis(),
                    &crate::events::origem_de(&headers, true),
                );
                return StatusCode::NOT_FOUND.into_response();
            }
            tocar(&app.state, &name, Env::Dev)
        }
        // Painel e desconhecido não têm container de app para manter acordado.
        ManagedHost::Panel | ManagedHost::Unknown(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Registra a interação (04) e responde 204. Falha de gravação vira warn com
/// causa — nunca silencioso; o navegador não lê a resposta de qualquer forma.
fn tocar(st: &crate::state::State, name: &str, env: Env) -> Response {
    if let Err(e) = st.touch_interaction(name, env) {
        tracing::warn!("beat de {name}/{env} não registrou interação: {e}");
    }
    StatusCode::NO_CONTENT.into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::State;
    use flux_common::config::GlobalConfig;

    fn app() -> Arc<AppState> {
        let st = State::in_memory().unwrap();
        Arc::new(AppState::new(st, &GlobalConfig::default(), None))
    }

    async fn pedir(app: &Arc<AppState>, host: &str) -> StatusCode {
        pedir_com(app, host, |h| {
            h.insert(
                "x-forwarded-for",
                header::HeaderValue::from_static("192.168.140.1"),
            );
        })
        .await
    }

    /// Beat com mutação de headers no caminho: LAN por padrão (browser real
    /// chega pelo Caddy, que seta o XFF); testes de origem sobrescrevem.
    async fn pedir_com(
        app: &Arc<AppState>,
        host: &str,
        ajusta: impl FnOnce(&mut HeaderMap),
    ) -> StatusCode {
        let mut h = HeaderMap::new();
        h.insert(header::HOST, header::HeaderValue::from_str(host).unwrap());
        ajusta(&mut h);
        beat(State(app.clone()), h).await.status()
    }

    #[tokio::test]
    async fn beat_de_host_gerenciado_devolve_204_e_zera_o_timer() {
        let app = app();
        app.state
            .register_project("vipers", "name = \"vipers\"", "hash")
            .unwrap();
        assert_eq!(
            pedir(&app, "vipers-d.bitello.xyz").await,
            StatusCode::NO_CONTENT,
            "host dev gerenciado tem que responder 204"
        );
        let (last, _, _, _) = app.state.sleep_row("vipers", Env::Dev).unwrap();
        assert!(
            last.is_some(),
            "beat tem que zerar last_interaction do dev via touch_interaction"
        );
    }

    #[tokio::test]
    async fn beat_de_host_prod_gerenciado_toca_o_env_de_prod() {
        let app = app();
        app.state
            .register_project("vipers", "name = \"vipers\"", "hash")
            .unwrap();
        assert_eq!(
            pedir(&app, "vipers.bitello.xyz").await,
            StatusCode::NO_CONTENT,
            "host prod gerenciado tem que responder 204"
        );
        let (last, _, _, _) = app.state.sleep_row("vipers", Env::Prod).unwrap();
        assert!(last.is_some(), "beat de prod tem que tocar o env de prod");
    }

    #[tokio::test]
    async fn beat_de_fantasma_e_404_sem_tocar_nada() {
        let app = app();
        assert_eq!(
            pedir(&app, "zzz.bitello.xyz").await,
            StatusCode::NOT_FOUND,
            "fora da zona/projeto fantasma é 404"
        );
        let (last, _, _, _) = app.state.sleep_row("zzz", Env::Prod).unwrap();
        assert!(last.is_none(), "fantasma não pode ganhar linha de sono");
    }

    /// Gate LAN-only (24/10): beat de FORA (marcador do túnel) NÃO mantém o
    /// dev acordado — 404, sem touch, com access auditado no log.
    #[tokio::test]
    async fn beat_externo_nao_mantem_dev_acordado_e_fica_auditado() {
        let app = app();
        app.state
            .register_project("vipers", "name = \"vipers\"", "hash")
            .unwrap();
        let status = pedir_com(&app, "vipers-d.bitello.xyz", |h| {
            h.insert(
                "cf-connecting-ip",
                header::HeaderValue::from_static("203.0.113.9"),
            );
        })
        .await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "beat externo não passa do gate LAN-only de dev"
        );
        let (last, _, _, _) = app.state.sleep_row("vipers", Env::Dev).unwrap();
        assert!(last.is_none(), "beat externo NÃO zera o timer de sono");

        let msg: String = app
            .state
            .with(|c| {
                c.query_row(
                    "SELECT message FROM events WHERE scope='access' ORDER BY id DESC LIMIT 1",
                    [],
                    |row| row.get(0),
                )
            })
            .unwrap();
        assert!(
            msg.contains("POST /__flux/beat 404") && msg.contains("hdrs["),
            "beat retido auditado com dump: {msg}"
        );
    }

    #[tokio::test]
    async fn beat_do_painel_e_404_sem_tocar_nada() {
        let app = app();
        assert_eq!(
            pedir(&app, "flux.bitello.xyz").await,
            StatusCode::NOT_FOUND,
            "painel não é host de projeto — beat dele não toca nada"
        );
        let (last, _, _, _) = app.state.sleep_row("flux", Env::Prod).unwrap();
        assert!(last.is_none(), "painel não pode ganhar linha de sono");
    }

    #[test]
    fn script_cobre_todos_os_eventos_o_endpoint_e_os_intervalos_da_04() {
        for ev in [
            "mousemove",
            "mousedown",
            "keydown",
            "wheel",
            "scroll",
            "touchstart",
        ] {
            assert!(SCRIPT.contains(ev), "SCRIPT sem o evento {ev} da spec 04");
        }
        assert!(
            SCRIPT.contains("navigator.sendBeacon('/__flux/beat')"),
            "SCRIPT sem o sendBeacon do endpoint /__flux/beat"
        );
        assert!(
            SCRIPT.contains("30000") && SCRIPT.contains("60000"),
            "SCRIPT sem throttle de 30 s e intervalo de 60 s"
        );
        assert!(
            SCRIPT.contains("passive:true"),
            "listeners têm que ser passive (zero interferência, 04)"
        );
        assert!(
            !SCRIPT.contains('\n'),
            "SCRIPT tem que ser uma linha só, sem dependência"
        );
    }
}
