//! gate (24/10): gestão do desafio de senha dos hosts de dev — senha,
//! confiança por IP (30 dias), bans por 5 falhas e o Turnstile do form.
//! Comando GLOBAL de propósito (igual ao redroom): o gate é da plataforma,
//! NÃO usa contexto de projeto.

use crate::GateCmd;
use crate::api::ApiClient;
use crate::contexto::falhar;

/// `flux gate status|senha|unban|revogar|turnstile` — a régua do desafio é
/// do fluxd; aqui é só o painel de controle do operador.
pub(crate) async fn gate(cmd: GateCmd) {
    let cliente = ApiClient::from_env();
    match cmd {
        GateCmd::Status => {
            let out = match cliente.gate_status().await {
                Ok(o) => o,
                Err(e) => falhar(e),
            };
            println!(
                "senha: {} · turnstile: {} · {} falhas = ban · confiança = {} dias",
                if out.senha_definida {
                    "DEFINIDA"
                } else {
                    "NÃO definida (desafio nunca passa)"
                },
                if out.turnstile {
                    "ativo"
                } else {
                    "sem captcha"
                },
                out.teto_tentativas,
                out.dias_confianca,
            );
            if out.confiancas.is_empty() && out.bans.is_empty() && out.tentativas.is_empty() {
                println!("nenhum IP confiável, banido ou com falhas registradas");
                return;
            }
            if !out.confiancas.is_empty() {
                println!("confiáveis:");
                for c in &out.confiancas {
                    println!("  {} (expira na época {})", c.ip, c.expira_em);
                }
            }
            if !out.tentativas.is_empty() {
                println!("com falhas:");
                for t in &out.tentativas {
                    println!(
                        "  {} ({}/{} até o ban)",
                        t.ip, t.falhas, out.teto_tentativas
                    );
                }
            }
            if !out.bans.is_empty() {
                println!("banidos (desbloqueia com `flux gate unban <ip>`):");
                for b in &out.bans {
                    println!("  {} (desde a época {})", b.ip, b.criado_em);
                }
            }
        }
        GateCmd::Senha { senha } => {
            let out = match cliente.gate_senha(senha).await {
                Ok(o) => o,
                Err(e) => falhar(e),
            };
            match out.senha {
                Some(s) => {
                    println!("SENHA GERADA (mostrada UMA vez — guarde agora):");
                    println!("{s}");
                }
                None => println!("senha do gate definida"),
            }
        }
        GateCmd::Unban { ip } => {
            let out = match cliente.gate_unban(ip).await {
                Ok(o) => o,
                Err(e) => falhar(e),
            };
            if out.desbanido == Some(true) {
                println!("ban removido — o IP volta ao desafio normal");
            }
        }
        GateCmd::Revogar { ip } => {
            let out = match cliente.gate_revogar(ip).await {
                Ok(o) => o,
                Err(e) => falhar(e),
            };
            if out.revogada == Some(true) {
                println!("confiança revogada — o IP volta ao desafio no próximo request");
            }
        }
        GateCmd::Turnstile { site, secret } => {
            let out = match cliente.gate_turnstile(site, secret).await {
                Ok(o) => o,
                Err(e) => falhar(e),
            };
            if out.turnstile == Some(true) {
                println!("Turnstile configurado — o form do desafio agora exige o captcha");
            }
        }
    }
}
