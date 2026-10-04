//! Daemon configuration from the environment, and the local endpoint file through which
//! programs on the same computer (`ember-term`, the VS Code companion extension) find the daemon.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const DEFAULT_LISTEN: &str = "127.0.0.1:8741";

#[derive(Debug, Clone)]
pub struct NodeConfig {
    /// Bearer token every request except `/v1/health` must carry.
    pub token: String,
    /// Allowed roots for file operations and command working directories.
    pub roots: Vec<PathBuf>,
    /// Where persistent terminal metadata (and the local endpoint file) live. `None`: terminal
    /// sessions are kept in memory only and no keeper is started.
    pub state_dir: Option<PathBuf>,
    /// The `ember-node` binary, started as a PTY keeper per terminal session so sessions can
    /// survive a node restart. `None` disables keepers.
    pub pty_keeper: Option<PathBuf>,
    /// Which destinations `/v1/egress` may reach (FR-R1); default: enabled, everything allowed.
    pub egress: crate::egress::EgressPolicy,
}

impl NodeConfig {
    /// A configuration with no persistent state (tests, embedding).
    pub fn new(token: impl Into<String>, roots: Vec<PathBuf>) -> Self {
        Self { token: token.into(), roots, state_dir: None, pty_keeper: None, egress: Default::default() }
    }

    /// - `EMBER_NODE_TOKEN` (required, non-empty): bearer token.
    /// - `EMBER_NODE_ROOTS`: colon-separated allowed roots (default `$HOME`).
    /// - `EMBER_NODE_STATE_DIR`: state directory (default `$HOME/.ember/node`).
    /// - `EMBER_NODE_KEEP_PTY`: `0` disables PTY keepers (terminal sessions then end with the
    ///   daemon).
    /// - `EMBER_NODE_EGRESS`, `EMBER_NODE_EGRESS_DENY`: the browser egress policy
    ///   ([`crate::egress::EgressPolicy::from_env`]).
    pub fn from_env() -> anyhow::Result<Self> {
        let token = std::env::var("EMBER_NODE_TOKEN").unwrap_or_default();
        anyhow::ensure!(!token.is_empty(), "EMBER_NODE_TOKEN must be set to a non-empty secret");
        let roots: Vec<PathBuf> = match std::env::var_os("EMBER_NODE_ROOTS") {
            Some(v) if !v.is_empty() => std::env::split_paths(&v).filter(|p| !p.as_os_str().is_empty()).collect(),
            _ => vec![PathBuf::from(
                std::env::var_os("HOME").ok_or_else(|| anyhow::anyhow!("neither EMBER_NODE_ROOTS nor HOME is set"))?,
            )],
        };
        let state_dir = default_state_dir();
        let pty_keeper = match std::env::var("EMBER_NODE_KEEP_PTY").as_deref() {
            Ok("0") | Ok("false") | Ok("no") => None,
            _ => std::env::current_exe().ok(),
        };
        let egress = crate::egress::EgressPolicy::from_env()?;
        Ok(Self { token, roots, state_dir, pty_keeper, egress })
    }
}

/// `EMBER_NODE_STATE_DIR`, else `$HOME/.ember/node`.
pub fn default_state_dir() -> Option<PathBuf> {
    match std::env::var_os("EMBER_NODE_STATE_DIR") {
        Some(v) if !v.is_empty() => Some(PathBuf::from(v)),
        _ => std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".ember").join("node")),
    }
}

/// `EMBER_NODE_LISTEN` (default `127.0.0.1:8741`).
pub fn listen_addr() -> anyhow::Result<SocketAddr> {
    Ok(std::env::var("EMBER_NODE_LISTEN").unwrap_or_else(|_| DEFAULT_LISTEN.into()).parse()?)
}

/// `<state dir>/local.json`, written by the daemon at start (mode 0600): how local programs of
/// the same user reach it. The token in it grants nothing the user does not already have on
/// this computer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocalEndpoint {
    /// `http://127.0.0.1:<port>`.
    pub url: String,
    pub token: String,
    pub pid: u32,
}

pub const LOCAL_ENDPOINT_FILE: &str = "local.json";

impl LocalEndpoint {
    /// The URL local programs use for a listener bound to `addr` (a wildcard bind is reached on
    /// loopback).
    pub fn for_addr(addr: SocketAddr, token: &str) -> Self {
        let host = if addr.ip().is_unspecified() {
            if addr.is_ipv6() { "[::1]".to_string() } else { "127.0.0.1".to_string() }
        } else if addr.is_ipv6() {
            format!("[{}]", addr.ip())
        } else {
            addr.ip().to_string()
        };
        Self { url: format!("http://{host}:{}", addr.port()), token: token.into(), pid: std::process::id() }
    }

    pub fn write(&self, state_dir: &Path) -> std::io::Result<()> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::create_dir_all(state_dir)?;
        let path = state_dir.join(LOCAL_ENDPOINT_FILE);
        let tmp = state_dir.join(format!("{LOCAL_ENDPOINT_FILE}.tmp"));
        let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
        f.write_all(&serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?)?;
        drop(f);
        std::fs::rename(tmp, path)
    }

    pub fn read(state_dir: &Path) -> std::io::Result<Self> {
        let data = std::fs::read(state_dir.join(LOCAL_ENDPOINT_FILE))?;
        serde_json::from_slice(&data).map_err(std::io::Error::other)
    }

    /// How a local client finds the daemon: `EMBER_NODE_URL` + `EMBER_NODE_TOKEN` if both are
    /// set, else the endpoint file in [`default_state_dir`].
    pub fn discover() -> anyhow::Result<Self> {
        let url = std::env::var("EMBER_NODE_URL").ok().filter(|s| !s.is_empty());
        let token = std::env::var("EMBER_NODE_TOKEN").ok().filter(|s| !s.is_empty());
        if let (Some(url), Some(token)) = (url.clone(), token.clone()) {
            return Ok(Self { url, token, pid: 0 });
        }
        let dir = default_state_dir().ok_or_else(|| anyhow::anyhow!("no state dir (set HOME or EMBER_NODE_STATE_DIR)"))?;
        let mut ep = Self::read(&dir).map_err(|e| {
            anyhow::anyhow!("ember node not found ({}: {e}); is it running?", dir.join(LOCAL_ENDPOINT_FILE).display())
        })?;
        if let Some(url) = url {
            ep.url = url;
        }
        if let Some(token) = token {
            ep.token = token;
        }
        Ok(ep)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_endpoint_round_trip_and_wildcard_bind() {
        let d = tempfile::tempdir().unwrap();
        let ep = LocalEndpoint::for_addr("0.0.0.0:8741".parse().unwrap(), "t");
        assert_eq!(ep.url, "http://127.0.0.1:8741");
        ep.write(d.path()).unwrap();
        assert_eq!(LocalEndpoint::read(d.path()).unwrap(), ep);
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(d.path().join(LOCAL_ENDPOINT_FILE)).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
