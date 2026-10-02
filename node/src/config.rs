//! Daemon configuration from the environment.

use std::net::SocketAddr;
use std::path::PathBuf;

pub const DEFAULT_LISTEN: &str = "127.0.0.1:8741";

#[derive(Debug, Clone)]
pub struct NodeConfig {
    /// Bearer token every request except `/v1/health` must carry.
    pub token: String,
    /// Allowed roots for file operations and command working directories.
    pub roots: Vec<PathBuf>,
}

impl NodeConfig {
    /// - `EMBER_NODE_TOKEN` (required, non-empty): bearer token.
    /// - `EMBER_NODE_ROOTS`: colon-separated allowed roots (default `$HOME`).
    pub fn from_env() -> anyhow::Result<Self> {
        let token = std::env::var("EMBER_NODE_TOKEN").unwrap_or_default();
        anyhow::ensure!(!token.is_empty(), "EMBER_NODE_TOKEN must be set to a non-empty secret");
        let roots: Vec<PathBuf> = match std::env::var_os("EMBER_NODE_ROOTS") {
            Some(v) if !v.is_empty() => std::env::split_paths(&v).filter(|p| !p.as_os_str().is_empty()).collect(),
            _ => vec![PathBuf::from(
                std::env::var_os("HOME").ok_or_else(|| anyhow::anyhow!("neither EMBER_NODE_ROOTS nor HOME is set"))?,
            )],
        };
        Ok(Self { token, roots })
    }
}

/// `EMBER_NODE_LISTEN` (default `127.0.0.1:8741`).
pub fn listen_addr() -> anyhow::Result<SocketAddr> {
    Ok(std::env::var("EMBER_NODE_LISTEN").unwrap_or_else(|_| DEFAULT_LISTEN.into()).parse()?)
}
