//! Testes do módulo de sync (spec 02). Ponte `#[path]` sobre `src/sync/mod.rs` —
//! PERMANENTE, não transitória: `main.rs` JÁ declara `mod sync;`, mas crate
//! só-binário (sem [lib]) não expõe módulos a testes de integração, então o
//! target recompila `src/sync.rs` como módulo avulso — mesmas deps do pacote,
//! sem tocar o binário. Constraint da ponte: `src/sync.rs` não pode
//! referenciar `crate::` do main.rs (a quebra é pega pelo clippy
//! --all-targets do ci.yml — barulhenta, não silenciosa).

#[path = "../src/sync/mod.rs"]
mod sync;

use std::path::{Path, PathBuf};

use sync::{Beta, SessionStatus, StatusState, SyncError, SyncSpec};

// ------------------------------------------------------------------ helpers

/// Diretório temporário único por pid — sessões e dados de teste nunca
/// colidem com nada do usuário.
fn tmp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("flux-sync-test-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn escrever(base: &Path, relativo: &str, conteudo: &str) {
    let alvo = base.join(relativo);
    std::fs::create_dir_all(alvo.parent().unwrap()).unwrap();
    std::fs::write(alvo, conteudo).unwrap();
}

/// Gate da convenção do projeto: sem `MUTAGEN_BIN` o teste PASSA vazio.
fn gate_sync_real() -> bool {
    if std::env::var(sync::ENV_BIN).is_err() {
        eprintln!(
            "pulado: {} não definido — teste de sync real local<->local requer o binário mutagen no PATH ou apontado pela env",
            sync::ENV_BIN
        );
        return false;
    }
    true
}

/// Espera condicional com prazo generoso — sync local converge em segundos
/// (spec 02: latência típica < 1 s, pior caso < 5 s).
fn espera(cond: impl Fn() -> bool, descricao: &str) {
    let prazo = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while std::time::Instant::now() < prazo {
        if cond() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    panic!("sync não convergiu em 60 s: {descricao}");
}

/// Sessão mínima local<->local com cleanup garantido pelo chamador.
fn spec_local(tag: &str) -> (SyncSpec, PathBuf, PathBuf) {
    let alpha = tmp_dir(&format!("{tag}-alpha"));
    let beta = tmp_dir(&format!("{tag}-beta"));
    escrever(&alpha, "README.md", "v1");
    let spec = SyncSpec::new(
        &format!("sync-{tag}"),
        alpha.clone(),
        Beta::Local(beta.clone()),
        None,
    )
    .unwrap();
    // Teste que panica não chega ao terminate do chamador: a sessão órfã
    // envenena a próxima criação ("config vigente é a da criação original;
    // ignores novos só entram com flux sync reset") e o sync nunca converge
    // para os tempdirs novos. Cleanup best-effort ANTES de criar (Err =
    // não existia, ignorado de propósito).
    let _ = sync::terminate(&spec.session());
    (spec, alpha, beta)
}

// ------------------------------------------------------- pureza: sessão/args

#[test]
fn nome_de_sessao_e_deterministico_por_projeto_e_branch() {
    let spec = SyncSpec::new(
        "vipers",
        PathBuf::from("C:\\dev\\vipers"),
        Beta::Local(PathBuf::from("/tmp/b")),
        None,
    )
    .unwrap();
    assert_eq!(spec.session(), "flux-vipers");
    let spec2 = SyncSpec::new(
        "vipers",
        PathBuf::from("a"),
        Beta::Local(PathBuf::from("b")),
        Some("feat/API-2"),
    )
    .unwrap();
    assert_eq!(spec2.session(), "flux-vipers-feat-api-2");
    // Slug é armazenado já sanitizado — duas chamadas com a mesma branch dão a
    // mesma sessão (spec 02: URL derivada determinística).
    let de_novo = SyncSpec::new(
        "vipers",
        PathBuf::from("a"),
        Beta::Local(PathBuf::from("b")),
        Some("feat/API-2"),
    )
    .unwrap();
    assert_eq!(spec2, de_novo);
}

#[test]
fn slug_de_branch_segue_a_sanitizacao_da_spec_02() {
    assert_eq!(sync::branch_slug("main").unwrap(), "main");
    assert_eq!(sync::branch_slug("feat/API-2").unwrap(), "feat-api-2");
    // `/` vira `-` e repetidos colapsam (spec 02: sanitizado, determinístico).
    assert_eq!(sync::branch_slug("feature//x").unwrap(), "feature-x");
    assert_eq!(sync::branch_slug("MAIN").unwrap(), "main");
    assert_eq!(
        sync::branch_slug("-ponto-.no-meio-").unwrap(),
        "ponto-no-meio"
    );
    // Branch sem nenhum caractere salvo não tem host derivável — erro explícito.
    assert!(matches!(
        sync::branch_slug("---"),
        Err(SyncError::Config(_))
    ));
}

#[test]
fn projeto_vazio_e_recusado() {
    assert!(matches!(
        SyncSpec::new(
            "",
            PathBuf::from("a"),
            Beta::Local(PathBuf::from("b")),
            None
        ),
        Err(SyncError::Config(_))
    ));
}

#[test]
fn ignores_globais_sao_verbatim_da_spec_02() {
    // 18/10: public/{fonts,css,js} saíram — asset publicado commitado no repo
    // (filament:assets do vippers) precisa sincronizar; artefato de build vive
    // em public/build/, que continua ignorado.
    // 18/10 parte 2: vendor/, node_modules/ e storage/ ancorados na raiz —
    // padrão solto casava qualquer nível e engolia
    // resources/views/vendor/ (override de blade Laravel oficial).
    // 22/10: /bootstrap/cache/ e public/storage — runtime do app na VM
    // (cache gerado com DONO do container + uploads do disco público que
    // só existem na VM; o ignore PROTEGE a mídia de delete do one-way).
    assert_eq!(
        sync::GLOBAL_IGNORES,
        &[
            ".env",
            ".env.*",
            "public/hot",
            "/vendor/",
            "/node_modules/",
            "public/build/",
            "/storage/",
            "/bootstrap/cache/",
            "public/storage",
        ]
    );
}

#[test]
fn dirs_gerados_da_raiz_nao_engolem_aninhados() {
    // resources/views/vendor/ é override de blade Laravel oficial: o segmento
    // "vendor" aninhado TEM que sincronizar — o ignore vale pro dir da raiz.
    let spec = SyncSpec::new(
        "vippers",
        PathBuf::from("a"),
        Beta::Local(PathBuf::from("b")),
        None,
    )
    .unwrap();
    let ignores = spec.ignores();
    assert!(!crate::sync::caminho_ignorado(
        "resources/views/vendor/filament-tables/index.blade.php",
        &ignores
    ));
    assert!(crate::sync::caminho_ignorado(
        "vendor/laravel/framework",
        &ignores
    ));
    assert!(crate::sync::caminho_ignorado(
        "storage/framework/views/x.php",
        &ignores
    ));
    assert!(!crate::sync::caminho_ignorado(
        "app/Support/storage-notes.md",
        &ignores
    ));
}

#[test]
fn extra_ignore_adiciona_e_nunca_subtrai_globais() {
    let mut spec = SyncSpec::new(
        "vipers",
        PathBuf::from("a"),
        Beta::Local(PathBuf::from("b")),
        None,
    )
    .unwrap();
    // Projeto tenta "adicionar" um global de novo e um novo padrão: global não
    // duplica, novo entra no fim (globais primeiro, ordem estável).
    // ".env" segue global solto: tentar "adicionar" de novo não duplica.
    spec.extra_ignore = vec![".env".into(), "resources/svg/**".into()];
    let ignores = spec.ignores();
    assert_eq!(ignores.len(), sync::GLOBAL_IGNORES.len() + 1);
    for (i, global) in sync::GLOBAL_IGNORES.iter().enumerate() {
        assert_eq!(&ignores[i], global);
    }
    assert_eq!(ignores.last().unwrap(), "resources/svg/**");
}

#[test]
fn create_args_tem_formato_da_spec_02() {
    let mut spec = SyncSpec::new(
        "vipers",
        PathBuf::from("C:\\dev\\vipers"),
        Beta::Ssh {
            user: sync::VM_USER.into(),
            host: sync::VM_HOST.into(),
            path: "/srv/flux/projects/vipers".into(),
        },
        None,
    )
    .unwrap();
    spec.extra_ignore = vec!["docs/**".into()];
    let args = spec.create_args();

    // Flags de identidade e modo verbatim da spec (nome da sessão, label,
    // one-way-replica: Windows = alpha fonte, VM = beta réplica).
    assert_eq!(&args[0..2], &["sync".to_owned(), "create".to_owned()]);
    assert!(args.contains(&"--name=flux-vipers".to_owned()));
    assert!(args.contains(&"--label=flux".to_owned()));
    assert!(args.contains(&"--mode=one-way-replica".to_owned()));

    // Ignora em pares `-i <padrão>`, globais + extras, nesta ordem.
    let pares: Vec<&String> = args
        .iter()
        .zip(args.iter().skip(1))
        .filter(|(flag, _)| flag.as_str() == "-i")
        .map(|(_, valor)| valor)
        .collect();
    let esperados: Vec<String> = spec.ignores();
    assert_eq!(pares, esperados.iter().collect::<Vec<_>>());

    // Alpha (Windows) e beta (VM) posicionais, nessa ordem, no fim.
    assert_eq!(args[args.len() - 2], "C:\\dev\\vipers");
    assert_eq!(
        args.last().unwrap(),
        &format!(
            "{}@{}:{}",
            sync::VM_USER,
            sync::VM_HOST,
            "/srv/flux/projects/vipers"
        )
    );
}

#[test]
fn beta_local_vira_path_e_ssh_vira_url() {
    assert_eq!(
        Beta::Local(PathBuf::from(r"C:\temp\réplica")).to_url(),
        r"C:\temp\réplica"
    );
    assert_eq!(
        Beta::Ssh {
            user: "flux".into(),
            host: "192.168.140.2".into(),
            path: "/srv/flux/projects/pedi@api2".into(),
        }
        .to_url(),
        "flux@192.168.140.2:/srv/flux/projects/pedi@api2"
    );
}

#[test]
fn vm_beta_monta_caminho_da_vm_por_worktree() {
    let principal = sync::vm_beta("pedi", None);
    assert_eq!(
        principal.to_url(),
        "flux@192.168.140.2:/srv/flux/projects/pedi"
    );
    let worktree = sync::vm_beta("pedi", Some("feat-x"));
    assert_eq!(
        worktree.to_url(),
        "flux@192.168.140.2:/srv/flux/projects/pedi@feat-x"
    );
}

#[test]
fn args_de_sessao_e_list_usam_nome_completo() {
    assert_eq!(
        SyncSpec::session_args("pause", "flux-vipers"),
        vec![
            "sync".to_owned(),
            "pause".to_owned(),
            "flux-vipers".to_owned()
        ]
    );
    let list = SyncSpec::list_args(Some("flux-vipers"));
    assert_eq!(&list[0..2], &["sync".to_owned(), "list".to_owned()]);
    assert!(list[2].starts_with("--template="));
    assert!(list[2].contains("json"));
    assert_eq!(list[3], "flux-vipers");
    assert_eq!(SyncSpec::list_args(None).len(), 3);
}

// ------------------------------------------------------------- pureza: parse

/// Fixture no shape REAL do modelo público do mutagen
/// (pkg/api/models/synchronization): status é enum protobuf NUMÉRICO, problemas
/// vêm como lista + contador de truncamento, conflitos como lista.
const SESSAO_REAL: &str = r#"{"identifier":"abc123","version":2,"creationTime":"2026-10-13T10:00:00Z","creatingVersion":"0.18.0","alpha":{"protocol":0,"path":"C:\\proj\\vipers","connected":true,"scanned":true,"directories":3,"files":10,"scanProblems":[{"path":"estranho.txt","error":"acesso negado"}],"excludedScanProblems":2},"beta":{"protocol":1,"user":"flux","host":"192.168.140.2","port":22,"path":"/srv/flux/projects/vipers","connected":true,"files":10,"transitionProblems":[{"path":"x","error":"y"}],"excludedTransitionProblems":1},"name":"flux-vipers","labels":{"flux":""},"paused":false,"status":6,"lastError":"ciclo anterior falhou","conflicts":[{"alphaChanges":[],"betaChanges":[]}],"excludedConflicts":1}"#;

#[test]
fn parse_de_json_lines_no_shape_real_do_mutagen() {
    let saida = format!("{SESSAO_REAL}\n");
    let sessoes = sync::parse_list(&saida);
    assert_eq!(sessoes.len(), 1);
    let s = &sessoes[0];
    assert_eq!(s.name, "flux-vipers");
    assert_eq!(s.identifier, "abc123");
    assert!(!s.paused);
    assert_eq!(s.status, StatusState::Ociosa);
    // Alpha local (sem host), beta remoto.
    assert_eq!(s.alpha.path, r"C:\proj\vipers");
    assert!(!s.alpha.remote);
    assert_eq!(s.beta.path, "/srv/flux/projects/vipers");
    assert!(s.beta.remote);
    assert!(s.beta.connected);
    // Problemas = lista + truncados (EndpointState do mutagen).
    assert_eq!(s.alpha.scan_problems, 3);
    assert_eq!(s.beta.transition_problems, 2);
    assert_eq!(s.beta.files, Some(10));
    // Conflitos em one-way NÃO deveriam existir — se existirem, contam.
    assert_eq!(s.conflicts, 2);
    assert_eq!(s.last_error.as_deref(), Some("ciclo anterior falhou"));
    assert!(!s.saudavel());
}

#[test]
fn sessao_ociosa_sem_pendencias_e_saudavel() {
    // Uma linha só: o formato real do template é JSON Lines.
    let limpa = r#"{"identifier":"x","name":"flux-pedi","paused":false,"status":6,"alpha":{"path":"C:\\p","connected":true},"beta":{"host":"vm","path":"/srv","connected":true}}"#;
    let sessoes = sync::parse_list(limpa);
    let s = &sessoes[0];
    assert!(s.saudavel());
    assert_eq!(
        s.alpha.files, None,
        "sem scan ainda — arquivos ausentes, não zero"
    );
}

#[test]
fn parse_aceita_array_unico_e_descarta_ruido() {
    // Formato alternativo defensivo (array único) + linha vazia final do
    // template — nada disso quebra o parse.
    let array = format!("[{}]", SESSAO_REAL);
    assert_eq!(sync::parse_list(&array).len(), 1);
    let com_ruido = format!("linha perdida\n{SESSAO_REAL}\n\n");
    assert_eq!(sync::parse_list(&com_ruido).len(), 1);
    assert!(sync::parse_list("   \n").is_empty());
}

#[test]
fn objeto_sem_identifier_nao_e_sessao() {
    let v = serde_json::json!({"name": "sem-identifier"});
    assert!(sync::session_de(&v).is_none());
}

#[test]
fn tabela_de_status_cobre_o_enum_do_state_proto() {
    // pkg/synchronization/state.proto: cada valor na sua classe.
    use StatusState::{Atencao, Desconhecido, Ociosa, Parada, Trabalhando};
    assert_eq!(StatusState::from_mutagen(6), Ociosa);
    for n in [4, 5, 7, 9, 10, 11, 12, 13] {
        assert_eq!(StatusState::from_mutagen(n), Trabalhando, "n={n}");
    }
    for n in [0, 8] {
        assert_eq!(StatusState::from_mutagen(n), Atencao, "n={n}");
    }
    for n in [1, 2, 3] {
        assert_eq!(StatusState::from_mutagen(n), Parada, "n={n}");
    }
    assert_eq!(StatusState::from_mutagen(99), Desconhecido(99));
    // Halted exige reset (spec 02); desconhecido não finge que é ok.
    assert!(StatusState::Parada.rotulo().contains("reset"));
    assert!(StatusState::Desconhecido(42).rotulo().contains("42"));
}

#[test]
fn status_em_string_de_mutagen_futuro_tambem_parseia() {
    let v = serde_json::json!("Watching for changes");
    assert_eq!(StatusState::from_json(&v), StatusState::Ociosa);
    assert_eq!(
        StatusState::from_json(&serde_json::json!("Staging files on beta")),
        StatusState::Trabalhando
    );
    assert_eq!(
        StatusState::from_json(&serde_json::json!("Halted on root deletion")),
        StatusState::Parada
    );
    assert_eq!(
        StatusState::from_json(&serde_json::json!("Waiting for rescan")),
        StatusState::Atencao
    );
    assert_eq!(
        StatusState::from_json(&serde_json::json!("qualquer-coisa-nova")),
        StatusState::Desconhecido(-1)
    );
}

#[test]
fn pause_e_ultimo_erro_derrubam_a_saude() {
    let pausada = r#"{"identifier":"x","name":"flux-pedi","paused":true,"status":6,"alpha":{"path":"a","connected":true},"beta":{"path":"b","connected":true}}"#;
    let sessoes = sync::parse_list(pausada);
    assert!(sessoes[0].paused);
    assert!(!sessoes[0].saudavel());

    let com_erro = r#"{"identifier":"x","name":"flux-pedi","paused":false,"status":6,"alpha":{"path":"a","connected":true},"beta":{"path":"b","connected":true},"lastError":"beta write failed"}"#;
    assert!(!sync::parse_list(com_erro)[0].saudavel());
}

// --------------------------------------------------------- pureza: create id

#[test]
fn identifier_do_create_e_extraido_da_resposta_padrao() {
    // Formato limpo...
    assert_eq!(
        sync::parse_created_id("Created session sync_PmrL4GyyDhta6WFt").unwrap(),
        "sync_PmrL4GyyDhta6WFt"
    );
    // ...e com progresso ANTES da marca (saída real do mutagen 0.18).
    assert_eq!(
        sync::parse_created_id(
            "Creating session...     Connecting to agent (POSIX)...                  Created session sync_aTN3qL8gGyy1BviLUMcMeBb9j0TNAxgEQJA3GTieQ5D"
        )
        .unwrap(),
        "sync_aTN3qL8gGyy1BviLUMcMeBb9j0TNAxgEQJA3GTieQ5D"
    );
    let err = sync::parse_created_id("saída estranha").unwrap_err();
    assert!(matches!(err, SyncError::Saida { .. }));
}

// ------------------------------------------------------------- flux.toml (06)

#[test]
fn for_project_le_nome_e_extra_ignore_do_flux_toml() {
    let dir = tmp_dir("toml");
    escrever(
        &dir,
        "flux.toml",
        "name = \"vipers\"\n\n[sync]\nextra_ignore = [\"resources/svg/**\", \".env\"]\n",
    );
    let spec = SyncSpec::for_project(&dir, Beta::Local(PathBuf::from("C:\\b")), None).unwrap();
    assert_eq!(spec.project, "vipers");
    // Extra repetindo global não duplica (nunca subtrai, nunca duplica).
    assert_eq!(spec.ignores().len(), sync::GLOBAL_IGNORES.len() + 1);
    let (spec_sem_toml_extra, _, _) = spec_local("toml-sem");
    assert_eq!(
        spec_sem_toml_extra.ignores().len(),
        sync::GLOBAL_IGNORES.len()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn for_project_sem_toml_usa_nome_da_pasta() {
    let dir = tmp_dir("sem-toml");
    escrever(&dir, "README.md", "oi");
    let spec = SyncSpec::for_project(&dir, Beta::Local(PathBuf::from("C:\\b")), None).unwrap();
    assert_eq!(spec.project, dir.file_name().unwrap().to_string_lossy());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn flux_toml_quebrado_e_erro_nunca_silencioso() {
    let dir = tmp_dir("toml-quebrado");
    escrever(&dir, "flux.toml", "naame = \"vipers\"");
    let err = SyncSpec::for_project(&dir, Beta::Local(PathBuf::from("C:\\b")), None).unwrap_err();
    assert!(err.to_string().contains("naame"), "erro: {err}");
    let _ = std::fs::remove_dir_all(&dir);
}

// ----------------------------------------------- gated: sync real com MUTAGEN_BIN
//
// Convenção do projeto: sem `MUTAGEN_BIN` estes testes PASSAM vazios. Com o
// binário definido, provam o mecanismo inteiro da spec 02 em local<->local:
// criação de sessão, exclusões obrigatórias, `.gitignore`, propagação de
// edição e de delete, convergência para "ociosa · sem pendências", pause/
// resume e reset (terminate + create).

#[test]
fn gated_sync_real_exclui_globais_e_propaga_edicao_e_delete() {
    if !gate_sync_real() {
        return;
    }
    let (spec, alpha, beta) = spec_local("gated");

    // Conteúdo que CRUZA (arquivo normal) + um de cada classe de exclusão
    // global da spec 02 + `.env.*` + `.gitignore` respeitado por padrão.
    escrever(&alpha, ".env", "APP_KEY=secreto-vm-only");
    escrever(&alpha, ".env.local", "DEBUG=1");
    escrever(&alpha, "vendor/pacote/lib.php", "<?php");
    escrever(&alpha, "node_modules/dep/index.js", "module.exports=1");
    escrever(&alpha, "storage/logs/laravel.log", "log");
    escrever(&alpha, "storage/framework/cache/x", "cache");
    escrever(&alpha, "public/hot", "espiã do npm run dev");
    escrever(&alpha, "public/build/app.js", "artefato");
    // NOTA DE CONTRATO (19/10): `.gitignore` NÃO é respeitado pela sessão —
    // a spec 02:67 promete `respectGitignore = true`, mas `create_args` nunca
    // mandou o flag (desde F2, e959857). Aqui codifica-se o contrato REAL
    // (GLOBAL_IGNORES + extra_ignore); implementar o flag é FEATURE, fora do
    // escopo da refatoração — gap registrado no diário e na spec 02.
    escrever(&alpha, ".gitignore", "ignorado.log\n");
    escrever(
        &alpha,
        "ignorado.log",
        "via .gitignore — CRUZA (sem respectGitignore, gap registrado)",
    );

    let id = sync::create(&spec).unwrap();
    assert!(!id.is_empty());

    // Arquivo normal chega (aceite 2).
    espera(
        || std::fs::read_to_string(beta.join("README.md")).is_ok_and(|c| c == "v1"),
        "arquivo normal deveria chegar ao beta",
    );

    // Convergência para o estado saudável (aceite 5: ok · sem pendências).
    espera(
        || {
            sync::status_of(&spec.session())
                .ok()
                .flatten()
                .map(|s| s.saudavel())
                .unwrap_or(false)
        },
        "sessão deveria convergir para ociosa · sem pendências",
    );

    // Exclusões obrigatórias (aceite 3 e "o que NUNCA fazer"): nada de segredo,
    // dependência, estado ou artefato de build. `ignorado.log` CRUZA de
    // propósito — sem `respectGitignore`, `.gitignore` não é filtro (gap da
    // spec 02:67 registrado; ver NOTA DE CONTRATO acima).
    for proibido in [
        ".env",
        ".env.local",
        "vendor",
        "node_modules",
        "storage",
        "public/hot",
        "public/build",
    ] {
        assert!(
            !beta.join(proibido).exists(),
            "`{proibido}` NUNCA deveria cruzar para o beta"
        );
    }
    // E o cruzamento do gitignored é ASSERTIDO — o teste trava o contrato
    // real; se um dia o flag entrar, ESTE teste quebra e o mapa atualiza.
    espera(
        || beta.join("ignorado.log").is_file(),
        "ignorado.log deveria CRUZAR (sem respectGitignore hoje)",
    );

    // Edição propaga (aceite 1: < 5 s típico).
    std::fs::write(alpha.join("README.md"), "v2").unwrap();
    espera(
        || std::fs::read_to_string(beta.join("README.md")).is_ok_and(|c| c == "v2"),
        "edição deveria propagar para o beta",
    );

    // Criação e delete propagam (aceite 2: cria → aparece; apaga → some).
    std::fs::write(alpha.join("temporario.txt"), "vida curta").unwrap();
    espera(
        || beta.join("temporario.txt").exists(),
        "arquivo novo deveria aparecer no beta",
    );
    std::fs::remove_file(alpha.join("temporario.txt")).unwrap();
    espera(
        || !beta.join("temporario.txt").exists(),
        "delete deveria propagar (one-way-replica)",
    );

    sync::terminate(&spec.session()).unwrap();
    let _ = std::fs::remove_dir_all(&alpha);
    let _ = std::fs::remove_dir_all(&beta);
}

#[test]
fn gated_sync_real_pause_e_resume_controlam_a_sessao() {
    if !gate_sync_real() {
        return;
    }
    let (spec, alpha, beta) = spec_local("gated-pause");
    sync::create(&spec).unwrap();
    espera(
        || {
            sync::status_of(&spec.session())
                .ok()
                .flatten()
                .map(|s| s.saudavel())
                .unwrap_or(false)
        },
        "sessão inicial deveria ficar saudável",
    );

    sync::pause(&spec.session()).unwrap();
    espera(
        || {
            sync::status_of(&spec.session())
                .ok()
                .flatten()
                .map(|s| s.paused)
                .unwrap_or(false)
        },
        "sessão deveria reportar paused após pause",
    );
    // Pausada NÃO é saudável — o painel mostra banner (spec 02, controles).
    let pausada: SessionStatus = sync::status_of(&spec.session()).unwrap().unwrap();
    assert!(!pausada.saudavel());

    // Enquanto pausada, edição não cruza (é o ponto do pause).
    std::fs::write(alpha.join("README.md"), "durante-o-pause").unwrap();
    std::thread::sleep(std::time::Duration::from_secs(3));
    assert_ne!(
        std::fs::read_to_string(beta.join("README.md")).unwrap_or_default(),
        "durante-o-pause",
        "sync pausada não deveria propagar edição"
    );

    sync::resume(&spec.session()).unwrap();
    espera(
        || {
            sync::status_of(&spec.session())
                .ok()
                .flatten()
                .map(|s| !s.paused && s.saudavel())
                .unwrap_or(false)
        },
        "sessão deveria voltar a ficar saudável após resume",
    );
    // E o pendurado da pausa chega no resume.
    espera(
        || std::fs::read_to_string(beta.join("README.md")).is_ok_and(|c| c == "durante-o-pause"),
        "edição feita na pausa deveria propagar após resume",
    );

    sync::terminate(&spec.session()).unwrap();
    let _ = std::fs::remove_dir_all(&alpha);
    let _ = std::fs::remove_dir_all(&beta);
}

#[test]
fn gated_sync_reset_recria_sessao_com_rescan_completo() {
    if !gate_sync_real() {
        return;
    }
    let (spec, alpha, beta) = spec_local("gated-reset");
    let id_inicial = sync::create(&spec).unwrap();
    espera(
        || {
            sync::status_of(&spec.session())
                .ok()
                .flatten()
                .map(|s| s.saudavel())
                .unwrap_or(false)
        },
        "sessão inicial deveria ficar saudável",
    );

    // Reset da spec 02: mata a sessão e recria com full rescan (terminate +
    // create, porque o reset NATIVO do mutagen só limpa histórico).
    let id_novo = sync::reset(&spec).unwrap();
    assert_ne!(id_novo, id_inicial, "reset deveria recriar a sessão");
    espera(
        || {
            sync::status_of(&spec.session())
                .ok()
                .flatten()
                .map(|s| s.saudavel())
                .unwrap_or(false)
        },
        "sessão recriada deveria convergir (rescan termina sozinho)",
    );
    // Rescan reaplica o conteúdo (full rescan dos dois lados).
    espera(
        || std::fs::read_to_string(beta.join("README.md")).is_ok_and(|c| c == "v1"),
        "conteúdo deveria ser resincronizado após reset",
    );

    sync::terminate(&spec.session()).unwrap();
    let _ = std::fs::remove_dir_all(&alpha);
    let _ = std::fs::remove_dir_all(&beta);
}
