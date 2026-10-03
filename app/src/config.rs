//! Where the app connects and where it keeps its files.
//!
//! - `EMBER_SERVER_URL`: the main server, default `http://127.0.0.1:8740`.
//! - Files live under the OS data directory (`dirs::data_dir()`), in `ember/`:
//!   `client-cache.json` (the client's cold-start snapshot, FR-L1), `prefs.json` (last-used
//!   choices, pins, renames) and `exports/` (FR-L9 exports).
//! - `EMBER_DATA_DIR` overrides that directory (tests, portable installs).

use std::path::{Path, PathBuf};
use std::time::Duration;

use ember_client::ClientConfig;

pub const DEFAULT_SERVER_URL: &str = "http://127.0.0.1:8740";

#[derive(Debug, Clone, PartialEq)]
pub struct AppConfig {
    pub server_url: String,
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
        AppConfig { server_url, data_dir }
    }

    pub fn cache_path(&self) -> Option<PathBuf> {
        self.data_dir.as_deref().map(|d| d.join("client-cache.json"))
    }

    pub fn prefs_path(&self) -> Option<PathBuf> {
        self.data_dir.as_deref().map(|d| d.join("prefs.json"))
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

    #[test]
    fn file_stems_are_safe() {
        assert_eq!(safe_file_stem("Fix the: build / now!"), "Fix-the-build-now");
        assert_eq!(safe_file_stem("///"), "session");
        assert_eq!(safe_file_stem("한글 제목"), "한글-제목");
    }
}
