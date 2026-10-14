//! Overrides de compose (06): o fluxd aplica as regras de ouro na HORA do
//! `up`, o arquivo original do projeto segue intocado. Gera um override em
//! `/srv/flux/state/compose/{name}-{env}.override.yml` passado via `-f`:
//!
//! - `ports: !reset null` em TODOS os services (nada publicado na VM — o
//!   acesso é sempre Caddy → fluxd → IP interno, 03);
//! - todos os services na rede `flux-net` (external — é lá que mora o
//!   `flux-db`, 05); isolamento por projeto é o F14;
//! - `restart: "no"` em dev (o sono controla o ciclo, 04);
//! - `DB_HOST=flux-db`/`DB_PORT=3306` forçados (override vence .env velho);
//!   resto de credencial vem do `.env` exclusivo da VM (02/09).
//!
//! Os nomes dos services vêm do PRÓPRIO engine (`docker compose config
//! --services`) — sem parse de YAML aqui (zero dep nova; o engine é a
//! verdade).

/// Escolhe o arquivo de compose a passar com `-f`: o da raiz quando tem
/// `services:` de verdade; se a raiz é só um `include:`, usa o alvo em
/// `docker/`. Motivo: o include do compose resolve caminhos relativos ao
/// DIRETÓRIO CORRENTE (não ao arquivo incluído) — `./serve` do include
/// viraria `{raiz}/serve`, que não existe.
pub fn escolher_compose(dir: &Path) -> Option<PathBuf> {
    for nome in [
        "compose.yaml",
        "compose.yml",
        "docker-compose.yaml",
        "docker-compose.yml",
    ] {
        let raiz = dir.join(nome);
        if raiz.is_file() {
            let so_include = std::fs::read_to_string(&raiz)
                .map(|c| {
                    c.lines()
                        .filter(|l| {
                            let l = l.trim();
                            !l.is_empty() && !l.starts_with('#')
                        })
                        .all(|l| l.starts_with("include:") || l.starts_with('-'))
                })
                .unwrap_or(false);
            if !so_include {
                return Some(raiz);
            }
            for sub in ["docker", "deploy"] {
                for sub_nome in [
                    "compose.yaml",
                    "compose.yml",
                    "docker-compose.yaml",
                    "docker-compose.yml",
                ] {
                    let alvo = dir.join(sub).join(sub_nome);
                    if alvo.is_file() {
                        return Some(alvo);
                    }
                }
            }
            return Some(raiz);
        }
    }
    None
}

use std::path::{Path, PathBuf};

use crate::events;

/// Gera o YAML de override para os services dados.
pub fn override_yaml(services: &[String], env: &str) -> String {
    let mut y = String::from("services:\n");
    for svc in services {
        y.push_str(&format!("  {svc}:\n"));
        y.push_str("    ports: !reset null\n");
        y.push_str("    restart: \"no\"\n");
        y.push_str("    labels:\n      flux.managed: \"true\"\n");
        y.push_str("    environment:\n      DB_HOST: flux-db\n      DB_PORT: \"3306\"\n");
        y.push_str("    networks: !override [default, flux-net]\n");
    }
    y.push_str("networks:\n  flux-net:\n    external: true\n");
    let _ = env; // dev hoje; prod troca restart (F10) — parâmetro reservado
    y
}

/// Roda `docker compose config --services` no diretório do projeto
/// (compose acha o arquivo raiz sozinho — includes respeitados).
pub async fn services_do(dir: &Path, compose: &Path, projeto: &str) -> Result<Vec<String>, String> {
    let saida = tokio::process::Command::new("docker")
        .args(["compose", "-p", projeto])
        .arg("-f")
        .arg(compose)
        .args(["config", "--services"])
        .current_dir(dir)
        .output()
        .await
        .map_err(|e| e.to_string())?;
    if !saida.status.success() {
        return Err(format!(
            "compose config falhou em {}: {}",
            dir.display(),
            String::from_utf8_lossy(&saida.stderr).trim()
        ));
    }
    let mut svcs: Vec<String> = String::from_utf8_lossy(&saida.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    svcs.sort();
    Ok(svcs)
}

/// Caminho do override: `/srv/flux/state/compose/{name}-{env}.override.yml`
/// (06). Cria o diretório se precisar.
pub fn caminho_override(state_dir: &Path, name: &str, env: &str) -> std::io::Result<PathBuf> {
    let dir = state_dir.join("compose");
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join(format!("{name}-{env}.override.yml")))
}

/// Garante a rede `flux-net` (external) — idempotente.
pub async fn garantir_rede(nome: &str) -> Result<(), String> {
    let saida = tokio::process::Command::new("docker")
        .args(["network", "inspect", nome])
        .output()
        .await
        .map_err(|e| e.to_string())?;
    if saida.status.success() {
        return Ok(());
    }
    let criada = tokio::process::Command::new("docker")
        .args(["network", "create", nome])
        .output()
        .await
        .map_err(|e| e.to_string())?;
    if criada.status.success() {
        Ok(())
    } else {
        Err(format!(
            "network create {nome}: {}",
            String::from_utf8_lossy(&criada.stderr).trim()
        ))
    }
}

// ------------------------------------------------------------------ up (06)

/// Ciclo de subida do dev (06, passos 5–6), rodando EM BACKGROUND após o
/// 202 da API: guard de imagens de novo no estado do repo sincronizado,
/// override gerado dos services REAIS do engine, `compose up -d --build`,
/// estado/proxy_target gravados. Cada passo vira evento — nada silencioso.
pub async fn subir_projeto(app: std::sync::Arc<crate::AppState>, name: String, env: String) {
    let projeto = format!("flux-{name}-{env}");
    let dir = PathBuf::from(crate::temporary::PROJECTS_ROOT_PADRAO).join(&name);
    let st = &app.state;

    if !dir.is_dir() {
        events::event(
            st,
            "error",
            "up",
            &format!(
                "{projeto}: pasta {} não existe — o sync (mutagen) roda antes do up (02)",
                dir.display()
            ),
        );
        return;
    }

    // Guard de imagens (05) no estado ATUAL do repo — o compose pode ter
    // chegado na sync depois do registro. Varre compose*/Dockerfile* até
    // profundidade 3 da pasta do projeto.
    let block: Vec<String> = flux_common::config::ImageGuardSection::default()
        .block_list()
        .to_vec();
    let mut arquivos: Vec<(String, String, String)> = Vec::new();
    coletar_guard(&dir, 0, &mut arquivos);
    let refs: Vec<(&str, &str, &str)> = arquivos
        .iter()
        .map(|(f, k, c)| (f.as_str(), k.as_str(), c.as_str()))
        .collect();
    let violacoes = crate::guard::scan_files(&refs, &block);
    if !violacoes.is_empty() {
        let resumo: Vec<String> = violacoes
            .iter()
            .map(|v| format!("{}:{} imagem '{}'", v.file, v.line, v.image))
            .collect();
        events::event(
            st,
            "error",
            "up",
            &format!("{projeto}: guard bloqueou a subida — {}", resumo.join("; ")),
        );
        return;
    }

    if let Err(e) = garantir_rede("flux-net").await {
        events::event(st, "error", "up", &format!("{projeto}: rede flux-net: {e}"));
        return;
    }

    let Some(compose_file) = escolher_compose(&dir) else {
        events::event(
            st,
            "error",
            "up",
            &format!("{projeto}: nenhum compose na pasta do projeto"),
        );
        return;
    };
    let services = match services_do(&dir, &compose_file, &projeto).await {
        Ok(s) if !s.is_empty() => s,
        Ok(_) => {
            events::event(
                st,
                "error",
                "up",
                &format!("{projeto}: compose sem services"),
            );
            return;
        }
        Err(e) => {
            events::event(st, "error", "up", &format!("{projeto}: {e}"));
            return;
        }
    };
    let Some(state_dir) = app.state_dir.clone() else {
        events::event(
            st,
            "error",
            "up",
            &format!("{projeto}: state_dir indefinido"),
        );
        return;
    };
    let override_path = match caminho_override(&state_dir, &name, &env) {
        Ok(p) => p,
        Err(e) => {
            events::event(st, "error", "up", &format!("{projeto}: override: {e}"));
            return;
        }
    };
    if let Err(e) = std::fs::write(&override_path, override_yaml(&services, &env)) {
        events::event(
            st,
            "error",
            "up",
            &format!("{projeto}: escrevendo override: {e}"),
        );
        return;
    }

    events::event(
        st,
        "info",
        "up",
        &format!(
            "{projeto}: compose up -d --build (services: {})",
            services.join(", ")
        ),
    );
    let saida = tokio::process::Command::new("docker")
        .args(["compose", "-p", &projeto])
        .arg("-f")
        .arg(&compose_file)
        .arg("-f")
        .arg(&override_path)
        .args(["up", "-d", "--build"])
        .current_dir(&dir)
        .output()
        .await;
    let saida = match saida {
        Ok(s) => s,
        Err(e) => {
            events::event(st, "error", "up", &format!("{projeto}: compose up: {e}"));
            return;
        }
    };
    if !saida.status.success() {
        let cauda: String = String::from_utf8_lossy(&saida.stderr)
            .lines()
            .rev()
            .take(8)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join(" | ");
        events::event(
            st,
            "error",
            "up",
            &format!("{projeto}: compose up falhou: {cauda}"),
        );
        return;
    }

    // Estado real pós-up: a verdade vem do engine (inspect via labels).
    if let Some(d) = &app.docker {
        use crate::decision::HostSource;
        let src = crate::docker::DockerSource::new(d.clone(), st);
        let env_enum = if env == "prod" {
            flux_common::protocol::Env::Prod
        } else {
            flux_common::protocol::Env::Dev
        };
        let estado = src.env_state(&name, env_enum);
        let _ = st.set_env_state(&name, env_enum, estado);
        if let Some(target) = src.proxy_target(&name, env_enum) {
            let _ = st.set_proxy_target(&name, env_enum, &target);
        }
    }
    let _ = st.touch_interaction(&name, flux_common::protocol::Env::Dev);
    events::event(
        st,
        "info",
        "up",
        &format!("{projeto}: subiu (compose up -d --build)"),
    );
}

/// Varre `base` (até `profundidade` 3) coletando compose*/Dockerfile* para o
/// guard — (caminho relativo, tipo, conteúdo).
fn coletar_guard(dir: &Path, profundidade: usize, out: &mut Vec<(String, String, String)>) {
    if profundidade > 3 {
        return;
    }
    let Ok(entradas) = std::fs::read_dir(dir) else {
        return;
    };
    for entrada in entradas.flatten() {
        let caminho = entrada.path();
        let Some(nome) = caminho.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if caminho.is_dir() {
            if nome.starts_with('.') || nome == "node_modules" || nome == "vendor" {
                continue; // nada de guard dentro de dependência
            }
            coletar_guard(&caminho, profundidade + 1, out);
        } else if (nome.starts_with("compose") || nome.starts_with("docker-compose"))
            && (nome.ends_with(".yml") || nome.ends_with(".yaml"))
        {
            if let Ok(c) = std::fs::read_to_string(&caminho) {
                out.push((nome.to_owned(), crate::guard::KIND_COMPOSE.to_owned(), c));
            }
        } else if nome.starts_with("Dockerfile")
            && let Ok(c) = std::fs::read_to_string(&caminho)
        {
            out.push((nome.to_owned(), crate::guard::KIND_DOCKERFILE.to_owned(), c));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn override_cobre_todos_os_services_com_as_regras_da_06() {
        let y = override_yaml(&["app".into(), "worker".into()], "dev");
        assert!(y.contains("  app:\n"));
        assert!(y.contains("  worker:\n"));
        assert_eq!(
            y.matches("ports: !reset null").count(),
            2,
            "ports fora em todos"
        );
        assert_eq!(
            y.matches("restart: \"no\"").count(),
            2,
            "dev não compete com o sono"
        );
        assert!(y.contains("DB_HOST: flux-db"));
        assert!(y.contains("networks: !override [default, flux-net]"));
        assert!(y.contains("external: true"), "flux-net é a rede do flux-db");
        // Nada publicado = portscan da 09 continua só 22/80/443.
        assert!(!y.contains("ports:\n    -"));
    }
}

#[cfg(test)]
mod escolha {
    use super::*;

    #[test]
    fn raiz_com_services_vence_e_include_vai_pro_docker() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("docker-compose.yml"),
            "services:\n  app:\n    image: nginx\n",
        )
        .unwrap();
        let r = escolher_compose(tmp.path()).unwrap();
        assert!(r.ends_with("docker-compose.yml"), "{r:?}");
        assert_eq!(r.parent().unwrap(), tmp.path(), "raiz direta");

        // Raiz só-include: o alvo real mora em docker/ (presente-digital/DJM).
        std::fs::create_dir_all(tmp.path().join("docker")).unwrap();
        std::fs::write(
            tmp.path().join("docker-compose.yml"),
            "include:\n  - docker/docker-compose.yml\n",
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("docker").join("docker-compose.yml"),
            "services:\n  app:\n    build: ./serve\n",
        )
        .unwrap();
        let r2 = escolher_compose(tmp.path()).unwrap();
        assert!(
            r2.ends_with(std::path::Path::new("docker/docker-compose.yml")),
            "{r2:?}"
        );
    }

    #[test]
    fn sem_compose_nada_escolhido() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(escolher_compose(tmp.path()).is_none());
    }
}
