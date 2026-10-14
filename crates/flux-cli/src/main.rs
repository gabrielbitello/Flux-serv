//! `flux` — CLI da plataforma no Windows (06). Árvore de comandos completa
//! conforme a spec: a superfície de comandos é design de API, então nasce
//! inteira com stubs que recusam com fase e exit ≠ 0 ("nada silencioso").
//! Fase F6: os comandos do ciclo de dev falam com a API de controle do fluxd
//! (:7420, 07) via `api.rs` — o CLI NUNCA toca em Docker direto (regra da 06);
//! sem fluxd no ar, a mensagem traz a causa e o exit é ≠ 0. Contexto do
//! projeto = diretório atual (`flux.toml` explícito ou nome da pasta).

mod api;
// Sync contínua (spec 02): monta argumentos e executa o mutagen — o ciclo
// `flux up`/`flux sync` consome daqui quando a fase CLI da F2 ligar; o
// módulo carrega `#![allow(dead_code)]` justificado até esse consumo existir.
mod sync;
// Túnel SSH p/ a API de controle + config global do usuário (spec 09).
mod tunel;

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use flux_common::config::ProjectToml;
use flux_common::protocol::Env;
use sha2::{Digest, Sha256};

use crate::api::{ApiClient, ApiError};

/// Lease do `flux up` (04): cobre uma sessão de trabalho padrão; renovado a
/// cada ttl/2 enquanto o comando vive e morrendo sozinho depois (o sono é o
/// padrão da plataforma, não exceção).
const LEASE_UP_SECS: i64 = 15 * 60;

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
    /// Ciclo completo do dev no repo atual (06).
    Up {
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
    /// Logs do container (lease de sono ativo; janela histórica 13).
    Logs {
        #[arg(short, long)]
        follow: bool,
        #[arg(long)]
        since: Option<String>,
        #[arg(long)]
        until: Option<String>,
        service: Option<String>,
        #[arg(default_value = "dev")]
        env: String,
    },
    /// `docker exec` no container principal.
    Exec {
        #[arg(last = true)]
        command: Vec<String>,
        #[arg(default_value = "dev")]
        env: String,
    },
    /// `flux exec bash`.
    Shell {
        #[arg(default_value = "dev")]
        env: String,
    },
    /// Build das imagens, sem subir.
    Build {
        #[arg(default_value = "dev")]
        env: String,
    },
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
        Cmd::Up { env } => up(&env).await,
        Cmd::Down { env } => ciclo_api("down", &env).await,
        Cmd::Restart { env } => ciclo_api("restart", &env).await,
        Cmd::Status { env } => status(&env).await,
        Cmd::Ps { env } => ciclo_api("ps", &env).await,
        Cmd::Logs {
            follow,
            since,
            until,
            service,
            env,
        } => logs(follow, since, until, service, &env).await,
        Cmd::Exec { command, env } => exec(command, &env).await,
        Cmd::Shell { env } => exec(vec!["bash".to_owned()], &env).await,
        Cmd::Build { env } => ciclo_api("build", &env).await,
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
        Cmd::Watch => stub("fase F2/F11 (specs 02/03) — reporte de branches ao fluxd"),
        Cmd::Init { target: _ } => stub("fase F9 (spec 10) — workflow de deploy por push"),
        Cmd::Prod { .. } => stub("fase F9 (spec 10) — prod é git push + rollback"),
        Cmd::Maintenance { .. } => stub("fase F18 (spec 15) — modo manutenção com bypass"),
        Cmd::Shield { .. } => stub("fase F19 (spec 16) — WAF próprio, shadow antes de enforce"),
        Cmd::Project { .. } => stub("fase F9 (specs 09/10) — token de deploy por projeto"),
        Cmd::Db { cmd } => match cmd {
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
        Cmd::Sync { .. } => stub("fase F2 (spec 02) — sync contínua via mutagen"),
        Cmd::Env { .. } => stub("fase F2/F12 (specs 02/09) — .env exclusivo da VM"),
        Cmd::Vm { cmd } => match cmd {
            VmCmd::Upgrade => stub("fase F16 (spec 14) — upgrade do fluxd com rollback"),
            _ => stub("fase F1 (spec 01) — VM Hyper-V Bookworm"),
        },
        Cmd::Setup { .. } => stub("fase F1 (spec 01) — setup do host Windows + VM"),
        Cmd::Token {
            sub: TokenCmd::Generate,
        } => token_generate(),
        Cmd::Tunnel { .. } => stub("fase F7 (spec 03) — tunnel Cloudflare e domínios"),
        Cmd::Panel => stub("fase F8 (spec 08) — painel web com auth"),
        Cmd::Export => stub("fase F16 (spec 14) — DR export"),
        Cmd::Import { .. } => stub("fase F16 (spec 14) — DR import"),
        Cmd::SelfUpdate => stub("fase F16 (spec 14) — self-update com checksum"),
    }
}

// ------------------------------------------------------------- contexto 06

/// Projeto do diretório atual (06: "diretório = contexto"). `flux.toml`
/// explícito vence; sem ele, detecção mínima pelo nome da pasta com AVISO —
/// nunca dedução silenciosa.
struct Contexto {
    name: String,
    toml: String,
    toml_hash: String,
}

fn contexto_atual() -> Contexto {
    let cwd = match std::env::current_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("erro: não consegui ler o diretório atual: {e}");
            std::process::exit(1);
        }
    };
    let pasta = cwd
        .file_name()
        .and_then(|n| n.to_str())
        .map(str::to_owned)
        .unwrap_or_else(|| {
            eprintln!(
                "erro: diretório atual sem nome de pasta utilizável: {}",
                cwd.display()
            );
            std::process::exit(1);
        });
    let caminho = cwd.join("flux.toml");
    let (toml, name) = if caminho.exists() {
        let src = match std::fs::read_to_string(&caminho) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("erro: lendo {}: {e}", caminho.display());
                std::process::exit(1);
            }
        };
        let parsed = match ProjectToml::parse_str(&src) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("erro: {} inválido: {e}", caminho.display());
                std::process::exit(2);
            }
        };
        (src, parsed.effective_name(&pasta).to_owned())
    } else {
        eprintln!(
            "aviso: sem flux.toml em {} — projeto herdando o nome da pasta ({pasta}) e defaults da plataforma",
            cwd.display()
        );
        (String::new(), pasta)
    };
    Contexto {
        toml_hash: sha256_hex(toml.as_bytes()),
        name,
        toml,
    }
}

fn sha256_hex(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    let mut s = String::with_capacity(64);
    for b in h.finalize() {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn parse_env(env: &str) -> Env {
    match env.parse() {
        Ok(e) => e,
        Err(m) => {
            eprintln!("erro: {m}");
            std::process::exit(2);
        }
    }
}

/// Saída de erro padronizada: causa do `ApiError` no stderr + exit code do
/// erro (06: erro nunca sai com exit 0; 09: causa sempre visível).
fn falhar(e: ApiError) -> ! {
    eprintln!("erro: {e}");
    std::process::exit(e.exit_code())
}

// ------------------------------------------------------- token da API (09)

/// 64 chars hex de 32 bytes do CSPRNG do SO — token da API de controle.
fn token_novo() -> String {
    let mut buf = [0u8; 32];
    getrandom::fill(&mut buf).expect("CSPRNG do SO indisponível — não há token seguro sem isso");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// `flux token generate` (09, estilo key:generate): token novo → sha256
/// gravado no SQLite do fluxd via SSH (`settings.api_token_hash`) + valor
/// claro no config do usuário. O config SÓ é escrito DEPOIS do hash na VM
/// dar certo — falha no meio nunca deixa CLI com token que o fluxd não
/// conhece. Token antigo morre na hora (revogar = gerar outro).
fn token_generate() {
    let token = token_novo();
    let hash = sha256_hex(token.as_bytes());
    let ssh_alvo = format!("{}@{}", tunel::VM_USER, tunel::VM_HOST);
    let remoto = format!(
        "sudo sqlite3 /srv/flux/state/flux.sqlite3 \"INSERT OR REPLACE INTO \
         settings(k, v) VALUES ('api_token_hash', '{hash}');\""
    );
    let saida = match std::process::Command::new("ssh")
        .args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=8", &ssh_alvo, &remoto])
        .output()
    {
        Ok(s) => s,
        Err(e) => {
            eprintln!("erro: ssh não lançado (OpenSSH no PATH?): {e}");
            std::process::exit(1);
        }
    };
    if !saida.status.success() {
        eprintln!(
            "erro: gravar api_token_hash na VM falhou (ssh {}): {}",
            ssh_alvo,
            String::from_utf8_lossy(&saida.stderr).trim()
        );
        std::process::exit(1);
    }
    let caminho = match tunel::salvar_token_no_config(&token) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("erro: {e}");
            std::process::exit(1);
        }
    };
    println!("hash gravado na VM ({ssh_alvo}) · config atualizado: {}", caminho.display());
    println!();
    println!("token (MOSTRA UMA VEZ — guarde no gerenciador de senhas):");
    println!("  {token}");
    println!();
    println!("próximo comando `flux` já usa o token novo (config.toml [api] token).");
}

// ---------------------------------------------------------- comandos F6

/// `flux up [env]` (06, fluxo passo a passo). Fase F6: passos 1–4 reais
/// (contexto, saúde do fluxd, registro, lease renovável) + status consolidado;
/// build/sync/banco (passos 5–7) são orquestrados pelo fluxd e entram nas
/// fases F7+ — o que a API expõe hoje aparece com rótulo honesto.
async fn up(env: &str) {
    let env = parse_env(env);
    let ctx = contexto_atual();
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
        "[3/5] lease de {}/{} por {} s — renova sozinho até o comando terminar",
        ctx.name, env, LEASE_UP_SECS
    );

    let st = match cliente.status(&ctx.name).await {
        Ok(s) => s,
        Err(e) => falhar(e),
    };
    imprime_status(&st);
    println!(
        "pronto: https://{} · {} · lease até epoch {}",
        st.dev_host,
        st.sleep.dev.mode.as_deref().unwrap_or("sem sinal"),
        lease.lease_until
    );
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

/// down/restart/ps/build — o CLI já fala o protocolo da 07: chama o endpoint
/// do projeto, mostra a resposta e sai com exit ≠ 0 enquanto o backend do
/// fluxd ainda é 501. Quando a fase do backend chegar, o comando funciona sem
/// mudança aqui.
async fn ciclo_api(acao: &str, env: &str) {
    let env = parse_env(env);
    let ctx = contexto_atual();
    let cliente = ApiClient::from_env();
    let (method, caminho) = match acao {
        "down" => ("POST", format!("/projects/{}/down?env={env}", ctx.name)),
        "restart" => ("POST", format!("/projects/{}/restart?env={env}", ctx.name)),
        "ps" => ("GET", format!("/projects/{}/ps?env={env}", ctx.name)),
        "build" => ("POST", format!("/projects/{}/build?env={env}", ctx.name)),
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

async fn logs(
    follow: bool,
    since: Option<String>,
    until: Option<String>,
    service: Option<String>,
    env: &str,
) {
    let env = parse_env(env);
    let ctx = contexto_atual();
    let cliente = ApiClient::from_env();
    let mut caminho = format!("/projects/{}/logs?env={env}&follow={follow}", ctx.name);
    if let Some(s) = since {
        caminho.push_str(&format!("&since={s}"));
    }
    if let Some(u) = until {
        caminho.push_str(&format!("&until={u}"));
    }
    if let Some(svc) = service {
        caminho.push_str(&format!("&service={svc}"));
    }
    // Quando o backend de stream (SSE, 07) chegar, o `-f` vira consumo de
    // stream contínuo — o endpoint e o contrato já são estes.
    match cliente.call_raw("GET", &caminho, None).await {
        Ok(corpo) => println!("{corpo}"),
        Err(e) => falhar(e),
    }
}

async fn exec(command: Vec<String>, env: &str) {
    if command.is_empty() {
        eprintln!(
            "erro: uso: flux exec -- <comando…> [env] (ex.: flux exec dev -- php artisan route:list)"
        );
        std::process::exit(2);
    }
    let env = parse_env(env);
    let ctx = contexto_atual();
    let cliente = ApiClient::from_env();
    let corpo = match serde_json::to_string(&serde_json::json!({ "command": command })) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("erro: serializando comando: {e}");
            std::process::exit(1);
        }
    };
    match cliente
        .call_raw(
            "POST",
            &format!("/projects/{}/exec?env={env}", ctx.name),
            Some(corpo),
        )
        .await
    {
        Ok(saida) => println!("{saida}"),
        Err(e) => falhar(e),
    }
}

/// Stub explícito: recusa com fase e exit ≠ 0 — nunca finge que fez (06).
fn stub(fase: &str) -> ! {
    eprintln!("erro: comando ainda não implementado — chega na {fase}");
    std::process::exit(2)
}
