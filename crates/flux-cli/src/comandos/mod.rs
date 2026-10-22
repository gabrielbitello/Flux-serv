//! Domínios de comando do CLI (L7.2): um módulo por domínio, extraídos
//! do main.rs. Cada um recebe o contexto do projeto de contexto.rs.

pub(crate) mod build;
pub(crate) mod ciclo;
pub(crate) mod db;
pub(crate) mod env_cmd;
pub(crate) mod live;
pub(crate) mod logs;
pub(crate) mod mcp;
pub(crate) mod redroom;
pub(crate) mod sync_cmd;
pub(crate) mod tokens;
pub(crate) mod up;
pub(crate) mod watch;
