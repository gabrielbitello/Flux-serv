//! Forward SSH do CLI para a API de controle (09): a `:7420` NUNCA sai do
//! localhost da VM — o CLI sobe `ssh -L 127.0.0.1:7420:127.0.0.1:7420` sob
//! demanda (idempotente: porta aberta = nada a fazer) e lê o config global
//! `%USERPROFILE%\.flux\config.toml` ([api] token/base_url) quando env não
//! define. A chave SSH vem da entrada `192.168.140.2` no `~/.ssh/config`
//! (`IdentitiesOnly` — a chave do Flux nunca vai para outra máquina, 09).
//!
//! Falha de forward é AVISO no stderr e o erro real (com causa) vem depois
//! do `call()` — nunca silencioso, nunca panic.

use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::PathBuf;
use std::time::Duration;

/// Porta da API de controle (07).
pub const API_PORT: u16 = 7420;
/// Host SSH da VM (01/09, rede host-only).
// L2.5: fonte única em sync.rs (a ponte #[path] exige sync.rs
// autossuficiente); aqui é só reexport para os consumidores do túnel.
pub use crate::sync::{VM_HOST, VM_USER};
/// Janela de espera pela porta depois de lançar o ssh (16 × 500 ms = 8 s).
const TENTATIVAS: usize = 16;
const PASSO: Duration = Duration::from_millis(500);

/// Recorte `[api]` do config global (09). Ausente/ilegível = tudo `None`
/// (env vence; arquivo é o default da instalação).
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ConfigApi {
    pub token: Option<String>,
    pub base_url: Option<String>,
}

/// Lê `%USERPROFILE%\.flux\config.toml`. Arquivo ausente NÃO é erro — é o
/// estado de quem nunca rodou `flux setup`.
pub fn config_do_usuario() -> ConfigApi {
    match std::env::var_os("USERPROFILE") {
        Some(home) => config_de(&PathBuf::from(home).join(".flux").join("config.toml")),
        None => ConfigApi::default(),
    }
}

fn config_de(caminho: &std::path::Path) -> ConfigApi {
    let Ok(texto) = std::fs::read_to_string(caminho) else {
        return ConfigApi::default();
    };
    let Ok(valor) = texto.parse::<toml::Value>() else {
        return ConfigApi::default();
    };
    let api = valor.get("api");
    ConfigApi {
        token: api
            .and_then(|a| a.get("token"))
            .and_then(|t| t.as_str())
            .map(str::to_owned),
        base_url: api
            .and_then(|a| a.get("base_url"))
            .and_then(|t| t.as_str())
            .map(str::to_owned),
    }
}

/// Grava/atualiza o token da API no config do usuário (09: valor claro em
/// `%USERPROFILE%\.flux\config.toml`, hash no SQLite da VM). Preserva TODAS
/// as outras seções/chaves do arquivo — só `api.token` muda. Arquivo inexistente
/// é criado (com o diretório).
pub fn salvar_token_no_config(novo: &str) -> Result<PathBuf, String> {
    let home = std::env::var_os("USERPROFILE")
        .ok_or("env USERPROFILE ausente — onde gravar o config.toml?")?;
    let caminho = PathBuf::from(home).join(".flux").join("config.toml");
    salvar_token_em(&caminho, novo)?;
    Ok(caminho)
}

fn salvar_token_em(caminho: &std::path::Path, novo: &str) -> Result<(), String> {
    let mut raiz: toml::Value = std::fs::read_to_string(caminho)
        .ok()
        .and_then(|t| t.parse().ok())
        .unwrap_or_else(|| toml::Value::Table(Default::default()));
    let tabela = raiz.as_table_mut().ok_or_else(|| {
        format!(
            "{} não é um TOML de nível superior válido",
            caminho.display()
        )
    })?;
    let api = tabela
        .entry("api")
        .or_insert_with(|| toml::Value::Table(Default::default()));
    let api_t = api
        .as_table_mut()
        .ok_or_else(|| format!("[api] em {} não é uma tabela", caminho.display()))?;
    api_t.insert("token".to_owned(), toml::Value::String(novo.to_owned()));
    let serializado =
        toml::to_string_pretty(&raiz).map_err(|e| format!("serializar config: {e}"))?;
    if let Some(pai) = caminho.parent() {
        std::fs::create_dir_all(pai).map_err(|e| format!("criar {}: {e}", pai.display()))?;
    }
    std::fs::write(caminho, serializado).map_err(|e| format!("escrever {}: {e}", caminho.display()))
}

fn porta_aberta(port: u16) -> bool {
    let alvo: SocketAddr = (Ipv4Addr::LOCALHOST, port).into();
    TcpStream::connect_timeout(&alvo, Duration::from_millis(400)).is_ok()
}

/// Sobe o túnel se a porta local não responde. `is_default_url` falso =
/// usuário apontou `FLUX_API_URL`/config para outro lugar — não mexo.
pub fn garantir_forward(is_default_url: bool) -> Result<(), String> {
    if !is_default_url || porta_aberta(API_PORT) {
        return Ok(());
    }
    // SEM `-f`: o OpenSSH do WINDOWS (System32) não faz fork como o do
    // Unix — o CLI mesmo lança o processo DESPEGADO (CREATE_NO_WINDOW) e
    // ele sobrevive à saída do comando. Túnel vivo = reuso; morreu = sobe
    // de novo na próxima invocação (idempotente pelo probe da porta).
    let mut cmd = std::process::Command::new("ssh");
    cmd.args([
        "-N".to_owned(),
        "-o".to_owned(),
        "ExitOnForwardFailure=yes".to_owned(),
        "-o".to_owned(),
        "ConnectTimeout=8".to_owned(),
        "-o".to_owned(),
        "ServerAliveInterval=30".to_owned(),
        "-o".to_owned(),
        "ServerAliveCountMax=3".to_owned(),
        "-L".to_owned(),
        format!("127.0.0.1:{API_PORT}:127.0.0.1:{API_PORT}"),
        format!("{VM_USER}@{VM_HOST}"),
    ]);
    // stdin/stdout/stderr nulos: o filho fica solto sem herdar console.
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW — ssh de fundo sem piscar console no PowerShell.
        cmd.creation_flags(0x0800_0000);
    }
    // spawn(), NUNCA output(): esperaria o ssh para sempre (-N não termina).
    // Se auth falhar, o filho morre sozinho e o poll abaixo reporta honesto.
    cmd.spawn().map_err(|e| {
        format!("não consegui lançar o ssh do túnel (CLI precisa do OpenSSH no PATH): {e}")
    })?;
    for _ in 0..TENTATIVAS {
        if porta_aberta(API_PORT) {
            return Ok(());
        }
        std::thread::sleep(PASSO);
    }
    Err(format!(
        "túnel SSH não abriu a :{API_PORT} local em {} s — rode `ssh {VM_USER}@{VM_HOST} echo ok` \
         na mão para ver o erro real (chave/config em ~/.ssh/config)",
        (TENTATIVAS as u64 * PASSO.as_millis() as u64) / 1000
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_ausente_e_default_nao_erro() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(
            config_de(&tmp.path().join("config.toml")),
            ConfigApi::default(),
            "arquivo ausente = estado válido de setup pendente"
        );
    }

    #[test]
    fn config_le_api_token_e_base_url() {
        let tmp = tempfile::tempdir().unwrap();
        let caminho = tmp.path().join("config.toml");
        std::fs::write(
            &caminho,
            "[api]\ntoken = \"tok-123\"\nbase_url = \"http://127.0.0.1:9999\"\n\n[outro]\nx = 1\n",
        )
        .unwrap();
        let cfg = config_de(&caminho);
        assert_eq!(cfg.token.as_deref(), Some("tok-123"));
        assert_eq!(cfg.base_url.as_deref(), Some("http://127.0.0.1:9999"));
    }

    #[test]
    fn config_lixo_vira_default_sem_panic() {
        let tmp = tempfile::tempdir().unwrap();
        let caminho = tmp.path().join("config.toml");
        std::fs::write(&caminho, "isto não é toml [[[").unwrap();
        assert_eq!(config_de(&caminho), ConfigApi::default());
    }

    #[test]
    fn salvar_token_preserva_o_resto_do_config() {
        let tmp = tempfile::tempdir().unwrap();
        let caminho = tmp.path().join("config.toml");
        std::fs::write(
            &caminho,
            "[api]\ntoken = \"velho\"\nbase_url = \"http://127.0.0.1:9999\"\n\n[vm]\ncpus = 8\n",
        )
        .unwrap();
        salvar_token_em(&caminho, "novo-xyz").unwrap();
        let cfg = config_de(&caminho);
        assert_eq!(cfg.token.as_deref(), Some("novo-xyz"));
        assert_eq!(cfg.base_url.as_deref(), Some("http://127.0.0.1:9999"));
        let texto = std::fs::read_to_string(&caminho).unwrap();
        assert!(texto.contains("[vm]"), "seção de fora intacta: {texto}");
        assert!(texto.contains("cpus = 8"), "chave de fora intacta: {texto}");
        assert!(!texto.contains("velho"));
    }

    #[test]
    fn salvar_token_cria_config_inexistente() {
        let tmp = tempfile::tempdir().unwrap();
        let caminho = tmp.path().join("sub").join("config.toml");
        salvar_token_em(&caminho, "primeiro").unwrap();
        assert_eq!(config_de(&caminho).token.as_deref(), Some("primeiro"));
    }

    #[test]
    fn porta_fechada_nao_abre_sozinha() {
        // Porta efêmera provavelmente livre: probe deve falhar (sem ssh real
        // nos testes — garantir_forward com is_default=false nem lança ssh).
        let tmp = tempfile::tempdir().unwrap();
        let livre = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        drop(tmp);
        assert!(!porta_aberta(livre), "porta livre não responde");
    }

    #[test]
    fn forward_com_url_custom_nao_toca_em_nada() {
        // Contrato do comando: FLUX_API_URL custom = zero efeito colateral.
        assert_eq!(garantir_forward(false), Ok(()));
    }
}
