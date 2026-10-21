//! redroom (21/10): modo de contenção GLOBAL da plataforma. Comando GLOBAL
//! de propósito — NÃO usa contexto de projeto (`contexto_atual`), funciona
//! em qualquer diretório: a contenção é da plataforma inteira, não de um
//! repo. O falhar vem do contexto (mesmo contrato dos comandos do ciclo).

use crate::api::ApiClient;
use crate::contexto::falhar;

/// `flux redroom on|off|status` — o interruptor: ligado, o router do fluxd
/// devolve a página estática de contenção para TODO request (Cloudflare e
/// rede local, host gerenciado ou não) e nenhum container é acordado ou
/// proxyado. A API de controle :7420 é listener próprio FORA do router — o
/// `off` funciona mesmo com a contenção no ar (por túnel SSH).
pub(crate) async fn redroom(acao: &str) {
    let cliente = ApiClient::from_env();
    match acao {
        "on" => {
            let out = match cliente.redroom_definir(true).await {
                Ok(o) => o,
                Err(e) => falhar(e),
            };
            if out.redroom {
                println!("redroom ON — todo o router do fluxd está atrás da página de contenção");
                println!(
                    "nenhum container é acordado ou proxyado; a API :7420 e o painel :7421 (túnel SSH) seguem de pé"
                );
                println!("desliga com: flux redroom off");
            }
        }
        "off" => {
            let out = match cliente.redroom_definir(false).await {
                Ok(o) => o,
                Err(e) => falhar(e),
            };
            if !out.redroom {
                println!("redroom OFF — acesso público restaurado");
            }
        }
        "status" => {
            let out = match cliente.redroom_status().await {
                Ok(o) => o,
                Err(e) => falhar(e),
            };
            println!(
                "redroom {}",
                if out.redroom {
                    "ON — plataforma inteira em contenção"
                } else {
                    "OFF"
                }
            );
        }
        // Clap só deixa passar on|off|status (value_parser) — braço defensivo
        // para nunca casar silencioso.
        outro => {
            eprintln!("erro: uso: flux redroom on|off|status (recebido: {outro:?})");
            std::process::exit(2);
        }
    }
}
