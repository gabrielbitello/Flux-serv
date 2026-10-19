//! Tipos compartilhados entre o `flux` (CLI, Windows) e o `fluxd` (VM): schemas
//! de configuração, modelo de domínios gerenciados, taxonomia de erros e o
//! protocolo da API de controle. Nada aqui fala com rede, Docker ou SQLite.

pub mod config;
pub mod error;
pub mod framework;
pub mod host;
pub mod nomes;
pub mod protocol;
