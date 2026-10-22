//! Tools de AÇÃO do MCP (22/10): acordar dev, toolchain de build na VM e
//! leitura SQL no banco próprio. As guardas de escopo moram AQUI e são a
//! única superfície: `flux_tool` só aceita npm/npx/composer/php-artisan
//! (whitelist, não blacklist); a guarda do SQL é do fluxd (fonte única —
//! `banco::sql_e_leitura`), o cliente manda verbatim e exibe a recusa.

use serde_json::{Value, json};

use super::consultas;

/// Whitelist do `flux_tool` (pedido do Gabriel, 22/10): build/install/update
/// com npm e composer, ciclo Laravel com artisan. Primeiro token decide;
/// `php` só passa com `artisan` em segundo.
fn comandos_permitidos(cmd: &[String]) -> Result<(), String> {
    let Some(primeira) = cmd.first().map(String::as_str) else {
        return Err(
            "comando vazio — mande um array de tokens, ex.: [\"npm\", \"run\", \"build\"]".into(),
        );
    };
    match primeira {
        "npm" | "npx" | "composer" => Ok(()),
        "php" => {
            if cmd.get(1).map(String::as_str) == Some("artisan") {
                Ok(())
            } else {
                Err("php só pelo `php artisan …` — nada mais roda aqui".into())
            }
        }
        outra => Err(format!(
            "`{outra}` não é toolchain permitida — só npm, npx, composer e php artisan (whitelist)"
        )),
    }
}

/// Acorda o projeto (lease + POST /up) e espera o engine dizer `running`.
pub(crate) async fn acordar(args: &Value) -> Result<Value, String> {
    let projeto = consultas::projeto_de(args)?;
    let env = consultas::env_de(args)?;
    let esperar = super::ferramentas::arg_int(args, "esperar_secs", 60, 1, 180);
    let cliente = consultas::cliente();
    cliente
        .lease(&projeto, &env.to_string(), crate::contexto::LEASE_UP_SECS)
        .await
        .map_err(|e| e.to_string())?;
    let aceito = cliente
        .subir(&projeto, &env.to_string())
        .await
        .map_err(consultas::erro_com_dica)?;

    let inicio = std::time::Instant::now();
    let mut estado_final: Option<String> = None;
    while inicio.elapsed() < std::time::Duration::from_secs(esperar) {
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        if let Ok(st) = cliente.status(&projeto).await
            && let Some(estado) = st.dev_container_state
        {
            let running = estado == "running";
            estado_final = Some(estado);
            if running {
                break;
            }
        }
    }
    let acordou = estado_final.as_deref() == Some("running");
    Ok(json!({
        "aceito": aceito.started,
        "project": aceito.project,
        "env": env.to_string(),
        "estado_final": estado_final,
        "acordou": acordou,
        "esperado_secs": inicio.elapsed().as_secs(),
        "dica": if acordou {
            "dev acordado — flux_status detalha"
        } else {
            "não chegou a `running` no teto — build frio demora minutos; siga com flux_status/flux_logs"
        }
    }))
}

/// SQL de leitura no banco próprio — a recusa de escrita é do fluxd (422
/// com a causa verbatim); aqui só transporte e formato.
pub(crate) async fn db_query(args: &Value) -> Result<Value, String> {
    let projeto = consultas::projeto_de(args)?;
    let sql = super::ferramentas::arg_str(args, "sql")
        .ok_or_else(|| "argumento obrigatório ausente: `sql`".to_owned())?;
    let database = super::ferramentas::arg_str(args, "database");
    let saida = consultas::cliente()
        .db_query(&projeto, &sql, database.as_deref())
        .await
        .map_err(|e| e.to_string())?;
    Ok(json!({
        "projeto": projeto,
        "database": database,
        "saida": saida.saida,
    }))
}

/// Toolchain no container de dev (teto de 120 s do exec no fluxd). Lease
/// curto antes: acordar o container se estiver dormindo.
pub(crate) async fn ferramenta_exec(args: &Value) -> Result<Value, String> {
    let projeto = consultas::projeto_de(args)?;
    let env = consultas::env_de(args)?;
    let cmd: Vec<String> = args
        .get("comando")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    comandos_permitidos(&cmd)?;
    let cliente = consultas::cliente();
    cliente
        .lease(&projeto, &env.to_string(), crate::contexto::LEASE_UP_SECS)
        .await
        .map_err(|e| e.to_string())?;
    let (saida, exit) = cliente
        .executar(&projeto, &env.to_string(), &cmd)
        .await
        .map_err(|e| e.to_string())?;
    Ok(json!({
        "projeto": projeto,
        "env": env.to_string(),
        "comando": cmd,
        "exit": exit,
        "saida": saida,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(tokens: &[&str]) -> Vec<String> {
        tokens.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn whitelist_so_deixa_toolchain_de_build() {
        for aceito in [
            vec!["npm", "run", "build"],
            vec!["npm", "install"],
            vec!["npm", "update"],
            vec!["npx", "vite", "build"],
            vec!["composer", "install"],
            vec!["composer", "update"],
            vec!["php", "artisan", "migrate", "--force"],
            vec!["php", "artisan", "cache:clear"],
        ] {
            assert!(
                comandos_permitidos(&cmd(&aceito)).is_ok(),
                "deveria passar: {aceito:?}"
            );
        }
    }

    #[test]
    fn whitelist_barra_o_resto_e_php_sem_artisan() {
        for recusado in [
            vec!["bash"],
            vec!["sh", "-c", "rm -rf /"],
            vec!["rm", "-rf", "/"],
            vec!["docker", "exec", "x"],
            vec!["php", "-r", "evil();"],
            vec!["php", "script.php"],
            vec!["mysql"],
            vec![],
        ] {
            assert!(
                comandos_permitidos(&cmd(&recusado)).is_err(),
                "deveria recusar: {recusado:?}"
            );
        }
    }
}
