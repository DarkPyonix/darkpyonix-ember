//! The DarkPyonix manager's editor server, as `ember-node` and `ember-server` reach it (SPEC
//! FR-W4a, FR-W4b, FR-W4c, `INTENT.md` D55, #1064).
//!
//! The manager (darkpyonix-core D26, D27) installs the editor server, runs it as a child it
//! supervises on a 0700 Unix socket with no connection token, and serves it at its own endpoint
//! under `/ide/<runtime>/`. Ember starts no OSE: it asks the manager for a launch ticket and joins
//! the relay's bytes to the manager's endpoint, replacing nothing.
//!
//! **Where the manager is.** [`ManagerSource::discover`] reads `<state dir>/manager.json`
//! (`{"url": ..., "token": ...}`, mode 0600, written by the node's first-run manager install),
//! else the environment pair `DARKPYONIX_MANAGER_URL` and `DARKPYONIX_TOKEN` (both or neither).
//! Without either, every launch answers [`ManagerError::NotInstalled`]. The file is read on every
//! launch, so a manager installed while the node runs is found without a restart.
//!
//! **The manager's routes** (under `/api`, Bearer with the manager's master token; a share token
//! gets 403):
//! - `POST {url}/api/editor/{runtime}/launch` answers 200 `{ticket, path: "/ide/<runtime>/", commit,
//!   version}`, starting the server if it is not running; 409 `{code, error}` when that runtime is
//!   not installed.
//! - `POST {url}/api/editor/{runtime}/extensions {id, version?}` installs an extension (409 when
//!   the runtime does not take one, as for `vsc`).
//!
//! **The relay.** [`EditorRelays`] keeps the launches this process handed out, by an opaque id,
//! with the relay limits of FR-W4b (16 per launch, 64 in all, 120 s idle). A relay opens a TCP
//! connection to the manager's `host:port` (TLS when the url is `https`) and the caller joins the
//! client's bytes to it. The client authenticates to the manager with the ticket (`?dpx-ticket=`
//! on the first page, or `Cookie: dpx-editor=<ticket>`); the manager token never leaves this
//! process.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// The file in the node's state dir that names the manager (`{url, token}`, mode 0600).
pub const MANAGER_FILE: &str = "manager.json";
/// The manager's url when there is no [`MANAGER_FILE`].
pub const ENV_URL: &str = "DARKPYONIX_MANAGER_URL";
/// The manager's token when there is no [`MANAGER_FILE`].
pub const ENV_TOKEN: &str = "DARKPYONIX_TOKEN";
/// The runtime both Ember targets use (the manager also has `vsc`, which Ember does not choose).
pub const OSE_RUNTIME: &str = "ose";
/// Longest wait for a launch: the manager starts the server and waits for its socket.
pub const LAUNCH_TIMEOUT: Duration = Duration::from_secs(20);
/// Longest wait for an extension install: the manager's own limit is 120 s.
pub const INSTALL_TIMEOUT: Duration = Duration::from_secs(150);
/// Longest wait for the relay's connection to the manager.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
/// Default idle timeout of one relayed connection (`FR-W4b`).
pub const RELAY_IDLE_TIMEOUT: Duration = Duration::from_secs(120);
/// Maximum concurrent relays per launch (`FR-W4b`).
pub const MAX_RELAYS_PER_LAUNCH: usize = 16;
/// Maximum concurrent relays of one process (`FR-W4b`).
pub const MAX_RELAYS_IN_ALL: usize = 64;
/// A launch with no relay for this long is forgotten: the manager's ticket lives 12 h after its
/// last use, so the launch is no use after that either.
pub const LAUNCH_RETENTION: Duration = Duration::from_secs(12 * 60 * 60);
/// The most launches kept; the oldest idle one goes first when a new one would pass it.
pub const MAX_LAUNCHES: usize = 256;

/// The message of a launch with no manager to ask.
pub const NOT_INSTALLED: &str = "the DarkPyonix manager is not installed on this computer: ember-node \
    installs it at its first run (or set DARKPYONIX_MANAGER_URL and DARKPYONIX_TOKEN)";

/// The manager's endpoint and token. The token is never printed.
#[derive(Clone, PartialEq, Eq, Deserialize)]
pub struct ManagerEndpoint {
    pub url: String,
    pub token: String,
}

impl std::fmt::Debug for ManagerEndpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ManagerEndpoint").field("url", &self.url).field("token", &"[REDACTED]").finish()
    }
}

/// Why the manager gave no answer.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ManagerError {
    /// No manager is known (no file, no environment), or what names it is unusable.
    #[error("{0}")]
    NotInstalled(String),
    /// The manager did not answer, or answered something that is not its contract.
    #[error("{0}")]
    Unreachable(String),
    /// The manager answered a status other than 2xx, with its `error` text.
    #[error("{error}")]
    Refused { status: u16, error: String },
}

/// Where a process finds its manager.
#[derive(Clone, Debug)]
pub struct ManagerSource(Source);

#[derive(Clone, Debug)]
enum Source {
    /// `<state dir>/manager.json`, else the environment.
    Discover(Option<PathBuf>),
    /// A fixed endpoint, or none (tests, and a process that has no manager).
    Fixed(Option<ManagerEndpoint>),
}

impl Default for ManagerSource {
    fn default() -> Self {
        Self::none()
    }
}

impl ManagerSource {
    /// The manager named by `<state_dir>/manager.json`, else by the environment.
    pub fn discover(state_dir: Option<PathBuf>) -> Self {
        Self(Source::Discover(state_dir))
    }

    /// A manager at `url` with `token` (tests, and fake managers).
    pub fn fixed(url: impl Into<String>, token: impl Into<String>) -> Self {
        Self(Source::Fixed(Some(ManagerEndpoint { url: url.into(), token: token.into() })))
    }

    /// No manager: every launch answers [`ManagerError::NotInstalled`].
    pub fn none() -> Self {
        Self(Source::Fixed(None))
    }

    /// The endpoint now, checked.
    pub fn resolve(&self) -> Result<ManagerEndpoint, ManagerError> {
        match &self.0 {
            Source::Fixed(Some(e)) => validate(e.clone()),
            Source::Fixed(None) => Err(ManagerError::NotInstalled(NOT_INSTALLED.into())),
            Source::Discover(dir) => {
                let file = dir.as_ref().map(|d| d.join(MANAGER_FILE));
                resolve_from(file.as_deref(), env(ENV_URL), env(ENV_TOKEN))
            }
        }
    }

    /// A client of the endpoint now.
    pub fn client(&self) -> Result<ManagerClient, ManagerError> {
        ManagerClient::new(self.resolve()?)
    }
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

/// The endpoint from `file` when it exists, else from the environment values `url` and `token`
/// (both or neither). Pure apart from reading `file`, for tests.
pub fn resolve_from(file: Option<&Path>, url: Option<String>, token: Option<String>) -> Result<ManagerEndpoint, ManagerError> {
    if let Some(path) = file {
        if let Some(endpoint) = read_manager_file(path).map_err(ManagerError::NotInstalled)? {
            return validate(endpoint);
        }
    }
    let url = url.filter(|v| !v.trim().is_empty());
    let token = token.filter(|v| !v.trim().is_empty());
    match (url, token) {
        (Some(url), Some(token)) => validate(ManagerEndpoint { url, token }),
        (None, None) => Err(ManagerError::NotInstalled(NOT_INSTALLED.into())),
        _ => Err(ManagerError::NotInstalled(format!("set both {ENV_URL} and {ENV_TOKEN}, or neither"))),
    }
}

/// `Ok(None)` when the file does not exist. The file holds the manager token, so it must be a
/// regular file (not a symlink) that only its owner can read.
fn read_manager_file(path: &Path) -> Result<Option<ManagerEndpoint>, String> {
    let md = match std::fs::symlink_metadata(path) {
        Ok(md) => md,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    };
    if !md.file_type().is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if md.permissions().mode() & 0o077 != 0 {
            return Err(format!("{} must be mode 0600: it holds the manager token", path.display()));
        }
    }
    let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    serde_json::from_str(&text).map(Some).map_err(|e| format!("{} is not {{url, token}}: {e}", path.display()))
}

/// An `http` or `https` url with a host, and a token; the url without a trailing `/`.
fn validate(e: ManagerEndpoint) -> Result<ManagerEndpoint, ManagerError> {
    let url = e.url.trim().trim_end_matches('/').to_string();
    let parsed = reqwest::Url::parse(&url).map_err(|err| ManagerError::NotInstalled(format!("invalid manager url {url:?}: {err}")))?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(ManagerError::NotInstalled(format!("invalid manager url {url:?}: it must be http(s)://host[:port]")));
    }
    if e.token.trim().is_empty() {
        return Err(ManagerError::NotInstalled("the manager token is empty".into()));
    }
    Ok(ManagerEndpoint { url, token: e.token })
}

/// The manager's answer to `POST /api/editor/{runtime}/launch`. The ticket is never printed.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditorLaunch {
    /// The launch ticket: the client's credential at `path` (`?dpx-ticket=` or the `dpx-editor`
    /// cookie), valid for this runtime only.
    pub ticket: String,
    /// `/ide/<runtime>/`, where the manager serves the editor server.
    pub path: String,
    /// The Code-OSS commit of the editor server (SPEC FR-E10).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

impl std::fmt::Debug for EditorLaunch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EditorLaunch")
            .field("ticket", &"[REDACTED]")
            .field("path", &self.path)
            .field("commit", &self.commit)
            .field("version", &self.version)
            .finish()
    }
}

/// True for a runtime name the manager's routes take (`ose`, `vsc`): lowercase letters and digits.
fn valid_runtime(runtime: &str) -> bool {
    !runtime.is_empty() && runtime.len() <= 16 && runtime.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

/// A commit the editor can compare: hex, at most 64 characters, lowercased.
fn clean_commit(commit: Option<String>) -> Option<String> {
    commit
        .map(|c| c.trim().to_ascii_lowercase())
        .filter(|c| !c.is_empty() && c.len() <= 64 && c.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// The `error` of a manager's refusal body, else its text, capped.
fn reason_of(status: u16, body: &[u8]) -> String {
    let parsed = serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(String::from));
    let text = parsed.unwrap_or_else(|| String::from_utf8_lossy(body).trim().to_string());
    let text: String = text.chars().take(512).collect();
    if text.is_empty() { format!("the manager answered {status}") } else { text }
}

/// The byte stream to the manager: TCP, or TLS over TCP.
pub trait ManagerIo: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send> ManagerIo for T {}

/// One manager endpoint's client.
#[derive(Clone)]
pub struct ManagerClient {
    endpoint: ManagerEndpoint,
    http: reqwest::Client,
}

impl std::fmt::Debug for ManagerClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ManagerClient").field("endpoint", &self.endpoint).finish()
    }
}

impl ManagerClient {
    pub fn new(endpoint: ManagerEndpoint) -> Result<Self, ManagerError> {
        let endpoint = validate(endpoint)?;
        // No redirects: a manager answer is its own, and the token must not follow a redirect.
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
            .map_err(|e| ManagerError::Unreachable(format!("cannot build the manager client: {e}")))?;
        Ok(Self { endpoint, http })
    }

    pub fn endpoint(&self) -> &ManagerEndpoint {
        &self.endpoint
    }

    fn route(&self, path: &str) -> String {
        format!("{}{path}", self.endpoint.url)
    }

    /// `POST /api/editor/{runtime}/launch`: starts the runtime's server if needed and answers a new
    /// ticket. 409 (the runtime is not installed) and other refusals are [`ManagerError::Refused`].
    pub async fn launch(&self, runtime: &str) -> Result<EditorLaunch, ManagerError> {
        if !valid_runtime(runtime) {
            return Err(ManagerError::Refused { status: 400, error: format!("invalid editor runtime: {runtime:?}") });
        }
        let res = self
            .http
            .post(self.route(&format!("/api/editor/{runtime}/launch")))
            .bearer_auth(&self.endpoint.token)
            .timeout(LAUNCH_TIMEOUT)
            .send()
            .await
            .map_err(|e| ManagerError::Unreachable(format!("manager unreachable: {e}")))?;
        let status = res.status().as_u16();
        let body = res.bytes().await.map_err(|e| ManagerError::Unreachable(format!("manager unreachable: {e}")))?;
        if !(200..300).contains(&status) {
            return Err(ManagerError::Refused { status, error: reason_of(status, &body) });
        }
        let mut launch: EditorLaunch = serde_json::from_slice(&body)
            .map_err(|e| ManagerError::Unreachable(format!("the manager answered an invalid launch: {e}")))?;
        let path_ok = launch.path.starts_with("/ide/") && launch.path.ends_with('/') && !launch.path.contains("..");
        if launch.ticket.is_empty() || !path_ok {
            return Err(ManagerError::Unreachable("the manager answered a launch without a ticket or an /ide/ path".into()));
        }
        launch.commit = clean_commit(launch.commit.take());
        Ok(launch)
    }

    /// `POST /api/editor/{runtime}/extensions` with `body` as it came; the manager's status and body
    /// are answered unchanged.
    pub async fn install_extension(&self, runtime: &str, body: Vec<u8>) -> Result<(u16, Vec<u8>), ManagerError> {
        if !valid_runtime(runtime) {
            return Err(ManagerError::Refused { status: 400, error: format!("invalid editor runtime: {runtime:?}") });
        }
        let res = self
            .http
            .post(self.route(&format!("/api/editor/{runtime}/extensions")))
            .bearer_auth(&self.endpoint.token)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body)
            .timeout(INSTALL_TIMEOUT)
            .send()
            .await
            .map_err(|e| ManagerError::Unreachable(format!("manager unreachable: {e}")))?;
        let status = res.status().as_u16();
        let body = res.bytes().await.map_err(|e| ManagerError::Unreachable(format!("manager unreachable: {e}")))?;
        Ok((status, body.to_vec()))
    }

    /// `DELETE /api/editor/{runtime}/extensions/{extension}`; the manager's status and body are
    /// answered unchanged. `extension` is a checked `publisher.name`.
    pub async fn uninstall_extension(&self, runtime: &str, extension: &str) -> Result<(u16, Vec<u8>), ManagerError> {
        if !valid_runtime(runtime) {
            return Err(ManagerError::Refused { status: 400, error: format!("invalid editor runtime: {runtime:?}") });
        }
        let res = self
            .http
            .delete(self.route(&format!("/api/editor/{runtime}/extensions/{extension}")))
            .bearer_auth(&self.endpoint.token)
            .timeout(INSTALL_TIMEOUT)
            .send()
            .await
            .map_err(|e| ManagerError::Unreachable(format!("manager unreachable: {e}")))?;
        let status = res.status().as_u16();
        let body = res.bytes().await.map_err(|e| ManagerError::Unreachable(format!("manager unreachable: {e}")))?;
        Ok((status, body.to_vec()))
    }

    /// A byte stream to the manager's `host:port`: plain TCP for `http`, TLS for `https`.
    pub async fn connect(&self) -> std::io::Result<Box<dyn ManagerIo>> {
        connect_endpoint(&self.endpoint).await
    }
}

/// See [`ManagerClient::connect`].
pub async fn connect_endpoint(endpoint: &ManagerEndpoint) -> std::io::Result<Box<dyn ManagerIo>> {
    let url = reqwest::Url::parse(&endpoint.url).map_err(std::io::Error::other)?;
    let host = url.host_str().ok_or_else(|| std::io::Error::other("the manager url has no host"))?;
    // An IPv6 host is written in brackets in a url.
    let host = host.trim_start_matches('[').trim_end_matches(']').to_string();
    let port = url.port_or_known_default().ok_or_else(|| std::io::Error::other("the manager url has no port"))?;
    let tcp = tokio::net::TcpStream::connect((host.as_str(), port)).await?;
    let _ = tcp.set_nodelay(true);
    if url.scheme() != "https" {
        return Ok(Box::new(tcp));
    }
    use tokio_rustls::rustls;
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(std::io::Error::other)?
        .with_root_certificates(roots)
        .with_no_client_auth();
    let name = rustls::pki_types::ServerName::try_from(host.as_str()).map_err(std::io::Error::other)?.to_owned();
    let tls = tokio_rustls::TlsConnector::from(Arc::new(config)).connect(name, tcp).await?;
    Ok(Box::new(tls))
}

/// Why a relay could not be opened before the WebSocket upgrade.
#[derive(Debug, PartialEq, Eq)]
pub enum RelayLookupError {
    /// 404: no launch with this id.
    NotFound,
    /// 429: the per-launch or overall relay limit is reached.
    TooManyRelays,
}

/// RAII guard of one relay: gives its places back on drop.
struct RelayPermit {
    launch: Arc<AtomicUsize>,
    all: Arc<AtomicUsize>,
}

impl Drop for RelayPermit {
    fn drop(&mut self) {
        self.launch.fetch_sub(1, Ordering::SeqCst);
        self.all.fetch_sub(1, Ordering::SeqCst);
    }
}

/// What one relay connects to: the manager of its launch, with the idle timeout and the permit.
pub struct RelayTarget {
    pub endpoint: ManagerEndpoint,
    pub idle_timeout: Duration,
    _permit: RelayPermit,
}

impl std::fmt::Debug for RelayTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RelayTarget").field("endpoint", &self.endpoint).field("idle_timeout", &self.idle_timeout).finish()
    }
}

struct LaunchEntry {
    endpoint: ManagerEndpoint,
    active: Arc<AtomicUsize>,
    last_used: Instant,
}

/// The launches this process handed out and their relays (SPEC FR-W4b, FR-W4c).
pub struct EditorRelays {
    source: ManagerSource,
    launches: Mutex<HashMap<String, LaunchEntry>>,
    active: Arc<AtomicUsize>,
    per_launch: usize,
    in_all: usize,
    idle_timeout: Duration,
}

/// One launch: its id (32 hex, the `{launch_id}` of the relay routes) and the manager's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayLaunch {
    pub launch_id: String,
    pub launch: EditorLaunch,
}

impl EditorRelays {
    pub fn new(source: ManagerSource) -> Self {
        Self {
            source,
            launches: Mutex::new(HashMap::new()),
            active: Arc::new(AtomicUsize::new(0)),
            per_launch: MAX_RELAYS_PER_LAUNCH,
            in_all: MAX_RELAYS_IN_ALL,
            idle_timeout: RELAY_IDLE_TIMEOUT,
        }
    }

    /// Overrides the per-launch and overall relay limits (tests).
    pub fn with_relay_limits(mut self, per_launch: usize, in_all: usize) -> Self {
        self.per_launch = per_launch;
        self.in_all = in_all;
        self
    }

    /// Overrides the relay idle timeout (tests).
    pub fn with_relay_idle_timeout(mut self, timeout: Duration) -> Self {
        self.idle_timeout = timeout;
        self
    }

    pub fn source(&self) -> &ManagerSource {
        &self.source
    }

    /// Asks the manager for a launch of `runtime` and keeps it under a new id.
    pub async fn launch(&self, runtime: &str) -> Result<RelayLaunch, ManagerError> {
        let client = self.source.client()?;
        let launch = client.launch(runtime).await?;
        let launch_id = uuid::Uuid::new_v4().simple().to_string();
        let mut launches = self.launches.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        launches.retain(|_, l| l.active.load(Ordering::SeqCst) > 0 || now.duration_since(l.last_used) < LAUNCH_RETENTION);
        while launches.len() >= MAX_LAUNCHES {
            let oldest = launches
                .iter()
                .filter(|(_, l)| l.active.load(Ordering::SeqCst) == 0)
                .min_by_key(|(_, l)| l.last_used)
                .map(|(id, _)| id.clone());
            match oldest {
                Some(id) => {
                    launches.remove(&id);
                }
                None => break,
            }
        }
        launches.insert(
            launch_id.clone(),
            LaunchEntry { endpoint: client.endpoint().clone(), active: Arc::new(AtomicUsize::new(0)), last_used: now },
        );
        Ok(RelayLaunch { launch_id, launch })
    }

    /// Registers a launch on `endpoint` without asking the manager (relay tests). Answers its id.
    pub fn register_launch_for_test(&self, endpoint: ManagerEndpoint) -> String {
        let launch_id = uuid::Uuid::new_v4().simple().to_string();
        self.launches.lock().unwrap_or_else(|e| e.into_inner()).insert(
            launch_id.clone(),
            LaunchEntry { endpoint, active: Arc::new(AtomicUsize::new(0)), last_used: Instant::now() },
        );
        launch_id
    }

    /// The launch `launch_id` with a relay place taken.
    pub fn open_relay(&self, launch_id: &str) -> Result<RelayTarget, RelayLookupError> {
        let mut launches = self.launches.lock().unwrap_or_else(|e| e.into_inner());
        let Some(entry) = launches.get_mut(launch_id) else {
            return Err(RelayLookupError::NotFound);
        };
        if entry.active.load(Ordering::SeqCst) >= self.per_launch || self.active.load(Ordering::SeqCst) >= self.in_all {
            return Err(RelayLookupError::TooManyRelays);
        }
        entry.active.fetch_add(1, Ordering::SeqCst);
        self.active.fetch_add(1, Ordering::SeqCst);
        entry.last_used = Instant::now();
        Ok(RelayTarget {
            endpoint: entry.endpoint.clone(),
            idle_timeout: self.idle_timeout,
            _permit: RelayPermit { launch: entry.active.clone(), all: self.active.clone() },
        })
    }

    /// Forwards an extension install of `runtime` to the manager.
    pub async fn install_extension(&self, runtime: &str, body: Vec<u8>) -> Result<(u16, Vec<u8>), ManagerError> {
        self.source.client()?.install_extension(runtime, body).await
    }

    /// Forwards an extension uninstall of `runtime` to the manager.
    pub async fn uninstall_extension(&self, runtime: &str, extension: &str) -> Result<(u16, Vec<u8>), ManagerError> {
        self.source.client()?.uninstall_extension(runtime, extension).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_file(dir: &Path, text: &str, mode: u32) -> PathBuf {
        let path = dir.join(MANAGER_FILE);
        std::fs::write(&path, text).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        }
        #[cfg(not(unix))]
        let _ = mode;
        path
    }

    /// FR-W4b (D55, #1064): the file wins over the environment, the environment needs both
    /// values, and nothing at all is "not installed".
    #[test]
    fn fr_w4b_manager_is_the_file_else_both_env_values() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join(MANAGER_FILE);
        let env = |u: &str, t: &str| (Some(u.to_string()), Some(t.to_string()));

        let (u, t) = env("http://127.0.0.1:7/", "tok");
        let e = resolve_from(Some(&missing), u, t).unwrap();
        assert_eq!(e.url, "http://127.0.0.1:7");
        assert_eq!(e.token, "tok");
        assert!(matches!(resolve_from(Some(&missing), None, None), Err(ManagerError::NotInstalled(m)) if m == NOT_INSTALLED));
        assert!(matches!(resolve_from(None, Some("http://h".into()), None), Err(ManagerError::NotInstalled(m)) if m.contains("both")));
        assert!(matches!(resolve_from(None, None, Some("t".into())), Err(ManagerError::NotInstalled(_))));

        let file = write_file(dir.path(), r#"{"url":"http://127.0.0.1:9","token":"ft","pid":1}"#, 0o600);
        let (u, t) = env("http://127.0.0.1:7", "tok");
        let e = resolve_from(Some(&file), u, t).unwrap();
        assert_eq!((e.url.as_str(), e.token.as_str()), ("http://127.0.0.1:9", "ft"));
        assert!(!format!("{e:?}").contains("ft\""), "the token is printed: {e:?}");

        assert!(resolve_from(None, Some("ftp://h".into()), Some("t".into())).is_err());
        assert!(resolve_from(None, Some("not a url".into()), Some("t".into())).is_err());
    }

    /// FR-W4b (D55, #1064): a manager file that others can read is refused, because it holds the
    /// manager token.
    #[cfg(unix)]
    #[test]
    fn fr_w4b_manager_file_must_be_0600() {
        let dir = tempfile::tempdir().unwrap();
        let file = write_file(dir.path(), r#"{"url":"http://127.0.0.1:9","token":"ft"}"#, 0o644);
        match resolve_from(Some(&file), None, None) {
            Err(ManagerError::NotInstalled(m)) => assert!(m.contains("0600"), "{m}"),
            other => panic!("{other:?}"),
        }
    }

    /// FR-W4b (D55, #1064): the launch table counts relays per launch and in all, and an unknown
    /// id is not found.
    #[test]
    fn fr_w4b_relay_limits_are_per_launch_and_in_all() {
        let relays = EditorRelays::new(ManagerSource::none()).with_relay_limits(1, 2);
        let endpoint = ManagerEndpoint { url: "http://127.0.0.1:1".into(), token: "t".into() };
        let a = relays.register_launch_for_test(endpoint.clone());
        let b = relays.register_launch_for_test(endpoint.clone());
        let c = relays.register_launch_for_test(endpoint);
        assert_eq!(relays.open_relay("nope").err(), Some(RelayLookupError::NotFound));
        let held_a = relays.open_relay(&a).unwrap();
        assert_eq!(relays.open_relay(&a).err(), Some(RelayLookupError::TooManyRelays));
        let _held_b = relays.open_relay(&b).unwrap();
        assert_eq!(relays.open_relay(&c).err(), Some(RelayLookupError::TooManyRelays), "the overall limit");
        drop(held_a);
        assert!(relays.open_relay(&a).is_ok(), "a released place is free again");
    }

    #[test]
    fn fr_w4b_launch_debug_hides_the_ticket() {
        let l = EditorLaunch { ticket: "secret-ticket".into(), path: "/ide/ose/".into(), commit: None, version: None };
        assert!(!format!("{l:?}").contains("secret-ticket"));
        assert_eq!(clean_commit(Some(" ABC123 ".into())).as_deref(), Some("abc123"));
        assert_eq!(clean_commit(Some("not hex".into())), None);
    }
}
