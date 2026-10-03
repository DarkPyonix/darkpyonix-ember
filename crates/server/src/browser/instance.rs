//! One project's browser: process lifecycle, egress switching, screencast, input, agent state.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context};
use axum::body::Bytes;
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;

use super::cdp::{Cdp, CdpEvent};
use super::chrome::{self, LaunchOptions};
use super::egress::{Egress, SharedEgressResolver};
use super::{BrowserConfig, STREAM_VERSION};

/// An agent command within this long counts as "agent is active" (FR-R3 indicator).
pub const AGENT_ACTIVE_WINDOW: Duration = Duration::from_secs(3);

/// Screencast parameters (`Page.startScreencast`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScreencastOptions {
    /// JPEG quality, 0–100.
    pub quality: u8,
    pub max_width: u32,
    pub max_height: u32,
    /// Send every n-th frame Chrome paints.
    pub every_nth_frame: u32,
}

impl Default for ScreencastOptions {
    fn default() -> Self {
        ScreencastOptions { quality: 70, max_width: 1280, max_height: 800, every_nth_frame: 1 }
    }
}

/// A tab the viewer can select.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Tab {
    pub target_id: String,
    pub url: String,
    pub title: String,
}

/// Everything a viewer shows around the frames; sent as a `state` message on every change.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ViewState {
    pub running: bool,
    /// Proxy URL the browser egresses through; `null` = the server's own network.
    pub egress: Option<String>,
    /// The computer the browser egresses from, when the egress is a registered computer
    /// (`egress` is then that computer's loopback SOCKS5 listener).
    pub egress_computer: Option<String>,
    /// The tab being streamed and receiving input.
    pub target_id: Option<String>,
    pub url: String,
    pub title: String,
    pub tabs: Vec<Tab>,
    /// An agent sent a DevTools command within the last few seconds.
    pub agent_active: bool,
    /// Agent DevTools connections currently open through the relay.
    pub agent_connections: usize,
    /// The user has taken over: agent commands are held until released.
    pub takeover: bool,
    /// Viewers connected to the stream.
    pub viewers: usize,
    /// Set when the browser stopped unexpectedly or failed to start.
    pub error: Option<String>,
}

/// Summary for listings.
#[derive(Debug, Clone, Serialize)]
pub struct BrowserInfo {
    pub project: String,
    pub profile_dir: PathBuf,
    /// The agent-facing DevTools port on 127.0.0.1 (direct, bypasses the relay's flag/pausing).
    pub debug_port: Option<u16>,
    pub state: ViewState,
}

/// Input from a viewer (WebSocket text message or `POST …/input`). Coordinates are CSS pixels in
/// the page viewport — the frame metadata's `deviceWidth` × `deviceHeight` space.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InputEvent {
    Mouse {
        /// `mousePressed`, `mouseReleased` or `mouseMoved`.
        event: String,
        x: f64,
        y: f64,
        #[serde(default = "default_button")]
        button: String,
        #[serde(default)]
        click_count: u32,
        #[serde(default)]
        modifiers: u32,
    },
    Wheel {
        x: f64,
        y: f64,
        delta_x: f64,
        delta_y: f64,
        #[serde(default)]
        modifiers: u32,
    },
    Key {
        /// `keyDown`, `keyUp`, `rawKeyDown` or `char`.
        event: String,
        #[serde(default)]
        key: String,
        #[serde(default)]
        code: String,
        #[serde(default)]
        text: Option<String>,
        /// Windows virtual key code (Enter = 13, Backspace = 8, …).
        #[serde(default)]
        key_code: Option<u32>,
        #[serde(default)]
        modifiers: u32,
    },
    /// Commit text as if typed (IME / mobile keyboards).
    Text { text: String },
    Navigate { url: String },
    Reload,
    Back,
    Forward,
    /// Take over (`on: true`) or hand back to the agent.
    Takeover { on: bool },
    /// Stream this tab; `null` follows the newest tab.
    SelectTab { target_id: Option<String> },
    Screencast { quality: Option<u8>, max_width: Option<u32>, max_height: Option<u32> },
}

fn default_button() -> String {
    "none".into()
}

/// A page we are attached to.
#[derive(Debug, Clone)]
struct PageRef {
    target_id: String,
    session_id: String,
}

struct Running {
    child: tokio::process::Child,
    cdp: Arc<Cdp>,
    port: u16,
    ws_url: String,
    driver: JoinHandle<()>,
}

pub struct BrowserInstance {
    pub project: String,
    pub profile_dir: PathBuf,
    launch: LaunchOptions,
    chrome_found: bool,
    run: tokio::sync::Mutex<Option<Running>>,
    /// The user's choice; resolved to a proxy URL at every start.
    egress: Mutex<Egress>,
    resolver: Option<SharedEgressResolver>,
    page: Mutex<Option<(PageRef, Arc<Cdp>)>>,
    /// Packed binary frame messages (see `docs/design/REMOTE-BROWSER.md`).
    frames: broadcast::Sender<Bytes>,
    frame_seq: AtomicU64,
    state: watch::Sender<ViewState>,
    viewers: watch::Sender<usize>,
    takeover: watch::Sender<bool>,
    follow: watch::Sender<Option<String>>,
    screencast: watch::Sender<ScreencastOptions>,
    agent_last: Mutex<Option<Instant>>,
}

impl BrowserInstance {
    pub(super) fn new(
        project: &str,
        profile_dir: PathBuf,
        cfg: &BrowserConfig,
        egress: Egress,
        resolver: Option<SharedEgressResolver>,
    ) -> Arc<Self> {
        let launch = LaunchOptions {
            chrome: cfg.chrome.clone().unwrap_or_default(),
            profile_dir: profile_dir.clone(),
            proxy: None,
            headless: cfg.headless,
            window: cfg.window,
        };
        let me = Arc::new(BrowserInstance {
            project: project.to_string(),
            profile_dir,
            launch,
            chrome_found: cfg.chrome.is_some(),
            run: Default::default(),
            egress: Mutex::new(egress),
            resolver,
            page: Mutex::new(None),
            frames: broadcast::channel(4).0,
            frame_seq: AtomicU64::new(0),
            state: watch::channel(ViewState {
                running: false,
                egress: None,
                egress_computer: None,
                target_id: None,
                url: String::new(),
                title: String::new(),
                tabs: Vec::new(),
                agent_active: false,
                agent_connections: 0,
                takeover: false,
                viewers: 0,
                error: None,
            })
            .0,
            viewers: watch::channel(0).0,
            takeover: watch::channel(false).0,
            follow: watch::channel(None).0,
            screencast: watch::channel(cfg.screencast).0,
            agent_last: Mutex::new(None),
        });
        // Flip `agent_active` off once the agent goes quiet.
        let weak = Arc::downgrade(&me);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_millis(250));
            loop {
                tick.tick().await;
                let Some(me) = Weak::upgrade(&weak) else { return };
                let active = me.agent_active();
                me.state.send_if_modified(|s| std::mem::replace(&mut s.agent_active, active) != active);
            }
        });
        me
    }

    pub fn state(&self) -> watch::Receiver<ViewState> {
        self.state.subscribe()
    }

    pub fn frames(&self) -> broadcast::Receiver<Bytes> {
        self.frames.subscribe()
    }

    pub async fn info(&self) -> BrowserInfo {
        let port = self.run.lock().await.as_ref().map(|r| r.port);
        BrowserInfo {
            project: self.project.clone(),
            profile_dir: self.profile_dir.clone(),
            debug_port: port,
            state: self.state.borrow().clone(),
        }
    }

    /// The proxy URL the browser was last started with (`None`: direct, or never started).
    pub fn egress(&self) -> Option<String> {
        self.state.borrow().egress.clone()
    }

    /// The egress the user chose (persisted by the manager).
    pub fn egress_choice(&self) -> Egress {
        self.egress.lock().unwrap().clone()
    }

    /// Record `egress` without touching a running browser; it applies at the next start.
    pub(super) fn set_egress_choice(&self, egress: Egress) {
        *self.egress.lock().unwrap() = egress;
    }

    pub async fn is_running(&self) -> bool {
        self.run.lock().await.is_some()
    }

    /// The proxy URL for `egress`, resolving a computer to its loopback SOCKS5 listener.
    pub fn resolve_egress(&self, egress: &Egress) -> anyhow::Result<Option<String>> {
        let proxy = match egress {
            Egress::Direct => None,
            Egress::Proxy { url } => Some(url.clone()),
            Egress::Computer { id } => self
                .resolver
                .as_ref()
                .ok_or_else(|| anyhow!("egress through a computer is not available on this server"))?
                .proxy_for_computer(id)
                .with_context(|| format!("egress through computer {id}"))?,
        };
        if let Some(p) = &proxy {
            chrome::check_proxy(p)?;
        }
        Ok(proxy)
    }

    /// The current DevTools browser endpoint, starting the browser if needed.
    pub async fn devtools_ws(self: &Arc<Self>) -> anyhow::Result<String> {
        self.ensure_running().await?;
        let run = self.run.lock().await;
        Ok(run.as_ref().context("browser not running")?.ws_url.clone())
    }

    /// Start the browser if it is not running (or has died).
    pub async fn ensure_running(self: &Arc<Self>) -> anyhow::Result<()> {
        let mut run = self.run.lock().await;
        if let Some(r) = run.as_mut() {
            if r.cdp.is_open() && r.child.try_wait().ok().flatten().is_none() {
                return Ok(());
            }
            Self::stop_running(run.take().unwrap()).await;
        }
        let choice = self.egress_choice();
        // Never fall back to direct when the chosen egress cannot be resolved: that would leak
        // the server's own address where the user asked for another computer's.
        let started = match self.resolve_egress(&choice) {
            Ok(proxy) => self.start(proxy.clone()).await.map(|r| (r, proxy)),
            Err(e) => Err(e),
        };
        match started {
            Ok((r, proxy)) => {
                *run = Some(r);
                self.state.send_modify(|s| {
                    s.running = true;
                    s.egress = proxy;
                    s.egress_computer = choice.computer().map(str::to_string);
                    s.error = None;
                });
                Ok(())
            }
            Err(e) => {
                self.state.send_modify(|s| {
                    s.running = false;
                    s.error = Some(format!("{e:#}"));
                });
                Err(e)
            }
        }
    }

    /// Switch egress to a proxy URL (`None`: direct). Not persisted; see
    /// [`super::BrowserManager::set_egress`].
    pub async fn set_egress(self: &Arc<Self>, egress: Option<String>) -> anyhow::Result<()> {
        self.switch_egress(Egress::from_proxy(egress)).await
    }

    /// Switch egress: restart Chrome with the new proxy on the same profile (FR-R2), or start it.
    /// Viewers stay connected; agent relay connections are closed and must reconnect.
    pub async fn switch_egress(self: &Arc<Self>, egress: Egress) -> anyhow::Result<()> {
        let proxy = self.resolve_egress(&egress)?;
        {
            let run = self.run.lock().await;
            if run.is_some() && self.egress_choice() == egress && self.egress() == proxy {
                drop(run);
                return self.ensure_running().await;
            }
        }
        self.set_egress_choice(egress);
        self.stop().await;
        self.ensure_running().await
    }

    /// Stop the browser gracefully so the profile is flushed to disk.
    pub async fn stop(&self) {
        let r = self.run.lock().await.take();
        if let Some(r) = r {
            Self::stop_running(r).await;
        }
        *self.page.lock().unwrap() = None;
        self.state.send_modify(|s| {
            s.running = false;
            s.target_id = None;
            s.tabs.clear();
        });
    }

    /// Wipe the profile (FR-R4); restarts the browser if it was running.
    pub async fn clear_data(self: &Arc<Self>) -> anyhow::Result<()> {
        let was_running = self.run.lock().await.is_some();
        self.stop().await;
        match std::fs::remove_dir_all(&self.profile_dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).context("removing browser profile"),
        }
        if was_running {
            self.ensure_running().await?;
        }
        Ok(())
    }

    async fn stop_running(mut r: Running) {
        r.driver.abort();
        // `Browser.close` makes Chrome write cookies and storage before it exits.
        let _ = tokio::time::timeout(Duration::from_secs(5), r.cdp.call("Browser.close", json!({}), None))
            .await;
        if tokio::time::timeout(Duration::from_secs(10), r.child.wait()).await.is_err() {
            let _ = r.child.kill().await;
        }
    }

    async fn start(self: &Arc<Self>, egress: Option<String>) -> anyhow::Result<Running> {
        if !self.chrome_found {
            anyhow::bail!("no Chrome/Chromium found; set EMBER_CHROME_BIN");
        }
        let mut opts = self.launch.clone();
        opts.proxy = egress;
        let launched = chrome::launch(&opts).await?;
        let cdp = Cdp::connect(&launched.ws_url).await?;
        let driver = tokio::spawn(drive(Arc::downgrade(self), cdp.clone()));
        Ok(Running { child: launched.child, cdp, port: launched.port, ws_url: launched.ws_url, driver })
    }

    // ---- viewers and input ----

    /// Register a viewer; screencasting runs while at least one is registered.
    pub fn add_viewer(self: &Arc<Self>) -> ViewerGuard {
        self.viewers.send_modify(|n| *n += 1);
        let n = *self.viewers.borrow();
        self.state.send_modify(|s| s.viewers = n);
        ViewerGuard(self.clone())
    }

    /// Apply one viewer input event (user control).
    pub async fn input(&self, ev: InputEvent) -> anyhow::Result<()> {
        match ev {
            InputEvent::Takeover { on } => {
                self.takeover.send_replace(on);
                self.state.send_modify(|s| s.takeover = on);
                return Ok(());
            }
            InputEvent::SelectTab { target_id } => {
                self.follow.send_replace(target_id);
                return Ok(());
            }
            InputEvent::Screencast { quality, max_width, max_height } => {
                self.screencast.send_modify(|o| {
                    if let Some(q) = quality {
                        o.quality = q.min(100);
                    }
                    if let Some(w) = max_width {
                        o.max_width = w.clamp(64, 4096);
                    }
                    if let Some(h) = max_height {
                        o.max_height = h.clamp(64, 4096);
                    }
                });
                return Ok(());
            }
            _ => {}
        }
        let (page, cdp) = self.page.lock().unwrap().clone().ok_or_else(|| anyhow!("no page attached"))?;
        let s = Some(page.session_id.as_str());
        match ev {
            InputEvent::Mouse { event, x, y, button, click_count, modifiers } => {
                let buttons = if event == "mousePressed" { button_mask(&button) } else { 0 };
                cdp.call(
                    "Input.dispatchMouseEvent",
                    json!({ "type": event, "x": x, "y": y, "button": button, "buttons": buttons,
                            "clickCount": click_count, "modifiers": modifiers }),
                    s,
                )
                .await?;
            }
            InputEvent::Wheel { x, y, delta_x, delta_y, modifiers } => {
                cdp.call(
                    "Input.dispatchMouseEvent",
                    json!({ "type": "mouseWheel", "x": x, "y": y, "deltaX": delta_x,
                            "deltaY": delta_y, "modifiers": modifiers }),
                    s,
                )
                .await?;
            }
            InputEvent::Key { event, key, code, text, key_code, modifiers } => {
                let mut p = json!({ "type": event, "key": key, "code": code, "modifiers": modifiers });
                if let Some(t) = text {
                    p["text"] = json!(t);
                    p["unmodifiedText"] = json!(t);
                }
                if let Some(k) = key_code {
                    p["windowsVirtualKeyCode"] = json!(k);
                    p["nativeVirtualKeyCode"] = json!(k);
                }
                cdp.call("Input.dispatchKeyEvent", p, s).await?;
            }
            InputEvent::Text { text } => {
                cdp.call("Input.insertText", json!({ "text": text }), s).await?;
            }
            InputEvent::Navigate { url } => {
                cdp.call("Page.navigate", json!({ "url": url }), s).await?;
            }
            InputEvent::Reload => {
                cdp.call("Page.reload", json!({}), s).await?;
            }
            InputEvent::Back | InputEvent::Forward => {
                let js = if matches!(ev, InputEvent::Back) { "history.back()" } else { "history.forward()" };
                cdp.call("Runtime.evaluate", json!({ "expression": js }), s).await?;
            }
            InputEvent::Takeover { .. } | InputEvent::SelectTab { .. } | InputEvent::Screencast { .. } => {
                unreachable!()
            }
        }
        Ok(())
    }

    /// Evaluate JavaScript in the streamed page (tests and diagnostics).
    pub async fn evaluate(&self, expression: &str) -> anyhow::Result<Value> {
        let (page, cdp) = self.page.lock().unwrap().clone().ok_or_else(|| anyhow!("no page attached"))?;
        let r = cdp
            .call(
                "Runtime.evaluate",
                json!({ "expression": expression, "returnByValue": true, "awaitPromise": true }),
                Some(&page.session_id),
            )
            .await?;
        if let Some(ex) = r.get("exceptionDetails") {
            anyhow::bail!("evaluate threw: {ex}");
        }
        Ok(r["result"]["value"].clone())
    }

    /// Wait until a page is attached (after start or a restart).
    pub async fn wait_page(&self, timeout: Duration) -> anyhow::Result<()> {
        let mut st = self.state.subscribe();
        tokio::time::timeout(timeout, async {
            loop {
                if self.page.lock().unwrap().is_some() {
                    return;
                }
                if st.changed().await.is_err() {
                    return;
                }
            }
        })
        .await
        .map_err(|_| anyhow!("no page attached within {timeout:?}"))
    }

    // ---- agents ----

    /// Called by the CDP relay for every agent command. Waits while the user has taken over, so
    /// the agent pauses instead of fighting the user (FR-R3); returns once it may proceed.
    pub async fn agent_command(&self) {
        let mut t = self.takeover.subscribe();
        while *t.borrow_and_update() {
            if t.changed().await.is_err() {
                break;
            }
        }
        *self.agent_last.lock().unwrap() = Some(Instant::now());
        self.state.send_if_modified(|s| !std::mem::replace(&mut s.agent_active, true));
    }

    pub fn agent_connected(&self, delta: isize) {
        self.state.send_modify(|s| {
            s.agent_connections = s.agent_connections.saturating_add_signed(delta);
        });
    }

    fn agent_active(&self) -> bool {
        self.agent_last.lock().unwrap().is_some_and(|t| t.elapsed() < AGENT_ACTIVE_WINDOW)
    }

    fn pack_frame(&self, page: &PageRef, params: &Value) -> Option<Vec<u8>> {
        let jpeg = base64::engine::general_purpose::STANDARD
            .decode(params.get("data")?.as_str()?)
            .ok()?;
        let st = self.state.borrow();
        let header = json!({
            "type": "frame",
            "v": STREAM_VERSION,
            "seq": self.frame_seq.fetch_add(1, Ordering::Relaxed),
            "format": "jpeg",
            "target_id": page.target_id,
            "metadata": params.get("metadata").cloned().unwrap_or(Value::Null),
            "agent_active": st.agent_active,
            "takeover": st.takeover,
        })
        .to_string();
        let mut out = Vec::with_capacity(4 + header.len() + jpeg.len());
        out.extend_from_slice(&(header.len() as u32).to_be_bytes());
        out.extend_from_slice(header.as_bytes());
        out.extend_from_slice(&jpeg);
        Some(out)
    }
}

fn button_mask(button: &str) -> u32 {
    match button {
        "left" => 1,
        "right" => 2,
        "middle" => 4,
        _ => 0,
    }
}

/// Unregisters a viewer on drop.
pub struct ViewerGuard(Arc<BrowserInstance>);

impl Drop for ViewerGuard {
    fn drop(&mut self) {
        self.0.viewers.send_modify(|n| *n = n.saturating_sub(1));
        let n = *self.0.viewers.borrow();
        self.0.state.send_modify(|s| s.viewers = n);
    }
}

// ---- the per-process driver: tabs, attach, screencast ----

fn tab_of(info: &Value) -> Option<Tab> {
    (info["type"] == "page").then(|| Tab {
        target_id: info["targetId"].as_str().unwrap_or_default().to_string(),
        url: info["url"].as_str().unwrap_or_default().to_string(),
        title: info["title"].as_str().unwrap_or_default().to_string(),
    })
}

/// Keeps one page attached for streaming and input, follows new tabs, and runs the screencast
/// while anyone watches. Ends when the connection closes or the instance is gone.
async fn drive(me: Weak<BrowserInstance>, cdp: Arc<Cdp>) {
    if let Err(e) = drive_inner(&me, &cdp).await {
        tracing::warn!("browser driver ended: {e:#}");
    }
    if let Some(me) = me.upgrade() {
        *me.page.lock().unwrap() = None;
        if !cdp.is_open() {
            me.state.send_modify(|s| {
                s.running = false;
                s.target_id = None;
            });
        }
    }
}

async fn drive_inner(me: &Weak<BrowserInstance>, cdp: &Arc<Cdp>) -> anyhow::Result<()> {
    let mut events = cdp.events();
    cdp.call("Target.setDiscoverTargets", json!({ "discover": true }), None).await?;
    let targets = cdp.call("Target.getTargets", json!({}), None).await?;
    // Newest last; Chrome lists newest first.
    let mut tabs: Vec<Tab> = targets["targetInfos"]
        .as_array()
        .map(|a| a.iter().rev().filter_map(tab_of).collect())
        .unwrap_or_default();

    let (mut viewers, mut follow, mut sc_opts) = {
        let me = me.upgrade().context("gone")?;
        (me.viewers.subscribe(), me.follow.subscribe(), me.screencast.subscribe())
    };

    loop {
        // Choose the tab: the selected one if it still exists, else the newest.
        let want = follow.borrow_and_update().clone();
        let target = match want.and_then(|w| tabs.iter().find(|t| t.target_id == w).cloned()) {
            Some(t) => t,
            None => match tabs.last().cloned() {
                Some(t) => t,
                None => {
                    let r = cdp.call("Target.createTarget", json!({ "url": "about:blank" }), None).await?;
                    let id = r["targetId"].as_str().unwrap_or_default().to_string();
                    let t = Tab { target_id: id, url: "about:blank".into(), title: String::new() };
                    tabs.push(t.clone());
                    t
                }
            },
        };
        let att = cdp
            .call("Target.attachToTarget", json!({ "targetId": target.target_id, "flatten": true }), None)
            .await;
        let session_id = match att {
            Ok(v) => v["sessionId"].as_str().unwrap_or_default().to_string(),
            Err(e) => {
                tracing::debug!("attach {} failed: {e:#}", target.target_id);
                tabs.retain(|t| t.target_id != target.target_id);
                continue;
            }
        };
        let page = PageRef { target_id: target.target_id.clone(), session_id: session_id.clone() };
        let sid = Some(session_id.as_str());
        cdp.call("Page.enable", json!({}), sid).await.ok();
        {
            let me = me.upgrade().context("gone")?;
            *me.page.lock().unwrap() = Some((page.clone(), cdp.clone()));
            me.state.send_modify(|s| {
                s.target_id = Some(target.target_id.clone());
                s.url = target.url.clone();
                s.title = target.title.clone();
                s.tabs = tabs.clone();
            });
        }

        let mut casting = false;
        let switch = 'attached: loop {
            let want_cast = *viewers.borrow_and_update() > 0;
            if want_cast != casting {
                if want_cast {
                    let o = *sc_opts.borrow_and_update();
                    cdp.call(
                        "Page.startScreencast",
                        json!({ "format": "jpeg", "quality": o.quality, "maxWidth": o.max_width,
                                "maxHeight": o.max_height, "everyNthFrame": o.every_nth_frame }),
                        sid,
                    )
                    .await
                    .ok();
                } else {
                    cdp.call("Page.stopScreencast", json!({}), sid).await.ok();
                }
                casting = want_cast;
            }
            tokio::select! {
                r = viewers.changed() => { if r.is_err() { return Ok(()); } }
                r = sc_opts.changed() => {
                    if r.is_err() { return Ok(()); }
                    if casting {
                        // Restart with the new settings on the next turn.
                        cdp.call("Page.stopScreencast", json!({}), sid).await.ok();
                        casting = false;
                    }
                }
                r = follow.changed() => {
                    if r.is_err() { return Ok(()); }
                    let w = follow.borrow().clone();
                    if w.as_deref().is_some_and(|w| w != page.target_id)
                        || (w.is_none() && tabs.last().is_some_and(|t| t.target_id != page.target_id))
                    {
                        break 'attached true;
                    }
                }
                ev = events.recv() => {
                    let ev: CdpEvent = match ev {
                        Ok(ev) => ev,
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => return Ok(()),
                    };
                    let me = me.upgrade().context("gone")?;
                    match ev.method.as_str() {
                        "Page.screencastFrame" if ev.session_id.as_deref() == sid => {
                            if let Some(frame) = me.pack_frame(&page, &ev.params) {
                                let _ = me.frames.send(Bytes::from(frame));
                            }
                            // Ack after handing the frame on; Chrome waits for it before the
                            // next one, which bounds the in-flight frames.
                            cdp.send_nowait(
                                "Page.screencastFrameAck",
                                json!({ "sessionId": ev.params["sessionId"] }),
                                sid,
                            );
                        }
                        "Target.targetCreated" => {
                            if let Some(t) = tab_of(&ev.params["targetInfo"]) {
                                if !tabs.iter().any(|x| x.target_id == t.target_id) {
                                    tabs.push(t);
                                    me.state.send_modify(|s| s.tabs = tabs.clone());
                                    if me.follow.borrow().is_none() {
                                        break 'attached true;
                                    }
                                }
                            }
                        }
                        "Target.targetInfoChanged" => {
                            if let Some(t) = tab_of(&ev.params["targetInfo"]) {
                                if let Some(x) = tabs.iter_mut().find(|x| x.target_id == t.target_id) {
                                    *x = t.clone();
                                }
                                me.state.send_modify(|s| {
                                    s.tabs = tabs.clone();
                                    if t.target_id == page.target_id {
                                        s.url = t.url;
                                        s.title = t.title;
                                    }
                                });
                            }
                        }
                        "Target.targetDestroyed" => {
                            let id = ev.params["targetId"].as_str().unwrap_or_default();
                            tabs.retain(|t| t.target_id != id);
                            me.state.send_modify(|s| s.tabs = tabs.clone());
                            if id == page.target_id {
                                break 'attached false;
                            }
                        }
                        "Target.detachedFromTarget"
                            if ev.params["sessionId"].as_str() == Some(session_id.as_str()) =>
                        {
                            break 'attached false;
                        }
                        _ => {}
                    }
                }
            }
        };
        if let Some(me) = me.upgrade() {
            *me.page.lock().unwrap() = None;
        }
        if switch {
            cdp.call("Target.detachFromTarget", json!({ "sessionId": session_id }), None).await.ok();
        }
    }
}
