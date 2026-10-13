//! `fluxd` — daemon da plataforma Flux na VM (07). Dois modos: `serve`
//! (systemd) e `ctl` (chamado por SSH pelo CLI). O binário é fino; tudo em
//! `fluxd::`.

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "fluxd", version, about = "Daemon da plataforma Flux (VM)")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Serviço principal: router :8080 + API de controle :7420 (systemd).
    Serve {
        /// Banco de estado SQLite (/srv/flux/state/flux.sqlite3 em prod).
        #[arg(long, env = "FLUXD_DB", default_value = "/srv/flux/state/flux.sqlite3")]
        db: std::path::PathBuf,
        /// Config global (mesmo schema da spec 09). Ausente = defaults + warn.
        #[arg(long, env = "FLUXD_CONFIG")]
        config: Option<std::path::PathBuf>,
        /// Porta do router — Caddy catch-all aponta para cá (03).
        #[arg(long, default_value_t = 8080)]
        port: u16,
        /// Porta da API de controle — bind 127.0.0.1, nunca na LAN (07).
        #[arg(long, default_value_t = 7420)]
        api_port: u16,
    },
    /// Subcomandos de baixo nível chamados por SSH (07).
    Ctl {
        #[command(subcommand)]
        sub: CtlCmd,
    },
}

#[derive(Subcommand)]
enum CtlCmd {
    /// Registra/atualiza um projeto a partir do flux.toml.
    Register,
    /// Re-renderiza os overrides de compose do projeto.
    Render,
    /// Segue logs do container (canal do `flux logs -f`).
    TailLogs,
    /// Despeja o estado interno para diagnóstico.
    DumpState,
}

fn main() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let cli = Cli::parse();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("runtime tokio");

    match cli.cmd {
        Cmd::Serve {
            db,
            config,
            port,
            api_port,
        } => {
            let global = config.map(|p| match flux_common::config::GlobalConfig::load(&p) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("erro: {e}");
                    std::process::exit(1);
                }
            });
            if let Err(e) = rt.block_on(fluxd::serve(db, global, port, api_port)) {
                eprintln!("erro: fluxd parou: {e}");
                std::process::exit(1);
            }
        }
        Cmd::Ctl { .. } => {
            eprintln!(
                "erro: fluxd ctl ainda não implementado — chega nas fases F5–F6 (docs/specs/)"
            );
            std::process::exit(2);
        }
    }
}
