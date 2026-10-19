//! `flux` — CLI da plataforma no Windows (06). Árvore de comandos completa
//! conforme a spec: a superfície de comandos é design de API, então nasce
//! inteira com stubs que recusam com fase e exit ≠ 0 ("nada silencioso").
//! Fase F6: os comandos do ciclo de dev falam com a API de controle do fluxd
//! (:7420, 07) via `api.rs` — o CLI NUNCA toca em Docker direto (regra da 06);
//! sem fluxd no ar, a mensagem traz a causa e o exit é ≠ 0. Contexto do
//! projeto = diretório atual (`flux.toml` explícito ou nome da pasta).

mod api;
// Sync contínua (spec 02): monta argumentos e executa o mutagen — consumida
// por `flux sync`, `flux build`, `flux watch` e pelo `flux up` (fase 02
// ligada; o allow de dead_code do módulo saiu junto com o motivo).
mod comandos;
mod contexto;
mod sync;
// Túnel SSH p/ a API de controle + config global do usuário (spec 09).
mod tunel;
// Wizard `flux up -m` que gera o flux.toml (spec 06).
mod wizard;

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use contexto::{LEASE_UP_SECS, contexto_atual, falhar, parse_env};
use flux_common::protocol::Env;
use sha2::{Digest, Sha256};

use crate::api::{ApiClient, ApiError};
use comandos::build::build_assets;
use comandos::env_cmd::{env_edit, env_pull, env_push};
use comandos::live::live_toggle;
use comandos::logs::logs;
use comandos::sync_cmd::{sync_pause_cmd, sync_reset_cmd, sync_resume_cmd, sync_status_cmd};
use comandos::tokens::{project_token, token_generate, vm_ssh};

/// Teto de espera pelo acordar do container no `flux up` (build de primeira
/// subida é de minutos; acima disso o problema é visível no painel).
const UP_TIMEOUT_SECS: u64 = 600;

#[derive(Parser)]
#[command(
    name = "flux",
    version,
    about = "CLI da plataforma Flux — docker/compose do repo, sem prompt de sync"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Ciclo completo do dev no repo atual (06). -m/--manifest abre o wizard
    /// que GERA o flux.toml antes de subir.
    Up {
        #[arg(short = 'm', long = "manifest", default_value_t = false)]
        manifesto: bool,
        /// Força seed no rebuild do banco (05) — vence o flux.toml.
        #[arg(long, conflicts_with = "no_seed")]
        seed: bool,
        /// Proíbe seed no rebuild do banco (05) — vence o flux.toml.
        #[arg(long)]
        no_seed: bool,
        #[arg(default_value = "dev")]
        env: String,
    },
    /// Para os containers do projeto (banco único segue de pé).
    Down {
        #[arg(default_value = "dev")]
        env: String,
    },
    /// Down + up, sem rebuild.
    Restart {
        #[arg(default_value = "dev")]
        env: String,
    },
    /// Sync, containers, health, banco, migrations pendentes, URL, sono.
    Status {
        #[arg(default_value = "dev")]
        env: String,
    },
    /// Espelho do `docker compose ps`.
    Ps {
        #[arg(default_value = "dev")]
        env: String,
    },
    /// Logs do container (lease de sono ativo, 04; janela histórica 13).
    Logs {
        #[arg(short, long)]
        follow: bool,
        /// Últimas N linhas (contrato CLI ↔ fluxd; padrão 200).
        #[arg(long)]
        tail: Option<u64>,
        #[arg(long)]
        since: Option<String>,
        #[arg(long)]
        until: Option<String>,
        /// env ANTES de [svc]: `flux logs dev` tem de virar env, não serviço
        /// (o caso comum). A ordem [svc] [env] da spec 06 entra na fase 13,
        /// quando o fluxd aplicar a janela.
        #[arg(default_value = "dev")]
        env: String,
        /// Serviço específico (fase 13 — hoje o fluxd não aplica).
        service: Option<String>,
    },
    /// `docker exec` no container principal (contrato CLI ↔ fluxd: corpo
    /// `{"cmd":[…]}`; exit do comando é repassado). O `--` separa o comando:
    /// `flux exec [ENV] -- <cmd…>`.
    Exec {
        #[arg(default_value = "dev")]
        env: String,
        #[arg(last = true)]
        command: Vec<String>,
    },
    /// `flux exec bash`.
    Shell {
        #[arg(default_value = "dev")]
        env: String,
    },
    /// Build de assets DENTRO do container de dev na VM (02): garante a
    /// sessão de sync, espera a janela de silêncio da árvore no Windows
    /// ([build].silence_window_secs, default 180 s) e roda [build].command
    /// (default por framework: laravel = `npm run build`) sob o lock global
    /// `/srv/flux/build.lock` — com teto próprio de 1800 s (o cap de 120 s
    /// do exec NÃO vale aqui) e exit code real do comando.
    Build,
    /// Reativa o release anterior em prod, sem build (10).
    Prod {
        #[command(subcommand)]
        cmd: ProdCmd,
    },
    /// Modo manutenção: 503 + Retry-After, bypass com token 24 h (15).
    Maintenance {
        #[arg(value_parser = ["on", "off"])]
        mode: String,
        name: String,
        #[arg(long)]
        bypass: bool,
    },
    /// Desbloqueia IP banido pelo WAF próprio (16).
    Shield {
        #[command(subcommand)]
        cmd: ShieldCmd,
    },
    /// Escreve o workflow de deploy no projeto a partir do template (10).
    Init {
        /// Só `ci` na spec.
        #[arg(value_parser = ["ci"])]
        target: String,
    },
    /// Gera/rotaciona o token de deploy (vai para GitHub Secrets, 09/10).
    Project {
        #[command(subcommand)]
        cmd: ProjectCmd,
    },
    /// Toolchain sob demanda em container efêmero na VM (R17): nada no PC.
    Tool {
        /// node | php | composer | rust
        runtime: String,
        #[arg(last = true)]
        args: Vec<String>,
    },
    /// Serviço leve: monitora branches/worktrees e reporta ao fluxd (02).
    Watch,
    /// Banco do projeto (05); `console` abre cliente SQL no flux-db.
    Db {
        #[command(subcommand)]
        cmd: DbCmd,
    },
    /// Controle da sync contínua (02).
    Sync {
        #[command(subcommand)]
        cmd: SyncCmd,
    },
    /// Ciclo do `.env` exclusivo da VM (02).
    Env {
        #[command(subcommand)]
        cmd: EnvCmd,
    },
    /// Ciclo da VM (01) + upgrade do fluxd com rollback (14).
    Vm {
        #[command(subcommand)]
        cmd: VmCmd,
    },
    /// Provisionamento: host | vm | tunnel | uptime (01/03/09/13).
    Setup {
        #[arg(value_parser = ["host", "vm", "tunnel", "uptime"])]
        target: String,
    },
    /// Token da API de controle (09): gera, grava hash na VM e valor no config.
    Token {
        #[command(subcommand)]
        sub: TokenCmd,
    },
    /// Saúde do cloudflared (03).
    Tunnel {
        #[command(subcommand)]
        cmd: TunnelCmd,
    },
    /// Liga/desliga o live server (HMR) SEM down (06/live): OFF mata o dev
    /// server, limpa o public/hot e o site volta pro manifest na hora; ON
    /// religa o comando do [dev] live. O comando fica guardado em
    /// .flux/live.bak (gitignored) enquanto desligado.
    Live {
        /// Dev (padrão).
        env: Option<String>,
    },
    /// Abre o painel no browser (login único).
    Panel,
    /// DR: zip de config+state+secrets cifrado com passphrase (14).
    Export,
    /// DR: reergue em VM nova a partir do zip exportado (14).
    Import { arquivo: PathBuf },
    /// Atualiza o CLI no Windows com checksum verificado (14).
    SelfUpdate,
}

#[derive(Subcommand)]
enum ProdCmd {
    Rollback { name: String },
}

#[derive(Subcommand)]
enum TokenCmd {
    /// Gera token novo (CSPRNG 32 bytes), grava o sha256 no SQLite do fluxd
    /// via SSH e o valor claro no config do usuário — estilo key:generate.
    Generate,
}

#[derive(Subcommand)]
enum ShieldCmd {
    Unban { ip: String },
}

#[derive(Subcommand)]
enum ProjectCmd {
    Token { name: String },
}

#[derive(Subcommand)]
enum DbCmd {
    Status {
        #[arg(default_value = "dev")]
        env: String,
    },
    Migrate {
        #[arg(default_value = "dev")]
        env: String,
    },
    Fresh {
        /// Força a reconstrução (mata os dados de dev) — sem isso, recusa.
        #[arg(long)]
        yes: bool,
        /// Liga a seed depois do fresh (precedência sobre o flux.toml).
        #[arg(long, conflicts_with = "no_seed")]
        seed: bool,
        /// Desliga a seed (precedência sobre o flux.toml).
        #[arg(long, conflicts_with = "seed")]
        no_seed: bool,
        #[arg(default_value = "dev")]
        env: String,
    },
    Seed {
        #[arg(default_value = "dev")]
        env: String,
    },
    Console {
        #[arg(default_value = "dev")]
        env: String,
    },
    Dump {
        #[arg(default_value = "dev")]
        env: String,
    },
    /// Restaura dump pré-migrate de prod — exige --yes (14).
    Restore {
        name: String,
        arquivo: PathBuf,
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
enum SyncCmd {
    Status,
    Pause,
    Resume,
    Reset,
}

#[derive(Subcommand)]
enum EnvCmd {
    Pull,
    Push,
    Edit,
}

#[derive(Subcommand)]
enum VmCmd {
    Status,
    Start,
    Stop,
    Ssh,
    Checkpoint,
    /// Checkpoint Hyper-V + novo fluxd com rollback automático (14).
    Upgrade,
}

#[derive(Subcommand)]
enum TunnelCmd {
    Status,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    match cli.cmd {
        // ------- F6: comandos que falam com a API de controle (07) -------
        Cmd::Up {
            manifesto,
            seed,
            no_seed,
            env,
        } => {
            if manifesto {
                let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                let pasta = cwd
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("projeto")
                    .to_owned();
                match wizard::gerar(&cwd, &pasta) {
                    Ok(Some(_)) => {}
                    Ok(None) => std::process::exit(1),
                    Err(e) => {
                        eprintln!("erro no wizard do flux.toml: {e}");
                        std::process::exit(1);
                    }
                }
            }
            up(&env, seed_escolhida(seed, no_seed)).await
        }
        Cmd::Down { env } => down(&env).await,
        Cmd::Restart { env } => ciclo_api("restart", &env).await,
        Cmd::Status { env } => status(&env).await,
        Cmd::Ps { env } => ps(&env).await,
        Cmd::Logs {
            follow,
            since,
            until,
            service,
            tail,
            env,
        } => logs(follow, since, until, service, tail, &env).await,
        Cmd::Exec { command, env } => exec(command, &env).await,
        Cmd::Shell { env } => exec(vec!["bash".to_owned()], &env).await,
        Cmd::Build => build_assets().await,
        // ------- fases futuras: recusa com fase e exit ≠ 0 -------
        Cmd::Tool { runtime: _, args } => {
            if args.is_empty() {
                eprintln!(
                    "erro: uso: flux tool <runtime> -- <comando> (ex.: flux tool node -- npm run build)"
                );
                std::process::exit(2);
            }
            stub("fase futura (spec 06) — toolchain sob demanda na VM");
        }
        Cmd::Watch => watch().await,
        Cmd::Init { target: _ } => stub("fase F9 (spec 10) — workflow de deploy por push"),
        Cmd::Prod { .. } => stub("fase F9 (spec 10) — prod é git push + rollback"),
        Cmd::Maintenance { .. } => stub("fase F18 (spec 15) — modo manutenção com bypass"),
        Cmd::Shield { .. } => stub("fase F19 (spec 16) — WAF próprio, shadow antes de enforce"),
        Cmd::Db { cmd } => match cmd {
            DbCmd::Fresh {
                yes,
                seed,
                no_seed,
                env,
            } => {
                if !yes {
                    eprintln!(
                        "erro: fresh APAGA os dados de dev (migrate:fresh) — repasse --yes para confirmar"
                    );
                    std::process::exit(2);
                }
                db_fresh(
                    &env,
                    if seed {
                        Some(true)
                    } else if no_seed {
                        Some(false)
                    } else {
                        None
                    },
                )
                .await;
            }
            DbCmd::Restore { yes, .. } => {
                if !yes {
                    eprintln!(
                        "erro: restore de prod é destrutivo — repasse --yes para confirmar (spec 14)"
                    );
                    std::process::exit(2);
                }
                stub("fase F16 (spec 14) — snapshot pré-migrate e restore")
            }
            _ => stub("fase F7+ (spec 05) — banco único e migrations via fluxd"),
        },
        Cmd::Sync { cmd } => match cmd {
            SyncCmd::Status => sync_status_cmd(),
            SyncCmd::Pause => sync_pause_cmd(),
            SyncCmd::Resume => sync_resume_cmd(),
            SyncCmd::Reset => sync_reset_cmd(),
        },
        Cmd::Env { cmd } => {
            let ctx = contexto_atual();
            match cmd {
                EnvCmd::Pull => env_pull(&ctx.name),
                EnvCmd::Push => env_push(&ctx.name),
                EnvCmd::Edit => env_edit(&ctx.name),
            }
        }
        Cmd::Vm { cmd } => match cmd {
            VmCmd::Ssh => vm_ssh(),
            VmCmd::Upgrade => stub("fase F16 (spec 14) — upgrade do fluxd com rollback"),
            _ => stub("fase F1 (spec 01) — VM Hyper-V Bookworm"),
        },
        Cmd::Setup { .. } => stub("fase F1 (spec 01) — setup do host Windows + VM"),
        Cmd::Token {
            sub: TokenCmd::Generate,
        } => token_generate(),
        Cmd::Project {
            cmd: ProjectCmd::Token { name },
        } => project_token(&name),
        Cmd::Tunnel { .. } => stub("fase F7 (spec 03) — tunnel Cloudflare e domínios"),
        Cmd::Live { env } => live_toggle(env.as_deref().unwrap_or("dev")),
        Cmd::Panel => stub("fase F8 (spec 08) — painel web com auth"),
        Cmd::Export => stub("fase F16 (spec 14) — DR export"),
        Cmd::Import { .. } => stub("fase F16 (spec 14) — DR import"),
        Cmd::SelfUpdate => stub("fase F16 (spec 14) — self-update com checksum"),
    }
}

// ------------------------------------------------------------- contexto 06

// ------------------------------------------------------- token da API (09)

// ------------------------------------------- token de deploy (09/10, F4)

// ---------------------------------------------------------- comandos F6

/// `flux up [env]` (06, fluxo passo a passo). Fase F6: passos 1–4 reais
/// (contexto, saúde do fluxd, registro, lease renovável) + status consolidado;
/// build/sync/banco (passos 5–7) são orquestrados pelo fluxd e entram nas
/// fases F7+ — o que a API expõe hoje aparece com rótulo honesto. `seed`:
/// None = fluxd decide pelo flux.toml; Some = flag explícita vence o toml (05).
async fn up(env: &str, seed: Option<bool>) {
    let env = parse_env(env);
    let ctx = contexto_atual();
    let cfg = flux_common::config::ProjectToml::parse_str(&ctx.toml).unwrap_or_else(|e| {
        eprintln!("erro: flux.toml inválido: {e}");
        std::process::exit(1);
    });
    let cliente = ApiClient::from_env();

    let h = match cliente.health().await {
        Ok(h) => h,
        Err(e) => falhar(e),
    };
    println!(
        "[1/5] fluxd {} saudável em {}",
        h.version,
        cliente.base_url()
    );

    let resumo = match cliente.register(&ctx.name, &ctx.toml, &ctx.toml_hash).await {
        Ok(r) => r,
        Err(e) => falhar(e),
    };
    println!(
        "[2/5] projeto {} ({}) registrado — prod https://{} · dev https://{}",
        resumo.name, resumo.framework, resumo.prod_host, resumo.dev_host
    );

    let lease = match cliente
        .lease(&ctx.name, &env.to_string(), LEASE_UP_SECS)
        .await
    {
        Ok(l) => l,
        Err(e) => falhar(e),
    };
    // Renovação em linha enquanto o comando vive; no fim da função a guarda
    // para de renovar e o lease morre no TTL (04).
    let _guarda = api::manter_lease(
        cliente.clone(),
        ctx.name.clone(),
        env.to_string(),
        LEASE_UP_SECS,
    );
    println!(
        "[3/6] lease de {}/{} por {} s — renova sozinho até o comando terminar",
        ctx.name, env, LEASE_UP_SECS
    );

    // [4/6] Sync (02): sessão one-way-replica Windows → VM, idempotente.
    if env == Env::Dev {
        let beta = crate::sync::vm_beta(&ctx.name, None);
        match crate::sync::SyncSpec::for_project(&ctx.dir, beta, None) {
            Ok(spec) => match crate::sync::create(&spec) {
                Ok(_) => {
                    println!(
                        "[4/6] sync `{}` ativa — pasta da VM: /srv/flux/projects/{} (full scan inicial roda em fundo)",
                        spec.session(),
                        ctx.name
                    );
                    std::thread::sleep(std::time::Duration::from_secs(2));
                }
                Err(e) => falhar(api::ApiError::Protocolo {
                    msg: format!("sync: {e}"),
                }),
            },
            Err(e) => falhar(api::ApiError::Protocolo {
                msg: format!("montando sync: {e}"),
            }),
        }
    } else {
        println!("[4/6] prod não sincroniza do Windows (02) — release vem do deploy (10)");
    }

    // [4.5/6] Overlays (02/06): `[dev] overlays = true` + `.flux/overlays/`
    // presente → espelha os arquivos por cima do repo NA VM (tar por ssh),
    // antes do compose/migrate. Repo no Windows NUNCA é tocado; os arquivos
    // são gitignored e o conteúdo nunca é exibido. Sem flag ou sem dir = nada.
    if overlays_habilitados(&ctx.toml) {
        let origem = ctx.dir.join(".flux/overlays");
        match aplicar_overlays(&origem, &ctx.name) {
            Ok(n) if n > 0 => {
                println!("[4.5/6] overlays aplicados na VM: {n} arquivo(s) de .flux/overlays/")
            }
            Ok(_) => println!(
                "[4.5/6] overlays: flag ligada mas .flux/overlays/ vazio ou ausente — nada a aplicar"
            ),
            Err(e) => eprintln!("aviso: overlays não aplicados: {e} — o up seguiu"),
        }
    }

    // [5/6] Subida: 202 do fluxd + apuração pelo estado do engine.
    let _aceito = match cliente.subir(&ctx.name, &env.to_string()).await {
        Ok(a) => a,
        Err(e) => falhar(e),
    };
    println!("[5/6] compose up -d --build rodando no fluxd — apurando estado…");
    let mut st = match cliente.status(&ctx.name).await {
        Ok(s) => s,
        Err(e) => falhar(e),
    };
    let acordado_em = |s: &api::ProjectStatusOut| s.dev_container_state.as_deref() == Some("awake");
    let mut acordado = acordado_em(&st);
    let mut esperado = 0u64;
    while !acordado && esperado < UP_TIMEOUT_SECS {
        std::thread::sleep(std::time::Duration::from_secs(3));
        esperado += 3;
        match cliente.status(&ctx.name).await {
            Ok(s) => {
                st = s;
                acordado = acordado_em(&st);
                if esperado.is_multiple_of(30) {
                    println!(
                        "      … {} s (estado: {})",
                        esperado,
                        st.sleep.dev.mode.as_deref().unwrap_or("sem sinal")
                    );
                }
            }
            Err(e) => {
                eprintln!("aviso: apuração falhou uma vez: {e}");
            }
        }
    }
    if !acordado {
        falhar(api::ApiError::Protocolo {
            msg: format!(
                "container não acordou em {UP_TIMEOUT_SECS} s — veja os eventos no painel                  (https://flux.bitello.xyz) ou `flux events` quando existir; causa comum: build                  falhando no compose"
            ),
        });
    }
    println!("[5/6] container acordado (engine RUNNING) após {esperado} s");

    // [6/6] Migrations (05): o CLI coleta `database/migrations/*.php` do repo
    // (nome + sha256) e manda no corpo — o contrato é esse (api.rs: "o CLI lê
    // do repo e manda"); o fluxd compara com o hash aplicado: dev novo/mutado
    // reconstrói, prod só aplica. Seed segue a precedência da flag
    // (--seed/--no-seed vencem o flux.toml). ANTES de coletar, espera a
    // estabilidade da subárvore de migrations ([db].silence_window_secs,
    // default 5 s — 02): coleta no meio de uma escrita produz hash que
    // diverge do que a sync replica, e o plano de rebuild erra por dados
    // falsos — instável = pipeline PULADO com aviso, nada enviado ao fluxd.
    let janela_db = cfg
        .db
        .as_ref()
        .map(|d| d.silence_window_secs())
        .unwrap_or(5);
    let extra_sync: Vec<String> = crate::sync::ignores_do_projeto(&ctx.dir, cfg.sync.as_ref())
        .unwrap_or_else(|e| {
            eprintln!("erro: {e}");
            std::process::exit(1);
        });
    match crate::sync::esperar_estabilidade(
        &ctx.dir,
        &extra_sync,
        Some(Path::new("database/migrations")),
        std::time::Duration::from_secs(janela_db),
        |msg| println!("      {msg}"),
    ) {
        Err(e) => eprintln!(
            "aviso: migrations instáveis ({e}) — pipeline de schema PULADO, nada enviado ao fluxd; rode `flux up` de novo quando a árvore parar"
        ),
        Ok(()) => {
            let migrations = coletar_migrations(&extra_sync);
            if migrations.is_empty() {
                eprintln!("[6/6] banco: nenhuma migration em database/migrations — schema pulado");
            }
            match cliente
                .schema(&ctx.name, &env.to_string(), &migrations, seed)
                .await
            {
                Ok(o) => println!(
                    "[6/6] banco: {} · aplicadas {} · fresh {}",
                    o.outcome, o.aplicadas, o.fresh
                ),
                Err(e) => {
                    eprintln!("aviso: pipeline de migrations falhou: {e} — o resto do up seguiu")
                }
            }
        }
    }
    imprime_status(&st);
    println!(
        "pronto: https://{} · lease até epoch {}",
        st.dev_host, lease.lease_until
    );
}

/// `[dev] overlays` ligado no flux.toml do projeto? Parse tolerante: toml
/// quebrado = false (o register já vai reclamar dele).
fn overlays_habilitados(toml_src: &str) -> bool {
    flux_common::config::ProjectToml::parse_str(toml_src)
        .ok()
        .and_then(|t| t.dev)
        .and_then(|d| d.overlays)
        .unwrap_or(false)
}

/// Aplica `.flux/overlays/**` sobre o repo NA VM: tar do dir local, pipe por
/// ssh, extração em `/srv/flux/projects/{name}/`. Devolve quantos arquivos
/// foram aplicados (contagem local — a lista nunca contém conteúdo, só paths).
fn aplicar_overlays(origem: &Path, name: &str) -> Result<usize, String> {
    let mut arquivos = Vec::new();
    coletar_overlays(origem, &mut arquivos)?;
    if arquivos.is_empty() {
        return Ok(0);
    }
    let destino = format!("/srv/flux/projects/{name}/");
    let status = std::process::Command::new("tar")
        .arg("czf")
        .arg("-")
        .arg("-C")
        .arg(origem)
        .arg(".")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .and_then(|mut tar| {
            let mut ssh = std::process::Command::new("ssh")
                .args([
                    "-o",
                    "BatchMode=yes",
                    "-o",
                    "ConnectTimeout=8",
                    &format!("{}@{}", tunel::VM_USER, tunel::VM_HOST),
                    &format!("tar xzf - -C {destino}"),
                ])
                .stdin(tar.stdout.take().expect("stdout do tar pipado"))
                .stderr(std::process::Stdio::piped())
                .spawn()?;
            tar.wait()?;
            ssh.wait()
        })
        .map_err(|e| e.to_string())?;
    if !status.success() {
        return Err("tar/ssh do overlay falhou".to_owned());
    }
    Ok(arquivos.len())
}

/// Walk recursivo simples: só arquivos regulares, paths relativos.
fn coletar_overlays(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let caminho = entry.path();
        if caminho.is_dir() {
            coletar_overlays(&caminho, out)?;
        } else {
            out.push(caminho);
        }
    }
    Ok(())
}

/// Coleta `database/migrations/*.php` do repo ATUAL (o `flux up` roda na
/// pasta do projeto), ordenado por nome (ordem cronológica do Laravel) e
/// com sha256 de cada conteúdo — o par (arquivo, hash) que o contrato do
/// schema espera. Dir ausente = lista vazia (projeto sem migrations).
/// Filtra pelos MESMOS ignores da sync (GLOBAL_IGNORES + `[sync]`):
/// arquivo fora da sync não chega à VM — gravar o hash dele no fluxd
/// registraria "aplicada" uma migration que o `php artisan migrate` nunca
/// vê (o estado mentiria sobre o banco).
fn coletar_migrations(extra_ignore: &[String]) -> Vec<(String, String)> {
    let mut ignores: Vec<String> = crate::sync::GLOBAL_IGNORES
        .iter()
        .map(|s| s.to_string())
        .collect();
    ignores.extend(extra_ignore.iter().cloned());
    filtra_migrations_ignoradas(
        coletar_migrations_em(Path::new("database/migrations")),
        &ignores,
    )
}

/// Filtro puro, separado do I/O para testar sem chdir: descarta pares cujo
/// caminho relativo à raiz do repo casa com algum ignore da sync.
fn filtra_migrations_ignoradas(
    lista: Vec<(String, String)>,
    ignores: &[String],
) -> Vec<(String, String)> {
    lista
        .into_iter()
        .filter(|(nome, _)| {
            !crate::sync::caminho_ignorado(&format!("database/migrations/{nome}"), ignores)
        })
        .collect()
}

/// Versão injetável do coletor — pura o suficiente para testar sem chdir.
fn coletar_migrations_em(dir: &Path) -> Vec<(String, String)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut arquivos: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().map(|x| x == "php").unwrap_or(false))
        .collect();
    arquivos.sort();
    arquivos
        .iter()
        .filter_map(|p| {
            let corpo = std::fs::read(p).ok()?;
            let mut h = Sha256::new();
            h.update(&corpo);
            let nome = p.file_name()?.to_str()?.to_owned();
            Some((nome, format!("{:x}", h.finalize())))
        })
        .collect()
}

/// `flux ps [env]` — espelho do `docker compose ps` (06) com a regra do
/// `docker ps`: FORA de projeto (pasta sem flux.toml e não registrada)
/// lista TODOS os gerenciados; dentro, filtra o projeto (env afina
/// dev/prod). Estado cru do engine via GET /containers.
async fn ps(env: &str) {
    let cliente = ApiClient::from_env();
    let rows = match cliente.containers().await {
        Ok(r) => r,
        Err(e) => falhar(e),
    };
    if rows.is_empty() {
        println!("nenhum container gerenciado (nenhum `flux up` ainda)");
        return;
    }
    let cwd = std::env::current_dir().unwrap_or_default();
    let pasta = cwd
        .file_name()
        .and_then(|n| n.to_str())
        .map(str::to_owned)
        .unwrap_or_default();
    // Contexto: flux.toml vence (o `name` DELE, não o da pasta — djm mora
    // em DJ-is-magic); sem toml, o nome da pasta se estiver registrado.
    let projeto_ctx: Option<String> = if cwd.join("flux.toml").is_file() {
        std::fs::read_to_string(cwd.join("flux.toml"))
            .ok()
            .and_then(|src| flux_common::config::ProjectToml::parse_str(&src).ok())
            .map(|t| t.effective_name(&pasta).to_owned())
    } else {
        let registrados: Vec<String> = cliente
            .list_projects()
            .await
            .map(|l| l.into_iter().map(|p| p.name).collect())
            .unwrap_or_default();
        registrados.into_iter().find(|r| r == &pasta)
    };
    let env_filtro = if projeto_ctx.is_some() {
        parse_env(env);
        Some(env.to_owned())
    } else {
        None
    };

    let alvo = |r: &api::ContainerRowOut| {
        // compose project: flux-{name}-{env} (worktrees: flux-{name}-@{slug}-{env})
        r.compose_project
            .strip_prefix("flux-")
            .unwrap_or(&r.compose_project)
            .to_owned()
    };
    let nome_ctx: Option<String> = match (&projeto_ctx, env_filtro.as_ref()) {
        (Some(p), Some(e)) => Some(format!("flux-{p}-{e}")),
        (Some(p), None) => Some(format!("flux-{p}-")),
        _ => None,
    };

    let mut visiveis: Vec<&api::ContainerRowOut> = rows
        .iter()
        .filter(|r| match &nome_ctx {
            Some(prefixo) => r.compose_project.starts_with(prefixo.as_str()),
            None => true,
        })
        .collect();
    if visiveis.is_empty() {
        if let Some(p) = projeto_ctx {
            eprintln!(
                "nenhum container de '{p}' — rode `flux up` (ou `flux ps` fora do projeto para ver todos)"
            );
            std::process::exit(1);
        }
        return;
    }
    // Projeto do contexto primeiro; resto depois (fora de projeto = tudo).
    visiveis.sort_by_key(|r| {
        let do_ctx = projeto_ctx
            .as_ref()
            .is_some_and(|p| alvo(r).starts_with(p.as_str()));
        (!do_ctx, r.compose_project.clone(), r.service.clone())
    });

    // Agrupado por projeto ("pastas", pedido do Gabriel 14/10) — estado
    // agregado no header: DORMINDO = nada rodando; PARCIAL = misto (não
    // deveria existir: sono de dev para o projeto INTEIRO); ACORDADO = tudo up.
    let mut grupos: Vec<(String, Vec<&api::ContainerRowOut>)> = Vec::new();
    for r in &visiveis {
        match grupos.last_mut() {
            Some((p, v)) if *p == r.compose_project => v.push(r),
            _ => grupos.push((r.compose_project.clone(), vec![r])),
        }
    }
    for (projeto, containers) in &grupos {
        let rodando = containers.iter().filter(|c| c.state == "running").count();
        let estado_projeto = if rodando == 0 {
            "DORMINDO"
        } else if rodando == containers.len() {
            "ACORDADO"
        } else {
            "PARCIAL"
        };
        // flux-{name}-{env} → o nome que o Gabriel conhece (a pasta).
        let nome = projeto
            .strip_prefix("flux-")
            .unwrap_or(projeto)
            .trim_end_matches("-dev")
            .trim_end_matches("-prod");
        println!("{nome}  [{estado_projeto}]  ({projeto})");
        for c in containers {
            println!("    {:<12} {:<9} {}", c.service, c.state, c.status);
        }
        println!();
    }
    let total = rows.len();
    if projeto_ctx.is_some() && visiveis.len() < total {
        println!(
            "+ {} container(ns) de outros projetos (flux ps fora do projeto lista todos)",
            total - visiveis.len()
        );
    }
}

/// `flux status [env]` — o que a API de controle expõe hoje. Projeto do
/// diretório não registrado → mostra a lista do fluxd como hint e sai com
/// exit ≠ 0 (o caminho de conserto é `flux up` no repo).
async fn status(env: &str) {
    let _ = parse_env(env); // valida cedo, com exit 2 em valor ruim
    let ctx = contexto_atual();
    let cliente = ApiClient::from_env();
    let st = match cliente.status(&ctx.name).await {
        Ok(s) => s,
        Err(ApiError::NaoEncontrado { msg }) => {
            eprintln!("erro: {msg}");
            match cliente.list_projects().await {
                Ok(lista) if lista.is_empty() => {
                    eprintln!("o fluxd não tem nenhum projeto registrado ainda");
                }
                Ok(lista) => {
                    let nomes: Vec<&str> = lista.iter().map(|p| p.name.as_str()).collect();
                    eprintln!("registrados no fluxd: {}", nomes.join(", "));
                }
                Err(e) => eprintln!("aviso: não consegui listar os projetos do fluxd: {e}"),
            }
            std::process::exit(1);
        }
        Err(e) => falhar(e),
    };
    imprime_status(&st);
}

/// Colunas da 06 suportadas AGORA (sono 04, migrations 05): sync/health/build
/// em tempo real dependem de fases do fluxd — aparecem como "—", nunca
/// inventados.
fn imprime_status(st: &api::ProjectStatusOut) {
    println!(
        "{} · {} · prod https://{} · dev https://{}",
        st.name, st.framework, st.prod_host, st.dev_host
    );
    println!("  sync: — (fase F2 do fluxd) · health: — (fase F7 do fluxd)");
    for (rotulo, linha) in [("dev", &st.sleep.dev), ("prod", &st.sleep.prod)] {
        let mode = linha.mode.as_deref().unwrap_or("sem sinal");
        let lease = match linha.lease_until {
            Some(t) => format!(" · lease até epoch {t}"),
            None => String::new(),
        };
        println!("  sono {rotulo}: {mode}{lease}");
    }
    println!(
        "  migrations aplicadas — dev: {} · prod: {} (pendências: calculadas no `flux up` contra o disco do projeto)",
        st.migrations.dev.len(),
        st.migrations.prod.len()
    );
}

/// Precedência da flag de seed (05): flag explícita vence o flux.toml; sem
/// flag, None — o fluxd decide pelo `seed_on_rebuild`. `--seed` junto com
/// `--no-seed` é bloqueado pelo `conflicts_with` do clap.
fn seed_escolhida(seed: bool, no_seed: bool) -> Option<bool> {
    if seed {
        Some(true)
    } else if no_seed {
        Some(false)
    } else {
        None
    }
}

/// `flux down [env]` (06 + contrato CLI ↔ fluxd): POST /projects/{name}/down —
/// o fluxd roda `docker compose -p flux-{name}-{env} stop` (containers param;
/// banco único segue de pé) e responde `{"stopped":true,"project":…}`. Erro
/// sai com a causa e exit ≠ 0 (06).
async fn down(env: &str) {
    let env = parse_env(env);
    let ctx = contexto_atual();
    let cliente = ApiClient::from_env();
    match cliente.parar(&ctx.name, &env.to_string()).await {
        Ok(r) => {
            if r.stopped {
                println!(
                    "down {}: containers parados (banco único segue de pé)",
                    r.project
                );
            } else {
                // O contrato só devolve stopped=false se algo estranho ocorrer
                // com 200 — nunca tratar como sucesso silencioso.
                eprintln!("erro: fluxd respondeu stopped=false para {}", r.project);
                std::process::exit(1);
            }
        }
        Err(e) => falhar(e),
    }
}

/// restart/ps — o CLI já fala o protocolo da 07: chama o endpoint do
/// projeto, mostra a resposta e sai com exit ≠ 0 enquanto o backend do fluxd
/// ainda é 501. Quando a fase do backend chegar, o comando funciona sem
/// mudança aqui. (down saiu daqui: virou contrato real, `down()` acima;
/// build também — virou o `flux build` real da 02, `build_assets()` acima.)
async fn ciclo_api(acao: &str, env: &str) {
    let env = parse_env(env);
    let ctx = contexto_atual();
    let cliente = ApiClient::from_env();
    let (method, caminho) = match acao {
        "restart" => ("POST", format!("/projects/{}/restart?env={env}", ctx.name)),
        "ps" => ("GET", format!("/projects/{}/ps?env={env}", ctx.name)),
        _ => unreachable!("ação não mapeada: {acao}"),
    };
    match cliente.call_raw(method, &caminho, None).await {
        Ok(corpo) => {
            if corpo.trim().is_empty() {
                println!("{acao} {}: ok", ctx.name);
            } else {
                println!("{acao} {}: {corpo}", ctx.name);
            }
        }
        Err(e) => falhar(e),
    }
}

/// `flux logs [-f] [--tail N] [env]` (06 + contrato CLI ↔ fluxd). Lease ANTES
/// da primeira chamada (04: `flux logs` segura o ambiente acordado; a guarda
/// renova até o comando acabar e o lease morre sozinho no TTL depois). Sem
/// -f: GET devolve text/plain cru. Com -f: consome o stream SSE do fluxd
/// frame a frame até término limpo (fluxd corta; Ctrl+C mata o processo).
/// `flux db fresh [env] --yes [--seed/--no-seed]` — reconstrução forçada de
/// dev (pedido do Gabriel): POST /projects/{name}/fresh; prod é recusado no
/// fluxd (05) e o env argumento nem chega lá com outro valor (parse_env).
async fn db_fresh(env: &str, seed: Option<bool>) {
    let _ = parse_env(env);
    let ctx = contexto_atual();
    let cliente = ApiClient::from_env();
    let corpo = serde_json::to_string(&api::FreshIn {
        env: env.to_owned(),
        seed,
    })
    .map_err(|e| api::ApiError::Protocolo {
        msg: format!("serializando fresh: {e}"),
    })
    .unwrap_or_default();
    match cliente
        .call_raw(
            "POST",
            &format!("/projects/{}/fresh", ctx.name),
            Some(corpo),
        )
        .await
    {
        Ok(saida) => {
            println!("fresh {}/dev: reconstruído (dados apagados)", ctx.name);
            let cauda: Vec<&str> = saida.lines().rev().take(6).collect();
            for linha in cauda.iter().rev() {
                println!("  {linha}");
            }
        }
        Err(e) => falhar(e),
    }
}

/// `flux exec [env] -- <comando…>` (06 + contrato CLI ↔ fluxd): corpo
/// `{"cmd":[…]}`; o fluxd roda `docker exec` no container app e responde
/// text/plain com a ÚLTIMA linha `exit={code}` — o CLI repassa o código como
/// exit próprio. Lease idem logs (04).
async fn exec(command: Vec<String>, env: &str) {
    if command.is_empty() {
        eprintln!(
            "erro: uso: flux exec [env] -- <comando…> (ex.: flux exec dev -- php artisan migrate --force)"
        );
        std::process::exit(2);
    }
    let env = parse_env(env);
    let ctx = contexto_atual();
    let cliente = ApiClient::from_env();

    match cliente
        .lease(&ctx.name, &env.to_string(), LEASE_UP_SECS)
        .await
    {
        Ok(_) => {}
        Err(e) => falhar(e),
    }
    let _guarda = api::manter_lease(
        cliente.clone(),
        ctx.name.clone(),
        env.to_string(),
        LEASE_UP_SECS,
    );

    match cliente
        .executar(&ctx.name, &env.to_string(), &command)
        .await
    {
        Ok((saida, code)) => {
            print!("{saida}");
            if !saida.ends_with('\n') {
                println!();
            }
            std::process::exit(code);
        }
        Err(e) => falhar(e),
    }
}

// ----------------------------------------------- sync contínua (02, F2)

// ------------------------------------------------- build de assets (02)

// ------------------------------------------------- watch (02, serviço)

/// `flux watch` (02): serviço de longa vida que fecha o circuito CLI ↔ painel
/// — reporta as sessões `flux-*` do mutagen ao fluxd a cada 5 s
/// (POST /sync/report), aplica o rescan pedido pelo painel (GET /sync/status
/// → `sync::reset` → report com rescan_applied=true) e marca "absent" os
/// projetos do fluxd sem sessão local viva. Erro NUNCA derruba o serviço nem
/// passa em silêncio: loga com a origem e o próximo tick tenta de novo.
/// Ctrl-C encerra limpo (nenhuma sessão mutagen é tocada).
async fn watch() {
    let cliente = ApiClient::from_env();
    println!(
        "watch: reportando sessões flux-* ao fluxd ({}) a cada 5 s — Ctrl-C encerra",
        cliente.base_url()
    );
    loop {
        tick_watch(&cliente).await;
        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => {}
            _ = tokio::signal::ctrl_c() => {
                println!("watch: encerrado (Ctrl-C) — sessões mutagen intocadas");
                return;
            }
        }
    }
}

/// Uma rodada do watch (ver [`watch`]). Retorna sempre — erros viram log com
/// origem, o loop é do chamador.
async fn tick_watch(cliente: &ApiClient) {
    // 1) Sessões locais → report individual ("watching").
    let sessoes = match crate::sync::status_all() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("watch: status_all do mutagen falhou: {e}");
            return;
        }
    };
    let vivas: Vec<&crate::sync::SessionStatus> = sessoes
        .iter()
        .filter(|s| s.name.starts_with("flux-"))
        .collect();
    for s in &vivas {
        let report = api::SyncReportIn {
            // Projeto derivado da sessão (`flux-{name}` → {name}; a sessão de
            // worktree `flux-{name}-{slug}` deriva `{name}-{slug}`, que NÃO é
            // projeto registrado — o fluxd RECUSA com 404 e loga a
            // divergência: linha fantasma nunca nasce (o painel casa pelo
            // projeto registrado e o purge apaga pelo nome exato). O erro
            // sai no log do watch a cada tick — nada silencioso. O rescan
            // pedido para o projeto principal ALCANÇA a sessão de worktree:
            // o tick 2 casa sessão por prefixo `flux-{project}-`.
            project: s.name.strip_prefix("flux-").unwrap_or(&s.name).to_owned(),
            session: s.name.clone(),
            status: "watching".to_owned(),
            paused: s.paused,
            files: s.beta.files.or(s.alpha.files),
            problems: s.alpha.scan_problems
                + s.alpha.transition_problems
                + s.beta.scan_problems
                + s.beta.transition_problems
                + s.conflicts,
            last_error: s.last_error.clone(),
            rescan_applied: false,
        };
        if let Err(e) = cliente.sync_report(&report).await {
            eprintln!("watch: report de `{}`: {e}", s.name);
        }
    }
    // 2) Estado do fluxd: rescan pedido → reset + re-report com
    // rescan_applied=true; projeto sem sessão local → "absent".
    let st = match cliente.sync_status().await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("watch: GET /sync/status: {e}");
            return;
        }
    };
    for p in st.projects {
        let principal = format!("flux-{}", p.project);
        let prefixo = format!("flux-{}-", p.project);
        let locais: Vec<&&crate::sync::SessionStatus> = vivas
            .iter()
            .filter(|s| s.name == principal || s.name.starts_with(&prefixo))
            .collect();
        if locais.is_empty() {
            let report = api::SyncReportIn {
                project: p.project.clone(),
                session: principal,
                status: "absent".to_owned(),
                paused: false,
                files: None,
                problems: 0,
                last_error: None,
                // NÃO limpa a flag: absent não aplicou rescan nenhum.
                rescan_applied: false,
            };
            if let Err(e) = cliente.sync_report(&report).await {
                eprintln!("watch: report absent de `{}`: {e}", p.project);
            }
            if p.rescan_requested {
                eprintln!(
                    "watch: rescan pedido para `{}` sem sessão local viva — a flag segue no fluxd até a sessão voltar",
                    p.project
                );
            }
            continue;
        }
        if p.rescan_requested
            && let Err(e) = rescan_sessao(cliente, locais[0]).await
        {
            eprintln!("watch: rescan de `{}`: {e}", p.project);
        }
    }
}

/// Rescan pedido pelo painel (02): reconstrói a spec a partir da sessão viva
/// (o mutagen grava alpha e beta na sessão), faz terminate + create (full
/// rescan) e re-reporta com `rescan_applied=true` — é o que limpa a flag no
/// fluxd. Erro em qualquer ponto sobe com a causa (o tick loga).
async fn rescan_sessao(cliente: &ApiClient, s: &crate::sync::SessionStatus) -> Result<(), String> {
    let spec = spec_da_sessao(s)?;
    crate::sync::reset(&spec).map_err(|e| e.to_string())?;
    println!(
        "watch: rescan aplicado em `{}` (terminate + create)",
        spec.session()
    );
    // Estado fresco pós-recriação — vai estar trabalhando; é o que o painel
    // vê até o rescan terminar sozinho.
    let fresco = crate::sync::status_of(&spec.session())
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("sessão `{}` sumiu logo após o reset", spec.session()))?;
    let report = api::SyncReportIn {
        project: spec.project.clone(),
        session: fresco.name.clone(),
        status: "watching".to_owned(),
        paused: fresco.paused,
        files: fresco.beta.files.or(fresco.alpha.files),
        problems: fresco.alpha.scan_problems
            + fresco.alpha.transition_problems
            + fresco.beta.scan_problems
            + fresco.beta.transition_problems
            + fresco.conflicts,
        last_error: fresco.last_error.clone(),
        rescan_applied: true,
    };
    cliente
        .sync_report(&report)
        .await
        .map_err(|e| e.to_string())
}

/// Reconstrói a [`crate::sync::SyncSpec`] de uma sessão viva a partir do que
/// o mutagen gravou: alpha = path local do endpoint; beta = `user@host:path`
/// do endpoint remoto; projeto lido do flux.toml da pasta alpha (senão nome
/// da pasta). O nome da sessão recriada TEM de bater com o original — nome
/// divergente viraria "outra" sessão para o painel, então reconstrução que
/// não fecha é ERRO, nunca chute.
fn spec_da_sessao(s: &crate::sync::SessionStatus) -> Result<crate::sync::SyncSpec, String> {
    if s.alpha.path.trim().is_empty() {
        return Err(format!(
            "sessão `{}` sem path de alpha — impossível reconstruir a spec",
            s.name
        ));
    }
    let alpha = std::path::PathBuf::from(s.alpha.path.trim());
    let beta = if s.beta.remote {
        let (user, host) = s.beta.host.split_once('@').ok_or_else(|| {
            format!(
                "beta remoto da sessão `{}` sem `user@host`: `{}`",
                s.name, s.beta.host
            )
        })?;
        if s.beta.path.trim().is_empty() {
            return Err(format!(
                "beta remoto da sessão `{}` sem path — impossível reconstruir",
                s.name
            ));
        }
        crate::sync::Beta::Ssh {
            user: user.to_owned(),
            host: host.to_owned(),
            path: s.beta.path.trim().to_owned(),
        }
    } else {
        crate::sync::Beta::Local(std::path::PathBuf::from(s.beta.path.trim()))
    };
    let base = crate::sync::SyncSpec::for_project(&alpha, beta, None).map_err(|e| e.to_string())?;
    if s.name == base.session() {
        return Ok(base);
    }
    // Worktree: o slug vive no sufixo do nome da sessão (spec 02) — a pasta
    // alpha da worktree tem o MESMO project no flux.toml, então o sufixo é
    // recuperável com determinismo.
    let prefixo = format!("flux-{}-", base.project);
    if let Some(slug) = s.name.strip_prefix(&prefixo) {
        let mut spec = base.clone();
        spec.branch_slug = Some(slug.to_owned());
        if spec.session() == s.name {
            return Ok(spec);
        }
    }
    Err(format!(
        "sessão `{}` não reconstrói a partir de {} (o alpha da sessão aponta outra pasta?)",
        s.name,
        alpha.display()
    ))
}

/// Stub explícito: recusa com fase e exit ≠ 0 — nunca finge que fez (06).
fn stub(fase: &str) -> ! {
    eprintln!("erro: comando ainda não implementado — chega na {fase}");
    std::process::exit(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coletor_de_migrations_ordena_e_hashea_so_php() {
        let base = std::env::temp_dir().join(format!("flux-mig-test-{}", std::process::id()));
        let dir = base.join("database/migrations");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("2026_01_02_cria_x.php"), b"b").unwrap();
        std::fs::write(dir.join("2026_01_01_cria_a.php"), b"a").unwrap();
        std::fs::write(dir.join("notas.txt"), b"ignora").unwrap();

        let lista = coletar_migrations_em(&base.join("database/migrations"));
        assert_eq!(lista.len(), 2, "só .php");
        assert_eq!(lista[0].0, "2026_01_01_cria_a.php", "ordem cronológica");
        let esperado = {
            let mut h = Sha256::new();
            h.update(b"a");
            format!("{:x}", h.finalize())
        };
        assert_eq!(lista[0].1, esperado);
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn coletor_com_dir_ausente_devolve_vazio() {
        assert!(coletar_migrations_em(Path::new("definitivamente/inexistente")).is_empty());
    }

    #[test]
    fn coletor_filtra_migrations_fora_da_sync() {
        let lista = vec![
            ("2026_01_01_base.php".to_owned(), "h1".to_owned()),
            ("2026_01_02_spatial.php".to_owned(), "h2".to_owned()),
            ("2026_01_03_normal.php".to_owned(), "h3".to_owned()),
        ];
        // O ignore do [sync] aponta pelo caminho relativo à raiz — o mesmo
        // formato do flux.toml real; os GLOBAL_IGNORES não casam com nada
        // aqui e não podem filtrar migration nenhuma.
        let mut ignores: Vec<String> = crate::sync::GLOBAL_IGNORES
            .iter()
            .map(|s| s.to_string())
            .collect();
        ignores.push("database/migrations/2026_01_02_spatial.php".to_owned());

        let got = filtra_migrations_ignoradas(lista, &ignores);
        let nomes: Vec<&str> = got.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(nomes, ["2026_01_01_base.php", "2026_01_03_normal.php"]);
    }

    #[test]
    fn seed_da_flag_tem_precedencia_explicita() {
        assert_eq!(seed_escolhida(true, false), Some(true), "--seed força true");
        assert_eq!(
            seed_escolhida(false, true),
            Some(false),
            "--no-seed força false"
        );
        assert_eq!(
            seed_escolhida(false, false),
            None,
            "sem flag: o fluxd decide pelo seed_on_rebuild do flux.toml"
        );
        // Os dois juntos é bloqueado pelo conflicts_with do clap; aqui a
        // ordem de leitura é só documentada (seed vence).
        assert_eq!(seed_escolhida(true, true), Some(true));
    }
}
