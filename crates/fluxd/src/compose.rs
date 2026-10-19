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
//!
//! Live server de dev (`[dev] live` no flux.toml): após um up com SUCESSO em
//! env=dev, o fluxd dispara o comando de hot reload DENTRO do container `app`
//! — `docker compose exec -d app sh -c "cd /var/www && {live}"`. Erro do live
//! é warn no painel e NUNCA falha o up (o container já está de pé); em prod o
//! gatilho nem dispara.

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
                        .all(|l| {
                            let l = l.trim();
                            l.starts_with("include:") || l.starts_with('-')
                        })
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

/// Gera o YAML de override para os services dados. `stripped` = services da
/// block_list que o flux ASSUME (banco/cache é o flux-db, 05): entram como
/// `!reset null` (compose v2.24+ remove o service no merge) e os services
/// vivos ganham `depends_on: !reset null` — dependência de removido morre
/// junto (pedido do Gabriel: "algo automático que sempre desliga o
/// depends_on"). A base SEMPRE é a do projeto; o flux só muta isso.
///
/// `publicar_vite`: com live server declarado (`[dev] live`), publica a
/// porta do dev server no host — o hostname `vite-{dev}` do túnel aponta pra
/// cá (06/live; o fluxd roteia o host pro mesmo container, porta 5173).
pub fn override_yaml(
    services: &[String],
    env: &str,
    stripped: &[String],
    publicar_vite: bool,
    asset_host: &str,
) -> String {
    let mut y = String::from("services:\n");
    for svc in stripped {
        y.push_str(&format!("  {svc}: !reset null\n"));
    }
    for svc in services {
        y.push_str(&format!("  {svc}:\n"));
        if publicar_vite {
            y.push_str("    ports:\n      - \"5173:5173\"\n");
        } else {
            y.push_str("    ports: !reset null\n");
        }
        y.push_str("    depends_on: !reset null\n");
        y.push_str("    restart: \"no\"\n");
        y.push_str("    labels:\n      flux.managed: \"true\"\n");
        y.push_str("    environment:\n      DB_HOST: flux-db\n      DB_PORT: \"3306\"\n");
        // ASSET_URL público (06/live): o proxy do fluxd reescreve o Host para
        // o IP do container — sem esta env o Laravel monta asset do BUILD com
        // o IP interno (http://172.18.0.4/…), que o browser do dev não
        // alcança. Mesmo mecanismo do DB_HOST: a plataforma injeta por cima
        // do .env, que não manda em infraestrutura.
        y.push_str(&format!("      ASSET_URL: \"{asset_host}\"\n"));
        y.push_str("    networks: !override [default, flux-net]\n");
    }
    y.push_str("networks:\n  flux-net:\n    external: true\n");
    let _ = env; // dev hoje; prod troca restart (F10) — parâmetro reservado
    y
}

/// Service renderizado do compose efetivo — a fatia que interessa ao flux.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceRender {
    pub name: String,
    pub image: Option<String>,
}

/// Parse do `docker compose config --format json` (06/F6): o YAML vira
/// objeto nas mãos do PRÓPRIO engine — sem parser de YAML aqui.
pub fn parse_render(json: &str) -> Result<Vec<ServiceRender>, String> {
    let v: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let svcs = v
        .get("services")
        .and_then(|s| s.as_object())
        .ok_or_else(|| "render sem a chave services".to_owned())?;
    let mut out: Vec<ServiceRender> = Vec::new();
    for (name, corpo) in svcs {
        out.push(ServiceRender {
            name: name.clone(),
            image: corpo
                .get("image")
                .and_then(|i| i.as_str())
                .map(ToOwned::to_owned),
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Renderiza o compose efetivo do projeto via o engine (só leitura).
pub async fn render_config(
    dir: &Path,
    compose: &Path,
    projeto: &str,
) -> Result<Vec<ServiceRender>, String> {
    let saida = tokio::process::Command::new("docker")
        .args(["compose", "-p", projeto])
        .arg("-f")
        .arg(compose)
        .args(["config", "--format", "json"])
        .current_dir(dir)
        .output()
        .await
        .map_err(|e| e.to_string())?;
    if !saida.status.success() {
        return Err(format!(
            "compose config --format json falhou em {}: {}",
            dir.display(),
            String::from_utf8_lossy(&saida.stderr).trim()
        ));
    }
    parse_render(&String::from_utf8_lossy(&saida.stdout))
}

/// Services da block_list presentes no render — o flux assume esses
/// (strip no override). Services só de build (sem `image`) não entram.
pub fn strip_list(renderizado: &[ServiceRender], block: &[String]) -> Vec<String> {
    renderizado
        .iter()
        .filter(|s| {
            s.image
                .as_deref()
                .map(|i| crate::guard::is_blocked(i, block))
                .unwrap_or(false)
        })
        .map(|s| s.name.clone())
        .collect()
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
    let dir = PathBuf::from(crate::paths::PROJECTS_ROOT_PADRAO).join(&name);
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
    // profundidade 3 da pasta do projeto. DESDE o strip automático: Dockerfile
    // com FROM de banco/cache BLOQUEIA (base de build não sai com override);
    // imagem de banco em SERVICE de compose NÃO bloqueia — o flux assume o
    // service (strip no override, render abaixo) e o banco é o flux-db.
    let block: Vec<String> = flux_common::config::ImageGuardSection::default()
        .block_list()
        .to_vec();
    let mut arquivos: Vec<(String, String, String)> = Vec::new();
    coletar_guard(&dir, 0, &mut arquivos);
    let dockerfile_refs: Vec<(&str, &str, &str)> = arquivos
        .iter()
        .filter(|(_, kind, _)| *kind == crate::guard::KIND_DOCKERFILE)
        .map(|(f, k, c)| (f.as_str(), k.as_str(), c.as_str()))
        .collect();
    let violacoes = crate::guard::scan_files(&dockerfile_refs, &block);
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

    // Render do compose EFETIVO (06/F6) + strip automático (05): service com
    // imagem da block_list sai do merge via `!reset null`; banco/cache é o
    // flux-db, nunca container do projeto. A base é sempre a do projeto.
    let renderizado = match render_config(&dir, &compose_file, &projeto).await {
        Ok(r) => r,
        Err(e) => {
            events::event(
                st,
                "error",
                "up",
                &format!("{projeto}: render do compose: {e}"),
            );
            return;
        }
    };
    let strip = strip_list(&renderizado, &block);
    let services: Vec<String> = services
        .into_iter()
        .filter(|s| !strip.contains(s))
        .collect();
    if services.is_empty() {
        events::event(
            st,
            "error",
            "up",
            &format!(
                "{projeto}: todos os services eram de infraestrutura assumida ({}) — não sobe \
                 nada; o projeto precisa de um service de aplicação",
                strip.join(", ")
            ),
        );
        return;
    }
    if !strip.is_empty() {
        events::event(
            st,
            "info",
            "up",
            &format!(
                "{projeto}: flux assumiu services da block_list (strip no override): {} — banco/\
                 cache é o flux-db (05)",
                strip.join(", ")
            ),
        );
    }
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
    // Porta do dev server publicada só quando há live server (`[dev] live`,
    // 06/live): o hostname `vite-{dev}` do túnel aponta pra cá.
    let publicar_vite = st
        .project_toml(&name)
        .ok()
        .flatten()
        .and_then(|t| flux_common::config::ProjectToml::parse_str(&t).ok())
        .and_then(|t| t.dev)
        .and_then(|d| d.live)
        .map(|l| !l.trim().is_empty())
        .unwrap_or(false);
    // ASSET_URL público por ambiente (06/live): dev = host -d, prod = host
    // raiz — a env injetada vence o .env, igual DB_HOST.
    let asset_host = if env == "dev" {
        format!(
            "https://{}{}.{}",
            name, app.domain.dev_suffix, app.domain.zone
        )
    } else {
        format!("https://{}.{}", name, app.domain.zone)
    };
    if let Err(e) = std::fs::write(
        &override_path,
        override_yaml(&services, &env, &strip, publicar_vite, &asset_host),
    ) {
        events::event(
            st,
            "error",
            "up",
            &format!("{projeto}: escrevendo override: {e}"),
        );
        return;
    }

    // Guard de TÚNEL por camada (05, regra do Gabriel 14/10): cloudflared no
    // service principal (app) = NADA sobe; em secundário = só ele fica de
    // fora (--scale 0) com aviso.
    let pular = tunel_classificar(&dir, &compose_file, &projeto, &services);
    if pular.iter().any(|(svc, _)| svc == "app") {
        events::event(
            st,
            "error",
            "up",
            &format!(
                "{projeto}: túnel/cloudflared no SERVICE PRINCIPAL (app) — NADA sobe (05: o \
                 túnel é único, no host 03/12). Tire o connector do compose e use o do host."
            ),
        );
        return;
    }
    if !pular.is_empty() {
        let lista: Vec<String> = pular
            .iter()
            .map(|(svc, img)| format!("{svc} ({img})"))
            .collect();
        events::event(
            st,
            "warn",
            "up",
            &format!(
                "{projeto}: túnel/cloudflared em service SECUNDÁRIO — sobe o resto, fica de \
                 fora: {}",
                lista.join(", ")
            ),
        );
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
    let mut up = tokio::process::Command::new("docker");
    up.args(["compose", "-p", &projeto])
        .arg("-f")
        .arg(&compose_file)
        .arg("-f")
        .arg(&override_path)
        .args(["up", "-d", "--build"]);
    for (svc, _) in &pular {
        up.args(["--scale", &format!("{svc}=0")]);
    }
    let saida = up.current_dir(&dir).output().await;
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
        // L3.4: falha de REGISTRO depois de um up bem-sucedido NÃO é
        // silenciosa — o modo degradado (SQLite) serviria alvo/estado velho
        // sem ninguém saber. Warn com a causa; fluxo/status HTTP intocados
        // (o compose up já aconteceu).
        if let Err(e) = st.set_env_state(&name, env_enum, estado) {
            tracing::warn!("{projeto}: registro do estado pós-up falhou: {e}");
        }
        if let Some(target) = src.proxy_target(&name, env_enum)
            && let Err(e) = st.set_proxy_target(&name, env_enum, &target)
        {
            tracing::warn!("{projeto}: registro do proxy_target pós-up falhou: {e}");
        }
    }
    if let Err(e) = st.touch_interaction(&name, flux_common::protocol::Env::Dev) {
        tracing::warn!("{projeto}: touch_interaction pós-up falhou: {e}");
    }
    events::event(
        st,
        "info",
        "up",
        &format!("{projeto}: subiu (compose up -d --build)"),
    );

    // Live server de dev ([dev] live): só com up bem-sucedido e em dev —
    // qualquer falha aqui é warn, o up já terminou com sucesso. Service
    // principal segue a mesma regra do find_app_container: `app` se existir,
    // senão o primeiro (vipa usa `php`, presente usa `app`).
    if env == "dev" {
        let principal = if services.iter().any(|s| s == "app") {
            "app".to_owned()
        } else {
            services[0].clone()
        };
        // IP do container (proxy_target `ip:80` → ip): a porta do dev server
        // é conferida na MÁQUINA/VM, não dentro do container.
        use crate::decision::HostSource;
        let ip = if let Some(d) = &app.docker {
            crate::docker::DockerSource::new(d.clone(), st)
                .proxy_target(&name, flux_common::protocol::Env::Dev)
        } else {
            crate::sqlite_source::SqliteSource::new(st)
                .proxy_target(&name, flux_common::protocol::Env::Dev)
        }
        .and_then(|t| {
            t.rsplit_once(':')
                .map(|(parte_ip, _porta): (&str, &str)| parte_ip.to_owned())
        });
        subir_live_se_configurado(st, &app, &name, &projeto, &principal, ip.as_deref(), "up").await;
    }
}

/// Decisão do live server no up (06/live) — PURA, para testar sem engine.
/// O `public/hot` é o contrato do Vite e a :5173 é a porta fixa do
/// roteamento same-origin; as DUAS fontes juntas fecham os dois sintomas de
/// 18/10: dev server DUPLICADO (cada `up` re-disparava `npm run dev` — o
/// segundo via de "Port in use" pra 5174, invisível ao roteamento) e hot
/// ÓRFÃO (npm morto com SIGKILL não roda o `rm -f public/hot` do wrapper e o
/// Laravel segue apontando pro servidor morto — site sem CSS).
pub enum AcaoLive {
    /// hot vivo + porta respondendo: live já de pé — não duplica.
    Pular,
    /// alguma das duas fontes caiu: remover hot órfão (se houver), matar
    /// node zumbis (se houver) e subir de novo.
    Ressuscitar,
}

pub fn decidir_live(hot: bool, porta_viva: bool) -> AcaoLive {
    if hot && porta_viva {
        AcaoLive::Pular
    } else {
        AcaoLive::Ressuscitar
    }
}

/// A porta do dev server responde no IP do container? TCP connect rápido —
/// não importa O QUE escuta na porta para o "já está de pé" do up; o
/// roteamento same-origin do router valida o tráfego por cima.
pub(crate) async fn porta_tcp_viva(ip: &str, porta: u16) -> bool {
    let Ok(addr) = format!("{ip}:{porta}").parse::<std::net::SocketAddr>() else {
        return false;
    };
    tokio::time::timeout(std::time::Duration::from_millis(400), async {
        tokio::net::TcpStream::connect(addr).await.is_ok()
    })
    .await
    .unwrap_or(false)
}

/// Raiz da árvore sincronizada do projeto na VM — mesma derivação do router
/// (`state_dir` `/srv/flux/state` → irmão `projects`). `None` = fora da VM
/// (testes/dev host) — nada de live nem de limpeza de hot.
pub(crate) fn raiz_arvore(app: &crate::AppState, name: &str) -> Option<std::path::PathBuf> {
    app.state_dir
        .as_ref()
        .and_then(|p| p.parent())
        .map(|p| p.join("projects").join(name))
}

/// Monta os argumentos do live server: `docker compose -p {projeto} exec -d
/// {service} sh -c "{live}; rc=$?; rm -f public/hot; exit $rc"`. Pura, para o
/// teste cobrir a montagem sem engine (o `-d` solta o processo: exit 0 =
/// "iniciado", não "saudável"). O workdir é o do SERVICE (working_dir
/// declarado ou WORKDIR da imagem) — o flux não chumba caminho: cada projeto
/// tem o seu (/var/www, /app, ...).
///
/// O `rm -f public/hot` pós-live é a prova de hot órfão (06): dev server
/// (`npm run dev`) cria `public/hot` e MORRE por qualquer motivo → sem a
/// limpeza o Laravel continua servindo assets do servidor morto (site sem
/// CSS). Com o wrapper, TODA morte do live limpa o hot e o Laravel volta pro
/// manifest sozinho. Para `build --watch` o rm é no-op (watch não cria hot).
fn comando_live_args(projeto: &str, service: &str, live: &str) -> Vec<String> {
    vec![
        "compose".to_owned(),
        "-p".to_owned(),
        projeto.to_owned(),
        "exec".to_owned(),
        "-d".to_owned(),
        service.to_owned(),
        "sh".to_owned(),
        "-c".to_owned(),
        format!("{{ {live}; }}; rc=$?; rm -f public/hot; exit $rc"),
    ]
}

/// Mata `vite` zumbis dentro do service — loop `/proc` direto, sem depender
/// de ps/pkill no container. O sh/npm do wrapper SOBREVIVE (cmdline não tem
/// "vite") e completa o ciclo sozinho: filho morto → `rm -f public/hot` do
/// wrapper → exit. Erro é evento warn; o disparo do live segue de pé.
async fn matar_vite_zumbis(st: &crate::state::State, projeto: &str, service: &str) {
    // `${p#/proc/}` devolve "72/cmdline" — kill nesse argumento é EINVAL
    // silencioso (apanhado ao vivo: zumbi da 5174 sobreviveu a rodadas de
    // kill). O PID é basename(dirname(cmdline)).
    let script = "for p in /proc/[0-9]*/cmdline; do grep -qa vite \"$p\" 2>/dev/null && kill \"$(basename \"$(dirname \"$p\")\")\" 2>/dev/null; done; exit 0";
    let saida = tokio::process::Command::new("docker")
        .args([
            "compose", "-p", projeto, "exec", "-T", service, "sh", "-c", script,
        ])
        .output()
        .await;
    match saida {
        Ok(s) if s.status.success() => events::event(
            st,
            "info",
            "up",
            &format!("{projeto}: zumbis de vite mortos antes do re-disparo do live"),
        ),
        Ok(s) => {
            let cauda: String = String::from_utf8_lossy(&s.stderr)
                .lines()
                .take(3)
                .collect::<Vec<_>>()
                .join(" | ");
            events::event(
                st,
                "warn",
                "up",
                &format!("{projeto}: falha matando zumbis de vite: {cauda}"),
            );
        }
        Err(e) => events::event(
            st,
            "warn",
            "up",
            &format!("{projeto}: falha executando limpeza de zumbis: {e}"),
        ),
    }
}

/// Dispara o live server de dev quando o flux.toml do projeto (lido do state,
/// como o temporary lê) declara `[dev] live`. NUNCA falha o chamador: todo
/// erro é evento warn com a causa real. `origem` marca o rastro do evento
/// ("up" no fluxo de subida, "wake" no religar pós-sono).
async fn subir_live_se_configurado(
    st: &crate::state::State,
    app: &crate::AppState,
    name: &str,
    projeto: &str,
    service: &str,
    ip: Option<&str>,
    origem: &str,
) {
    let toml_src = match st.project_toml(name) {
        Ok(Some(t)) => t,
        Ok(None) => return, // projeto sem flux.toml registrado — não pede live
        Err(e) => {
            events::event(
                st,
                "warn",
                origem,
                &format!("{projeto}: live não disparado — falha lendo flux.toml do state: {e}"),
            );
            return;
        }
    };
    let toml = match flux_common::config::ProjectToml::parse_str(&toml_src) {
        Ok(t) => t,
        Err(e) => {
            events::event(
                st,
                "warn",
                origem,
                &format!("{projeto}: live não disparado — flux.toml do state não parseia: {e}"),
            );
            return;
        }
    };
    let Some(live) = toml.dev.as_ref().and_then(|d| d.live.as_deref()) else {
        return; // sem [dev] live — o projeto não pede live server
    };
    if live.trim().is_empty() {
        events::event(
            st,
            "warn",
            origem,
            &format!("{projeto}: [dev] live declarado VAZIO no flux.toml — nada a executar"),
        );
        return;
    }

    // Idempotência (06/live): hot + porta = live já de pé, NÃO re-dispara
    // (o segundo `npm run dev` ia de "Port in use" pra 5174 e ficava
    // invisível ao roteamento). Qualquer uma das fontes caída = zumbi/órfão:
    // limpa e ressuscita.
    let hot_vivo = raiz_arvore(app, name)
        .as_ref()
        .is_some_and(|r| r.join("public/hot").is_file());
    let porta_viva = match ip {
        Some(ip) => porta_tcp_viva(ip, crate::decision::VITE_PORT).await,
        None => false, // sem IP no state = não dá para conferir; tratar como caído
    };
    if let AcaoLive::Pular = decidir_live(hot_vivo, porta_viva) {
        events::event(
            st,
            "info",
            origem,
            &format!(
                "{projeto}: live server já ativo (hot vivo + :{} respondendo) — \
                 pulando re-disparo",
                crate::decision::VITE_PORT
            ),
        );
        return;
    }
    if hot_vivo {
        // Porta morta com hot de pé = órfão: o wrapper do npm morto com
        // SIGKILL não rodou o `rm -f public/hot`. Remover ANTES de subir,
        // senão o Laravel segue apontando pro servidor morto (site sem CSS).
        if let Some(r) = raiz_arvore(app, name) {
            match std::fs::remove_file(r.join("public/hot")) {
                Ok(()) => events::event(
                    st,
                    "warn",
                    origem,
                    &format!(
                        "{projeto}: hot ÓRFÃO removido — dev server morto sem a porta {}",
                        crate::decision::VITE_PORT
                    ),
                ),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => events::event(
                    st,
                    "warn",
                    origem,
                    &format!("{projeto}: falha removendo hot órfão: {e}"),
                ),
            }
        }
    }
    if porta_viva {
        // Porta viva SEM hot = npm zumbi que perdeu o contrato — mata antes
        // de subir o novo, senão o novo nasce na 5174 fora do roteamento.
        matar_vite_zumbis(st, projeto, service).await;
    }
    events::event(
        st,
        "info",
        origem,
        &format!("{projeto}: disparando live server de dev ({service}): {live}"),
    );
    match tokio::process::Command::new("docker")
        .args(comando_live_args(projeto, service, live))
        .output()
        .await
    {
        Ok(s) if s.status.success() => {
            events::event(
                st,
                "info",
                origem,
                &format!("{projeto}: live server iniciado ({live})"),
            );
        }
        Ok(s) => {
            let cauda: String = String::from_utf8_lossy(&s.stderr)
                .lines()
                .rev()
                .take(4)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join(" | ");
            events::event(
                st,
                "warn",
                origem,
                &format!(
                    "{projeto}: live server falhou ao iniciar (o up segue de pé): exit {} {cauda}",
                    s.status.code().unwrap_or(-1)
                ),
            );
        }
        Err(e) => {
            events::event(
                st,
                "warn",
                origem,
                &format!("{projeto}: live server não disparado (o up segue de pé): {e}"),
            );
        }
    }
}

/// Trava do religar pós-acordar: N requests chegando juntos no projeto que
/// acabou de acordar não disputam o spawn — o `decidir_live` de dentro já é
/// idempotente, mas duas checagens concorrentes podem se cruzar (ambas veem
/// a porta morta antes de qualquer spawn) e o segundo `npm run dev` nasce na
/// 5174 fora do roteamento. Global de propósito: spawn de live é raro e o
/// trecho crítico é curto (probe + `exec -d`).
static LIVE_REDISPATCH: std::sync::LazyLock<tokio::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));

/// Religa o live server de dev quando o projeto acorda PELO ROUTER (feature
/// 19/10): o `docker stop` do sono mata o vite junto com o container, e até
/// aqui só o `flux up` religava — quem abria o site acordava o app sem HMR
/// até rodar um comando na mão. Encontra o container app pelos labels (MESMA
/// regra do `find_app_container`: service `app` ou primeiro), deriva
/// projeto/service/IP do inspect e chama o mesmo caminho do up
/// (`subir_live_se_configurado`), cuja idempotência decide sozinho se há o
/// que fazer. Sem engine, sem container ou sem labels de compose = no-op
/// silencioso (rastro debug/warn, nunca evento nem erro de request).
pub async fn religar_live_pos_acordar(
    st: &crate::state::State,
    app: &crate::AppState,
    docker: &bollard::Docker,
    name: &str,
    env: flux_common::protocol::Env,
) {
    let _guard = LIVE_REDISPATCH.lock().await;
    // O label de compose project é `flux-{name}-{env}` (fonte única
    // `compose_project`) — o nome cru do projeto NUNCA acha container.
    let project = crate::docker::compose_project(name, env);
    let Some(c) = crate::docker::find_app_container(docker, &project).await else {
        tracing::debug!(
            "religar live {name}: sem container app (sem engine ou sem labels) — no-op"
        );
        return;
    };
    let labels = c.config.as_ref().and_then(|cfg| cfg.labels.as_ref());
    let Some(projeto) = labels
        .and_then(|l| l.get("com.docker.compose.project"))
        .cloned()
    else {
        tracing::warn!("religar live {name}: container app sem label de compose project — no-op");
        return;
    };
    let service = labels
        .and_then(|l| l.get("com.docker.compose.service"))
        .cloned()
        .unwrap_or_else(|| "app".to_owned());
    // IP na MESMA fonte do up (`proxy_target_from` sobre o inspect: `ip:80`
    // → ip): a porta do dev server é conferida na VM, não dentro do container.
    let ip = crate::docker::proxy_target_from(&c).and_then(|t| {
        t.rsplit_once(':')
            .map(|(parte_ip, _porta): (&str, &str)| parte_ip.to_owned())
    });
    subir_live_se_configurado(st, app, name, &projeto, &service, ip.as_deref(), "wake").await;
}

/// Túnel/cloudflared por service: `[(service, imagem)]` dos services que
/// carregam connector. Fonte = `docker compose config` em JSON (engine é a
/// verdade; imagem resolvida inclui build). Padrões: cloudflared.
fn tunel_classificar(
    dir: &Path,
    compose: &Path,
    projeto: &str,
    services: &[String],
) -> Vec<(String, String)> {
    let Ok(saida) = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(async {
            tokio::process::Command::new("docker")
                .args(["compose", "-p", projeto])
                .arg("-f")
                .arg(compose)
                .args(["config", "--format", "json"])
                .current_dir(dir)
                .output()
                .await
        })
    }) else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&saida.stdout) else {
        return Vec::new();
    };
    // L6.2: a REGRA é a mesma fn que o teste exercita — cópia apagada
    // (bug na regra real nunca mais passa batido pelo teste da cópia).
    classificar_de(&v, services)
}

/// Regra PURA do túnel (L6.2): `[(service, imagem)]` dos services cuja
/// imagem resolvida é cloudflared. Testada direto (o teste da cópia foi
/// apagado — era o único lugar onde a regra rodava).
fn classificar_de(v: &serde_json::Value, services: &[String]) -> Vec<(String, String)> {
    let mut achados = Vec::new();
    for svc in services {
        let Some(imagem) = v["services"][svc]["image"].as_str() else {
            continue;
        };
        let baixo = imagem.to_lowercase();
        if baixo.contains("cloudflared") || baixo.contains("cloudflare/cloudflared") {
            achados.push((svc.clone(), imagem.to_owned()));
        }
    }
    achados
}

// --------------------------------------------------- blue-green (10, R12)

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
        let y = override_yaml(
            &["app".into(), "worker".into()],
            "dev",
            &["mysql".into()],
            false,
            "https://vippers-d.bitello.xyz",
        );
        assert!(y.contains("  app:\n"));
        assert!(y.contains("  worker:\n"));
        // strip: service assumido entra como !reset null (remove no merge)
        assert!(y.contains("  mysql: !reset null\n"));
        // depends_on de service vivo morre — dependência de removido não sobra
        assert_eq!(
            y.matches("depends_on: !reset null").count(),
            2,
            "depends_on fora de todos os services vivos"
        );
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
        assert!(
            y.contains("ASSET_URL: \"https://vippers-d.bitello.xyz\""),
            "asset público injetado por cima do .env (mesmo mecanismo do DB)"
        );
        assert!(y.contains("networks: !override [default, flux-net]"));
        assert!(y.contains("external: true"), "flux-net é a rede do flux-db");
        // Nada publicado = portscan da 09 continua só 22/80/443.
        assert!(!y.contains("ports:\n    -"));
    }

    #[test]
    fn override_com_live_publica_a_porta_do_dev_server() {
        let sem = override_yaml(&["app".into()], "dev", &[], false, "https://v-d.x");
        let com = override_yaml(&["app".into()], "dev", &[], true, "https://v-d.x");
        assert!(
            !sem.contains("5173:5173"),
            "sem live declarado, nada publicado"
        );
        assert!(
            com.contains("      - \"5173:5173\"\n"),
            "com live, a porta do dev server sobe pro host (hostname vite-dev)"
        );
    }

    #[test]
    fn parse_render_extrai_nome_e_imagem_do_json_do_engine() {
        let json = r#"{
            "name": "flux-vipa-dev",
            "services": {
                "php": {"build": {"context": "docker"}},
                "mysql": {"image": "mysql:9.3", "depends_on": []}
            }
        }"#;
        let r = parse_render(json).expect("parseia");
        assert_eq!(r.len(), 2);
        let mysql = r.iter().find(|s| s.name == "mysql").expect("mysql");
        assert_eq!(mysql.image.as_deref(), Some("mysql:9.3"));
        let php = r.iter().find(|s| s.name == "php").expect("php");
        assert_eq!(php.image, None, "service de build não tem image");
    }

    #[test]
    fn strip_list_pega_so_service_com_imagem_bloqueada() {
        let block = vec!["mysql".into(), "redis".into()];
        let renderizado = vec![
            ServiceRender {
                name: "php".into(),
                image: None,
            },
            ServiceRender {
                name: "mysql".into(),
                image: Some("mysql:9.3".into()),
            },
            ServiceRender {
                name: "app".into(),
                image: Some("ghcr.io/projeto/app:dev".into()),
            },
        ];
        assert_eq!(strip_list(&renderizado, &block), vec!["mysql".to_owned()]);
    }

    #[test]
    fn decidir_live_cobre_as_quatro_combinacoes() {
        use AcaoLive::*;
        assert!(
            matches!(decidir_live(true, true), Pular),
            "tudo vivo = pular"
        );
        assert!(
            matches!(decidir_live(true, false), Ressuscitar),
            "hot órfão = ressuscitar"
        );
        assert!(
            matches!(decidir_live(false, true), Ressuscitar),
            "porta viva sem contrato = zumbi, ressuscitar"
        );
        assert!(matches!(decidir_live(false, false), Ressuscitar));
    }

    #[test]
    fn comando_live_monta_exec_detach_no_app_sem_chumbar_workdir() {
        let args = comando_live_args("flux-vipa-dev", "php", "npm run build -- --watch");
        assert_eq!(
            args[..6],
            ["compose", "-p", "flux-vipa-dev", "exec", "-d", "php"],
            "exec detached no service PRINCIPAL do projeto (app ou primeiro)"
        );
        assert_eq!(args[6], "sh", "shell é sh (paridade com o exec do db.rs)");
        assert_eq!(args[7], "-c");
        assert_eq!(
            args[8], "{ npm run build -- --watch; }; rc=$?; rm -f public/hot; exit $rc",
            "comando do toml verbatim dentro do wrapper que limpa hot órfão"
        );
    }

    #[test]
    fn comando_live_usa_app_quando_existe() {
        let args = comando_live_args(
            "flux-presente-digital-dev",
            "app",
            "npm run build -- --watch",
        );
        assert_eq!(args[5], "app");
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

#[cfg(test)]
mod tunel {
    // L6.2: os testes exercitam a REGRA REAL (`classificar_de` de compose.rs)
    // — a cópia local foi apagada (era o único lugar onde a regra rodava).
    use super::*;

    fn config_com(servicos: serde_json::Value) -> serde_json::Value {
        serde_json::json!({ "services": servicos })
    }

    #[test]
    fn cloudflared_em_app_e_o_principal_bloqueia() {
        let v = config_com(serde_json::json!({
            "app": { "image": "cloudflare/cloudflared:latest" }
        }));
        let achados = classificar_de(&v, &["app".to_owned()]);
        assert_eq!(achados.len(), 1);
        assert_eq!(
            achados[0].0, "app",
            "principal: o chamador NEGA o up inteiro"
        );
    }

    #[test]
    fn cloudflared_em_sidecar_sera_pulado() {
        let v = config_com(serde_json::json!({
            "app": { "image": "serve:local" },
            "tunnel": { "image": "cloudflare/cloudflared:latest" }
        }));
        let achados = classificar_de(&v, &["app".to_owned(), "tunnel".to_owned()]);
        assert_eq!(achados.len(), 1);
        assert_eq!(achados[0].0, "tunnel", "secundário: --scale 0 só nele");
    }

    #[test]
    fn imagem_build_sem_field_image_nao_e_tunel() {
        // Service com build (sem image no config JSON) NUNCA é túnel.
        let v = config_com(serde_json::json!({
            "app": { "build": { "context": "./serve" } }
        }));
        let achados = classificar_de(&v, &["app".to_owned()]);
        assert!(achados.is_empty());
    }
}
