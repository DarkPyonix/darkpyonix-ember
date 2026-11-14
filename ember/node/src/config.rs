//! Daemon configuration from the environment, and the local endpoint file through which
//! programs on the same computer (`ember-term`, the VS Code companion extension) find the daemon.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const DEFAULT_LISTEN: &str = "127.0.0.1:8741";

#[derive(Debug, Clone)]
pub struct NodeConfig {
    /// Bearer token every request except `/health` must carry.
    pub token: String,
    /// Allowed roots for file operations and command working directories.
    pub roots: Vec<PathBuf>,
    /// Paths inside the roots that the file API and command working directories refuse (see
    /// [`crate::policy::PathPolicy::with_denied`]). Default empty.
    pub denied: Vec<PathBuf>,
    /// Where persistent terminal metadata (and the local endpoint file) live. `None`: terminal
    /// sessions are kept in memory only and no keeper is started.
    pub state_dir: Option<PathBuf>,
    /// The `ember-node` binary, started as a PTY keeper per terminal session so sessions can
    /// survive a node restart. `None` disables keepers.
    pub pty_keeper: Option<PathBuf>,
    /// Which destinations `/egress` may reach (FR-R1); default: enabled, everything allowed.
    pub egress: crate::egress::EgressPolicy,
    /// Where received files go (`FR-F13`); `None`: `<state_dir>/received`.
    pub received_dir: Option<PathBuf>,
    /// Who may start a transfer without an answer on this node (`FR-F15`).
    pub receive: crate::transfers::ReceivePolicy,
}

impl NodeConfig {
    /// A configuration with no persistent state (tests, embedding).
    pub fn new(token: impl Into<String>, roots: Vec<PathBuf>) -> Self {
        Self {
            token: token.into(),
            roots,
            denied: Vec::new(),
            state_dir: None,
            pty_keeper: None,
            egress: Default::default(),
            received_dir: None,
            receive: Default::default(),
        }
    }

    /// Sets the denied paths (builder).
    pub fn with_denied(mut self, denied: Vec<PathBuf>) -> Self {
        self.denied = denied;
        self
    }

    /// - `EMBER_NODE_TOKEN` (non-empty): bearer token. When it is unset or empty, the `node_token`
    ///   of `<state dir>/enrol.json` is used (written by `ember-node enrol`); the variable wins
    ///   when both exist. See [`token_from`].
    /// - `EMBER_NODE_ROOTS`: colon-separated allowed roots (default `$HOME`).
    /// - `EMBER_NODE_DENY`: colon-separated paths the file API refuses (default none), see
    ///   [`parse_deny`].
    /// - `EMBER_NODE_STATE_DIR`: state directory (default `$HOME/.ember/node`).
    /// - `EMBER_NODE_KEEP_PTY`: `0` disables PTY keepers (terminal sessions then end with the
    ///   daemon).
    /// - `EMBER_NODE_RECEIVED_DIR`: received-files folder (default `<state dir>/received`,
    ///   `FR-F13`); `EMBER_NODE_RECEIVE`: `owner` (default), `ask` or `off` (`FR-F15`).
    /// - `EMBER_NODE_EGRESS`, `EMBER_NODE_EGRESS_DENY`: the browser egress policy
    ///   ([`crate::egress::EgressPolicy::from_env`]).
    pub fn from_env() -> anyhow::Result<Self> {
        let state_dir = default_state_dir();
        let (token, source) = token_with_source(std::env::var("EMBER_NODE_TOKEN").ok(), state_dir.as_deref())?;
        // The source is logged, never the value: a stale environment token silently beats a newer
        // enrol.json otherwise.
        match source {
            TokenSource::Env => {
                tracing::info!(source = "EMBER_NODE_TOKEN", "node token source");
                if let Some(dir) = state_dir.as_deref() {
                    if std::fs::symlink_metadata(dir.join(crate::enrol::ENROL_FILE)).is_ok() {
                        tracing::info!("EMBER_NODE_TOKEN is set and takes precedence over enrol.json");
                    }
                }
            }
            TokenSource::EnrolFile => tracing::info!(source = "enrol.json", "node token source"),
        }
        let roots: Vec<PathBuf> = match std::env::var_os("EMBER_NODE_ROOTS") {
            Some(v) if !v.is_empty() => std::env::split_paths(&v).filter(|p| !p.as_os_str().is_empty()).collect(),
            _ => vec![PathBuf::from(
                std::env::var_os("HOME")
                    .or_else(|| std::env::var_os("USERPROFILE"))
                    .ok_or_else(|| anyhow::anyhow!("neither EMBER_NODE_ROOTS nor HOME is set"))?,
            )],
        };
        let pty_keeper = match std::env::var("EMBER_NODE_KEEP_PTY").as_deref() {
            Ok("0") | Ok("false") | Ok("no") => None,
            _ => std::env::current_exe().ok(),
        };
        let egress = crate::egress::EgressPolicy::from_env()?;
        let denied = parse_deny(std::env::var_os("EMBER_NODE_DENY").as_deref());
        let received_dir = std::env::var_os("EMBER_NODE_RECEIVED_DIR").filter(|v| !v.is_empty()).map(PathBuf::from);
        let receive = crate::transfers::ReceivePolicy::parse(std::env::var("EMBER_NODE_RECEIVE").ok().as_deref())?;
        Ok(Self { token, roots, denied, state_dir, pty_keeper, egress, received_dir, receive })
    }
}

/// The bearer token: a non-empty `env_token` (`EMBER_NODE_TOKEN`) wins; otherwise the
/// `node_token` of `<state dir>/enrol.json`; otherwise an error. A malformed `enrol.json` is an
/// error that names the file and never quotes it.
pub fn token_from(env_token: Option<String>, state_dir: Option<&Path>) -> anyhow::Result<String> {
    token_with_source(env_token, state_dir).map(|(token, _)| token)
}

/// Where the bearer token came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenSource {
    /// `EMBER_NODE_TOKEN`.
    Env,
    /// `<state dir>/enrol.json`.
    EnrolFile,
}

/// [`token_from`] together with the source that supplied the token.
pub fn token_with_source(env_token: Option<String>, state_dir: Option<&Path>) -> anyhow::Result<(String, TokenSource)> {
    if let Some(t) = env_token.filter(|t| !t.is_empty()) {
        return Ok((t, TokenSource::Env));
    }
    if let Some(dir) = state_dir {
        if let Some(e) = crate::enrol::read_enrolment(dir)? {
            return Ok((e.node_token, TokenSource::EnrolFile));
        }
    }
    anyhow::bail!("EMBER_NODE_TOKEN must be set to a non-empty secret or this node enrolled (ember-node enrol)")
}

/// Splits an `EMBER_NODE_DENY` value like `PATH` (`:` on Unix, `;` on Windows), dropping empty
/// entries. `None` or empty gives no entries.
pub fn parse_deny(value: Option<&std::ffi::OsStr>) -> Vec<PathBuf> {
    match value {
        Some(v) if !v.is_empty() => std::env::split_paths(v).filter(|p| !p.as_os_str().is_empty()).collect(),
        _ => Vec::new(),
    }
}

/// `EMBER_NODE_STATE_DIR`, else `$HOME/.ember/node`.
pub fn default_state_dir() -> Option<PathBuf> {
    match std::env::var_os("EMBER_NODE_STATE_DIR") {
        Some(v) if !v.is_empty() => Some(PathBuf::from(v)),
        _ => std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(|h| PathBuf::from(h).join(".ember").join("node")),
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
        std::fs::create_dir_all(state_dir)?;
        let path = state_dir.join(LOCAL_ENDPOINT_FILE);
        let tmp = state_dir.join(format!("{LOCAL_ENDPOINT_FILE}.tmp"));
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
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
    fn parse_deny_splits_like_path_and_drops_empty_entries() {
        use std::ffi::OsStr;
        assert!(parse_deny(None).is_empty());
        assert!(parse_deny(Some(OsStr::new(""))).is_empty());
        assert_eq!(parse_deny(Some(OsStr::new("/a/.ember"))), vec![PathBuf::from("/a/.ember")]);
        assert_eq!(
            parse_deny(Some(OsStr::new("/a/.ember::/b/ose-data:"))),
            vec![PathBuf::from("/a/.ember"), PathBuf::from("/b/ose-data")]
        );
    }

    fn enrolled_dir(token: &str) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        let e = crate::enrol::Enrolment {
            server: "http://s".into(),
            node_token: token.into(),
            server_peer_id: None,
            name: "n".into(),
            computer_id: "c".into(),
            enrolled_ms: 1,
        };
        let lock = crate::enrol::lock_state_dir(d.path()).unwrap();
        crate::enrol::write_enrolment(&lock, &e, None, false).unwrap();
        drop(lock);
        d
    }

    #[test]
    fn fr_c2_node_config_env_token_wins_over_the_file() {
        let d = enrolled_dir("from-file");
        assert_eq!(token_from(Some("from-env".into()), Some(d.path())).unwrap(), "from-env");
        assert_eq!(token_from(Some("from-env".into()), None).unwrap(), "from-env");
    }

    #[test]
    fn fr_c2_node_config_falls_back_to_the_file_when_the_env_is_unset_or_empty() {
        let d = enrolled_dir("from-file");
        assert_eq!(token_from(None, Some(d.path())).unwrap(), "from-file");
        assert_eq!(token_from(Some(String::new()), Some(d.path())).unwrap(), "from-file");
    }

    #[test]
    fn fr_c2_node_config_without_env_or_file_keeps_the_error_text() {
        let d = tempfile::tempdir().unwrap();
        for (env, dir) in [(None, Some(d.path())), (Some(String::new()), Some(d.path())), (None, None)] {
            let err = token_from(env, dir).unwrap_err().to_string();
            assert!(err.contains("EMBER_NODE_TOKEN"), "{err}");
            assert!(err.contains("ember-node enrol"), "{err}");
        }
    }

    #[test]
    fn fr_c2_node_config_token_with_source_reports_env_file_or_error() {
        let d = enrolled_dir("from-file");
        assert_eq!(token_with_source(Some("from-env".into()), Some(d.path())).unwrap(), ("from-env".to_string(), TokenSource::Env));
        assert_eq!(token_with_source(None, Some(d.path())).unwrap(), ("from-file".to_string(), TokenSource::EnrolFile));
        assert_eq!(
            token_with_source(Some(String::new()), Some(d.path())).unwrap(),
            ("from-file".to_string(), TokenSource::EnrolFile)
        );
        let empty = tempfile::tempdir().unwrap();
        assert!(token_with_source(None, Some(empty.path())).is_err());
        assert!(token_with_source(None, None).is_err());
    }

    #[test]
    fn fr_c2_node_config_malformed_file_is_an_error_naming_the_file_not_its_contents() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join(crate::enrol::ENROL_FILE), "{\"node_token\": \"leaky-secret\" oops").unwrap();
        let err = format!("{:#}", token_from(None, Some(d.path())).unwrap_err());
        assert!(err.contains("enrol.json") && !err.contains("leaky-secret"), "{err}");
    }

    #[test]
    fn node_config_denied_defaults_to_empty_and_has_a_builder() {
        let c = NodeConfig::new("t", vec![PathBuf::from("/r")]);
        assert!(c.denied.is_empty());
        let c = c.with_denied(vec![PathBuf::from("/r/x")]);
        assert_eq!(c.denied, vec![PathBuf::from("/r/x")]);
    }

    #[test]
    fn local_endpoint_round_trip_and_wildcard_bind() {
        let d = tempfile::tempdir().unwrap();
        let ep = LocalEndpoint::for_addr("0.0.0.0:8741".parse().unwrap(), "t");
        assert_eq!(ep.url, "http://127.0.0.1:8741");
        ep.write(d.path()).unwrap();
        assert_eq!(LocalEndpoint::read(d.path()).unwrap(), ep);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(d.path().join(LOCAL_ENDPOINT_FILE)).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }
}
