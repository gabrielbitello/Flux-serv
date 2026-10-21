//! `flux` — CLI da plataforma no Windows (06). Árvore de comandos completa
//! conforme a spec: a superfície de comandos é design de API, então nasce
//! inteira com stubs que recusam com fase e exit ≠ 0 ("nada silencioso").
//! Fase F6: os comandos do ciclo de dev falam com a API de controle do fluxd
//! (:7420, 07) via `api.rs`; sem fluxd no ar, a mensagem traz a causa e o
//! exit é ≠ 0. Contexto do projeto = diretório atual (`flux.toml` explícito
//! ou nome da pasta). EXCEÇÃO declarada da regra "o CLI nunca toca em Docker
//! direto" (06): 3 rotas ssh→docker de longa data — `build` (flock + exec),
//! `live OFF` (kill do dev server) e `container_do_servico` (`docker ps` por
//! label); unificá-las exigiria endpoint novo no fluxd, fora de escopo.

mod api;
// Sync contínua (spec 02): monta argumentos e executa o mutagen — consumida
// por `flux sync`, `flux build`, `flux watch` e pelo `flux up` (fase 02
// ligada; o allow de dead_code do módulo saiu junto com o motivo).
mod comandos;
mod contexto;
mod sync;
// SSH batch p/ a VM (L7.3): fonte única dos flags e do alvo — os 9 sites
// chamam ssh_vm(); o vm_ssh interativo fica fora de propósito.
mod ssh_util;
// Túnel SSH p/ a API de controle + config global do usuário (spec 09).
mod tunel;
// Wizard `flux up -m` que gera o flux.toml (spec 06).
mod wizard;

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use contexto::contexto_atual;

use comandos::build::build_assets;
use comandos::ciclo::{ciclo_api, db_fresh, down, exec, ps, status};
use comandos::env_cmd::{env_edit, env_pull, env_push};
use comandos::live::live_toggle;
use comandos::logs::logs;
use comandos::sync_cmd::{sync_pause_cmd, sync_reset_cmd, sync_resume_cmd, sync_status_cmd};
use comandos::tokens::{project_token, token_generate, vm_ssh};
use comandos::up::{seed_escolhida, up};
use comandos::watch::watch;

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
    /// Modo de contenção GLOBAL (21/10): ligado, o router do fluxd devolve a
    /// página estática de contenção para TODO request — Cloudflare e rede
    /// local, host gerenciado ou não; nenhum container é acordado ou
    /// proxyado. A API :7420 e o painel :7421 (túnel SSH) seguem de pé — é
    /// por ali que o modo se desliga. Comando GLOBAL: não usa contexto de
    /// projeto.
    Redroom {
        #[arg(value_parser = ["on", "off", "status"])]
        acao: String,
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
        Cmd::Redroom { acao } => comandos::redroom::redroom(&acao).await,
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

// ----------------------------------------------- sync contínua (02, F2)

// ------------------------------------------------- build de assets (02)

// ------------------------------------------------- watch (02, serviço)

/// Stub explícito: recusa com fase e exit ≠ 0 — nunca finge que fez (06).
fn stub(fase: &str) -> ! {
    eprintln!("erro: comando ainda não implementado — chega na {fase}");
    std::process::exit(2)
}

#[cfg(test)]
mod tests {}
