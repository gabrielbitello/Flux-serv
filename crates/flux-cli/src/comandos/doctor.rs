//! doctor (27/10): valida a fábrica inteira num comando — fluxd, containers,
//! sync (visão do fluxd + sessões mutagen LOCAIS), banco, assets e resolução
//! de DNS dos hosts. Verde/vermelho por item; exit 1 com qualquer vermelho.
//! Comando GLOBAL de propósito (igual vigia/gate): diagnostica a plataforma,
//! não um repo.
//!
//! As checagens do lado VM são do `GET /doctor` (doctor.rs do fluxd — fonte
//! única; o CLI formata e acrescenta o que só o Windows sabe: sessões
//! mutagen locais e o getaddrinfo dos hosts). O doctor NUNCA acorda nada.

use crate::api::ApiClient;
use crate::sync::{self, SessionStatus};
use serde_json::Value;
use std::io::IsTerminal;
use std::net::ToSocketAddrs;

const VERDE: &str = "\x1b[32m";
const VERMELHO: &str = "\x1b[31m";
const RESET: &str = "\x1b[0m";

/// Item do relatório do fluxd já digerido. JSON malformado vira item
/// vermelho com a causa — nunca "ok" por engano de parse.
#[derive(Debug, PartialEq, Eq)]
struct ItemCli {
    ok: bool,
    estado: String,
    detalhe: Option<String>,
}

fn item_de(v: &Value) -> ItemCli {
    match (
        v.get("ok").and_then(Value::as_bool),
        v.get("estado").and_then(Value::as_str),
    ) {
        (Some(ok), Some(estado)) => ItemCli {
            ok,
            estado: estado.to_owned(),
            detalhe: v.get("detalhe").and_then(Value::as_str).map(str::to_owned),
        },
        _ => ItemCli {
            ok: false,
            estado: "ilegível".into(),
            detalhe: Some(format!("JSON inesperado: {v}")),
        },
    }
}

/// `  rotulo    [ok] estado · detalhe` — cor off (pipe/teste) devolve texto
/// puro, o mesmo padrão do resto do CLI.
fn linha(rotulo: &str, item: &ItemCli, cor: bool) -> String {
    let marca = if item.ok { "[ok]" } else { "[FALHA]" };
    let pintada = if !cor {
        marca.to_owned()
    } else if item.ok {
        format!("{VERDE}{marca}{RESET}")
    } else {
        format!("{VERMELHO}{marca}{RESET}")
    };
    match &item.detalhe {
        Some(d) => format!("  {rotulo:<11} {pintada} {} · {d}", item.estado),
        None => format!("  {rotulo:<11} {pintada} {}", item.estado),
    }
}

/// Resolução do host NO WINDOWS (getaddrinfo lê o hosts file). Três estados
/// honestos: IP host-only = LAN direta (verde); DNS público (Cloudflare) =
/// verde com o caminho na causa — os `*-d.bitello.xyz` funcionam pelo túnel
/// sem entrada no hosts file; não resolver = vermelho (site morto de fato).
fn item_dns(host: &str) -> ItemCli {
    match (host, 443u16).to_socket_addrs() {
        Err(e) => ItemCli {
            ok: false,
            estado: "não resolve".into(),
            detalhe: Some(e.to_string()),
        },
        Ok(mut ips) => match ips.next() {
            Some(addr) if addr.ip().to_string() == sync::VM_HOST => ItemCli {
                ok: true,
                estado: format!("{} (LAN direta)", sync::VM_HOST),
                detalhe: None,
            },
            Some(addr) => ItemCli {
                ok: true,
                estado: format!("→ {}", addr.ip()),
                detalhe: Some(
                    "via DNS público (túnel Cloudflare) — para LAN direta, entrada no hosts file"
                        .into(),
                ),
            },
            None => ItemCli {
                ok: false,
                estado: "sem endereço".into(),
                detalhe: Some("getaddrinfo devolveu lista vazia".into()),
            },
        },
    }
}

/// Veredito das sessões mutagen LOCAIS (prefixo `flux-`) — a visão que só o
/// Windows tem (o fluxd vê os relatos do watch; aqui é a sessão viva).
/// Retorna (linhas prontas, tudo verde).
fn veredito_sync_local(sessoes: &[SessionStatus]) -> Vec<(bool, String)> {
    let mut linhas = Vec::new();
    for s in sessoes.iter().filter(|s| s.name.starts_with("flux-")) {
        if s.saudavel() {
            linhas.push((true, format!("{}: ociosa · sem pendências", s.name)));
            continue;
        }
        let mut causas: Vec<String> = vec![s.status.rotulo()];
        if s.paused {
            causas.push("pausada — `flux sync resume`".into());
        }
        if s.conflicts > 0 {
            causas.push(format!("{} conflito(s)", s.conflicts));
        }
        let problemas = s.alpha.scan_problems
            + s.alpha.transition_problems
            + s.beta.scan_problems
            + s.beta.transition_problems;
        if problemas > 0 {
            causas.push(format!("{problemas} problema(s) de scan/transition"));
        }
        if let Some(e) = &s.last_error {
            causas.push(format!("último erro: {e}"));
        }
        linhas.push((false, format!("{}: {}", s.name, causas.join(" · "))));
    }
    if linhas.is_empty() {
        linhas.push((
            true,
            "nenhuma sessão local flux-* (nenhum projeto com sync ativa)".into(),
        ));
    }
    linhas
}

/// `flux doctor [projeto]` — imprime o relatório e sai 1 com qualquer
/// vermelho (fluxd inalcançável é o caso mais vermelho de todos: a causa do
/// túnel/API vai no stderr).
pub(crate) async fn doctor(projeto: Option<String>) {
    let cliente = ApiClient::from_env();
    let cor = std::io::stdout().is_terminal();

    let caminho = match &projeto {
        Some(p) => format!("/doctor?projeto={p}"),
        None => "/doctor".to_owned(),
    };
    let cru = match cliente.call_raw("GET", &caminho, None).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("flux doctor: fluxd inalcançável — nada mais dá pra checar sem ele.");
            eprintln!("  causa: {e}");
            eprintln!(
                "  pista clássica: túnel SSH morto — mate o ssh.exe do forward 7420 e rode de novo."
            );
            std::process::exit(1);
        }
    };
    let rel: Value = match serde_json::from_str(&cru) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("flux doctor: resposta do fluxd não é JSON: {e}");
            std::process::exit(1);
        }
    };

    let mut vermelhos: usize = 0;
    let mut marca = |ok: bool, texto: String| {
        if !ok {
            vermelhos += 1;
        }
        println!("{texto}");
        ok
    };

    match &projeto {
        Some(_) => {
            println!(
                "projeto {}",
                rel.get("projeto").and_then(Value::as_str).unwrap_or("?")
            );
        }
        None => {
            let fluxd = item_de(rel.get("fluxd").unwrap_or(&Value::Null));
            marca(fluxd.ok, linha("fluxd", &fluxd, cor));
            println!();
        }
    }

    let projetos: Vec<Value> = match &projeto {
        Some(_) => vec![rel.clone()],
        None => rel
            .get("projetos")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
    };
    for p in &projetos {
        let nome = p.get("projeto").and_then(Value::as_str).unwrap_or("?");
        let dev_host = p.get("dev_host").and_then(Value::as_str).unwrap_or("?");
        let prod_host = p.get("prod_host").and_then(Value::as_str).unwrap_or("?");
        println!("{nome} (dev={dev_host} · prod={prod_host})");
        for (rotulo, chave) in [
            ("containers", "containers"),
            ("sync", "sync"),
            ("banco", "banco"),
            ("assets", "assets"),
            ("sono", "sono"),
        ] {
            let item = item_de(p.get(chave).unwrap_or(&Value::Null));
            marca(item.ok, linha(rotulo, &item, cor));
        }
        // DNS é verdade do Windows: um item por host efetivo.
        for (rotulo, host) in [("dns dev", dev_host), ("dns prod", prod_host)] {
            let item = item_dns(host);
            marca(item.ok, linha(rotulo, &item, cor));
        }
        println!();
    }

    println!("sync local (sessões mutagen deste Windows)");
    for (ok, texto) in veredito_sync_local(&sync::status_all().unwrap_or_default()) {
        let item = ItemCli {
            ok,
            estado: texto,
            detalhe: None,
        };
        marca(ok, linha("sessão", &item, cor));
    }

    if vermelhos > 0 {
        println!();
        println!("{vermelhos} item(ns) vermelho(s) — o de cima costuma explicar o de baixo.");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item_json(ok: bool, estado: &str, detalhe: Option<&str>) -> Value {
        let mut v = serde_json::json!({ "ok": ok, "estado": estado });
        if let Some(d) = detalhe {
            v["detalhe"] = serde_json::json!(d);
        }
        v
    }

    #[test]
    fn item_de_consulta_verdade_e_json_maluco_e_vermelho() {
        let bom = item_de(&item_json(true, "running", Some("health=healthy")));
        assert!(bom.ok);
        assert_eq!(bom.estado, "running");
        assert_eq!(bom.detalhe.as_deref(), Some("health=healthy"));

        let sem_detalhe = item_de(&item_json(false, "sem-relato", None));
        assert!(!sem_detalhe.ok);
        assert_eq!(sem_detalhe.detalhe, None);

        let maluco = item_de(&serde_json::json!({ "verde": "por favor" }));
        assert!(!maluco.ok, "JSON inesperado NUNCA é verde");
        assert_eq!(maluco.estado, "ilegível");
        assert!(maluco.detalhe.unwrap_or_default().contains("verde"));
    }

    #[test]
    fn linha_com_e_sem_cor_e_estavel() {
        let verde = ItemCli {
            ok: true,
            estado: "ACORDADO".into(),
            detalhe: None,
        };
        let vermelho = ItemCli {
            ok: false,
            estado: "PARCIAL".into(),
            detalhe: Some("parados: db".into()),
        };
        assert_eq!(
            linha("containers", &verde, false),
            "  containers  [ok] ACORDADO"
        );
        assert_eq!(
            linha("containers", &vermelho, false),
            "  containers  [FALHA] PARCIAL · parados: db"
        );
        let colorida = linha("containers", &vermelho, true);
        assert!(
            colorida.contains(VERMELHO) && colorida.contains(RESET),
            "{colorida}"
        );
        assert!(colorida.contains("PARCIAL"), "conteúdo sobrevive à cor");
    }

    #[test]
    fn sync_local_flux_e_verde_quando_saudavel_e_vermelho_com_causa() {
        use crate::sync::{EndpointStatus, StatusState};

        let endpoint = EndpointStatus {
            path: "/p".into(),
            host: String::new(),
            remote: false,
            connected: true,
            scan_problems: 0,
            transition_problems: 0,
            files: Some(10),
        };
        let saudavel = SessionStatus {
            name: "flux-vipa".into(),
            identifier: "sync_1".into(),
            paused: false,
            status: StatusState::Ociosa,
            alpha: endpoint.clone(),
            beta: endpoint.clone(),
            last_error: None,
            conflicts: 0,
        };
        let insaudavel = SessionStatus {
            name: "flux-djm".into(),
            identifier: "sync_2".into(),
            paused: true,
            status: StatusState::Atencao,
            alpha: EndpointStatus {
                scan_problems: 2,
                ..endpoint.clone()
            },
            beta: endpoint,
            last_error: Some("boom".into()),
            conflicts: 1,
        };
        let fora = SessionStatus {
            name: "pessoal-nao-flux".into(),
            ..saudavel.clone()
        };

        let linhas = veredito_sync_local(&[saudavel, insaudavel, fora]);
        assert_eq!(linhas.len(), 2, "sessão fora do prefixo flux- não entra");
        assert!(linhas[0].0, "saudável é verde: {}", linhas[0].1);
        assert!(linhas[0].1.contains("flux-vipa"));
        assert!(!linhas[1].0, "insaudável é vermelho: {}", linhas[1].1);
        let texto = &linhas[1].1;
        assert!(texto.contains("flux-djm"), "{texto}");
        assert!(texto.contains("atenção"), "rótulo do status: {texto}");
        assert!(texto.contains("pausada"), "{texto}");
        assert!(texto.contains("2 problema(s)"), "{texto}");
        assert!(texto.contains("1 conflito(s)"), "{texto}");
        assert!(texto.contains("boom"), "{texto}");
    }

    #[test]
    fn sync_local_sem_sessao_e_neutro_verde() {
        let linhas = veredito_sync_local(&[]);
        assert_eq!(linhas.len(), 1);
        assert!(linhas[0].0, "nenhuma sessão não é defeito: {}", linhas[0].1);
    }
}
