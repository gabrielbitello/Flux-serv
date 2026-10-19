//! Caminhos canônicos da VM (L1.8).
//!
//! DELIBERADO: alguns fluxos derivam a raiz de `state_dir` (compose.rs,
//! router.rs) em vez de usar a constante — a derivação é GATE de "estou na
//! VM". Com `--db` custom os dois caminhos podem divergir; nenhum fluxo
//! consome os dois ao mesmo tempo. Não unificar sem reabrir essa decisão.

/// Raiz dos projetos na VM (02).
pub const PROJECTS_ROOT_PADRAO: &str = "/srv/flux/projects";
