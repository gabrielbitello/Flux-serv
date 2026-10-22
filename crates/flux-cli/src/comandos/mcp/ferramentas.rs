//! Catálogo e dispatch das tools MCP (22/10): as DEFINIÇÕES (nome, descrição,
//! inputSchema) e o roteador `chamar`. A regra de partição: leitura em
//! `consultas.rs`, ação em `acoes.rs` — este arquivo não conhece API, só o
//! envelope (erro de catálogo = JSON-RPC; falha de execução = `isError`).

use serde_json::{Value, json};

use super::{acoes, consultas};

/// Catálogo anunciado no `tools/list`. Toda tool aceita `projeto` (string) —
/// o processo MCP é long-lived, então o cwd não é contexto confiável.
pub(crate) fn catalogo() -> Vec<Value> {
    vec![
        ferramenta(
            "flux_projetos",
            "Lista os projetos registrados no fluxd (name, framework, hosts). Comece por aqui quando não souber o nome do projeto.",
            json!({}),
            &[],
        ),
        ferramenta(
            "flux_status",
            "Estado do projeto: containers, sono (dev/prod), migrations aplicadas, hosts. Use para saber se o dev está acordado ou dormindo.",
            json!({
                "projeto": texto("Nome do projeto (pasta ou `name` do flux.toml)."),
                "env": texto("Ambiente: dev (padrão) ou prod.")
            }),
            &[],
        ),
        ferramenta(
            "flux_ps",
            "Espelho do `docker compose ps` de TODOS os projetos gerenciados, agrupado com estado agregado (ACORDADO/PARCIAL/DORMINDO).",
            json!({
                "projeto": texto("Filtra por projeto (prefixo do compose project).")
            }),
            &[],
        ),
        ferramenta(
            "flux_sync_status",
            "Saúde da sincronização Windows→VM do projeto: sessão mutagen local (arquivos, pendências, conflitos) + visão do fluxd. Responde 'o sync está correto?'. A sessão local é resolvida pelo `dir` (padrão: cwd do processo MCP).",
            json!({
                "dir": texto("Raiz do projeto no Windows (onde fica o flux.toml). Ausente = cwd do processo MCP.")
            }),
            &[],
        ),
        ferramenta(
            "flux_db_status",
            "Banco próprio do projeto ([db] own): container do grupo, databases visíveis e TTL regressivo de cada database de dev.",
            json!({
                "projeto": texto("Nome do projeto.")
            }),
            &[],
        ),
        ferramenta(
            "flux_logs",
            "Cauda dos logs do container (SEM follow). `tail` entre 1 e 1000 (padrão 200).",
            json!({
                "projeto": texto("Nome do projeto."),
                "env": texto("Ambiente: dev (padrão) ou prod."),
                "tail": numero("Linhas do fim (1–1000).")
            }),
            &[],
        ),
        ferramenta(
            "flux_acordar",
            "Acorda o dev do projeto (POST /up + lease) e ESPERA até `esperar_secs` (padrão 60, teto 180) polling o estado. Build frio (primeiro up) pode passar do teto — nesse caso siga com flux_status.",
            json!({
                "projeto": texto("Nome do projeto."),
                "env": texto("Ambiente: dev (padrão) ou prod."),
                "esperar_secs": numero("Quanto esperar pelo estado running (1–180).")
            }),
            &[],
        ),
        ferramenta(
            "flux_db_query",
            "SQL de LEITURA no banco próprio do projeto (SELECT/SHOW/DESCRIBE/EXPLAIN, um statement, sem trava de escrita — o fluxd recusa o resto). Passe `database` para o USE; sem ele, qualifique com `db.tabela`. Saída = TSV cru do mysql -N (sem header).",
            json!({
                "projeto": texto("Nome do projeto (dono do banco próprio)."),
                "sql": texto("Statement único de leitura."),
                "database": texto("Database de contexto (USE).")
            }),
            &["sql"],
        ),
        ferramenta(
            "flux_tool",
            "Roda toolchain de build/dependências NO container de dev da VM: `npm`, `npx`, `composer` ou `php artisan` (migrate, cache:clear, config:clear...). Whitelist — nada além disso. Teto de 120 s do exec.",
            json!({
                "projeto": texto("Nome do projeto."),
                "env": texto("Ambiente: dev (padrão) ou prod."),
                "comando": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Tokens do comando, ex.: [\"php\",\"artisan\",\"cache:clear\"]."
                }
            }),
            &["comando"],
        ),
    ]
}

/// Roteador: tool de catálogo → implementação; falha de execução NUNCA é
/// erro de protocolo — volta `isError: true` com a causa (a IA lê e decide).
pub(crate) async fn chamar(params: &Value) -> Result<Value, (i32, String)> {
    let nome = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| (-32602, "tools/call sem `name`".to_owned()))?;
    if !catalogo().iter().any(|t| t["name"] == json!(nome)) {
        return Err((
            -32602,
            format!("tool desconhecida: {nome} — consulte tools/list"),
        ));
    }
    let args = params.get("arguments").cloned().unwrap_or(json!({}));
    let resultado = match nome {
        "flux_projetos" => consultas::projetos().await,
        "flux_status" => consultas::status(&args).await,
        "flux_ps" => consultas::ps(&args).await,
        "flux_sync_status" => consultas::sync_status(&args).await,
        "flux_db_status" => consultas::db_status(&args).await,
        "flux_logs" => consultas::logs(&args).await,
        "flux_acordar" => acoes::acordar(&args).await,
        "flux_db_query" => acoes::db_query(&args).await,
        "flux_tool" => acoes::ferramenta_exec(&args).await,
        _ => unreachable!("catálogo e roteador divergiram para {nome}"),
    };
    Ok(match resultado {
        Ok(valor) => json!({
            "content": [{ "type": "text", "text": serde_json::to_string_pretty(&valor).unwrap_or_default() }]
        }),
        Err(causa) => json!({
            "content": [{ "type": "text", "text": causa }],
            "isError": true
        }),
    })
}

// ------------------------------------------------------------ montagem pura

fn ferramenta(nome: &str, descricao: &str, propriedades: Value, obrigatorios: &[&str]) -> Value {
    json!({
        "name": nome,
        "description": descricao,
        "inputSchema": {
            "type": "object",
            "properties": propriedades,
            "required": obrigatorios
        }
    })
}

fn texto(descricao: &str) -> Value {
    json!({ "type": "string", "description": descricao })
}

fn numero(descricao: &str) -> Value {
    json!({ "type": "integer", "description": descricao })
}

// Argumento string do envelope (`args.get(...)`): ausente/estranho → None.
pub(crate) fn arg_str(args: &Value, chave: &str) -> Option<String> {
    args.get(chave)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// Argumento inteiro com teto e padrão (tail/espera) — fora da faixa NÃO é
/// erro: cai no limite mais próximo (a IA acerta pelo description; o teto é
/// defesa, não contrato).
pub(crate) fn arg_int(args: &Value, chave: &str, padrao: u64, min: u64, max: u64) -> u64 {
    args.get(chave)
        .and_then(Value::as_u64)
        .unwrap_or(padrao)
        .clamp(min, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalogo_tem_nomes_unicos_e_descricao() {
        let tools = catalogo();
        let mut nomes: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
        let total = nomes.len();
        nomes.sort_unstable();
        nomes.dedup();
        assert_eq!(nomes.len(), total, "nome de tool repetido no catálogo");
        for t in &tools {
            assert!(
                t["description"].as_str().is_some_and(|d| d.len() > 20),
                "tool sem descrição útil: {t}"
            );
        }
    }

    #[test]
    fn args_fora_da_faixa_caicam_no_limite() {
        let args = json!({ "tail": 999_999 });
        assert_eq!(arg_int(&args, "tail", 200, 1, 1000), 1000);
        assert_eq!(arg_int(&args, "tail", 200, 1, 1000).min(0), 0);
        let vazio = json!({});
        assert_eq!(arg_int(&vazio, "tail", 200, 1, 1000), 200, "padrão");
        assert_eq!(arg_int(&vazio, "esperar_secs", 60, 1, 180), 60);
    }

    #[test]
    fn arg_str_ignora_vazio_e_nao_string() {
        assert_eq!(
            arg_str(&json!({ "projeto": " vipers " }), "projeto").as_deref(),
            Some("vipers"),
            "trim no argumento"
        );
        assert_eq!(arg_str(&json!({ "projeto": "  " }), "projeto"), None);
        assert_eq!(arg_str(&json!({ "projeto": 7 }), "projeto"), None);
    }
}
