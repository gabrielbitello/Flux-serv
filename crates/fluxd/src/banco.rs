//! Banco próprio do projeto (own-db, 21/10 — pedido do vippers): o compose
//! do projeto declara o service de banco (mysql/mariadb) e o fluxd
//! materializa UM container dedicado por grupo — `flux-db-{grupo}` — na
//! `flux-net`, com volume nomeado próprio. Nada de porta publicada (mesma
//! regra do override, a68e4ba) e nada de parser de YAML: o `environment` e o
//! `image` vêm do RENDER do engine (`compose::CapturaBanco`).
//!
//! Ciclo "dorme junto, acorda junto":
//! - `up` de qualquer instância do grupo → `garantir` (container saudável +
//!   database da instância) ANTES do `compose up` do app;
//! - sono/`down` de uma instância → `dormir_se_ninguem`: se nenhuma outra
//!   instância do grupo tem container rodando, o banco para junto;
//! - wake (request/router e `ensure_awake`) → `acordar` ANTES dos containers
//!   do app — start de container com datadir quente é rápido (medido no
//!   cutover); o primeiro start (cold init do MySQL) só acontece no 1º `up`.
//!
//! Databases por instância: `{projeto}_{env}` (fonte única
//! `database_name`). TTL de dev: database sem interação há mais de
//! `[db] ttl_days` (default 7) é APAGADO com dump de backup em
//! `/srv/flux/state/db-backups/` — prod NUNCA expira. A senha de root NUNCA
//! aparece em argv: o `docker exec` herda o env do próprio container e o SQL
//! entra por stdin.

use std::path::{Path, PathBuf};

use bollard::Docker;
use flux_common::protocol::Env;
use rusqlite::OptionalExtension;

use crate::events;
use crate::state::State;

/// Teto de espera pela saúde do container de banco (cold init do MySQL na
/// 1ª subida leva dezenas de segundos + eventual pull de imagem; wake quente
/// é questão de segundos — o teto cobre o pior caso, não o caminho feliz).
const ESPERA_SAUDE_SECS: u64 = 300;
/// Teto do dump antes do drop (64 MiB — dev real do vippers tem ~3 MB).
const TETO_DUMP_BYTES: usize = 64 * 1024 * 1024;

/// Imagem do banco próprio quando o flux.toml não declara `[db] image` e o
/// compose não tem service de banco (design 21/10 v2, pedido do Gabriel:
/// "não é para ter mysql no compose — ele baixa sozinho"). MySQL 8.4 LTS.
pub const IMAGEM_PADRAO: &str = "mysql:8.4";

/// Configuração efetiva do banco próprio de um projeto, já resolvida do
/// `flux.toml` gravado no state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BancoConfig {
    pub grupo: String,
    pub ttl_days: u64,
    pub ttl_secs: i64,
}

/// Lê o banco próprio do TOML gravado. `None` = projeto não usa (`[db]`
/// ausente ou `own` false) — caminho de sempre. `Some(Err)` = declaração
/// quebrada (grupo inválido) — o chamador EXIGE tratamento visível.
pub fn config_de(toml_src: &str, name: &str) -> Option<Result<BancoConfig, String>> {
    let toml = flux_common::config::ProjectToml::parse_str(toml_src).ok()?;
    let db = toml.db.as_ref()?;
    if !db.own() {
        return None;
    }
    Some(db.db_grupo(name).map(|grupo| BancoConfig {
        ttl_days: db.ttl_days_efetivo(),
        ttl_secs: db.ttl_secs(),
        grupo,
    }))
}

/// Nome do container dedicado do grupo — FONTE ÚNICA (o override injeta
/// `DB_HOST` com este nome; o YAML gerado fixa `container_name` igual).
pub fn container_name(grupo: &str) -> String {
    format!("flux-db-{grupo}")
}

// ------------------------------------------------- definição sem compose

/// Lê/gera as credenciais do grupo no settings do state (k/v). Geradas UMA
/// vez por grupo com `randomblob` do próprio SQLite (zero dependência
/// nova): 48 chars hex, seguros por construção — sem aspas, sem escape,
/// viram user/senha MySQL válidos e entram limpos no YAML e no SQL.
/// Sempre devolve o MESMO conjunto para o grupo: container e apps convergem
/// (a sincronização no container cobre container que já existia com
/// credenciais antigas).
pub(crate) fn credenciais_de(st: &State, grupo: &str) -> Result<(String, String, String), String> {
    let gerar = |st: &State, chave: &str| -> Result<String, String> {
        let existente = st
            .with(|c| {
                c.query_row(
                    "SELECT v FROM settings WHERE k = ?1",
                    rusqlite::params![chave],
                    |r| r.get::<_, String>(0),
                )
                .optional()
            })
            .map_err(|e| e.to_string())?;
        if let Some(v) = existente {
            return Ok(v);
        }
        let novo: String = st
            .with(|c| c.query_row("SELECT lower(hex(randomblob(24)))", [], |r| r.get(0)))
            .map_err(|e| e.to_string())?;
        st.with(|c| {
            c.execute(
                "INSERT INTO settings(k, v) VALUES (?1, ?2)
                 ON CONFLICT(k) DO UPDATE SET v = excluded.v",
                rusqlite::params![chave, novo],
            )
        })
        .map_err(|e| e.to_string())?;
        Ok(novo)
    };
    let root = gerar(st, &format!("db-root/{grupo}"))?;
    let user = gerar(st, &format!("db-user/{grupo}"))?;
    let senha = gerar(st, &format!("db-pass/{grupo}"))?;
    Ok((root, user, senha))
}

/// Define o banco pelo flux.toml QUANDO o render do compose não tem service
/// de banco — o compose do projeto fica SEM banco e o flux baixa sozinho
/// (21/10 v2). Imagem = `[db] image` ou IMAGEM_PADRAO; credenciais geradas
/// uma vez no settings e sincronizadas no container em `pos_saude`.
pub fn definir_do_toml(
    st: &State,
    grupo: &str,
    imagem: Option<&str>,
) -> Result<crate::compose::CapturaBanco, String> {
    let (root, user, senha) = credenciais_de(st, grupo)?;
    identificador_seguro(&user, "usuário do banco gerado")?;
    Ok(crate::compose::CapturaBanco {
        imagem: imagem.unwrap_or(IMAGEM_PADRAO).to_owned(),
        environment: std::collections::BTreeMap::from([
            ("MYSQL_ROOT_PASSWORD".to_owned(), root),
            ("MYSQL_USER".to_owned(), user),
            ("MYSQL_PASSWORD".to_owned(), senha),
        ]),
        command: None,
    })
}

/// Cria/sincroniza o usuário do grupo: `CREATE USER IF NOT EXISTS` + `ALTER
/// USER ... IDENTIFIED BY` — cobre container que já existia com credenciais
/// diferentes do mecanismo novo e rotação futura. Senha sai do gerador
/// (hex), então entra no SQL sem escape especial.
async fn sincronizar_usuario(
    grupo: &str,
    root_senha: &str,
    user: &str,
    senha: &str,
) -> Result<(), String> {
    identificador_seguro(user, "usuário do banco")?;
    let sql = format!(
        "CREATE USER IF NOT EXISTS '{user}'@'%' IDENTIFIED BY '{senha}'; \
         ALTER USER '{user}'@'%' IDENTIFIED BY '{senha}'; FLUSH PRIVILEGES;"
    );
    sql_exec(grupo, root_senha, &sql, 64 * 1024)
        .await
        .map(|_| ())
}

/// Volume nomeado do dado do grupo — UM por grupo, fora do compose de
/// qualquer instância (volume de compose de instância morreria no
/// `down -v` do teardown de temporários).
pub fn volume_name(grupo: &str) -> String {
    format!("flux-db-{grupo}-data")
}

/// Database da instância: `{projeto}_{env}`. Projeto vem do contrato
/// `[a-z0-9-]`; `@` de worktree é defensivamente normalizado. O charset é
/// conferido — o nome entra em SQL identificado por backtick.
pub fn database_name(projeto: &str, env: Env) -> Result<String, String> {
    let saneado = projeto.replace('@', "-");
    if !flux_common::nomes::projeto_valido(&saneado) {
        return Err(format!(
            "nome de projeto '{projeto}' não vira database (use [a-z0-9-])"
        ));
    }
    Ok(format!("{saneado}_{}", env))
}

/// Identificador SQL seguro (charset de `database_name` + user do compose).
pub(crate) fn identificador_seguro(s: &str, o_que: &str) -> Result<(), String> {
    let ok = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-');
    if ok {
        Ok(())
    } else {
        Err(format!(
            "{o_que} '{s}' tem caractere fora de [a-z0-9_-] — recusado"
        ))
    }
}

/// Escapa string como escalar YAML (JSON string é YAML válido e o escape é
/// do serde — sem parser/serializer de YAML no repo).
fn yaml_str(v: &str) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "\"\"".to_owned())
}

/// Decisão pura do TTL: expirou quando HOUVE relógio e o ocioso estourou o
/// TTL. Sem relógio (nunca interagiu E sem created_at) NUNCA apaga — dado
/// ausente não vira "está velho há infinito" (mesma filosofia do temporary).
pub fn expirou(ultimo: Option<i64>, agora: i64, ttl_secs: i64) -> bool {
    match ultimo {
        Some(t) => agora.saturating_sub(t) > ttl_secs,
        None => false,
    }
}

/// Membros com banco próprio: `[(projeto, BancoConfig)]` de todos os
/// projetos registrados. Erro de TOML individual é warn (projeto pulado),
/// nunca silêncio.
pub fn membros(st: &State) -> Result<Vec<(String, BancoConfig)>, String> {
    let nomes = st.list_projects().map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for n in nomes {
        let src = match st.project_toml(&n) {
            Ok(Some(s)) => s,
            Ok(None) => continue,
            Err(e) => return Err(e.to_string()),
        };
        if let Some(cfg) = config_de(&src, &n) {
            match cfg {
                Ok(c) => out.push((n, c)),
                Err(e) => tracing::warn!("banco próprio: {n}: {e}"),
            }
        }
    }
    Ok(out)
}

// ------------------------------------------------------------ container (up)

/// Garante o container do grupo saudável + o database da instância criado.
/// Chamado pelo `up` DEPOIS do render (precisa da captura) e ANTES do
/// `compose up` do app. Idempotente: container saudável pula a subida.
pub async fn garantir(
    st: &State,
    state_dir: &Path,
    docker: &Docker,
    grupo: &str,
    captura: &crate::compose::CapturaBanco,
    db_novo: &str,
) -> Result<(), String> {
    identificador_seguro(db_novo, "database")?;
    let container = container_name(grupo);

    // Fast path: já está de pé e saudável — nada a fazer além do database.
    if estado_saude(docker, &container).await.as_deref() == Some("healthy") {
        return pos_saude(st, grupo, captura, db_novo).await;
    }

    // Compose gerado do render do engine — determinístico (BTreeMap), então
    // dois `up` concorrentes do mesmo grupo escrevem o MESMO arquivo.
    let dir = state_dir.join("db").join(grupo);
    std::fs::create_dir_all(&dir).map_err(|e| format!("criando {}: {e}", dir.display()))?;
    let arquivo = dir.join("compose.yml");
    std::fs::write(&arquivo, yaml_container(grupo, captura))
        .map_err(|e| format!("escrevendo {}: {e}", arquivo.display()))?;

    let projeto = container.clone();
    events::event(
        st,
        "info",
        "banco",
        &format!(
            "{projeto}: subindo container de banco próprio (imagem {})",
            captura.imagem
        ),
    );
    let inicio = std::time::Instant::now();
    let saida = tokio::process::Command::new("docker")
        .args(["compose", "-p", &projeto, "-f"])
        .arg(&arquivo)
        .args(["up", "-d"])
        .output()
        .await
        .map_err(|e| format!("compose up do banco: {e}"))?;
    if !saida.status.success() {
        return Err(format!(
            "compose up do banco falhou: {}",
            String::from_utf8_lossy(&saida.stderr).trim()
        ));
    }

    esperar_saude(docker, &container, ESPERA_SAUDE_SECS).await?;
    events::event(
        st,
        "info",
        "banco",
        &format!(
            "{projeto}: saudável em {:.1}s (cold init na 1ª subida, wake quente é bem menor)",
            inicio.elapsed().as_secs_f32()
        ),
    );
    pos_saude(st, grupo, captura, db_novo).await
}

/// Depois do container saudável (fast path ou subida): sincroniza o usuário
/// do grupo e garante o database da instância.
async fn pos_saude(
    st: &State,
    grupo: &str,
    captura: &crate::compose::CapturaBanco,
    db_novo: &str,
) -> Result<(), String> {
    let user = user_de(captura)?;
    let root = root_de(captura)?;
    sincronizar_usuario(grupo, &root, user, senha_de(captura)?).await?;
    garantir_database(st, grupo, &root, db_novo, user).await
}

fn root_de(captura: &crate::compose::CapturaBanco) -> Result<String, String> {
    captura
        .environment
        .get("MYSQL_ROOT_PASSWORD")
        .cloned()
        .ok_or_else(|| "banco próprio sem MYSQL_ROOT_PASSWORD — recusado".to_owned())
}

fn user_de(captura: &crate::compose::CapturaBanco) -> Result<&str, String> {
    let user = captura
        .environment
        .get("MYSQL_USER")
        .ok_or_else(|| "banco próprio sem MYSQL_USER — recusado (credenciais são obrigatórias, venham do compose ou do gerador)".to_owned())?;
    identificador_seguro(user, "usuário do banco")?;
    Ok(user.as_str())
}

fn senha_de(captura: &crate::compose::CapturaBanco) -> Result<&str, String> {
    captura
        .environment
        .get("MYSQL_PASSWORD")
        .map(String::as_str)
        .ok_or_else(|| "banco próprio sem MYSQL_PASSWORD — recusado".to_owned())
}

/// Espera o healthcheck do container virar `healthy` (teto em segundos).
async fn esperar_saude(docker: &Docker, container: &str, teto_secs: u64) -> Result<(), String> {
    let prazo = std::time::Instant::now() + std::time::Duration::from_secs(teto_secs);
    loop {
        if estado_saude(docker, container).await.as_deref() == Some("healthy") {
            return Ok(());
        }
        if std::time::Instant::now() > prazo {
            return Err(format!(
                "container {container} não ficou saudável em {teto_secs}s — veja `docker logs {container}`"
            ));
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
}

/// `health.status` do container por nome; `None` = sem healthcheck definido
/// ou sem estado — tratado como "não saudável" pelos chamadores.
async fn estado_saude(docker: &Docker, container: &str) -> Option<String> {
    let insp = docker.inspect_container(container, None).await.ok()?;
    insp.state?.health?.status.map(|s| s.to_string())
}

async fn garantir_database(
    st: &State,
    grupo: &str,
    root_senha: &str,
    db: &str,
    user: &str,
) -> Result<(), String> {
    let sql = format!(
        "CREATE DATABASE IF NOT EXISTS `{db}`; GRANT ALL PRIVILEGES ON `{db}`.* TO '{user}'@'%'; FLUSH PRIVILEGES;"
    );
    sql_exec(grupo, root_senha, &sql, 1024 * 1024).await?;
    events::event(
        st,
        "info",
        "banco",
        &format!(
            "{}: database {db} garantido (GRANT ALL para {user})",
            container_name(grupo)
        ),
    );
    Ok(())
}

/// Gera o YAML do container dedicado — PURO (testado sem engine). Env verbatim
/// do compose do projeto (compat: credenciais são as que o .env do projeto já
/// usa); volume nomeado do grupo; healthcheck com fallback MariaDB/MySQL;
/// restart "no" (o ciclo é do fluxd, igual aos apps) e NENHUMA porta
/// publicada (regra a68e4ba vale dobrado para banco).
fn yaml_container(grupo: &str, c: &crate::compose::CapturaBanco) -> String {
    let mut y = String::from("services:\n  db:\n");
    y.push_str(&format!("    image: {}\n", yaml_str(&c.imagem)));
    if let Some(cmd) = &c.command
        && !cmd.is_empty()
    {
        y.push_str("    command:\n");
        for a in cmd {
            y.push_str(&format!("      - {}\n", yaml_str(a)));
        }
    }
    y.push_str(&format!("    container_name: {}\n", container_name(grupo)));
    y.push_str("    environment:\n");
    for (k, v) in &c.environment {
        y.push_str(&format!("      {}: {}\n", yaml_str(k), yaml_str(v)));
    }
    y.push_str("    restart: \"no\"\n");
    y.push_str("    healthcheck:\n");
    let ping = "mysqladmin ping -h 127.0.0.1 -p\"${MYSQL_ROOT_PASSWORD:-$MARIADB_ROOT_PASSWORD}\"";
    y.push_str(&format!(
        "      test: [\"CMD-SHELL\", {}]\n",
        yaml_str(ping)
    ));
    y.push_str(
        "      interval: 2s\n      timeout: 3s\n      retries: 60\n      start_period: 10s\n",
    );
    y.push_str(&format!(
        "    volumes:\n      - {}:/var/lib/mysql\n",
        volume_name(grupo)
    ));
    y.push_str("    networks:\n      - flux-net\n");
    y.push_str("networks:\n  flux-net:\n    external: true\n");
    y.push_str(&format!("volumes:\n  {}:\n", volume_name(grupo)));
    y
}

// ----------------------------------------------------------------- SQL exec

/// Guarda de LEITURA do db/query (MCP, 22/10): aceita só um statement único
/// cuja primeira palavra seja SELECT/SHOW/DESCRIBE/DESC/EXPLAIN, sem travas
/// (`FOR UPDATE`/`LOCK IN SHARE MODE` — leitura que bloqueia escrita não é
/// leitura aqui). Regra com UM dono: o servidor valida; ninguém replica.
pub fn sql_e_leitura(sql: &str) -> Result<(), String> {
    let limpa = sql.trim();
    if limpa.is_empty() {
        return Err("SQL vazio".into());
    }
    // Um statement só: `;` no meio (não no fim) é segundo statement — recusa.
    let sem_ponto_virgula_final = limpa.trim_end_matches([';', ' ', '\n', '\r', '\t']);
    if sem_ponto_virgula_final.contains(';') {
        return Err("db/query aceita UM statement — remova o `;` extra".into());
    }
    let primeira = sem_ponto_virgula_final
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    match primeira.as_str() {
        "select" | "show" | "describe" | "desc" | "explain" => {}
        outra => {
            return Err(format!(
                "db/query é SÓ leitura (SELECT/SHOW/DESCRIBE/EXPLAIN) — recebido `{outra}`"
            ));
        }
    }
    let minusculo = sem_ponto_virgula_final.to_ascii_lowercase();
    for trava in ["for update", "lock in share mode"] {
        if minusculo.contains(trava) {
            return Err(format!(
                "db/query recusa trava de escrita (`{trava}`) — leitura não bloqueia"
            ));
        }
    }
    Ok(())
}

/// Roda SQL no container do grupo. A senha de root vem DO STATE (fonte da
/// verdade, `credenciais_de`) e entra via `-e MYSQL_ROOT_PASSWORD` do exec —
/// NUNCA da env do container: datadir que existe de antes pode ter senha
/// diferente da env atual (container recriado pula o init — apanhado ao
/// vivo no cutover v2). O `-e` aparece no argv do CLI do docker na VM por
/// instantes; VM single-user, aceito e documentado. SQL vai por stdin.
pub(crate) async fn sql_exec(
    grupo: &str,
    root_senha: &str,
    sql: &str,
    teto: usize,
) -> Result<String, String> {
    let cliente = "mysql -N -uroot -p\"${MYSQL_ROOT_PASSWORD:-$MARIADB_ROOT_PASSWORD}\"";
    let mut filho = tokio::process::Command::new("docker")
        .args([
            "exec",
            "-i",
            "-e",
            &format!("MYSQL_ROOT_PASSWORD={root_senha}"),
            &container_name(grupo),
            "sh",
            "-c",
            cliente,
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("docker exec no {}: {e}", container_name(grupo)))?;
    if let Some(mut stdin) = filho.stdin.take() {
        use tokio::io::AsyncWriteExt;
        stdin
            .write_all(sql.as_bytes())
            .await
            .map_err(|e| format!("escrevendo SQL no stdin: {e}"))?;
        drop(stdin); // EOF fecha o batch
    }
    let saida = filho.wait_with_output().await.map_err(|e| e.to_string())?;
    if !saida.status.success() {
        return Err(format!(
            "SQL falhou (exit {:?}): {}",
            saida.status.code(),
            String::from_utf8_lossy(&saida.stderr).trim()
        ));
    }
    if saida.stdout.len() > teto {
        return Err(format!(
            "saída do SQL passou do teto de {teto} bytes — recusado (dado maior do que o esperado)"
        ));
    }
    Ok(String::from_utf8_lossy(&saida.stdout).into_owned())
}

/// Databases existentes no container (fora os schemas de sistema). Senha
/// vem do settings do grupo.
pub async fn listar_databases(st: &State, grupo: &str) -> Result<Vec<String>, String> {
    let (root, _, _) = credenciais_de(st, grupo)?;
    let bruto = sql_exec(
        grupo,
        &root,
        "SELECT SCHEMA_NAME FROM information_schema.SCHEMATA;",
        1024 * 1024,
    )
    .await?;
    let sistema = ["information_schema", "mysql", "performance_schema", "sys"];
    Ok(bruto
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !sistema.contains(l))
        .map(ToOwned::to_owned)
        .collect())
}

/// Dump + DROP com backup ANTES (regra de casa: destrutivo um-way vem com
/// backup automático). Devolve o caminho do dump. O dump usa
/// `--databases` (leva CREATE DATABASE junto — restore é uma linha).
pub async fn apagar_database_com_backup(
    st: &State,
    backup_dir: &Path,
    grupo: &str,
    db: &str,
) -> Result<PathBuf, String> {
    identificador_seguro(db, "database")?;
    let (root, _, _) = credenciais_de(st, grupo)?;
    std::fs::create_dir_all(backup_dir)
        .map_err(|e| format!("criando {}: {e}", backup_dir.display()))?;
    let dump = sql_dump(grupo, &root, db, TETO_DUMP_BYTES).await?;
    let agora = crate::sleep::now_epoch();
    let caminho = backup_dir.join(format!("{db}-{agora}.sql"));
    std::fs::write(&caminho, &dump)
        .map_err(|e| format!("escrevendo dump {}: {e}", caminho.display()))?;
    sql_exec(
        grupo,
        &root,
        &format!("DROP DATABASE IF EXISTS `{db}`;"),
        1024,
    )
    .await?;
    events::event(
        st,
        "warn",
        "banco",
        &format!(
            "{}: database {db} APAGADO (dump {} bytes em {})",
            container_name(grupo),
            dump.len(),
            caminho.display()
        ),
    );
    Ok(caminho)
}

/// `mysqldump --databases {db}` com teto de leitura — estourou, o processo
/// morre e o ERRO volta (nunca dump truncado passado por bom).
async fn sql_dump(grupo: &str, root_senha: &str, db: &str, teto: usize) -> Result<Vec<u8>, String> {
    use tokio::io::AsyncReadExt;
    // O nome já passou por identificador_seguro (charset [a-z0-9_-]) — SEM
    // backticks aqui: isto é string de SHELL (sh -c), onde backtick é
    // command substitution (apanhado ao vivo: `db` virou comando). O mysql
    // resolve identificador simples sem escaping. Senha do STATE via -e
    // (ver sql_exec).
    let cliente = format!(
        "mysqldump -uroot -p\"${{MYSQL_ROOT_PASSWORD:-$MARIADB_ROOT_PASSWORD}}\" --databases {db} --single-transaction"
    );
    let mut filho = tokio::process::Command::new("docker")
        .args([
            "exec",
            "-i",
            "-e",
            &format!("MYSQL_ROOT_PASSWORD={root_senha}"),
            &container_name(grupo),
            "sh",
            "-c",
            &cliente,
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("mysqldump no {}: {e}", container_name(grupo)))?;
    // stderr lido em tarefa própria — warning do cliente mysql não trava o pipe.
    let stderr_tarefa = {
        let mut err = filho.stderr.take().expect("stderr piped");
        tokio::spawn(async move {
            let mut buf = Vec::new();
            let _ = err.read_to_end(&mut buf).await;
            buf
        })
    };
    let mut out = Vec::new();
    let mut truncado = false;
    if let Some(mut stdout) = filho.stdout.take() {
        let mut chunk = [0u8; 64 * 1024];
        loop {
            let n = stdout
                .read(&mut chunk)
                .await
                .map_err(|e| format!("lendo dump: {e}"))?;
            if n == 0 {
                break;
            }
            if out.len() + n > teto {
                truncado = true;
                break;
            }
            out.extend_from_slice(&chunk[..n]);
        }
    }
    if truncado {
        let _ = filho.kill().await;
        return Err(format!(
            "dump de {db} passou de {teto} bytes — abortado, nada apagado"
        ));
    }
    let status = filho.wait().await.map_err(|e| e.to_string())?;
    let stderr = stderr_tarefa.await.unwrap_or_default();
    if !status.success() {
        return Err(format!(
            "mysqldump falhou (exit {:?}): {}",
            status.code(),
            String::from_utf8_lossy(&stderr).trim()
        ));
    }
    Ok(out)
}

// ---------------------------------------------------------- ciclo de vida

/// Acorda o container do grupo (se existe e está parado). Missing = projeto
/// nunca deu `up` com own-db — no-op (nada para acordar). Erro de engine é
/// warn, nunca pânico do wake.
pub async fn acordar(st: &State, docker: &Docker, grupo: &str) {
    let container = container_name(grupo);
    match docker.inspect_container(&container, None).await {
        Ok(insp) => {
            let rodando = insp.state.as_ref().and_then(|s| s.running).unwrap_or(false);
            if rodando {
                return;
            }
            match docker.start_container(&container, None).await {
                Ok(()) => events::event(
                    st,
                    "info",
                    "banco",
                    &format!("{container}: acordou junto com o projeto"),
                ),
                Err(e) => tracing::warn!("banco: start de {container} falhou: {e}"),
            }
        }
        Err(_) => tracing::debug!("banco: {container} não existe ainda — nada a acordar"),
    }
}

/// Alguma instância do grupo tem container RUNNING? (Instância = projeto
/// registrado do grupo × env.)
async fn grupo_tem_rodando(docker: &Docker, membros_grupo: &[(String, BancoConfig)]) -> bool {
    for (projeto, _) in membros_grupo {
        for env in [Env::Dev, Env::Prod] {
            let project = crate::docker::compose_project(projeto, env);
            let ok = match crate::docker::listar_label(docker, Some(&project)).await {
                Ok(list) => list.iter().any(|c| {
                    c.state.as_ref().is_some_and(|s| {
                        matches!(s, bollard::models::ContainerSummaryStateEnum::RUNNING)
                    })
                }),
                Err(e) => {
                    tracing::warn!("banco: listagem de {project} falhou: {e}");
                    true // falha de engine NUNCA vira "pode dormir" (fail-closed)
                }
            };
            if ok {
                return true;
            }
        }
    }
    false
}

/// Para o container do grupo quando a ÚLTIMA instância parou. Devolve true
/// se parou agora. Falha de listagem = não dorme (fail-closed acima).
pub async fn dormir_se_ninguem(st: &State, docker: Option<&Docker>, grupo: &str) -> bool {
    let Some(d) = docker else { return false };
    let do_grupo: Vec<(String, BancoConfig)> = match membros(st) {
        Ok(m) => m.into_iter().filter(|(_, c)| c.grupo == grupo).collect(),
        Err(e) => {
            tracing::error!("banco: falha listando membros do grupo {grupo}: {e}");
            return false;
        }
    };
    if grupo_tem_rodando(d, &do_grupo).await {
        return false;
    }
    let container = container_name(grupo);
    let rodando = d
        .inspect_container(&container, None)
        .await
        .ok()
        .and_then(|i| i.state)
        .and_then(|s| s.running)
        .unwrap_or(false);
    if !rodando {
        return false;
    }
    match d.stop_container(&container, None).await {
        Ok(()) => {
            events::event(
                st,
                "info",
                "banco",
                &format!("{container}: dormiu junto (última instância do grupo {grupo} parou)"),
            );
            true
        }
        Err(e) => {
            tracing::warn!("banco: stop de {container} falhou: {e}");
            false
        }
    }
}

// ------------------------------------------------------------------- TTL

/// Limpeza de databases de DEV inativos (sweep, 60 s): por grupo com
/// container rodando, database de instância sem interação há mais do TTL →
/// dump + DROP (com evento warn). Container parado = nada a fazer (a
/// limpeza pega a rodada seguinte quando o grupo acordar). Prod NUNCA entra.
pub async fn limpeza_ttl(
    st: &State,
    docker: Option<&Docker>,
    backup_dir: Option<&Path>,
    agora: i64,
) {
    let Some(d) = docker else { return };
    let Some(backup_dir) = backup_dir else { return };
    let todos = match membros(st) {
        Ok(m) => m,
        Err(e) => {
            tracing::error!("banco: limpeza TTL pulada — falha listando membros: {e}");
            return;
        }
    };
    // Agrupa por grupo mantendo a ordem (determinístico para os eventos).
    let mut grupos: Vec<(String, Vec<String>)> = Vec::new();
    for (projeto, cfg) in &todos {
        match grupos.iter_mut().find(|(g, _)| g == &cfg.grupo) {
            Some((_, v)) => v.push(projeto.clone()),
            None => grupos.push((cfg.grupo.clone(), vec![projeto.clone()])),
        }
    }
    for (grupo, projetos) in grupos {
        let rodando = d
            .inspect_container(&container_name(&grupo), None)
            .await
            .ok()
            .and_then(|i| i.state)
            .and_then(|s| s.running)
            .unwrap_or(false);
        if !rodando {
            continue;
        }
        let cfg = todos
            .iter()
            .find(|(_, c)| c.grupo == grupo)
            .map(|(_, c)| c.clone())
            .expect("grupo veio de um membro");
        let existentes = match listar_databases(st, &grupo).await {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!("banco: listagem de databases de {grupo} falhou: {e}");
                continue;
            }
        };
        for projeto in projetos {
            let Ok(db) = database_name(&projeto, Env::Dev) else {
                continue;
            };
            if !existentes.contains(&db) {
                continue;
            }
            let ultimo = match st.temporary_clock(&projeto) {
                Ok((interacao, criado)) => interacao.or(criado),
                Err(e) => {
                    tracing::warn!("banco: relógio de {projeto} falhou: {e}");
                    continue;
                }
            };
            if !expirou(ultimo, agora, cfg.ttl_secs) {
                continue;
            }
            let dias_ocioso = ultimo.map(|t| (agora - t) / 86_400).unwrap_or(0);
            match apagar_database_com_backup(st, backup_dir, &grupo, &db).await {
                Ok(caminho) => events::event(
                    st,
                    "warn",
                    "banco",
                    &format!(
                        "TTL: {db} inativo há ~{dias_ocioso} dias (> {} dias) — apagado com dump \
                         em {} (seeders do projeto reconstroem)",
                        cfg.ttl_days,
                        caminho.display()
                    ),
                ),
                Err(e) => events::event(
                    st,
                    "error",
                    "banco",
                    &format!("TTL: falha apagando {db}: {e}"),
                ),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn cfg(grupo: &str) -> BancoConfig {
        BancoConfig {
            grupo: grupo.to_owned(),
            ttl_days: 7,
            ttl_secs: 7 * 86_400,
        }
    }

    #[test]
    fn sql_e_leitura_aceita_consulta_unica_de_leitura() {
        for sql in [
            "SELECT * FROM vippers_dev.users",
            "  select 1 ",
            "SHOW TABLES;",
            "describe vippers_dev.migrations",
            "EXPLAIN SELECT 1",
            "desc tabela",
        ] {
            assert!(sql_e_leitura(sql).is_ok(), "deveria passar: {sql}");
        }
    }

    #[test]
    fn sql_e_leitura_barra_escrita_e_segundo_statement() {
        for sql in [
            "DROP DATABASE vippers_dev",
            "DELETE FROM users",
            "UPDATE users SET x = 1",
            "INSERT INTO users VALUES (1)",
            "ALTER TABLE users ADD c INT",
            "SELECT 1; DROP DATABASE x",
            "USE mysql; SELECT 1",
            "",
            "   ",
        ] {
            assert!(sql_e_leitura(sql).is_err(), "deveria recusar: {sql:?}");
        }
    }

    #[test]
    fn sql_e_leitura_barra_trava_de_escrita() {
        assert!(sql_e_leitura("SELECT * FROM t FOR UPDATE").is_err());
        assert!(sql_e_leitura("select * from t lock in share mode").is_err());
        assert!(
            sql_e_leitura("SELECT * FROM for_update_test").is_ok(),
            "nome de tabela que CONTÉM o termo não é trava"
        );
    }

    #[test]
    fn container_e_volume_seguem_o_grupo() {
        assert_eq!(container_name("vippers"), "flux-db-vippers");
        assert_eq!(volume_name("vippers"), "flux-db-vippers-data");
        // Grupo != nome (worktree aponta pro principal): container é do grupo.
        assert_eq!(container_name("vippers-agenda"), "flux-db-vippers-agenda");
    }

    #[test]
    fn database_name_e_projeto_env_com_charset_conferido() {
        assert_eq!(
            database_name("vippers", Env::Dev).unwrap(),
            "vippers_dev",
            "nome_env é o contrato (compat com o .env injetado)"
        );
        assert_eq!(database_name("vippers", Env::Prod).unwrap(), "vippers_prod");
        assert_eq!(
            database_name("vippers-agenda", Env::Dev).unwrap(),
            "vippers-agenda_dev",
            "traço do nome passa (backtick protege no SQL)"
        );
        assert!(
            database_name("../escapou", Env::Dev).is_err(),
            "charset fora do contrato é ERRO, nunca SQL montado"
        );
    }

    #[test]
    fn expirou_só_com_relogio_e_teto_estourado() {
        let agora = 1_000_000_000;
        let ttl = 7 * 86_400;
        assert!(!expirou(None, agora, ttl), "sem relógio NUNCA apaga");
        assert!(
            !expirou(Some(agora - ttl), agora, ttl),
            "no limite ainda não"
        );
        assert!(
            expirou(Some(agora - ttl - 1), agora, ttl),
            "1s além do TTL expira"
        );
    }

    #[test]
    fn config_de_cobre_none_ok_e_erro() {
        assert!(
            config_de("name = \"djm\"", "djm").is_none(),
            "sem [db] = None (comportamento de sempre)"
        );
        assert!(
            config_de("[db]\nrebuild_policy = \"auto\"", "djm").is_none(),
            "[db] sem own = None"
        );
        let ok =
            config_de("[db]\nown = true\nttl_days = 3\n", "vippers").expect("own=true vira Some");
        assert_eq!(
            ok.unwrap(),
            BancoConfig {
                grupo: "vippers".into(),
                ttl_days: 3,
                ttl_secs: 3 * 86_400
            }
        );
        let com_grupo = config_de("[db]\nown = true\ngrupo = \"vippers\"\n", "vippers-agenda")
            .expect("own=true")
            .unwrap();
        assert_eq!(com_grupo.grupo, "vippers", "grupo declarado vence o nome");
        let ruim = config_de("[db]\nown = true\ngrupo = \"Vai Dar Nao\"\n", "x")
            .expect("own=true")
            .unwrap_err();
        assert!(ruim.contains("db.grupo"), "erro cita a chave: {ruim}");
    }

    fn captura_mysql() -> crate::compose::CapturaBanco {
        crate::compose::CapturaBanco {
            imagem: "mysql:9.3".into(),
            environment: BTreeMap::from([
                ("MYSQL_DATABASE".to_owned(), "vippers".to_owned()),
                ("MYSQL_PASSWORD".to_owned(), "password".to_owned()),
                ("MYSQL_ROOT_PASSWORD".to_owned(), "root_password".to_owned()),
                ("MYSQL_USER".to_owned(), "vippers".to_owned()),
            ]),
            command: None,
        }
    }

    #[test]
    fn yaml_do_container_cobre_as_regras_sem_porta_publicada() {
        let y = yaml_container("vippers", &captura_mysql());
        assert!(y.contains("    image: \"mysql:9.3\"\n"));
        assert!(y.contains("    container_name: flux-db-vippers\n"));
        assert!(
            y.contains("\"MYSQL_ROOT_PASSWORD\": \"root_password\"\n"),
            "env verbatim do compose do projeto, escapado: {y}"
        );
        assert!(y.contains("restart: \"no\""), "ciclo é do fluxd");
        assert!(
            y.contains("flux-db-vippers-data:/var/lib/mysql"),
            "volume nomeado do grupo (nunca volume de instância)"
        );
        assert!(
            y.contains("mysqladmin ping"),
            "healthcheck é o gate do esperar_saude"
        );
        assert!(
            y.contains("MYSQL_ROOT_PASSWORD:-$MARIADB_ROOT_PASSWORD"),
            "ping cobre mysql E mariadb"
        );
        assert!(y.contains("external: true"), "flux-net é external");
        assert!(
            !y.contains("ports:"),
            "banco próprio NUNCA publica porta (mesma regra do override, a68e4ba)"
        );
    }

    #[test]
    fn yaml_do_container_inclui_command_verbatim_quando_ha() {
        let mut c = captura_mysql();
        c.command = Some(vec!["mysqld".into(), "--skip-log-bin".into()]);
        let y = yaml_container("vippers", &c);
        assert!(
            y.contains("- \"--skip-log-bin\""),
            "command do render entra verbatim: {y}"
        );
    }

    #[test]
    fn user_de_recusa_captura_sem_credenciais() {
        assert_eq!(user_de(&captura_mysql()).unwrap(), "vippers");
        let mut c = captura_mysql();
        c.environment.remove("MYSQL_USER");
        let err = user_de(&c).unwrap_err();
        assert!(err.contains("MYSQL_USER"), "erro aponta a chave: {err}");
    }

    /// Design v2 (21/10, pedido do Gabriel): compose SEM service de banco —
    /// o flux baixa sozinho. Credenciais geradas UMA vez no settings e
    /// estáveis entre chamadas (container e apps convergem); imagem do
    /// toml vence o default.
    #[test]
    fn definir_do_toml_gera_creds_estaveis_e_imagem_certa() {
        let st = State::in_memory().unwrap();

        let primeira = definir_do_toml(&st, "vippers", Some("mysql:9.3")).unwrap();
        assert_eq!(primeira.imagem, "mysql:9.3");
        for chave in ["MYSQL_ROOT_PASSWORD", "MYSQL_USER", "MYSQL_PASSWORD"] {
            let v = primeira.environment.get(chave).expect(chave);
            assert_eq!(v.len(), 48, "randomblob(24) hex = 48 chars");
            assert!(
                v.chars().all(|c| c.is_ascii_hexdigit()),
                "hex puro = seguro em YAML e SQL sem escape: {v}"
            );
        }

        // Segunda chamada (outro up): MESMAS credenciais — nunca regenera.
        let segunda = definir_do_toml(&st, "vippers", None).unwrap();
        assert_eq!(
            primeira.environment, segunda.environment,
            "credenciais do grupo são estáveis no settings"
        );
        assert_eq!(
            segunda.imagem, IMAGEM_PADRAO,
            "sem [db] image = default da plataforma"
        );

        // Outro grupo tem credenciais próprias (não compartilha).
        let outro = definir_do_toml(&st, "outro", None).unwrap();
        assert_ne!(
            primeira.environment.get("MYSQL_ROOT_PASSWORD"),
            outro.environment.get("MYSQL_ROOT_PASSWORD"),
            "grupos distintos não dividem segredo"
        );

        // user gerado passa no identificador (entra no SQL sem escape).
        assert!(
            identificador_seguro(
                primeira.environment.get("MYSQL_USER").expect("user"),
                "usuário do banco gerado"
            )
            .is_ok()
        );
    }

    #[test]
    fn identificador_seguro_barra_o_errado() {
        assert!(identificador_seguro("vippers_dev", "database").is_ok());
        assert!(identificador_seguro("vippers-dev", "database").is_ok());
        assert!(identificador_seguro("", "database").is_err());
        assert!(identificador_seguro("vip`; DROP", "database").is_err());
    }
}
