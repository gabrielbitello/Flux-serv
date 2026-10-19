//! spec (L7.4): o que define UMA sessão — label, ignores, Beta/branch_slug,
//! SyncSpec. Extraído verbatim do sync.rs. Sem `crate::` (ponte #[path] de
//! tests/sync_tests.rs compila este módulo standalone); VM_USER/VM_HOST vêm
//! de super:: — fonte única do L2.5 preservada.

use std::path::Path;

use super::{LIST_TEMPLATE, SyncError, VM_HOST, VM_USER};
use flux_common::config::{ProjectToml, SyncSection};

/// Label que marca sessões gerenciadas pelo Flux (spec 02: `--label=flux`) —
/// valor vazio é válido e selecionável por `--label-selector flux`.
pub const LABEL: &str = "flux";

pub const VM_PROJECTS_ROOT: &str = "/srv/flux/projects";

/// Excludes GLOBAIS, da spec 02 ("Padrão global, não configurável por
/// segurança/estabilidade"). O projeto SÓ ADICIONA ignores via `flux.toml`
/// (`[sync].extra_ignore` e, opt-in, o `.dockerignore` — `ignores_do_projeto`);
/// remover daqui é impossível pela API do módulo —
/// é o que protege o container de perder estado a cada sync (spec 02, "o que
/// NUNCA fazer": nunca sincronizar storage/, vendor/, node_modules/, artefatos
/// de build).
/// 18/10: `public/{fonts,css,js}` saíram da lista — o vippers provou que esses
/// diretórios também hospedam asset PUBLICADO commitado no repo (60 arquivos
/// de `filament:assets` + estáticos do app): asset trackeado pelo git é fonte
/// da verdade e TEM que sincronizar; artefato de build do vite vive em
/// `public/build/`, que continua ignorado. Ignorado ≠ commitado.
/// 18/10 (parte 2): `vendor/`, `node_modules/` e `storage/` ficaram
/// ANCORADOS na raiz (`/vendor/` etc.) — padrão solto casa o segmento em
/// QUALQUER nível e engolia `resources/views/vendor/` (override de blade
/// LARAVEL OFICIAL, 4 arquivos trackeados no vippers: o tema custom inteiro
/// nunca chegava à VM). Dir gerado é o da RAIZ; aninhado é código.
pub const GLOBAL_IGNORES: &[&str] = &[
    ".env",
    ".env.*",
    "public/hot",
    "/vendor/",
    "/node_modules/",
    "public/build/",
    "/storage/",
];

/// Endpoint beta (réplica). Na vida real é a VM via SSH; em dev/testes, um
/// caminho local (local<->local, que é o que os testes gated provam).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Beta {
    /// VM via SSH: o mutagen injeta o agente automaticamente (spec 02).
    Ssh {
        user: String,
        host: String,
        path: String,
    },
    /// Caminho local — dev/testes. Path Windows com drive (`C:\...`) é tratado
    /// pelo parser de URL do próprio mutagen (letra de drive não vira protocolo).
    Local(std::path::PathBuf),
}

impl Beta {
    /// URL no formato que o mutagen consome como argumento posicional.
    pub fn to_url(&self) -> String {
        match self {
            Self::Ssh { user, host, path } => format!("{user}@{host}:{path}"),
            Self::Local(p) => p.display().to_string(),
        }
    }
}

/// Beta padrão da plataforma: `{user}@{host}:{root}/{projeto}` ou, para
/// worktree, `{root}/{projeto}@{slug}` (spec 02, "um diretório na VM por
/// worktree"). O slug é o mesmo da sessão — determinístico por branch.
pub fn vm_beta(project: &str, branch_slug: Option<&str>) -> Beta {
    let nome = match branch_slug {
        Some(slug) => format!("{project}@{slug}"),
        None => project.to_owned(),
    };
    Beta::Ssh {
        user: VM_USER.to_owned(),
        host: VM_HOST.to_owned(),
        path: format!("{VM_PROJECTS_ROOT}/{nome}"),
    }
}

/// Slug determinístico de branch (spec 02): fora de `[a-z0-9-]` vira `-`
/// (inclusive `/`), repetidos colapsam, `-` das pontas saem. Vazio é erro —
/// branch só de caracteres inválidos (ex.: `---`) não tem host derivável.
pub fn branch_slug(branch: &str) -> Result<String, SyncError> {
    let mut slug = String::with_capacity(branch.len());
    let mut ultimo_traco = true; // já "cola" traço inicial
    for c in branch.chars().flat_map(|c| c.to_lowercase()) {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            slug.push(c);
            ultimo_traco = false;
        } else if !ultimo_traco {
            slug.push('-');
            ultimo_traco = true;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    if slug.is_empty() {
        return Err(SyncError::Config(format!(
            "branch `{branch}` não produz slug [a-z0-9-] — renomeie a branch ou use a pasta principal"
        )));
    }
    Ok(slug)
}

/// Ignores de projeto do `[sync]` do `flux.toml`: `extra_ignore` + padrões do
/// `.dockerignore` quando `use_dockerignore = true` (append-only — nunca
/// re-inclui os globais). Comentários `#` e linhas vazias do arquivo caem
/// fora; os padrões passam VERBATIM — a semântica exata do dockerignore
/// (last-match-wins, exceções `!` com precedência própria) NÃO é reproduzida:
/// quem interpreta é o ignore do mutagen (sessão) e o `caminho_ignorado` da
/// plataforma (estabilidade), então padrões exóticos do Docker podem divergir.
/// Toggle ligado e arquivo ausente é ERRO — config declarada nunca vira
/// fallback silencioso.
pub fn ignores_do_projeto(dir: &Path, sync: Option<&SyncSection>) -> Result<Vec<String>, String> {
    let Some(sync) = sync else {
        return Ok(Vec::new());
    };
    let mut todos: Vec<String> = sync
        .extra_ignore
        .iter()
        .flatten()
        .map(|i| i.trim().to_owned())
        .filter(|i| !i.is_empty())
        .collect();
    if sync.use_dockerignore() {
        let caminho = dir.join(sync.dockerignore_path());
        let conteudo = std::fs::read_to_string(&caminho).map_err(|e| {
            format!(
                "[sync] use_dockerignore ligado mas {} não lê: {e}",
                caminho.display()
            )
        })?;
        for linha in conteudo.lines() {
            let linha = linha.trim();
            if linha.is_empty() || linha.starts_with('#') {
                continue;
            }
            if !todos.iter().any(|g| g == linha) {
                todos.push(linha.to_owned());
            }
        }
    }
    Ok(todos)
}

/// Especificação de uma sessão de sync de projeto (spec 02): quem é a sessão,
/// de onde para onde vai, e o que NUNCA cruza.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncSpec {
    /// Nome do projeto (identidade da sessão: `flux-{name}`).
    pub project: String,
    /// Slug da branch de worktree (`None` = pasta principal do projeto).
    pub branch_slug: Option<String>,
    /// Pasta do projeto no Windows (alpha, fonte da verdade).
    pub alpha: std::path::PathBuf,
    /// Réplica (beta): VM por SSH ou caminho local para dev/testes.
    pub beta: Beta,
    /// Ignores ADICIONAIS do `flux.toml` (`[sync].extra_ignore`) — nunca
    /// substituem os globais (spec 02).
    pub extra_ignore: Vec<String>,
}

impl SyncSpec {
    /// Especificação nova. Valida project não vazio e sanitiza a branch uma
    /// única vez — erro cedo, nunca na hora do comando.
    pub fn new(
        project: &str,
        alpha: std::path::PathBuf,
        beta: Beta,
        branch: Option<&str>,
    ) -> Result<Self, SyncError> {
        if project.is_empty() {
            return Err(SyncError::Config(
                "nome do projeto vazio — sessão mutagen precisa de nome".into(),
            ));
        }
        let slug = branch.map(branch_slug).transpose()?;
        Ok(Self {
            project: project.to_owned(),
            branch_slug: slug,
            alpha,
            beta,
            extra_ignore: Vec::new(),
        })
    }

    /// Especificação lida da pasta do projeto: nome efetivo do `flux.toml`
    /// (senão o nome da pasta, spec 06) e ignores do `[sync]` — `extra_ignore`
    /// mais `.dockerignore` quando ligado, via `ignores_do_projeto`.
    /// `flux.toml` ausente é suportado (defaults da plataforma); presente e
    /// quebrado é ERRO — nunca fallback silencioso.
    pub fn for_project(dir: &Path, beta: Beta, branch: Option<&str>) -> Result<Self, SyncError> {
        let pasta = dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let toml_path = dir.join("flux.toml");
        let mut spec = if toml_path.exists() {
            let t = ProjectToml::load(&toml_path).map_err(|e| SyncError::Config(e.to_string()))?;
            let nome = t.effective_name(&pasta).to_owned();
            let mut s = Self::new(&nome, dir.to_path_buf(), beta, branch)?;
            s.extra_ignore = ignores_do_projeto(dir, t.sync.as_ref()).map_err(SyncError::Config)?;
            s
        } else {
            Self::new(&pasta, dir.to_path_buf(), beta, branch)?
        };
        // Ignora extra vazio/whitespace — typo de toml não vira ignore vazio.
        spec.extra_ignore.retain(|i| !i.trim().is_empty());
        Ok(spec)
    }

    /// Nome da sessão: `flux-{name}` ou `flux-{name}-{slug}` (spec 02 —
    /// identificação da sessão por nome do projeto).
    pub fn session(&self) -> String {
        match &self.branch_slug {
            Some(slug) => format!("flux-{}-{slug}", self.project),
            None => format!("flux-{}", self.project),
        }
    }

    /// Ignores efetivos: globais OBRIGATÓRIOS primeiro (ordem estável),
    /// extras do projeto depois, sem duplicar os globais.
    pub fn ignores(&self) -> Vec<String> {
        let mut todos: Vec<String> = GLOBAL_IGNORES.iter().map(|s| s.to_string()).collect();
        for extra in &self.extra_ignore {
            if !todos.iter().any(|g| g == extra) {
                todos.push(extra.clone());
            }
        }
        todos
    }

    // -------------------------------------------------- montagem de argumentos

    /// Argumentos do `sync create`. Formato do label/nome verbatim da spec 02;
    /// `-i` por par (o mutagen aceita repetição); alpha e beta posicionais no
    /// fim, nessa ordem.
    pub fn create_args(&self) -> Vec<String> {
        let mut args = vec![
            "sync".into(),
            "create".into(),
            format!("--name={}", self.session()),
            format!("--label={LABEL}"),
            "--mode=one-way-replica".into(),
        ];
        for ig in self.ignores() {
            args.push("-i".into());
            args.push(ig);
        }
        args.push(self.alpha.display().to_string());
        args.push(self.beta.to_url());
        args
    }

    /// Argumentos de comando de sessão única por nome (`pause`, `resume`,
    /// `terminate`). Nome completo — nunca prefixo, para não casar sessão de
    /// worktree vizinha.
    pub fn session_args(sub: &str, name: &str) -> Vec<String> {
        vec!["sync".into(), sub.into(), name.into()]
    }

    /// Argumentos do `sync list` estruturado, filtrado por nome quando dado.
    pub fn list_args(name: Option<&str>) -> Vec<String> {
        let mut args = vec![
            "sync".into(),
            "list".into(),
            format!("--template={LIST_TEMPLATE}"),
        ];
        if let Some(n) = name {
            args.push(n.into());
        }
        args
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Toggle OFF: só `extra_ignore`, `.dockerignore` nem é lido.
    #[test]
    fn ignores_do_projeto_sem_toggle_so_extras() {
        let dir = std::env::temp_dir();
        let sync = SyncSection {
            extra_ignore: Some(vec!["resources/svg/**".into()]),
            use_dockerignore: Some(false),
            dockerignore_path: None,
        };
        let got = ignores_do_projeto(&dir, Some(&sync)).unwrap();
        assert_eq!(got, vec!["resources/svg/**".to_string()]);
        // Seção ausente: nada a adicionar.
        assert!(ignores_do_projeto(&dir, None).unwrap().is_empty());
    }

    /// Toggle ON: comentários e vazios caem fora, padrões passam verbatim
    /// (inclusive exceção `!`), extras vêm primeiro, duplicata some.
    #[test]
    fn ignores_do_projeto_soma_padroes_do_dockerignore() {
        let base = std::env::temp_dir().join(format!("flux-dockerignore-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(
            base.join(".dockerignore"),
            "# dependências\n\
             \n\
             node_modules\n\
             **/dist\n\
             tests/\n\
             !tests/keep.txt\n",
        )
        .unwrap();
        let sync = SyncSection {
            extra_ignore: Some(vec!["resources/svg/**".into(), "**/dist".into()]),
            use_dockerignore: Some(true),
            dockerignore_path: None,
        };
        let got = ignores_do_projeto(&base, Some(&sync)).unwrap();
        assert_eq!(
            got,
            vec![
                "resources/svg/**".to_string(),
                "**/dist".to_string(),
                "node_modules".to_string(),
                "tests/".to_string(),
                "!tests/keep.txt".to_string(),
            ]
        );
        std::fs::remove_dir_all(&base).ok();
    }

    /// Caminho custom (`dockerignore_path`) resolve relativo à raiz.
    #[test]
    fn ignores_do_projeto_respeita_dockerignore_path() {
        let base =
            std::env::temp_dir().join(format!("flux-dockerignore-path-{}", std::process::id()));
        std::fs::create_dir_all(base.join("config")).unwrap();
        std::fs::write(base.join("config").join("ctx.dockerignore"), b"docs/\n").unwrap();
        let sync = SyncSection {
            extra_ignore: None,
            use_dockerignore: Some(true),
            dockerignore_path: Some("config/ctx.dockerignore".into()),
        };
        let got = ignores_do_projeto(&base, Some(&sync)).unwrap();
        assert_eq!(got, vec!["docs/".to_string()]);
        std::fs::remove_dir_all(&base).ok();
    }

    /// Toggle ON e arquivo ausente é ERRO com o caminho na mensagem — config
    /// declarada nunca vira fallback silencioso.
    #[test]
    fn ignores_do_projeto_com_toggle_e_sem_arquivo_e_erro() {
        let base =
            std::env::temp_dir().join(format!("flux-dockerignore-ausente-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let sync = SyncSection {
            extra_ignore: None,
            use_dockerignore: Some(true),
            dockerignore_path: None,
        };
        let err = ignores_do_projeto(&base, Some(&sync)).unwrap_err();
        assert!(
            err.contains("use_dockerignore") && err.contains(".dockerignore"),
            "erro cita toggle e arquivo: {err}"
        );
        std::fs::remove_dir_all(&base).ok();
    }

    /// `for_project` integra os padrões do `.dockerignore` no spec da sessão.
    #[test]
    fn for_project_carrega_dockerignore_no_spec() {
        if !git_disponivel() {
            eprintln!("git indisponível — teste pulado");
            return;
        }
        let base = repo_temp("dockerignore-spec");
        std::fs::write(base.join("flux.toml"), b"[sync]\nuse_dockerignore = true\n").unwrap();
        std::fs::write(base.join(".dockerignore"), b"docs/\n**/target\n").unwrap();
        let spec = SyncSpec::for_project(&base, Beta::Local(base.join("replica")), None).unwrap();
        let ig = spec.ignores();
        for esperado in ["docs/", "**/target"] {
            assert!(
                ig.iter().any(|g| g == esperado),
                "{esperado} tem de entrar no spec: {ig:?}"
            );
        }
        // Globais OBRIGATÓRIOS continuam na frente, intocados.
        assert_eq!(ig.first(), Some(&".env".to_string()));
        std::fs::remove_dir_all(&base).ok();
    }

    use super::super::estabilidade::tests::{git_disponivel, repo_temp};
}
