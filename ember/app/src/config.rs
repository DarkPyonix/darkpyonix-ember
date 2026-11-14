//! Where the app connects and where it keeps its files.
//!
//! - `EMBER_SERVER_URL`: the main server, default `http://127.0.0.1:8740`.
//! - Or the main server over the transport (SPEC `FR-N9`): `EMBER_SERVER_PEER` for one run, or
//!   the address saved in 설정 (`Prefs::server_peer`); see [`server_choice`] for the order.
//! - Files live under the OS data directory (`dirs::data_dir()`), in `ember/`:
//!   `client-cache.json` (the client's cold-start snapshot, FR-L1), `prefs.json` (per-device
//!   choices: last-used combination, IDE target), `exports/` (FR-L9 exports) and
//!   `editor/User/` (the editor's `settings.json` and `keybindings.json`, FR-L31.17).
//! - `EMBER_DATA_DIR` overrides that directory (tests, portable installs).

use std::path::{Path, PathBuf};
use std::time::Duration;

use ember_client::ClientConfig;

use crate::device_link::PeerSetting;

pub const DEFAULT_SERVER_URL: &str = "http://127.0.0.1:8740";

#[derive(Debug, Clone, PartialEq)]
pub struct AppConfig {
    /// The HTTP base URL, or `peer:<id>` when [`AppConfig::server_peer`] is set.
    pub server_url: String,
    /// The main server over the transport (FR-N9); `None` is plain HTTP to `server_url`.
    pub server_peer: Option<PeerSetting>,
    /// `None` when the OS reports no data directory; the app then runs without cache or prefs.
    pub data_dir: Option<PathBuf>,
}

impl AppConfig {
    pub fn from_env() -> AppConfig {
        Self::from_values(
            std::env::var("EMBER_SERVER_URL").ok(),
            std::env::var_os("EMBER_DATA_DIR").map(PathBuf::from),
            dirs::data_dir(),
        )
    }

    /// Pure resolution, for tests.
    pub fn from_values(
        server_url: Option<String>,
        data_dir_override: Option<PathBuf>,
        os_data_dir: Option<PathBuf>,
    ) -> AppConfig {
        let server_url = server_url
            .map(|s| s.trim().trim_end_matches('/').to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_SERVER_URL.to_string());
        let data_dir = data_dir_override
            .filter(|p| !p.as_os_str().is_empty())
            .or_else(|| os_data_dir.map(|d| d.join("ember")));
        AppConfig { server_url, server_peer: None, data_dir }
    }

    /// The same configuration reaching the server over the transport when `peer` is set
    /// (FR-N9). `server_url` becomes `peer:<id>`, so nothing takes the local default for it.
    pub fn with_peer(mut self, peer: Option<PeerSetting>) -> AppConfig {
        if let Some(p) = &peer {
            self.server_url = p.base_url();
        }
        self.server_peer = peer;
        self
    }

    pub fn cache_path(&self) -> Option<PathBuf> {
        self.data_dir.as_deref().map(|d| d.join("client-cache.json"))
    }

    pub fn prefs_path(&self) -> Option<PathBuf> {
        self.data_dir.as_deref().map(|d| d.join("prefs.json"))
    }

    /// The folder of the editor's user settings on this machine (FR-L31.17): `settings.json` and
    /// `keybindings.json`, as Code-OSS keeps them in its `User` folder.
    pub fn editor_user_dir(&self) -> Option<PathBuf> {
        self.data_dir.as_deref().map(|d| d.join("editor").join("User"))
    }

    pub fn export_dir(&self) -> PathBuf {
        match &self.data_dir {
            Some(d) => d.join("exports"),
            None => std::env::temp_dir().join("ember-exports"),
        }
    }

    pub fn client_config(&self) -> ClientConfig {
        let mut c = ClientConfig::new(self.server_url.clone());
        c.cache_path = self.cache_path();
        // Defaults otherwise; a desktop app has no reason to differ from the engine's.
        c.lease_interval = Duration::from_secs(30);
        c
    }
}

/// Which reach the app uses (FR-N9), in this order:
///
/// 1. `EMBER_SERVER_PEER` (`env_peer`): the server over the transport for this run. A value that
///    does not parse is an error, as a malformed `EMBER_SERVER_URL` is.
/// 2. `EMBER_SERVER_URL` (`env_url_set`): plain HTTP, as before.
/// 3. The address saved in 설정 (`saved_peer`, `saved_relay`). One that does not parse is
///    logged and ignored, because prefs never block start-up.
/// 4. Nothing: plain HTTP to the default, as before.
///
/// The relay: `EMBER_RELAY_URL` when set (`env_relay_set`; `TransportConfig::from_env` reads
/// it), otherwise the saved relay, or the relay in a device line (`<address> relay <URL>`,
/// #863) when no relay is saved. `Ok(None)` is plain HTTP.
pub fn server_choice(
    env_peer: Option<&str>,
    env_url_set: bool,
    env_relay_set: bool,
    saved_peer: Option<&str>,
    saved_relay: Option<&str>,
) -> Result<Option<PeerSetting>, String> {
    let set = |v: Option<&str>| v.map(str::trim).filter(|v| !v.is_empty()).map(str::to_string);
    // `EMBER_RELAY_URL` beats any relay in the setting, the one in a device line too.
    let env_relay = |mut s: PeerSetting| {
        if env_relay_set {
            s.relay = None;
        }
        s
    };
    if let Some(peer) = set(env_peer) {
        return PeerSetting::parse(&peer, None)
            .map(|s| Some(env_relay(s)))
            .map_err(|e| format!("{}: {e}", crate::device_link::SERVER_PEER_ENV));
    }
    if env_url_set {
        return Ok(None);
    }
    let Some(peer) = set(saved_peer) else { return Ok(None) };
    let relay = if env_relay_set { None } else { set(saved_relay) };
    match PeerSetting::parse(&peer, relay.as_deref()) {
        Ok(s) => Ok(Some(env_relay(s))),
        Err(e) => {
            tracing::warn!("ignoring the saved server address: {e}");
            Ok(None)
        }
    }
}

/// A file name derived from free text (session titles), safe on every OS.
pub fn safe_file_stem(s: &str) -> String {
    let mut out: String = s
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect();
    while out.contains("--") {
        out = out.replace("--", "-");
    }
    let out = out.trim_matches('-');
    let out: String = out.chars().take(48).collect();
    if out.is_empty() { "session".into() } else { out }
}

pub fn ensure_dir(p: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_url_defaults_and_trims() {
        let c = AppConfig::from_values(None, None, None);
        assert_eq!(c.server_url, DEFAULT_SERVER_URL);
        assert_eq!(c.data_dir, None);
        assert_eq!(c.cache_path(), None);
        let c = AppConfig::from_values(Some(" http://mini.local:8740/ ".into()), None, None);
        assert_eq!(c.server_url, "http://mini.local:8740");
        let c = AppConfig::from_values(Some("".into()), None, None);
        assert_eq!(c.server_url, DEFAULT_SERVER_URL);
    }

    #[test]
    fn data_dir_is_under_os_data_dir_unless_overridden() {
        let c = AppConfig::from_values(None, None, Some(PathBuf::from("/data")));
        assert_eq!(c.cache_path(), Some(PathBuf::from("/data/ember/client-cache.json")));
        assert_eq!(c.prefs_path(), Some(PathBuf::from("/data/ember/prefs.json")));
        let c = AppConfig::from_values(None, Some(PathBuf::from("/x")), Some(PathBuf::from("/data")));
        assert_eq!(c.cache_path(), Some(PathBuf::from("/x/client-cache.json")));
        assert_eq!(c.client_config().cache_path, Some(PathBuf::from("/x/client-cache.json")));
    }

    fn peer() -> String {
        ember_transport::SecretKey::from_bytes([3; 32]).peer_id().to_string()
    }

    #[test]
    fn fr_n9_nothing_set_keeps_plain_http_to_the_default() {
        assert_eq!(server_choice(None, false, false, None, None), Ok(None));
        assert_eq!(server_choice(Some(" "), false, false, Some(""), Some("")), Ok(None));
        let c = AppConfig::from_values(None, None, None).with_peer(None);
        assert_eq!(c.server_url, DEFAULT_SERVER_URL);
        assert_eq!(c.server_peer, None);
    }

    #[test]
    fn fr_n9_server_choice_precedence() {
        let (env, saved) = (peer(), ember_transport::SecretKey::from_bytes([4; 32]).peer_id().to_string());
        let relay = format!("letify://{saved}@relay.example:7443");
        // 1. EMBER_SERVER_PEER wins over everything; the relay then comes from the environment.
        let s = server_choice(Some(&env), true, false, Some(&saved), Some(&relay)).unwrap().unwrap();
        assert_eq!(s.addr.peer.to_string(), env);
        assert_eq!(s.relay, None);
        // 2. EMBER_SERVER_URL beats the saved address.
        assert_eq!(server_choice(None, true, false, Some(&saved), Some(&relay)), Ok(None));
        // 3. The saved address with its relay, unless EMBER_RELAY_URL is set.
        let s = server_choice(None, false, false, Some(&saved), Some(&relay)).unwrap().unwrap();
        assert_eq!(s.addr.peer.to_string(), saved);
        assert!(s.relay.is_some());
        let s = server_choice(None, false, true, Some(&saved), Some(&relay)).unwrap().unwrap();
        assert_eq!(s.relay, None);
        // A bad environment value stops the start; a bad saved one is ignored.
        assert!(server_choice(Some("nope"), false, false, None, None).unwrap_err().contains("EMBER_SERVER_PEER"));
        assert_eq!(server_choice(None, false, false, Some("nope"), None), Ok(None));
        assert_eq!(server_choice(None, false, false, Some(&saved), Some("https://x")), Ok(None));
    }

    #[test]
    fn fr_n9_a_device_line_brings_its_relay_unless_ember_relay_url_is_set() {
        let id = peer();
        let url = format!("letify://{id}@203.0.113.4:7443");
        let line = format!("ember server device address: {id} relay {url}");
        let want = Some(ember_transport::RelayConfig::Custom(vec![url]));
        let s = server_choice(Some(&line), false, false, None, None).unwrap().unwrap();
        assert_eq!((s.addr.peer.to_string(), s.relay), (id.clone(), want.clone()));
        assert_eq!(server_choice(Some(&line), false, true, None, None).unwrap().unwrap().relay, None);
        let s = server_choice(None, false, false, Some(&line), None).unwrap().unwrap();
        assert_eq!(s.relay, want);
        assert_eq!(server_choice(None, false, true, Some(&line), None).unwrap().unwrap().relay, None);
    }

    #[test]
    fn fr_n9_with_peer_never_leaves_the_local_default_in_server_url() {
        let s = server_choice(Some(&peer()), false, false, None, None).unwrap();
        let c = AppConfig::from_values(None, None, None).with_peer(s.clone());
        assert_eq!(c.server_url, format!("peer:{}", peer()));
        assert_eq!(c.server_peer, s);
    }

    #[test]
    fn file_stems_are_safe() {
        assert_eq!(safe_file_stem("Fix the: build / now!"), "Fix-the-build-now");
        assert_eq!(safe_file_stem("///"), "session");
        assert_eq!(safe_file_stem("한글 제목"), "한글-제목");
    }
}
