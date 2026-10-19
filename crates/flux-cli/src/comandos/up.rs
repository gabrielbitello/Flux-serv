//! up (L7.2): extraído verbatim do main.rs — movimento sem "melhoria".

use std::path::{Path, PathBuf};

use crate::contexto::{LEASE_UP_SECS, contexto_atual, falhar, parse_env};
use flux_common::protocol::Env;
use sha2::{Digest, Sha256};

use crate::api::ApiClient;
use crate::comandos::ciclo::imprime_status;

/// Teto de espera pelo acordar do container no `flux up` (build de primeira
/// subida é de minutos; acima disso o problema é visível no painel).
pub(crate) const UP_TIMEOUT_SECS: u64 = 600;

/// Precedência da flag de seed (05): flag explícita vence o flux.toml; sem
/// flag, None — o fluxd decide pelo `seed_on_rebuild`. `--seed` junto com
/// `--no-seed` é bloqueado pelo `conflicts_with` do clap.
pub(crate) fn seed_escolhida(seed: bool, no_seed: bool) -> Option<bool> {
    if seed {
        Some(true)
    } else if no_seed {
        Some(false)
    } else {
        None
    }
}

/// `[dev] overlays` ligado no flux.toml do projeto? Parse tolerante: toml
/// quebrado = false (o register já vai reclamar dele).
pub(crate) fn overlays_habilitados(toml_src: &str) -> bool {
    flux_common::config::ProjectToml::parse_str(toml_src)
        .ok()
        .and_then(|t| t.dev)
        .and_then(|d| d.overlays)
        .unwrap_or(false)
}

/// Aplica `.flux/overlays/**` sobre o repo NA VM: tar do dir local, pipe por
/// ssh, extração em `/srv/flux/projects/{name}/`. Devolve quantos arquivos
/// foram aplicados (contagem local — a lista nunca contém conteúdo, só paths).
pub(crate) fn aplicar_overlays(origem: &Path, name: &str) -> Result<usize, String> {
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
                    &format!("{}@{}", crate::tunel::VM_USER, crate::tunel::VM_HOST),
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
pub(crate) fn coletar_overlays(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
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
pub(crate) fn coletar_migrations(extra_ignore: &[String]) -> Vec<(String, String)> {
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
pub(crate) fn filtra_migrations_ignoradas(
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
pub(crate) fn coletar_migrations_em(dir: &Path) -> Vec<(String, String)> {
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

/// `flux up [env]` (06, fluxo passo a passo). Fase F6: passos 1–4 reais
/// (contexto, saúde do fluxd, registro, lease renovável) + status consolidado;
/// build/sync/banco (passos 5–7) são orquestrados pelo fluxd e entram nas
/// fases F7+ — o que a API expõe hoje aparece com rótulo honesto. `seed`:
/// None = fluxd decide pelo flux.toml; Some = flag explícita vence o toml (05).
pub(crate) async fn up(env: &str, seed: Option<bool>) {
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
    let _guarda = crate::api::manter_lease(
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
                Err(e) => falhar(crate::api::ApiError::Protocolo {
                    msg: format!("sync: {e}"),
                }),
            },
            Err(e) => falhar(crate::api::ApiError::Protocolo {
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
    let acordado_em =
        |s: &crate::api::ProjectStatusOut| s.dev_container_state.as_deref() == Some("awake");
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
        falhar(crate::api::ApiError::Protocolo {
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
