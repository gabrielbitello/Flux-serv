//! vigia (26/10): limiar de recarga do vigia de anomalia, por projeto.
//! Páginas que se atualizam sozinhas (meta-refresh, polling HTMX) viram
//! tráfego legítimo de dezenas/min — o default global (24/min) tolera até
//! 23x/min, e o override por projeto sobe o teto SEM reiniciar o fluxd (o
//! POST roda o ciclo do vigia na hora). Martelada e ws derivam do base com
//! piso nos valores calibrados dos incidentes.

use crate::VigiaCmd;
use crate::api::ApiClient;
use crate::contexto::falhar;

/// `flux vigia status|set|reset` — painel de controle do detector. Comando
/// GLOBAL de propósito (igual ao gate): fala com a API, não com o projeto.
pub(crate) async fn vigia(cmd: VigiaCmd) {
    let cliente = ApiClient::from_env();
    match cmd {
        VigiaCmd::Status => {
            let out = match cliente.vigia_status().await {
                Ok(o) => o,
                Err(e) => falhar(e),
            };
            println!(
                "default: recarga {}x/min · martelada {}x/3min · ws {}/min",
                out.default_recarga_por_min, out.default_martelada_3min, out.default_ws_por_min
            );
            if out.overrides.is_empty() {
                println!("nenhum override por projeto");
                return;
            }
            println!("overrides (os demais limiares derivam do base):");
            for o in &out.overrides {
                println!(
                    "  {} base={} → recarga {}x/min · martelada {}x/3min · ws {}/min",
                    o.projeto, o.base, o.recarga_por_min, o.martelada_3min, o.ws_por_min
                );
            }
        }
        VigiaCmd::Set { projeto, limite } => {
            let out = match cliente.vigia_set(&projeto, Some(limite)).await {
                Ok(o) => o,
                Err(e) => falhar(e),
            };
            println!(
                "{}: base {} → recarga {}x/min · martelada {}x/3min · ws {}/min \
                 (ciclo do vigia executado na hora)",
                out.projeto, out.base, out.recarga_por_min, out.martelada_3min, out.ws_por_min
            );
        }
        VigiaCmd::Reset { projeto } => {
            let out = match cliente.vigia_set(&projeto, None).await {
                Ok(o) => o,
                Err(e) => falhar(e),
            };
            println!(
                "{}: override removido — volta ao default (recarga {}x/min) \
                 (ciclo do vigia executado na hora)",
                out.projeto, out.recarga_por_min
            );
        }
    }
}
