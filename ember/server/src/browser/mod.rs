//! Remote browser and agent browser use (SPEC §R, INTENT D6).
//!
//! One headless Chromium per project runs on the ember server. Its profile directory (cookies,
//! storage, logins, history) lives under `<data dir>/browser/<project>/profile` (FR-R2) and can be
//! wiped per project (FR-R4). Network egress is chosen per project (FR-R1, [`egress::Egress`]):
//! direct, a configured proxy URL, or a registered computer, whose ember node's SOCKS5 exit
//! (`/v1/egress`) is reached through a loopback listener in this server
//! ([`crate::computers::egress`]). The choice is persisted ([`egress::EgressStore`]) and survives
//! restarts. Switching egress restarts Chrome on the same profile, so logins survive (FR-R2).
//!
//! Three ways in, all under `/api/v1/browsers/{project}` (see [`api`] and
//! `docs/design/REMOTE-BROWSER.md`):
//! - **view**: a WebSocket carrying JPEG screencast frames out and pointer/keyboard input in
//!   (no webview in the client, E1);
//! - **cdp**: a DevTools-protocol WebSocket relay for agents' browser MCP servers (FR-R3), which
//!   also drives the "agent is active" flag and pauses the agent while the user has taken over;
//! - plain HTTP for open / egress / clear / input.

pub mod agent;
pub mod api;
pub mod cdp;
pub mod chrome;
pub mod egress;
mod instance;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::bail;

pub use egress::{Egress, EgressResolver, EgressStore};
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
    /// Where each project's egress choice is kept; `None` = memory only.
    store: Option<egress::SharedEgressStore>,
    /// Resolves [`Egress::Computer`]; `None` = computers cannot be chosen.
    resolver: Option<egress::SharedEgressResolver>,
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
    /// A manager whose egress choices live in memory only, without computer egress.
    pub fn new(cfg: BrowserConfig) -> Arc<Self> {
        Self::with_egress(cfg, None, None)
    }

    /// A manager that persists egress choices in `store` and resolves computers with `resolver`.
    pub fn with_egress(
        cfg: BrowserConfig,
        store: Option<egress::SharedEgressStore>,
        resolver: Option<egress::SharedEgressResolver>,
    ) -> Arc<Self> {
        Arc::new(BrowserManager { cfg, browsers: Default::default(), store, resolver })
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
        // The persisted choice (FR-R1 across restarts); applied when the browser starts.
        let egress = match &self.store {
            Some(store) => store.load(project)?.unwrap_or_default(),
            None => Egress::Direct,
        };
        let b = BrowserInstance::new(
            project,
            self.cfg.root.join(project).join("profile"),
            &self.cfg,
            egress,
            self.resolver.clone(),
        );
        map.insert(project.to_string(), b.clone());
        Ok(b)
    }

    /// Set and persist the project's egress. A running browser restarts with it (same profile,
    /// FR-R2); a stopped one starts when `start` is true and otherwise uses it at its next start.
    /// The egress is validated (proxy URL form, computer known and its listener started) before
    /// anything is saved.
    pub async fn set_egress(
        &self,
        project: &str,
        egress: Egress,
        start: bool,
    ) -> anyhow::Result<Arc<BrowserInstance>> {
        let b = self.instance(project).await?;
        b.resolve_egress(&egress)?;
        if let Some(store) = &self.store {
            store.save(project, &egress)?;
        }
        if start || b.is_running().await {
            b.switch_egress(egress).await?;
        } else {
            b.set_egress_choice(egress);
        }
        Ok(b)
    }

    /// An existing instance, without creating one.
    pub async fn get(&self, project: &str) -> Option<Arc<BrowserInstance>> {
        self.browsers.lock().await.get(project).cloned()
    }

    /// Start the project's browser (or keep it running), switching to the proxy `egress` if given
    /// (`Some(None)` = direct). The choice is persisted like [`BrowserManager::set_egress`].
    pub async fn open(
        &self,
        project: &str,
        egress: Option<Option<String>>,
    ) -> anyhow::Result<Arc<BrowserInstance>> {
        self.open_with(project, egress.map(Egress::from_proxy)).await
    }

    /// [`BrowserManager::open`] with any [`Egress`].
    pub async fn open_with(
        &self,
        project: &str,
        egress: Option<Egress>,
    ) -> anyhow::Result<Arc<BrowserInstance>> {
        match egress {
            Some(e) => self.set_egress(project, e, true).await,
            None => {
                let b = self.instance(project).await?;
                b.ensure_running().await?;
                Ok(b)
            }
        }
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

    /// Delete the project's profile: cookies, storage, history (FR-R4). A running browser is
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
    use super::*;

    struct FakeComputers;

    impl EgressResolver for FakeComputers {
        fn proxy_for_computer(&self, id: &str) -> anyhow::Result<Option<String>> {
            match id {
                "local" => Ok(None),
                "pi" => Ok(Some("socks5://127.0.0.1:1081".into())),
                _ => anyhow::bail!("computer {id} not found"),
            }
        }
    }

    fn cfg(root: &std::path::Path) -> BrowserConfig {
        BrowserConfig {
            root: root.to_path_buf(),
            // No Chrome: these tests never start a browser.
            chrome: None,
            headless: true,
            window: (800, 600),
            screencast: ScreencastOptions::default(),
        }
    }

    /// FR-R1: the chosen egress survives a server restart (a new manager over the same database),
    /// and a computer is resolved again rather than its old listener URL reused.
    #[tokio::test]
    async fn egress_survives_a_manager_restart() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("ember.db");
        let resolver: egress::SharedEgressResolver = Arc::new(FakeComputers);
        {
            let store: egress::SharedEgressStore = Arc::new(crate::store::Store::open(&db).unwrap());
            let m = BrowserManager::with_egress(cfg(dir.path()), Some(store), Some(resolver.clone()));
            let pi = Egress::Computer { id: "pi".into() };
            let b = m.set_egress("acme", pi.clone(), false).await.unwrap();
            assert_eq!(b.egress_choice(), pi);
            assert_eq!(b.resolve_egress(&pi).unwrap().as_deref(), Some("socks5://127.0.0.1:1081"));
            m.set_egress("proxied", Egress::Proxy { url: "socks5://127.0.0.1:9".into() }, false)
                .await
                .unwrap();
            // Invalid choices are refused and nothing is saved.
            assert!(m.set_egress("acme", Egress::Computer { id: "gone".into() }, false).await.is_err());
            assert!(m.set_egress("acme", Egress::Proxy { url: "ftp://x:1".into() }, false).await.is_err());
            assert!(!b.is_running().await);
        }
        let store: egress::SharedEgressStore = Arc::new(crate::store::Store::open(&db).unwrap());
        let m = BrowserManager::with_egress(cfg(dir.path()), Some(store.clone()), Some(resolver));
        assert_eq!(m.instance("acme").await.unwrap().egress_choice(), Egress::Computer { id: "pi".into() });
        assert_eq!(
            m.instance("proxied").await.unwrap().egress_choice(),
            Egress::Proxy { url: "socks5://127.0.0.1:9".into() }
        );
        assert_eq!(m.instance("other").await.unwrap().egress_choice(), Egress::Direct);
        // Back to direct removes the record.
        m.set_egress("acme", Egress::Direct, false).await.unwrap();
        assert_eq!(store.load("acme").unwrap(), None);
    }

    /// Without Chrome, starting fails, and a computer that cannot be resolved is an error, not a
    /// silent fallback to the server's own network.
    #[tokio::test]
    async fn unresolvable_computer_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let m = BrowserManager::new(cfg(dir.path()));
        let b = m.instance("p").await.unwrap();
        assert!(b.resolve_egress(&Egress::Computer { id: "pi".into() }).is_err());
        assert!(m.set_egress("p", Egress::Computer { id: "pi".into() }, false).await.is_err());
        assert_eq!(b.egress_choice(), Egress::Direct);
    }

    #[test]
    fn project_names() {
        assert!(super::check_project("acme-1.web_app").is_ok());
        for bad in ["", ".", "..", "a/b", "a b", "../x"] {
            assert!(super::check_project(bad).is_err(), "{bad}");
        }
    }
}
