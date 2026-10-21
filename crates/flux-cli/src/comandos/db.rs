//! `flux db status` (21/10): estado do banco próprio do projeto — container
//! dedicado do grupo, databases visíveis e quanto falta pro TTL de cada um.
//! Mesma conta que a limpeza do sweep faz (último contato × ttl_days), então
//! o que aparece aqui é o que vai acontecer.

use std::time::{SystemTime, UNIX_EPOCH};

use crate::api::{ApiClient, ApiError};
use crate::contexto::{contexto_atual, falhar, parse_env};

pub(crate) async fn status_cmd(env: &str) {
    let _ = parse_env(env); // valida cedo, com exit 2 em valor ruim
    let ctx = contexto_atual();
    let cliente = ApiClient::from_env();
    let st = match cliente.db_status(&ctx.name).await {
        Ok(s) => s,
        Err(e) => falhar(e),
    };
    if !st.own {
        println!(
            "{}: sem banco próprio ([db] own no flux.toml) — banco é o flux-db global",
            ctx.name
        );
        return;
    }
    let container = st.container.as_deref().unwrap_or("—");
    let grupo = st.grupo.as_deref().unwrap_or("—");
    let saude = st.saude.as_deref().unwrap_or("—");
    println!(
        "{} · container {} ({}) · health {} · grupo {} · TTL {} dias (dbs de dev)",
        ctx.name,
        container,
        st.estado.as_deref().unwrap_or("—"),
        saude,
        grupo,
        st.ttl_days.unwrap_or(0)
    );
    if st.databases.is_empty() {
        println!("  (nenhum database visível — container parado ou recém-criado)");
        return;
    }
    let agora = epoch();
    for db in &st.databases {
        if !db.gerenciado {
            println!("  {:<28} não gerenciado (nunca expira)", db.nome);
            continue;
        }
        match (db.ultimo_contato, db.expira_em_secs) {
            (Some(ultimo), Some(resta)) if resta <= 0 => println!(
                "  {:<28} último contato {} · EXPIRADO — cai no próximo sweep (dump antes)",
                db.nome,
                hah_quanto(ultimo, agora),
            ),
            (Some(ultimo), Some(resta)) => println!(
                "  {:<28} último contato {} · expira em {}",
                db.nome,
                hah_quanto(ultimo, agora),
                duracao_legivel(resta),
            ),
            _ => println!(
                "  {:<28} gerenciado, sem relógio — NUNCA expira (dado ausente não chuta)",
                db.nome
            ),
        }
    }
}

fn epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// "`duracao_legivel` atrás" (relógio do fluxd é o mesmo epoch).
fn hah_quanto(ultimo: i64, agora: i64) -> String {
    format!("{} atrás", duracao_legivel((agora - ultimo).max(0)))
}

fn duracao_legivel(secs: i64) -> String {
    let dias = secs / 86_400;
    let horas = (secs % 86_400) / 3600;
    let minutos = (secs % 3600) / 60;
    if dias > 0 {
        format!("{dias}d {horas}h")
    } else if horas > 0 {
        format!("{horas}h {minutos}min")
    } else {
        format!("{minutos}min")
    }
}

// Erro de API carregado quando o cli api muda de forma (nada silencioso):
#[allow(dead_code)]
fn _garante_trait_em_escopo(e: ApiError) -> ! {
    falhar(e)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duracao_legivel_cobre_as_faixas() {
        assert_eq!(duracao_legivel(90), "1min");
        assert_eq!(duracao_legivel(2 * 3600 + 5 * 60), "2h 5min");
        assert_eq!(duracao_legivel(8 * 86_400 + 3 * 3600), "8d 3h");
    }

    #[test]
    fn hah_quanto_nunca_negativo() {
        assert_eq!(hah_quanto(1_000, 900), "0min atrás");
        assert_eq!(hah_quanto(990, 1_090), "1min atrás");
    }
}
