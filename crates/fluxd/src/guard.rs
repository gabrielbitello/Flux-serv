//! Guard de imagens (F5, spec 05): analisa compose/Dockerfile ANTES de
//! qualquer pull — pegou imagem de banco/cache na lista bloqueada = falha com
//! arquivo, linha e imagem apontados. Build/up não roda. Sem override.
//!
//! Limitação honesta da fundação: varredura por LINHA (chave `image:` do
//! compose e `FROM` do Dockerfile), pulando comentários — sem parse de YAML
//! completo. Suficiente para os formatos reais dos projetos; YAML malicioso
//! com `image:` dentro de comentário multilinha escapa — o parse estruturado
//! entra quando o fluxd renderiza os overrides (F6, onde o YAML já é objeto).

pub const KIND_COMPOSE: &str = "compose";
pub const KIND_DOCKERFILE: &str = "dockerfile";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub file: String,
    pub line: usize,
    pub kind: &'static str,
    pub image: String,
    pub blocked_pattern: String,
}

/// Nome de repo da imagem: caixa baixa, tag e digest fora
/// (`registry:5000/bitnami/mariadb:11` → `registry:5000/bitnami/mariadb`).
pub fn image_repo(image: &str) -> &str {
    let s = image.trim();
    let till_digest = s.split('@').next().unwrap_or(s);
    match till_digest.rfind(':') {
        Some(idx) if till_digest[idx + 1..].contains('/') => till_digest,
        Some(idx) => &till_digest[..idx],
        None => till_digest,
    }
}

/// Padrões da lista (spec 05): `mysql`, `mysql-*`, `postgresql*`…
/// Casa por BASENAME do repo — `bitnami/mariadb` cai em `mariadb`,
/// `library/postgres` em `postgres`.
pub fn is_blocked(image: &str, block: &[String]) -> bool {
    let repo = image_repo(image).to_ascii_lowercase();
    let Some(basename) = repo.rsplit('/').next() else {
        return false;
    };
    block.iter().any(|pattern| {
        let p = pattern.to_ascii_lowercase();
        if let Some(prefix) = p.strip_suffix('-') {
            // `mysql-*`: variante oficial — basename é o prefixo ou deriva dele
            basename == prefix || basename.starts_with(&format!("{prefix}-"))
        } else if let Some(prefix) = p.strip_suffix('*') {
            basename.starts_with(prefix)
        } else {
            basename == p || repo.ends_with(&format!("/{p}"))
        }
    })
}

fn push_if_blocked(
    out: &mut Vec<Violation>,
    file: &str,
    kind: &'static str,
    n: usize,
    image: &str,
    block: &[String],
) {
    if let Some(pattern) = block
        .iter()
        .find(|p| is_blocked(image, std::slice::from_ref(p)))
    {
        out.push(Violation {
            file: file.to_owned(),
            line: n,
            kind,
            image: image.trim().to_owned(),
            blocked_pattern: pattern.clone(),
        });
    }
}

/// Varre conteúdo de compose (`image:` por service). `file` é só rótulo.
pub fn scan_compose(file: &str, content: &str, block: &[String]) -> Vec<Violation> {
    let mut out = Vec::new();
    for (n, raw) in content.lines().enumerate() {
        let line = raw.trim();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        if key.trim() != "image" {
            continue;
        }
        let sem_comentario = value.split(" #").next().unwrap_or(value);
        let image = sem_comentario.trim().trim_matches('"').trim_matches('\'');
        if image.is_empty() {
            continue;
        }
        push_if_blocked(&mut out, file, KIND_COMPOSE, n + 1, image, block);
    }
    out
}

/// Varre conteúdo de Dockerfile (`FROM` por estágio, flags `--platform` fora).
pub fn scan_dockerfile(file: &str, content: &str, block: &[String]) -> Vec<Violation> {
    let mut out = Vec::new();
    for (n, raw) in content.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some(rest) = line.strip_prefix("FROM ") else {
            continue;
        };
        let base = rest
            .split_whitespace()
            .find(|t| !t.starts_with("--"))
            .unwrap_or("");
        if base.is_empty() || base.eq_ignore_ascii_case("scratch") {
            continue;
        }
        push_if_blocked(&mut out, file, KIND_DOCKERFILE, n + 1, base, block);
    }
    out
}

/// Varre um conjunto de arquivos: `(rótulo, tipo, conteúdo)`.
pub fn scan_files(files: &[(&str, &str, &str)], block: &[String]) -> Vec<Violation> {
    let mut out = Vec::new();
    for (file, kind, content) in files {
        match *kind {
            KIND_COMPOSE => out.extend(scan_compose(file, content, block)),
            KIND_DOCKERFILE => out.extend(scan_dockerfile(file, content, block)),
            other => tracing::warn!("tipo de arquivo desconhecido no guard: {other}"),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block() -> Vec<String> {
        [
            "mysql",
            "mariadb",
            "mysql-*",
            "mariadb-*",
            "postgres",
            "postgresql*",
            "mongo",
            "mongo*",
            "redis",
            "valkey",
            "memcached",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    }

    #[test]
    fn repos_variados_casam_a_lista() {
        let b = block();
        for img in [
            "mysql:8.4",
            "mysql",
            "bitnami/mariadb:11",
            "library/postgres:16",
            "postgres:16-alpine",
            "mongo*",
            "MARIADB-FOO:9",
            "mysql:8",
            "redis:7",
            "docker.io/library/valkey:8",
            "registry:5000/memcached:1",
        ] {
            assert!(is_blocked(img, &b), "{img} deveria ser bloqueada");
        }
    }

    #[test]
    fn imagens_licitas_passam() {
        let b = block();
        for img in [
            "php:8.4-fpm",
            "node:22",
            "rust:1",
            "nginx:alpine",
            "alpine:3.20",
            "ghcr.io/vippers/site:latest",
            "mysqlworkbench", // basename ≠ mysql (prefixo exato exige -*)
        ] {
            assert!(!is_blocked(img, &b), "{img} NÃO deveria ser bloqueada");
        }
    }

    #[test]
    fn mysql_sem_coringa_nao_pega_variante() {
        // `mysql` puro é exato; a variante precisa de `mysql-*` na lista
        let só_mysql = vec!["mysql".to_owned()];
        assert!(is_blocked("mysql:8.4", &só_mysql));
        assert!(!is_blocked("mysql-router:8", &só_mysql));
        let com_coringa = vec!["mysql-*".to_owned()];
        assert!(is_blocked("mysql-router:8", &com_coringa));
    }

    #[test]
    fn compose_aponta_arquivo_e_linha() {
        let compose = r#"services:
  app:
    image: php:8.4-fpm
  db:
    image: "mysql:8.4"   # escondido nem adianta
"#;
        let v = scan_compose("compose.yaml", compose, &block());
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].line, 5);
        assert_eq!(v[0].image, "mysql:8.4");
        assert_eq!(v[0].file, "compose.yaml");
    }

    #[test]
    fn compose_com_comentario_e_sem_imagem_nao_viola() {
        let compose = "# image: mysql:8 (comentário ignora)\nservices:\n  app:\n    build: .\n";
        assert!(scan_compose("c.yaml", compose, &block()).is_empty());
    }

    #[test]
    fn dockerfile_multistage_e_flags() {
        let df = r#"FROM --platform=$BUILDPLATFORM rust:1 AS build
WORKDIR /app
COPY . .
RUN cargo build --release

FROM debian:bookworm-slim
COPY --from=build /app/target/release/app /usr/local/bin/app
"#;
        let v = scan_dockerfile("Dockerfile", df, &block());
        assert!(v.is_empty(), "rust/debian passam: {v:?}");

        let df_ruim = "FROM php:8.4-cli\nFROM mariadb:11\n";
        let v = scan_dockerfile("Dockerfile", df_ruim, &block());
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].line, 2);
        assert_eq!(v[0].kind, KIND_DOCKERFILE);
    }

    #[test]
    fn scan_files_agrega_os_dois_tipos() {
        let files = [
            (
                "compose.yaml",
                KIND_COMPOSE,
                "services:\n  db:\n    image: mongo*",
            ),
            ("Dockerfile", KIND_DOCKERFILE, "FROM redis:7\n"),
        ];
        let v = scan_files(&files, &block());
        assert_eq!(v.len(), 2);
    }
}
