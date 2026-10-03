//! Remote browser and agent browser use (SPEC §R, INTENT D6).
//!
//! One headless Chromium per project runs on the ember server. Its profile directory — cookies,
//! storage, logins, history — lives under `<data dir>/browser/<project>/profile` (FR-R2) and can be
//! wiped per project (FR-R4). Network egress goes through a proxy chosen per browser (FR-R1; today
//! a configured `socks5://…` URL, later the ember node's SOCKS5 exit over the transport). Switching
//! egress restarts Chrome on the same profile, so logins survive (FR-R2).
//!
//! Three ways in, all under `/api/v1/browsers/{project}` (see [`api`] and
//! `docs/design/REMOTE-BROWSER.md`):
//! - **view** — a WebSocket carrying JPEG screencast frames out and pointer/keyboard input in
//!   (no webview in the client, E1);
//! - **cdp** — a DevTools-protocol WebSocket relay for agents' browser MCP servers (FR-R3), which
//!   also drives the "agent is active" flag and pauses the agent while the user has taken over;
//! - plain HTTP for open / egress / clear / input.

pub mod agent;
pub mod api;
pub mod cdp;
pub mod chrome;
mod instance;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::bail;

pub use instance::{BrowserInfo, BrowserInstance, InputEvent, ScreencastOptions, ViewState};

/// Version of the view stream protocol (frames, state, input). Bump on incompatible change.
pub const STREAM_VERSION: u32 = 1;

/// Server-wide browser settings.
#[derive(Debug, Clone)]
pub struct BrowserConfig {
    /// Root for per-project profiles: `<root>/<project>/profile`.
    pub root: PathBuf,
    /// The browser binary; `None` means none was found (the API reports it).
    pub chrome: Option<PathBuf>,
    pub headless: bool,
    /// Viewport (window) size in CSS pixels.
    pub window: (u32, u32),
    pub screencast: ScreencastOptions,
}

impl BrowserConfig {
    /// Defaults under `data_dir`, with the browser from [`chrome::find_chrome`].
    ///
    /// - `EMBER_CHROME_BIN`: browser binary
    /// - `EMBER_BROWSER_HEADFUL=1`: show a window (debugging)
    pub fn from_env(data_dir: &std::path::Path) -> Self {
        BrowserConfig {
            root: data_dir.join("browser"),
            chrome: chrome::find_chrome(),
            headless: std::env::var("EMBER_BROWSER_HEADFUL").as_deref() != Ok("1"),
            window: (1280, 800),
            screencast: ScreencastOptions::default(),
        }
    }
}

/// All browsers on this server, one per project.
pub struct BrowserManager {
    cfg: BrowserConfig,
    browsers: tokio::sync::Mutex<HashMap<String, Arc<BrowserInstance>>>,
}

/// A project name usable as a directory name and URL segment.
pub fn check_project(project: &str) -> anyhow::Result<()> {
    let ok = !project.is_empty()
        && project.len() <= 128
        && project != "."
        && project != ".."
        && project.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if !ok {
        bail!("invalid project name {project:?} (letters, digits, '-', '_', '.')");
    }
    Ok(())
}

impl BrowserManager {
    pub fn new(cfg: BrowserConfig) -> Arc<Self> {
        Arc::new(BrowserManager { cfg, browsers: Default::default() })
    }

    pub fn config(&self) -> &BrowserConfig {
        &self.cfg
    }

    /// The instance for `project`, created (not started) if new.
    pub async fn instance(&self, project: &str) -> anyhow::Result<Arc<BrowserInstance>> {
        check_project(project)?;
        let mut map = self.browsers.lock().await;
        if let Some(b) = map.get(project) {
            return Ok(b.clone());
        }
        let b = BrowserInstance::new(project, self.cfg.root.join(project).join("profile"), &self.cfg);
        map.insert(project.to_string(), b.clone());
        Ok(b)
    }

    /// An existing instance, without creating one.
    pub async fn get(&self, project: &str) -> Option<Arc<BrowserInstance>> {
        self.browsers.lock().await.get(project).cloned()
    }

    /// Start the project's browser (or keep it running), with `egress` if given.
    pub async fn open(
        &self,
        project: &str,
        egress: Option<Option<String>>,
    ) -> anyhow::Result<Arc<BrowserInstance>> {
        let b = self.instance(project).await?;
        match egress {
            Some(e) => b.set_egress(e).await?,
            None => b.ensure_running().await?,
        }
        Ok(b)
    }

    pub async fn list(&self) -> Vec<BrowserInfo> {
        let all: Vec<_> = self.browsers.lock().await.values().cloned().collect();
        let mut out = Vec::new();
        for b in all {
            out.push(b.info().await);
        }
        out.sort_by(|a, b| a.project.cmp(&b.project));
        out
    }

    /// Delete the project's profile — cookies, storage, history (FR-R4). A running browser is
    /// restarted on the empty profile with the same egress.
    pub async fn clear_data(&self, project: &str) -> anyhow::Result<()> {
        let b = self.instance(project).await?;
        b.clear_data().await
    }

    /// Stop every browser gracefully (server shutdown), so cookies are flushed to disk.
    pub async fn shutdown(&self) {
        let all: Vec<_> = self.browsers.lock().await.values().cloned().collect();
        for b in all {
            b.stop().await;
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn project_names() {
        assert!(super::check_project("acme-1.web_app").is_ok());
        for bad in ["", ".", "..", "a/b", "a b", "../x"] {
            assert!(super::check_project(bad).is_err(), "{bad}");
        }
    }
}
