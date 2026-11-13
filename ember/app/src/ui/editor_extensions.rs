//! The 확장 tool window of the Ember 에디터 window (SPEC FR-L31.14, #446): what Code-OSS's
//! Extensions view does, in the mockup's island style.
//!
//! - The installed list is the scan the session reads on the OSE management connection
//!   (`remoteExtensionsScanner.scanExtensions`); [`ExtensionScan`] lives in the host state.
//! - Search asks the Ember server, which asks Open VSX (`GET /api/ide/extensions/search`). The
//!   client never contacts the gallery itself.
//! - Install and uninstall are the server's routes over the OSE CLI
//!   (`/api/computers/{id}/ide/extensions`).
//! - Enable and disable are this device's prefs (`Prefs::disabled_extensions`), applied the next
//!   time the extension host starts, as the Code-OSS workbench keeps its own disabled list.
//!
//! This module holds the state and the rules as plain functions; [`view`] draws them.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use dioxus_compose::prelude::*;
use ember_editor::extensions::{group, id_key, InstalledExtension};

use crate::server::{GalleryExtension, GallerySearch, ServerError};

/// The pause in typing before a search is sent.
pub const SEARCH_DEBOUNCE_MS: u64 = 300;

pub const RESTART_NOTE: &str = "변경 사항은 에디터 창을 다시 열면 적용됩니다.";
pub const NO_COMPUTER_NOTE: &str = "이 창은 프로젝트의 컴퓨터를 알 수 없어 확장을 설치하거나 제거할 수 없습니다.";
pub const BUSY_NOTE: &str = "다른 확장을 설치하거나 제거하는 중입니다. 끝난 뒤 다시 시도하세요.";

/// What the OSE has installed and what the extension host runs. Owned by the host state: the
/// session task writes it, the panel reads it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ExtensionScan {
    /// The newest scan, disabled extensions included.
    pub installed: Vec<InstalledExtension>,
    /// (id key, version) of every extension the extension host was started with.
    pub running: BTreeSet<(String, String)>,
    /// The connect-time scan has arrived.
    pub loaded: bool,
    pub rescanning: bool,
    /// Why the last rescan failed.
    pub error: Option<String>,
}

impl ExtensionScan {
    /// The session connected: `installed` is its scan, `running` what its extension host got.
    pub fn connected(&mut self, installed: Vec<InstalledExtension>, running: &[InstalledExtension]) {
        self.installed = installed;
        self.running = running.iter().map(|e| (e.key(), e.version.clone())).collect();
        self.loaded = true;
        self.rescanning = false;
        self.error = None;
    }

    pub fn begin_rescan(&mut self) {
        self.rescanning = true;
    }

    /// Takes a rescan answer. A failure keeps the last list and says why.
    pub fn finish_rescan<E: std::fmt::Display>(&mut self, result: Result<Vec<InstalledExtension>, E>) {
        self.rescanning = false;
        match result {
            Ok(list) => {
                self.installed = list;
                self.error = None;
            }
            Err(e) => self.error = Some(format!("설치된 확장을 다시 읽지 못했습니다: {e}")),
        }
    }

    /// The installed version of `id`, if it is installed.
    pub fn version_of(&self, id: &str) -> Option<&str> {
        let key = id_key(id);
        self.installed.iter().find(|e| e.key() == key).map(|e| e.version.as_str())
    }
}

/// One row of the installed list.
#[derive(Debug, Clone, PartialEq)]
pub struct InstalledRow {
    pub ext: InstalledExtension,
    pub enabled: bool,
}

impl InstalledRow {
    /// The second line: publisher, and the state when it is not the usual one.
    pub fn meta(&self) -> String {
        let mut parts: Vec<&str> = Vec::new();
        if !self.ext.publisher.is_empty() {
            parts.push(&self.ext.publisher);
        }
        if !self.enabled {
            parts.push("사용 안 함");
        }
        parts.join(" · ")
    }

    pub fn label(&self) -> String {
        let state = if self.enabled { "사용" } else { "사용 안 함" };
        format!("{} {}, {}, {state}", self.ext.display_name, self.ext.version, self.ext.publisher)
    }
}

/// The installed list in its two groups, each sorted by display name: (설치됨, 기본 제공).
/// `disabled` holds ids in any case.
pub fn installed_rows(scan: &ExtensionScan, disabled: &[String]) -> (Vec<InstalledRow>, Vec<InstalledRow>) {
    let off: BTreeSet<String> = disabled.iter().map(|d| id_key(d)).collect();
    let rows = |builtin: bool| -> Vec<InstalledRow> {
        group(&scan.installed, builtin).into_iter().map(|ext| InstalledRow { enabled: !off.contains(&ext.key()), ext }).collect()
    };
    (rows(false), rows(true))
}

/// One search result with what the OSE already has of it.
#[derive(Debug, Clone, PartialEq)]
pub struct ResultRow {
    pub ext: GalleryExtension,
    /// The installed version, when this extension is installed.
    pub installed: Option<String>,
}

impl ResultRow {
    pub fn meta(&self) -> String {
        let mut parts: Vec<String> = vec![self.ext.namespace.clone()];
        if self.ext.verified {
            parts.push("확인된 게시자".into());
        }
        if self.ext.downloads > 0 {
            parts.push(format!("다운로드 {}", downloads_text(self.ext.downloads)));
        }
        if self.ext.deprecated {
            parts.push("지원 중단".into());
        }
        if let Some(v) = &self.installed {
            parts.push(format!("설치됨 {v}"));
        }
        parts.retain(|p| !p.is_empty());
        parts.join(" · ")
    }

    pub fn label(&self) -> String {
        let state = if self.installed.is_some() { "설치됨" } else { "설치되지 않음" };
        format!("{} {}, {}, {state}", self.ext.display_name, self.ext.version, self.ext.namespace)
    }
}

pub fn result_rows(results: &[GalleryExtension], scan: &ExtensionScan) -> Vec<ResultRow> {
    results.iter().map(|ext| ResultRow { installed: scan.version_of(&ext.id).map(str::to_string), ext: ext.clone() }).collect()
}

/// A download count in the short Korean form: 950, 1.2천, 35만, 1.2억.
pub fn downloads_text(n: u64) -> String {
    fn one_decimal(n: u64, unit: u64) -> String {
        let tenths = n * 10 / unit;
        if tenths % 10 == 0 || tenths >= 100 { format!("{}", tenths / 10) } else { format!("{}.{}", tenths / 10, tenths % 10) }
    }
    match n {
        0..=999 => n.to_string(),
        1_000..=9_999 => format!("{}천", one_decimal(n, 1_000)),
        10_000..=99_999_999 => format!("{}만", one_decimal(n, 10_000)),
        _ => format!("{}억", one_decimal(n, 100_000_000)),
    }
}

/// Whether the extension host runs something other than what the list now says it should: the
/// enabled set (id and version) differs from the one it started with, or an install or uninstall
/// succeeded in this window (`touched`; the rescan after it is not relied on to show it).
/// Ember sends no `$deltaExtensions`, so every such change waits for the next start.
pub fn needs_restart(scan: &ExtensionScan, disabled: &[String], touched: bool) -> bool {
    if !scan.loaded {
        return false;
    }
    if touched {
        return true;
    }
    let off: BTreeSet<String> = disabled.iter().map(|d| id_key(d)).collect();
    let wanted: BTreeSet<(String, String)> =
        scan.installed.iter().filter(|e| !off.contains(&e.key())).map(|e| (e.key(), e.version.clone())).collect();
    wanted != scan.running
}

/// What a row is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Install,
    Uninstall,
}

impl Op {
    pub fn doing(self) -> &'static str {
        match self {
            Op::Install => "설치 중…",
            Op::Uninstall => "제거 중…",
        }
    }

    fn noun(self) -> &'static str {
        match self {
            Op::Install => "설치",
            Op::Uninstall => "제거",
        }
    }

    pub fn done_text(self, name: &str) -> String {
        match self {
            Op::Install => format!("{name} 확장을 설치했습니다."),
            Op::Uninstall => format!("{name} 확장을 제거했습니다."),
        }
    }
}

/// The search half of the panel.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum SearchLoad {
    /// The field is empty: the installed list shows.
    #[default]
    Idle,
    Loading,
    Ready,
    Failed(String),
}

/// Everything the panel keeps between frames besides the scan.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ExtState {
    pub query: String,
    pub load: SearchLoad,
    pub results: Vec<GalleryExtension>,
    /// How many extensions match in all.
    pub total: u64,
    pub loading_more: bool,
    /// The one install or uninstall on the way: (id key, what).
    pub busy: Option<(String, Op)>,
    /// The last install or uninstall failure, until the next one starts.
    pub op_error: Option<String>,
    /// An install or uninstall succeeded in this window.
    pub touched: bool,
    seq: u64,
}

impl ExtState {
    /// The field changed. Returns the number a search for it must carry, or `None` when the field
    /// is empty (the installed list shows, and an answer on the way is dropped).
    pub fn begin_search(&mut self, query: &str) -> Option<u64> {
        self.seq += 1;
        self.query = query.to_string();
        self.loading_more = false;
        if query.trim().is_empty() {
            self.load = SearchLoad::Idle;
            self.results.clear();
            self.total = 0;
            return None;
        }
        self.load = SearchLoad::Loading;
        Some(self.seq)
    }

    /// Whether `seq` is still the newest search (a debounced one that is not is never sent).
    pub fn is_current(&self, seq: u64) -> bool {
        self.seq == seq
    }

    /// Takes a search answer unless the field changed after it was asked. Returns whether it was taken.
    pub fn finish_search(&mut self, seq: u64, result: Result<GallerySearch, String>) -> bool {
        if seq != self.seq {
            return false;
        }
        match result {
            Ok(page) => {
                self.total = page.total.max(page.extensions.len() as u64);
                self.results = page.extensions;
                self.load = SearchLoad::Ready;
            }
            Err(message) => {
                self.results.clear();
                self.total = 0;
                self.load = SearchLoad::Failed(message);
            }
        }
        true
    }

    /// Whether the gallery has more results than the list shows.
    pub fn has_more(&self) -> bool {
        self.load == SearchLoad::Ready && !self.results.is_empty() && (self.results.len() as u64) < self.total
    }

    /// Starts reading the next page: (number, query, offset). `None` when there is none or one is on the way.
    pub fn begin_more(&mut self) -> Option<(u64, String, u64)> {
        if !self.has_more() || self.loading_more {
            return None;
        }
        self.loading_more = true;
        Some((self.seq, self.query.trim().to_string(), self.results.len() as u64))
    }

    /// Takes the next page unless the field changed meanwhile. An id already listed is not added
    /// twice; a page that adds nothing ends the list.
    pub fn finish_more(&mut self, seq: u64, result: Result<GallerySearch, String>) -> bool {
        if seq != self.seq {
            return false;
        }
        self.loading_more = false;
        match result {
            Ok(page) => {
                let before = self.results.len();
                for ext in page.extensions {
                    if !self.results.iter().any(|r| id_key(&r.id) == id_key(&ext.id)) {
                        self.results.push(ext);
                    }
                }
                self.total = if self.results.len() == before { before as u64 } else { page.total.max(self.results.len() as u64) };
            }
            Err(message) => self.op_error = Some(message),
        }
        true
    }

    /// Starts an install or uninstall of `id`. False when another one is on the way.
    pub fn begin_op(&mut self, id: &str, op: Op) -> bool {
        if self.busy.is_some() {
            return false;
        }
        self.busy = Some((id_key(id), op));
        self.op_error = None;
        true
    }

    /// The install or uninstall on the way ended.
    pub fn finish_op(&mut self, result: Result<(), String>) {
        self.busy = None;
        match result {
            Ok(()) => self.touched = true,
            Err(message) => self.op_error = Some(message),
        }
    }

    /// What `id`'s row is doing, if anything.
    pub fn op_of(&self, id: &str) -> Option<Op> {
        let key = id_key(id);
        self.busy.as_ref().filter(|(k, _)| *k == key).map(|(_, op)| *op)
    }

    /// The line under the field while the search results show.
    pub fn status_text(&self) -> Option<String> {
        match &self.load {
            SearchLoad::Idle => None,
            SearchLoad::Loading => Some("Open VSX에서 검색하는 중입니다…".into()),
            SearchLoad::Failed(message) => Some(message.clone()),
            SearchLoad::Ready if self.results.is_empty() => Some("일치하는 확장이 없습니다.".into()),
            SearchLoad::Ready => Some(format!("확장 {}개 중 {}개", self.total, self.results.len())),
        }
    }
}

/// The Korean reason for a failed search.
pub fn search_failure_text(error: &ServerError) -> String {
    match error {
        ServerError::Status { status: 502, message } => format!("확장 갤러리(Open VSX)에서 결과를 받지 못했습니다: {message}"),
        ServerError::Status { status: 400, message } => format!("검색어를 보낼 수 없습니다: {message}"),
        other => common_failure_text(other),
    }
}

/// The Korean reason for a failed install or uninstall. A 502 carries the command's own output.
pub fn op_failure_text(op: Op, name: &str, error: &ServerError) -> String {
    let reason = match error {
        ServerError::Status { status: 409, .. } => "이 컴퓨터에서 다른 설치 또는 제거가 진행 중입니다. 끝난 뒤 다시 시도하세요.".to_string(),
        ServerError::Status { status: 503, .. } => "서버에 OSE가 설치되어 있지 않습니다.".to_string(),
        ServerError::Status { status: 400, message } | ServerError::Status { status: 502, message } => message.clone(),
        other => common_failure_text(other),
    };
    format!("{name} {} 실패: {reason}", op.noun())
}

fn common_failure_text(error: &ServerError) -> String {
    match error {
        ServerError::Transport(_) | ServerError::Client(_) => "서버에 연결할 수 없습니다.".to_string(),
        ServerError::Unauthorized(_) => "서버가 인증을 거부했습니다.".to_string(),
        ServerError::Status { status, message } => format!("서버 오류({status}): {message}"),
    }
}

/// A state of the panel a capture asks for (debug builds only, [`capture_hook`]). Every state
/// opens the 확장 window when the editor window starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capture {
    /// The installed list as the OSE scan gives it.
    Open,
    /// The installed list with two sample user extensions added to the scan.
    Installed,
    /// Sample search results for "rust" (no request is sent), with more to load.
    Search,
    /// The sample results with an install on the way on the first row.
    Busy,
    /// The sample installed list with the restart notice and a failed install's message.
    Restart,
}

/// `EMBER_CR_EDITOR_EXTENSIONS=open|installed|search|busy|restart`: the capture state, in a
/// debug build only. A release build ignores the variable.
pub fn capture_hook_with(value: Option<&str>, debug: bool) -> Option<Capture> {
    if !debug {
        return None;
    }
    match value?.trim() {
        "open" => Some(Capture::Open),
        "installed" => Some(Capture::Installed),
        "search" => Some(Capture::Search),
        "busy" => Some(Capture::Busy),
        "restart" => Some(Capture::Restart),
        _ => None,
    }
}

pub fn capture_hook() -> Option<Capture> {
    capture_hook_with(std::env::var("EMBER_CR_EDITOR_EXTENSIONS").ok().as_deref(), cfg!(debug_assertions))
}

fn sample_results() -> Vec<GalleryExtension> {
    let item = |namespace: &str, name: &str, display: &str, description: &str, version: &str, downloads: u64, verified: bool| GalleryExtension {
        id: format!("{namespace}.{name}"),
        name: name.into(),
        namespace: namespace.into(),
        display_name: display.into(),
        description: description.into(),
        version: version.into(),
        downloads,
        verified,
        deprecated: false,
    };
    vec![
        item("rust-lang", "rust-analyzer", "rust-analyzer", "Rust language support for Visual Studio Code", "0.4.3116", 7_713_255, true),
        item("tamasfe", "even-better-toml", "Even Better TOML", "Fully-featured TOML support", "0.21.2", 1_254_300, false),
        item("vadimcn", "vscode-lldb", "CodeLLDB", "A native debugger powered by LLDB. Debug C++, Rust and other compiled languages.", "1.11.5", 2_480_000, true),
        item("serayuzgur", "crates", "crates", "Helps Rust developers managing dependencies with Cargo.toml.", "0.6.6", 356_000, false),
    ]
}

impl Capture {
    /// Whether the capture adds the sample user extensions to the scan.
    pub fn adds_samples(self) -> bool {
        !matches!(self, Capture::Open)
    }
}

impl ExtensionScan {
    /// Adds two sample user extensions for a capture (they are not on the OSE and not running).
    pub fn add_capture_samples(&mut self) {
        let sample = |id: &str, display: &str, version: &str, description: &str| {
            let (publisher, name) = id.split_once('.').unwrap_or(("", id));
            InstalledExtension {
                id: id.into(),
                name: name.into(),
                publisher: publisher.into(),
                version: version.into(),
                display_name: display.into(),
                description: description.into(),
                builtin: false,
            }
        };
        let samples = [
            sample("rust-lang.rust-analyzer", "rust-analyzer", "0.4.3116", "Rust language support for Visual Studio Code"),
            sample("tamasfe.even-better-toml", "Even Better TOML", "0.21.2", "Fully-featured TOML support"),
        ];
        for s in samples {
            if self.version_of(&s.id).is_none() {
                self.running.insert((s.key(), s.version.clone()));
                self.installed.push(s);
            }
        }
    }
}

impl ExtState {
    /// The panel state a capture starts in.
    pub fn sample(capture: Capture) -> ExtState {
        let mut st = ExtState::default();
        match capture {
            Capture::Open | Capture::Installed => {}
            Capture::Search | Capture::Busy => {
                let seq = st.begin_search("rust").unwrap_or_default();
                st.finish_search(seq, Ok(GallerySearch { offset: 0, total: 566, extensions: sample_results() }));
                if capture == Capture::Busy {
                    st.begin_op("vadimcn.vscode-lldb", Op::Install);
                }
            }
            Capture::Restart => {
                st.touched = true;
                st.op_error = Some(op_failure_text(
                    Op::Install,
                    "CodeLLDB",
                    &ServerError::Status { status: 502, message: "install failed: exit status: 1\nExtension 'vadimcn.vscode-lldb' not found.".into() },
                ));
            }
        }
        st
    }
}

/// The extensions handle the editor window shares: the panel state, the window's redraw counter
/// and the computer the install and uninstall calls name.
#[derive(Clone)]
pub struct Extensions {
    pub state: Arc<Mutex<ExtState>>,
    revision: dioxus_compose::prelude::SyncSignal<u64>,
    /// The id of the project's computer; empty when the window was not told.
    computer: String,
}

impl Extensions {
    pub fn new(computer: &str, revision: dioxus_compose::prelude::SyncSignal<u64>) -> Extensions {
        let state = capture_hook().map(ExtState::sample).unwrap_or_default();
        Extensions { state: Arc::new(Mutex::new(state)), revision, computer: computer.trim().to_string() }
    }

    pub fn snapshot(&self) -> ExtState {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn computer(&self) -> &str {
        &self.computer
    }

    /// Install and uninstall need the computer's id.
    pub fn can_manage(&self) -> bool {
        !self.computer.is_empty()
    }

    fn with<R>(&self, f: impl FnOnce(&mut ExtState) -> R) -> R {
        f(&mut self.state.lock().unwrap_or_else(|e| e.into_inner()))
    }

    fn redraw(revision: dioxus_compose::prelude::SyncSignal<u64>) {
        let mut revision = revision;
        *revision.write() += 1;
    }

    /// The ids disabled on this computer, from this device's prefs.
    pub fn disabled(&self) -> Vec<String> {
        crate::services::services().ui.prefs.get_untracked().disabled_extensions_for(&self.computer)
    }

    /// Enables or disables one extension for the next start of the extension host.
    pub fn set_enabled(&self, id: &str, enabled: bool) {
        let computer = self.computer.clone();
        let id = id.to_string();
        crate::services::services().ui.update_prefs(move |p| p.set_extension_enabled(&computer, &id, enabled));
        Self::redraw(self.revision);
    }

    /// Search for what the field says: after a pause in typing (`debounce`) or at once. An empty
    /// field goes back to the installed list.
    pub fn search(&self, query: &str, debounce: bool) {
        let seq = self.with(|s| s.begin_search(query));
        Self::redraw(self.revision);
        let Some(seq) = seq else { return };
        let (state, revision, q) = (self.state.clone(), self.revision, query.trim().to_string());
        let wait_state = state.clone();
        crate::services::run_shared(
            async move {
                if debounce {
                    tokio::time::sleep(std::time::Duration::from_millis(SEARCH_DEBOUNCE_MS)).await;
                    if !wait_state.lock().unwrap_or_else(|e| e.into_inner()).is_current(seq) {
                        return None;
                    }
                }
                Some(crate::services::services().server.ide_extension_search(&q, 0).await.map_err(|e| search_failure_text(&e)))
            },
            move |result| {
                let Some(result) = result else { return };
                if state.lock().unwrap_or_else(|e| e.into_inner()).finish_search(seq, result) {
                    Self::redraw(revision);
                }
            },
        );
    }

    /// Read the next page of the results.
    pub fn more(&self) {
        let Some((seq, q, offset)) = self.with(ExtState::begin_more) else { return };
        Self::redraw(self.revision);
        let (state, revision) = (self.state.clone(), self.revision);
        crate::services::run_shared(
            async move { crate::services::services().server.ide_extension_search(&q, offset).await.map_err(|e| search_failure_text(&e)) },
            move |result| {
                if state.lock().unwrap_or_else(|e| e.into_inner()).finish_more(seq, result) {
                    Self::redraw(revision);
                }
            },
        );
    }

    /// Install or uninstall `id` on the window's computer, then have the host read the scan again.
    pub fn run(&self, op: Op, id: &str, name: &str, host: &crate::editor_host::EditorHost) {
        if !self.can_manage() {
            crate::ui::cr::notice::post(NO_COMPUTER_NOTE);
            return;
        }
        if !self.with(|s| s.begin_op(id, op)) {
            crate::ui::cr::notice::post(BUSY_NOTE);
            return;
        }
        Self::redraw(self.revision);
        let (state, revision, host) = (self.state.clone(), self.revision, host.clone());
        let (computer, id, name) = (self.computer.clone(), id.to_string(), name.to_string());
        crate::services::run_shared(
            async move {
                let server = &crate::services::services().server;
                match op {
                    Op::Install => server.ide_extension_install(&computer, &id).await,
                    Op::Uninstall => server.ide_extension_uninstall(&computer, &id).await,
                }
            },
            move |result| {
                let outcome = match &result {
                    Ok(_) => Ok(()),
                    Err(e) => Err(op_failure_text(op, &name, e)),
                };
                let done = outcome.is_ok();
                state.lock().unwrap_or_else(|e| e.into_inner()).finish_op(outcome);
                if done {
                    crate::ui::cr::notice::post(op.done_text(&name));
                    host.send(crate::editor_host::HostCommand::RescanExtensions);
                }
                Self::redraw(revision);
            },
        );
    }
}

pub mod view {
    //! The panel. Its two view-only values are signals of the window's root scope, kept here so the
    //! shared window state does not grow for one tool window.

    use std::cell::Cell;

    use dioxus_compose::html::prelude::dioxus_core::ScopeId;
    use dioxus_compose::html::prelude::dioxus_elements::Key;
    use dioxus_compose::html::prelude::dioxus_signals::{ReadableExt, Signal, WritableExt};
    use dioxus_compose::html::prelude::*;

    use super::{installed_rows, needs_restart, result_rows, InstalledRow, Op, ResultRow, NO_COMPUTER_NOTE, RESTART_NOTE};
    use crate::editor_host::HostCommand;
    use crate::ui::editor_workbench::page::{icon, icon_px};
    use crate::ui::editor_workbench::Shared;

    #[derive(Clone, Copy)]
    struct Ui {
        /// The built-in group is open (it is long, so it starts closed).
        show_builtin: Signal<bool>,
    }

    thread_local! { static UI: Cell<Option<Ui>> = const { Cell::new(None) }; }

    fn ui() -> Ui {
        UI.with(|cell| {
            if let Some(ui) = cell.get() {
                return ui;
            }
            let ui = Ui { show_builtin: Signal::new_in_scope(false, ScopeId::ROOT) };
            cell.set(Some(ui));
            ui
        })
    }

    const FIELD: &str = "flex:1; min-width:0; border:0; background:none; outline:none; color:var(--text); font:inherit;";
    const BOX: &str = "width:auto; height:30px; cursor:text;";
    // #798: a row never grows past the panel (ellipsis drawing: dioxus-compose#184). The name and meta lines end in an ellipsis, the
    // description wraps, the version and the buttons keep their size, so 제거 and 설치 stay visible at any width.
    const ROW: &str = "display:flex; flex-direction:column; gap:2px; padding:var(--sp2) var(--sp2) var(--sp2) var(--sp3); width:100%; min-width:0; max-width:100%; overflow:hidden; box-sizing:border-box;";
    const LINE: &str = "display:flex; align-items:center; gap:var(--sp2); width:100%; min-width:0; max-width:100%; overflow:hidden;";
    const NAME: &str = "font-weight:var(--fw-medium); overflow:hidden; text-overflow:ellipsis; white-space:nowrap; min-width:0; flex:0 1 auto; display:block;";
    const VERSION: &str = "color:var(--muted); font-size:var(--fs-xs); flex:none;";
    const META: &str = "color:var(--muted); font-size:var(--fs-xs); overflow:hidden; text-overflow:ellipsis; white-space:nowrap; display:block; width:100%; min-width:0; max-width:100%;";
    /// The buttons get a line of their own at the row's end, right-aligned, so a long name never
    /// pushes them out of the panel (#798: on the name line 제거 was cut off in the docked panel;
    /// beside the publisher, the renderer gave the publisher no width).
    const ACTIONS: &str = "display:flex; align-items:center; justify-content:flex-end; gap:var(--sp2); width:100%;";
    const DESCRIPTION: &str = "color:var(--text-2); font-size:var(--fs-xs); white-space:normal; overflow-wrap:anywhere; word-break:break-word; display:block; width:100%; min-width:0; max-width:100%;";
    /// The two lists scroll down, never sideways (#798).
    const LIST: &str = "overflow-x:hidden;";
    const ACTION: &str = "border:1px solid var(--line); height:24px; padding:0 var(--sp2); font-size:var(--fs-xs); flex:none;";
    const NOTE: &str = "padding:var(--sp1) var(--sp3); color:var(--muted); font-size:var(--fs-xs); white-space:normal;";
    const GROUP: &str = "--d:0; font-weight:var(--fw-medium);";

    fn installed_row(s: &Shared, row: InstalledRow, op: Option<Op>, can_manage: bool, any_busy: bool) -> Element {
        let (toggle_s, remove_s, remove_host) = (s.extensions.clone(), s.extensions.clone(), s.host.clone());
        let (id, name) = (row.ext.id.clone(), row.ext.display_name.clone());
        let (toggle_id, remove_id, remove_name) = (id.clone(), id.clone(), name.clone());
        let enabled = row.enabled;
        let toggle_text = if enabled { "사용 안 함" } else { "사용" };
        let toggle_label = format!("{name} {toggle_text}");
        let remove_label = format!("{name} 제거");
        let meta = row.meta();
        let label = row.label();
        let name_style = if enabled { NAME.to_string() } else { format!("{NAME} color:var(--muted);") };
        rsx! {
            div { key: "{id}", role: "listitem", "aria-label": "{label}", style: "{ROW}",
                div { style: "{LINE}",
                    span { style: "{name_style}", "{name}" }
                    span { style: "{VERSION}", "{row.ext.version}" }
                }
                if !row.ext.description.is_empty() { div { style: "{DESCRIPTION}", "{row.ext.description}" } }
                if !meta.is_empty() { div { style: "{META}", "{meta}" } }
                div { style: "{ACTIONS}",
                    if let Some(op) = op {
                        span { role: "status", style: "{VERSION}", "{op.doing()}" }
                    } else {
                        button { class: "ed-pj", style: "{ACTION}", "aria-label": "{toggle_label}", "aria-pressed": "{!enabled}",
                            onclick: move |_| toggle_s.set_enabled(&toggle_id, !enabled), "{toggle_text}" }
                        if !row.ext.builtin && can_manage {
                            button { class: "ed-pj", style: "{ACTION}", "aria-label": "{remove_label}", disabled: any_busy,
                                onclick: move |_| remove_s.run(Op::Uninstall, &remove_id, &remove_name, &remove_host), "제거" }
                        }
                    }
                }
            }
        }
    }

    fn result_row(s: &Shared, row: ResultRow, op: Option<Op>, can_manage: bool, any_busy: bool) -> Element {
        let (install_s, install_host) = (s.extensions.clone(), s.host.clone());
        let (id, name) = (row.ext.id.clone(), row.ext.display_name.clone());
        let (install_id, install_name) = (id.clone(), name.clone());
        let install_label = format!("{name} 설치");
        let meta = row.meta();
        let label = row.label();
        let installed = row.installed.is_some();
        rsx! {
            div { key: "{id}", role: "listitem", "aria-label": "{label}", style: "{ROW}",
                div { style: "{LINE}",
                    span { style: "{NAME}", "{name}" }
                    span { style: "{VERSION}", "{row.ext.version}" }
                }
                if !row.ext.description.is_empty() { div { style: "{DESCRIPTION}", "{row.ext.description}" } }
                if !meta.is_empty() { div { style: "{META}", "{meta}" } }
                div { style: "{ACTIONS}",
                    if let Some(op) = op {
                        span { role: "status", style: "{VERSION}", "{op.doing()}" }
                    } else if !installed && can_manage {
                        button { class: "ed-pj", style: "{ACTION}", "aria-label": "{install_label}", disabled: any_busy,
                            onclick: move |_| install_s.run(Op::Install, &install_id, &install_name, &install_host), "설치" }
                    }
                }
            }
        }
    }

    pub fn panel(s: &Shared) -> Element {
        let ui = ui();
        let mut show_builtin = ui.show_builtin;
        let builtin_open = *show_builtin.read();
        let mut stripes = s.stripes;
        let ext = s.extensions.clone();
        let st = ext.snapshot();
        let (scan, connected) = {
            let state = s.host.state.lock().unwrap();
            (state.extensions.clone(), state.connected)
        };
        let disabled = ext.disabled();
        let can_manage = ext.can_manage();
        let any_busy = st.busy.is_some();
        let searching = st.load != super::SearchLoad::Idle;
        let restart = needs_restart(&scan, &disabled, st.touched);
        let (user_rows, builtin_rows) = installed_rows(&scan, &disabled);
        let (user_count, builtin_count) = (user_rows.len(), builtin_rows.len());
        let results = result_rows(&st.results, &scan);
        let status = st.status_text();
        let has_more = st.has_more();
        let loading_more = st.loading_more;
        let query = st.query.clone();
        let (input_s, key_s, clear_s, more_s, refresh_host) = (ext.clone(), ext.clone(), ext.clone(), ext.clone(), s.host.clone());
        let enter_query = query.clone();
        let list_note = if !connected && !scan.loaded {
            Some("에디터가 연결되면 설치된 확장이 표시됩니다.")
        } else if scan.loaded && user_count == 0 {
            Some("설치한 확장이 없습니다. 위에서 검색해 설치하세요.")
        } else {
            None
        };
        rsx! {
            section { class: "ed-tw ed-left", "aria-label": "확장",
                div { class: "ed-twh",
                    b { class: "ed-twt", "확장" }
                    div { class: "spacer" }
                    button { class: "ed-ib", title: "새로 고침", "aria-label": "설치된 확장 새로 고침", disabled: scan.rescanning || !connected,
                        onclick: move |_| { refresh_host.send(HostCommand::RescanExtensions); }, {icon("refresh")} }
                    button { class: "ed-ib", title: "숨기기", "aria-label": "확장 창 숨기기", onclick: move |_| stripes.write().hide(crate::ui::editor::Side::Left), {icon("close")} }
                }
                div { role: "search", "aria-label": "확장 검색", style: "display:flex; flex-direction:column; gap:var(--sp1); padding:var(--sp2) var(--sp2) var(--sp1); flex:none;",
                    div { class: "ed-find", style: "{BOX}",
                        {icon("search")}
                        input { style: "{FIELD}", "aria-label": "Open VSX에서 확장 검색", placeholder: "Open VSX에서 확장 검색", value: "{query}", autofocus: true,
                            oninput: move |event| input_s.search(&event.value(), true),
                            onkeydown: move |event| {
                                match event.key() {
                                    Key::Enter => { event.stop_propagation(); key_s.search(&enter_query, false); }
                                    Key::Escape => { event.stop_propagation(); key_s.search("", false); }
                                    _ => {}
                                }
                            },
                        }
                        if searching {
                            button { class: "ed-ib", style: "width:22px; height:22px;", title: "검색 지우기", "aria-label": "검색 지우기", onclick: move |_| clear_s.search("", false), {icon_px("close", 12)} }
                        }
                    }
                    if let Some(text) = status {
                        div { role: "status", style: "{NOTE}", "{text}" }
                    }
                }
                if restart {
                    div { role: "status", style: "margin:0 var(--sp2) var(--sp2); padding:var(--sp2); background:var(--c-orange-soft); color:var(--text); font-size:var(--fs-xs); white-space:normal; flex:none;", "{RESTART_NOTE}" }
                }
                if let Some(error) = st.op_error.clone() {
                    div { role: "alert", style: "margin:0 var(--sp2) var(--sp2); padding:var(--sp2); border:1px solid var(--line); color:var(--text); font-size:var(--fs-xs); white-space:pre-wrap; flex:none;", "{error}" }
                }
                if let Some(error) = scan.error.clone() {
                    div { role: "alert", style: "{NOTE}", "{error}" }
                }
                if !can_manage {
                    div { role: "note", style: "{NOTE}", "{NO_COMPUTER_NOTE}" }
                }
                if searching {
                    div { class: "ed-tree", role: "list", "aria-label": "검색 결과", style: "{LIST}",
                        for row in results {
                            {
                                let op = st.op_of(&row.ext.id);
                                result_row(s, row, op, can_manage, any_busy)
                            }
                        }
                        if has_more {
                            div { style: "padding:var(--sp2) var(--sp3);",
                                button { class: "ed-pj", style: "{ACTION}", disabled: loading_more, onclick: move |_| more_s.more(),
                                    if loading_more { "불러오는 중…" } else { "더 보기" }
                                }
                            }
                        }
                    }
                } else {
                    div { class: "ed-tree", role: "list", "aria-label": "설치된 확장", style: "{LIST}",
                        div { class: "ed-tn", role: "heading", "aria-level": "3", style: "{GROUP}",
                            span { "설치됨" }
                            div { class: "spacer" }
                            span { class: "ed-sc", "aria-hidden": "true", "{user_count}" }
                        }
                        if let Some(note) = list_note {
                            div { style: "{NOTE}", "{note}" }
                        }
                        for row in user_rows {
                            {
                                let op = st.op_of(&row.ext.id);
                                installed_row(s, row, op, can_manage, any_busy)
                            }
                        }
                        div { class: "ed-tn", role: "button", tabindex: "0", "aria-expanded": "{builtin_open}", "aria-label": "기본 제공 확장 {builtin_count}개", style: "{GROUP}",
                            onclick: move |_| show_builtin.set(!builtin_open),
                            onkeydown: move |event| {
                                if matches!(event.key(), Key::Enter) || matches!(event.key(), Key::Character(ref c) if c == " ") {
                                    event.stop_propagation();
                                    event.prevent_default();
                                    show_builtin.set(!builtin_open);
                                }
                            },
                            if builtin_open { {icon_px("chev-dn", 12)} } else { {icon_px("chev", 12)} }
                            span { "기본 제공" }
                            div { class: "spacer" }
                            span { class: "ed-sc", "aria-hidden": "true", "{builtin_count}" }
                        }
                        if builtin_open {
                            for row in builtin_rows {
                                {
                                    let op = st.op_of(&row.ext.id);
                                    installed_row(s, row, op, can_manage, any_busy)
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[cfg(test)]
    mod fit_tests {
        use super::*;

        #[test]
        fn fr_l31_14_rows_fit_the_panel_and_keep_their_buttons() {
            for style in [ROW, LINE, NAME, META, DESCRIPTION] {
                assert!(style.contains("min-width:0"), "shrinks with the panel: {style}");
            }
            assert!(ROW.contains("max-width:100%") && ROW.contains("overflow:hidden"), "a row is never wider than the panel");
            for style in [NAME, META] {
                assert!(style.contains("text-overflow:ellipsis"), "long text ends in an ellipsis: {style}");
            }
            assert!(DESCRIPTION.contains("overflow-wrap:anywhere"), "a long description wraps inside the row");
            assert!(ACTION.contains("flex:none") && VERSION.contains("flex:none"), "buttons and the version never shrink away");
            for style in [ROW, LINE, META, DESCRIPTION, ACTIONS] {
                assert!(style.contains("width:100%"), "bounded by the panel, not by its text: {style}");
            }
            for style in [NAME, META, DESCRIPTION] {
                assert!(style.contains("display:block"), "ellipsis and wrapping need a block box (dioxus-compose#184): {style}");
            }
            assert!(LIST.contains("overflow-x:hidden"), "the lists never scroll sideways");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ext(id: &str, display: &str, version: &str, builtin: bool) -> InstalledExtension {
        let (publisher, name) = id.split_once('.').unwrap();
        InstalledExtension {
            id: id.into(),
            name: name.into(),
            publisher: publisher.into(),
            version: version.into(),
            display_name: display.into(),
            description: String::new(),
            builtin,
        }
    }

    fn scan() -> ExtensionScan {
        let installed = vec![
            ext("vscode.json-language-features", "JSON Language Features", "1.0.0", true),
            ext("rust-lang.rust-analyzer", "rust-analyzer", "0.4.1", false),
            ext("RedHat.java", "Language Support for Java", "1.40.0", false),
        ];
        let mut scan = ExtensionScan::default();
        scan.connected(installed.clone(), &installed);
        scan
    }

    fn gallery(id: &str) -> GalleryExtension {
        let (namespace, name) = id.split_once('.').unwrap();
        GalleryExtension { id: id.into(), name: name.into(), namespace: namespace.into(), display_name: name.into(), version: "2.0.0".into(), ..Default::default() }
    }

    fn page(ids: &[&str], total: u64) -> GallerySearch {
        GallerySearch { offset: 0, total, extensions: ids.iter().map(|id| gallery(id)).collect() }
    }

    #[test]
    fn fr_l31_14_installed_rows_are_grouped_sorted_and_carry_the_enabled_state() {
        let (user, builtin) = installed_rows(&scan(), &["REDHAT.JAVA".into()]);
        let names: Vec<&str> = user.iter().map(|r| r.ext.display_name.as_str()).collect();
        assert_eq!(names, ["Language Support for Java", "rust-analyzer"]);
        assert!(!user[0].enabled, "a disabled id matches without regard to case");
        assert!(user[1].enabled);
        assert_eq!(user[0].meta(), "RedHat · 사용 안 함");
        assert_eq!(user[1].meta(), "rust-lang");
        assert_eq!(user[1].label(), "rust-analyzer 0.4.1, rust-lang, 사용");
        assert_eq!(builtin.len(), 1);
        assert!(builtin[0].ext.builtin && builtin[0].enabled);
    }

    #[test]
    fn fr_l31_14_no_restart_notice_until_the_enabled_set_changes() {
        let mut scan = scan();
        assert!(!needs_restart(&ExtensionScan::default(), &[], false), "nothing to say before the first scan");
        assert!(!needs_restart(&scan, &[], false));
        // Disable: the extension host still runs it. Enabling again takes the notice away.
        assert!(needs_restart(&scan, &["redhat.java".into()], false));
        assert!(!needs_restart(&scan, &[], false));
        // A disabled id that is not installed changes nothing.
        assert!(!needs_restart(&scan, &["not.installed".into()], false));
        // A rescan shows a new extension, a new version or a missing one.
        scan.finish_rescan::<String>(Ok(vec![ext("rust-lang.rust-analyzer", "rust-analyzer", "0.4.2", false)]));
        assert!(needs_restart(&scan, &[], false));
    }

    #[test]
    fn fr_l31_14_an_extension_disabled_at_start_needs_no_restart_until_enabled() {
        let installed = vec![ext("a.one", "One", "1.0.0", false), ext("a.two", "Two", "1.0.0", false)];
        let mut scan = ExtensionScan::default();
        scan.connected(installed.clone(), &installed[..1]);
        assert!(!needs_restart(&scan, &["a.two".into()], false));
        assert!(needs_restart(&scan, &[], false), "enabling it asks for a restart");
        // Uninstalling an extension that was not running changes nothing for the extension host,
        // but a finished install or uninstall always shows the notice.
        assert!(needs_restart(&scan, &["a.two".into()], true));
    }

    #[test]
    fn fr_l31_14_a_failed_rescan_keeps_the_list_and_says_why() {
        let mut scan = scan();
        scan.begin_rescan();
        assert!(scan.rescanning);
        scan.finish_rescan(Err::<Vec<InstalledExtension>, _>("socket closed"));
        assert!(!scan.rescanning);
        assert_eq!(scan.installed.len(), 3);
        assert_eq!(scan.error.as_deref(), Some("설치된 확장을 다시 읽지 못했습니다: socket closed"));
        scan.finish_rescan::<String>(Ok(Vec::new()));
        assert!(scan.error.is_none() && scan.installed.is_empty());
    }

    #[test]
    fn fr_l31_14_search_results_know_what_is_installed() {
        let rows = result_rows(&[gallery("redhat.java"), gallery("golang.go")], &scan());
        assert_eq!(rows[0].installed.as_deref(), Some("1.40.0"), "ids match without regard to case");
        assert_eq!(rows[1].installed, None);
        assert_eq!(rows[0].meta(), "redhat · 설치됨 1.40.0");
        assert_eq!(rows[1].label(), "go 2.0.0, golang, 설치되지 않음");
        let rich = ResultRow {
            ext: GalleryExtension { downloads: 7_713_255, verified: true, deprecated: true, ..gallery("rust-lang.rust-analyzer") },
            installed: None,
        };
        assert_eq!(rich.meta(), "rust-lang · 확인된 게시자 · 다운로드 771만 · 지원 중단");
    }

    #[test]
    fn fr_l31_14_the_capture_hook_is_debug_only_and_seeds_each_state() {
        assert_eq!(capture_hook_with(Some("search"), false), None, "a release build ignores it");
        assert_eq!(capture_hook_with(None, true), None);
        assert_eq!(capture_hook_with(Some("nope"), true), None);
        assert_eq!(capture_hook_with(Some(" open "), true), Some(Capture::Open));
        assert_eq!(ExtState::sample(Capture::Open), ExtState::default());
        let search = ExtState::sample(Capture::Search);
        assert_eq!((search.query.as_str(), search.results.len(), search.total), ("rust", 4, 566));
        assert!(search.has_more() && search.busy.is_none());
        assert_eq!(ExtState::sample(Capture::Busy).op_of("vadimcn.vscode-lldb"), Some(Op::Install));
        let restart = ExtState::sample(Capture::Restart);
        assert!(restart.touched && restart.op_error.is_some());
        // The sample extensions join the scan as running ones, so they alone raise no notice.
        let mut scan = scan();
        scan.add_capture_samples();
        assert_eq!(scan.installed.len(), 4, "rust-analyzer was there already");
        assert!(scan.running.contains(&("tamasfe.even-better-toml".to_string(), "0.21.2".to_string())));
        assert!(!Capture::Open.adds_samples() && Capture::Installed.adds_samples());
    }

    #[test]
    fn fr_l31_14_download_counts_are_short() {
        assert_eq!(downloads_text(0), "0");
        assert_eq!(downloads_text(950), "950");
        assert_eq!(downloads_text(1_000), "1천");
        assert_eq!(downloads_text(1_250), "1.2천");
        assert_eq!(downloads_text(35_474), "3.5만");
        assert_eq!(downloads_text(350_000), "35만");
        assert_eq!(downloads_text(7_713_255), "771만");
        assert_eq!(downloads_text(120_000_000), "1.2억");
    }

    #[test]
    fn fr_l31_14_a_stale_search_answer_is_dropped() {
        let mut st = ExtState::default();
        let first = st.begin_search("rus").unwrap();
        assert_eq!(st.load, SearchLoad::Loading);
        let second = st.begin_search("rust").unwrap();
        assert!(!st.is_current(first) && st.is_current(second));
        assert!(!st.finish_search(first, Ok(page(&["a.old"], 1))), "the answer for the older text is dropped");
        assert!(st.results.is_empty());
        assert!(st.finish_search(second, Ok(page(&["rust-lang.rust-analyzer"], 566))));
        assert_eq!(st.load, SearchLoad::Ready);
        assert_eq!(st.status_text().as_deref(), Some("확장 566개 중 1개"));
        // Emptying the field goes back to the installed list and drops what is on the way.
        let third = st.begin_search("x").unwrap();
        assert_eq!(st.begin_search("  "), None);
        assert_eq!(st.load, SearchLoad::Idle);
        assert!(st.results.is_empty());
        assert!(!st.finish_search(third, Ok(page(&["a.b"], 1))));
        assert_eq!(st.status_text(), None);
    }

    #[test]
    fn fr_l31_14_search_states_have_korean_text() {
        let mut st = ExtState::default();
        let seq = st.begin_search("zzz").unwrap();
        assert_eq!(st.status_text().as_deref(), Some("Open VSX에서 검색하는 중입니다…"));
        st.finish_search(seq, Ok(page(&[], 0)));
        assert_eq!(st.status_text().as_deref(), Some("일치하는 확장이 없습니다."));
        let seq = st.begin_search("zzzz").unwrap();
        st.finish_search(seq, Err("확장 갤러리(Open VSX)에서 결과를 받지 못했습니다: 500".into()));
        assert_eq!(st.load, SearchLoad::Failed("확장 갤러리(Open VSX)에서 결과를 받지 못했습니다: 500".into()));
        assert!(!st.has_more());
    }

    #[test]
    fn fr_l31_14_more_results_are_appended_once_and_end_when_a_page_adds_nothing() {
        let mut st = ExtState::default();
        let seq = st.begin_search("a").unwrap();
        st.finish_search(seq, Ok(page(&["a.one", "a.two"], 5)));
        assert!(st.has_more());
        let (more_seq, q, offset) = st.begin_more().unwrap();
        assert_eq!((more_seq, q.as_str(), offset), (seq, "a", 2));
        assert_eq!(st.begin_more(), None, "one page at a time");
        assert!(st.finish_more(more_seq, Ok(page(&["A.TWO", "a.three"], 5))));
        let ids: Vec<&str> = st.results.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["a.one", "a.two", "a.three"]);
        assert!(st.has_more());
        let (more_seq, _, offset) = st.begin_more().unwrap();
        assert_eq!(offset, 3);
        assert!(st.finish_more(more_seq, Ok(page(&["a.one"], 5))));
        assert!(!st.has_more(), "a page with nothing new ends the list");
        // A page for an older search is dropped.
        let seq = st.begin_search("b").unwrap();
        st.finish_search(seq, Ok(page(&["b.one"], 9)));
        let (old, _, _) = st.begin_more().unwrap();
        st.begin_search("c");
        assert!(!st.finish_more(old, Ok(page(&["b.two"], 9))));
    }

    #[test]
    fn fr_l31_14_one_install_or_uninstall_at_a_time() {
        let mut st = ExtState::default();
        assert!(st.begin_op("RedHat.java", Op::Install));
        assert_eq!(st.op_of("redhat.JAVA"), Some(Op::Install));
        assert_eq!(st.op_of("golang.go"), None);
        assert!(!st.begin_op("golang.go", Op::Uninstall), "a second one waits");
        st.finish_op(Err("Language Support for Java 설치 실패: x".into()));
        assert_eq!(st.busy, None);
        assert!(!st.touched);
        assert_eq!(st.op_error.as_deref(), Some("Language Support for Java 설치 실패: x"));
        assert!(st.begin_op("golang.go", Op::Uninstall));
        assert_eq!(st.op_error, None, "the last failure goes when the next one starts");
        st.finish_op(Ok(()));
        assert!(st.touched);
    }

    #[test]
    fn fr_l31_14_failures_have_korean_reasons() {
        let status = |status: u16, message: &str| ServerError::Status { status, message: message.into() };
        assert_eq!(
            op_failure_text(Op::Install, "Go", &status(409, "an install is already running on this computer")),
            "Go 설치 실패: 이 컴퓨터에서 다른 설치 또는 제거가 진행 중입니다. 끝난 뒤 다시 시도하세요."
        );
        assert_eq!(op_failure_text(Op::Uninstall, "Go", &status(503, "x")), "Go 제거 실패: 서버에 OSE가 설치되어 있지 않습니다.");
        assert_eq!(
            op_failure_text(Op::Install, "Go", &status(502, "install failed: exit status: 1\nExtension 'golang.go' not found.")),
            "Go 설치 실패: install failed: exit status: 1\nExtension 'golang.go' not found."
        );
        assert_eq!(op_failure_text(Op::Install, "Go", &ServerError::Unauthorized("no".into())), "Go 설치 실패: 서버가 인증을 거부했습니다.");
        assert_eq!(op_failure_text(Op::Install, "Go", &ServerError::Client("tls".into())), "Go 설치 실패: 서버에 연결할 수 없습니다.");
        assert_eq!(search_failure_text(&status(502, "the extension gallery answered 500")), "확장 갤러리(Open VSX)에서 결과를 받지 못했습니다: the extension gallery answered 500");
        assert_eq!(search_failure_text(&status(400, "q must not be empty")), "검색어를 보낼 수 없습니다: q must not be empty");
        assert_eq!(search_failure_text(&status(500, "boom")), "서버 오류(500): boom");
        assert_eq!(Op::Install.done_text("Go"), "Go 확장을 설치했습니다.");
        assert_eq!(Op::Uninstall.doing(), "제거 중…");
    }
}
