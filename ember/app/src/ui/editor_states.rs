//! FR-L31.5: what the Ember 에디터 window shows about the state of a file and of the connection, as
//! Code-OSS does: a failed save (with 다시 시도), a change on disk while the buffer has unsaved edits
//! (비교, 덮어쓰기, 디스크 버전으로 되돌리기; a clean buffer reloads without asking), a file deleted on
//! disk (the tab is struck through and says 삭제됨), a lost connection (a banner with 다시 연결), and
//! auto-save (Code-OSS `files.autoSave`: off, afterDelay with `files.autoSaveDelay` 1000 ms, and
//! onFocusChange).
//!
//! The pure parts are here and tested in `ember/app/tests/fr_l31_save_states.rs`; [`view`] draws them and is
//! called from the window (`editor_workbench.rs`) at a few points only, so the window's own code
//! changes little.

use std::collections::HashMap;

use ember_editor::ExternalChange;
use serde::{Deserialize, Serialize};

/// Code-OSS `files.autoSave`. Stored per device with the keymap (`Prefs::auto_save`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AutoSave {
    /// `off`, Code-OSS's default.
    #[default]
    #[serde(rename = "off")]
    Off,
    /// `afterDelay`: a dirty document is saved [`AUTO_SAVE_DELAY_MS`] after the last edit.
    #[serde(rename = "afterDelay")]
    AfterDelay,
    /// `onFocusChange`: a dirty document is saved when its editor stops being the active one.
    #[serde(rename = "onFocusChange")]
    OnFocusChange,
}

/// Code-OSS's default `files.autoSaveDelay`.
pub const AUTO_SAVE_DELAY_MS: u64 = 1000;

impl AutoSave {
    pub const ALL: [AutoSave; 3] = [AutoSave::Off, AutoSave::AfterDelay, AutoSave::OnFocusChange];

    /// The Code-OSS setting value.
    pub fn setting(self) -> &'static str {
        match self {
            AutoSave::Off => "off",
            AutoSave::AfterDelay => "afterDelay",
            AutoSave::OnFocusChange => "onFocusChange",
        }
    }

    /// The menu label.
    pub fn label(self) -> &'static str {
        match self {
            AutoSave::Off => "끄기",
            AutoSave::AfterDelay => "입력을 멈추고 1초 뒤",
            AutoSave::OnFocusChange => "다른 탭으로 옮길 때",
        }
    }
}

/// Why a save is about to happen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveTrigger {
    /// The auto-save delay after the last edit ran out.
    Delay,
    /// The document's editor stopped being the active one.
    FocusChange,
}

/// Whether an auto-save for `trigger` saves a document now. Only a dirty document is saved; one whose
/// file changed on disk (the person chooses first, as Code-OSS's save conflict) or was deleted (an
/// auto-save does not bring a deleted file back) is left alone.
pub fn should_auto_save(mode: AutoSave, trigger: SaveTrigger, dirty: bool, external: Option<ExternalChange>) -> bool {
    let wanted = match (mode, trigger) {
        (AutoSave::AfterDelay, SaveTrigger::Delay) => true,
        (AutoSave::OnFocusChange, SaveTrigger::FocusChange) => true,
        _ => false,
    };
    wanted && dirty && external.is_none()
}

/// What a manual save says instead of writing while the file on disk is newer than the buffer.
pub const CONFLICT_SAVE_NOTICE: &str =
    "디스크의 파일이 더 새롭습니다. 비교한 뒤 덮어쓰거나 디스크 버전으로 되돌리세요";

/// What a save says while the connection is lost.
pub const LOST_SAVE_NOTICE: &str = "서버와의 연결이 끊어져 저장할 수 없습니다. 다시 연결한 뒤 저장하세요";

/// Whether a manual save (Cmd/Ctrl+S) writes now. With the file changed on disk it does not, as in
/// Code-OSS where the save fails with a conflict and offers compare and overwrite; 덮어쓰기 is the
/// way to write anyway.
pub fn manual_save_gate(external: Option<ExternalChange>) -> Result<(), &'static str> {
    match external {
        Some(ExternalChange::ChangedWhileDirty) => Err(CONFLICT_SAVE_NOTICE),
        _ => Ok(()),
    }
}

/// The debounce of `afterDelay`: every edit of a path starts a new wait, and only the newest wait of a
/// path may save.
#[derive(Debug, Default)]
pub struct AutoSaveTimer {
    latest: HashMap<String, u64>,
    next: u64,
}

impl AutoSaveTimer {
    /// An edit of `path`: the number of the wait it starts.
    pub fn edited(&mut self, path: &str) -> u64 {
        self.next += 1;
        self.latest.insert(path.to_owned(), self.next);
        self.next
    }

    /// Wait `seq` of `path` ran out: true when no later edit started another one.
    pub fn fire(&mut self, path: &str, seq: u64) -> bool {
        if self.latest.get(path) == Some(&seq) {
            self.latest.remove(path);
            true
        } else {
            false
        }
    }

    /// Forget the wait of a path (closed, or saved by other means).
    pub fn forget(&mut self, path: &str) {
        self.latest.remove(path);
    }
}

/// A button of a document's bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocAction {
    /// Save again after a failure.
    Retry,
    /// Hide the save failure.
    Dismiss,
    /// Show the disk version beside the buffer.
    Compare,
    /// Write the buffer over the newer disk version.
    Overwrite,
    /// Throw the buffer away and take the disk version.
    Revert,
}

impl DocAction {
    pub fn label(self) -> &'static str {
        match self {
            DocAction::Retry => "다시 시도",
            DocAction::Dismiss => "닫기",
            DocAction::Compare => "비교",
            DocAction::Overwrite => "덮어쓰기",
            DocAction::Revert => "디스크 버전으로 되돌리기",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarTone {
    Error,
    Warning,
}

/// The bar over the code area of one document: Code-OSS's save error notification and its save
/// conflict choice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocBar {
    pub tone: BarTone,
    pub text: String,
    pub actions: Vec<DocAction>,
}

/// The bar a document shows, if any. A failed save comes first (it says why the edits are not on
/// disk); a change on disk under unsaved edits asks how to go on. A deleted file shows no bar, only the
/// tab mark ([`tab_label`]), as in Code-OSS.
pub fn doc_bar(name: &str, external: Option<ExternalChange>, save_error: Option<&str>) -> Option<DocBar> {
    let name = crate::ui::editor::display_name(name);
    if let Some(error) = save_error {
        let reason = crate::ui::editor::file_failure_reason(error);
        let mut actions = vec![DocAction::Retry];
        if external == Some(ExternalChange::ChangedWhileDirty) {
            actions.extend([DocAction::Compare, DocAction::Revert]);
        }
        actions.push(DocAction::Dismiss);
        return Some(DocBar { tone: BarTone::Error, text: format!("{name}을(를) 저장하지 못했습니다: {reason}"), actions });
    }
    match external {
        Some(ExternalChange::ChangedWhileDirty) => Some(DocBar {
            tone: BarTone::Warning,
            text: format!(
                "{name} 파일이 디스크에서 바뀌었습니다. 저장하지 않은 변경 사항과 비교하거나, 변경 사항으로 덮어쓰거나, 디스크 버전으로 되돌리세요"
            ),
            actions: vec![DocAction::Compare, DocAction::Overwrite, DocAction::Revert],
        }),
        _ => None,
    }
}

/// Whether the tab of a document shows the deleted mark.
pub fn is_deleted(external: Option<ExternalChange>) -> bool {
    external == Some(ExternalChange::Deleted)
}

/// The tab text: the name, and "(삭제됨)" after it when the file was deleted on disk.
pub fn tab_label(name: &str, deleted: bool) -> String {
    if deleted {
        format!("{name} (삭제됨)")
    } else {
        name.to_owned()
    }
}

/// The tab text's style: struck through when the file was deleted on disk.
pub fn tab_label_style(deleted: bool) -> &'static str {
    if deleted {
        "text-decoration:line-through;"
    } else {
        ""
    }
}

/// The connection banner: its text and the button it offers (none while a reconnect is running).
pub fn connection_banner(reconnecting: bool) -> (&'static str, Option<&'static str>) {
    if reconnecting {
        ("서버에 다시 연결하는 중입니다", None)
    } else {
        ("서버와의 연결이 끊어졌습니다. 저장하지 않은 변경 사항은 이 창에 남아 있습니다", Some("다시 연결"))
    }
}

/// One step of putting the tabs back after a reconnect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reopen {
    /// Open the file and show this buffer in it (the session leaves it clean when it equals the disk).
    Restore { path: String, text: String },
    /// Open the file (the window had no buffer for it).
    Open(String),
    /// Make this tab the session's active editor again.
    Focus(String),
}

/// The steps after a reconnect: every tab in order, with the buffer the window holds for it, then the
/// current tab focused. The new session knows nothing of the old one, so the window's buffers are the
/// only copy of unsaved edits (and of edits typed while the connection was lost).
pub fn reopen_plan(tabs: &[(String, Option<String>)], current: Option<&str>) -> Vec<Reopen> {
    let mut plan: Vec<Reopen> = tabs
        .iter()
        .map(|(path, text)| match text {
            Some(text) => Reopen::Restore { path: path.clone(), text: text.clone() },
            None => Reopen::Open(path.clone()),
        })
        .collect();
    if let Some(current) = current.filter(|c| tabs.iter().any(|(p, _)| p == c)) {
        plan.push(Reopen::Focus(current.to_owned()));
    }
    plan
}

/// Which side a compare row comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompareKind {
    Same,
    /// Only in the disk version.
    Disk,
    /// Only in the buffer.
    Mine,
}

/// One row of the compare view: the line numbers (from 1) on each side and the text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompareRow {
    pub kind: CompareKind,
    pub disk_line: Option<u32>,
    pub mine_line: Option<u32>,
    pub text: String,
}

fn lines_of(text: &str) -> Vec<&str> {
    let t = text.strip_suffix('\n').unwrap_or(text);
    if t.is_empty() {
        Vec::new()
    } else {
        t.split('\n').collect()
    }
}

/// The disk version against the buffer, line by line (both `\n` text). Common lines at both ends are
/// paired first; the middle is a longest-common-subsequence diff while it fits
/// `MAX_DIFF_MATRIX_CELLS`, and past that every disk line of the middle is shown removed and every
/// buffer line added, so a large file costs no large matrix.
pub fn compare_rows(disk: &str, mine: &str) -> Vec<CompareRow> {
    let disk = disk.replace("\r\n", "\n");
    let mine = mine.replace("\r\n", "\n");
    let (a, b) = (lines_of(&disk), lines_of(&mine));
    let prefix = a.iter().zip(b.iter()).take_while(|(x, y)| x == y).count();
    let suffix = a[prefix..].iter().rev().zip(b[prefix..].iter().rev()).take_while(|(x, y)| x == y).count();
    let (am, bm) = (&a[prefix..a.len() - suffix], &b[prefix..b.len() - suffix]);
    let mut rows = Vec::with_capacity(a.len().max(b.len()));
    let same = |rows: &mut Vec<CompareRow>, i: usize, j: usize, text: &str| {
        rows.push(CompareRow { kind: CompareKind::Same, disk_line: Some(i as u32 + 1), mine_line: Some(j as u32 + 1), text: text.to_owned() });
    };
    for (i, line) in a[..prefix].iter().enumerate() {
        same(&mut rows, i, i, line);
    }
    let disk_row = |i: usize, text: &str| CompareRow { kind: CompareKind::Disk, disk_line: Some(i as u32 + 1), mine_line: None, text: text.to_owned() };
    let mine_row = |j: usize, text: &str| CompareRow { kind: CompareKind::Mine, disk_line: None, mine_line: Some(j as u32 + 1), text: text.to_owned() };
    let (m, n) = (am.len(), bm.len());
    if m.saturating_mul(n) > crate::ui::editor::MAX_DIFF_MATRIX_CELLS {
        rows.extend(am.iter().enumerate().map(|(k, t)| disk_row(prefix + k, t)));
        rows.extend(bm.iter().enumerate().map(|(k, t)| mine_row(prefix + k, t)));
    } else {
        // dp[i][j]: the longest common subsequence of am[i..] and bm[j..].
        let mut dp = vec![vec![0u32; n + 1]; m + 1];
        for i in (0..m).rev() {
            for j in (0..n).rev() {
                dp[i][j] = if am[i] == bm[j] { dp[i + 1][j + 1] + 1 } else { dp[i + 1][j].max(dp[i][j + 1]) };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < m || j < n {
            if i < m && j < n && am[i] == bm[j] {
                same(&mut rows, prefix + i, prefix + j, am[i]);
                i += 1;
                j += 1;
            } else if j < n && (i == m || dp[i][j + 1] >= dp[i + 1][j]) {
                rows.push(mine_row(prefix + j, bm[j]));
                j += 1;
            } else {
                rows.push(disk_row(prefix + i, am[i]));
                i += 1;
            }
        }
    }
    for k in 0..suffix {
        let (i, j) = (a.len() - suffix + k, b.len() - suffix + k);
        same(&mut rows, i, j, a[i]);
    }
    rows
}

/// The rows the compare view draws: every changed row with `context` unchanged rows around it; `None`
/// stands for a run of unchanged rows left out.
pub fn visible_compare_rows(rows: &[CompareRow], context: usize) -> Vec<Option<&CompareRow>> {
    let changed: Vec<usize> = rows.iter().enumerate().filter(|(_, r)| r.kind != CompareKind::Same).map(|(i, _)| i).collect();
    let mut keep = vec![false; rows.len()];
    for &i in &changed {
        let (lo, hi) = (i.saturating_sub(context), (i + context).min(rows.len().saturating_sub(1)));
        for k in keep.iter_mut().take(hi + 1).skip(lo) {
            *k = true;
        }
    }
    let mut out = Vec::new();
    let mut gap = false;
    for (i, row) in rows.iter().enumerate() {
        if keep[i] {
            if gap {
                out.push(None);
                gap = false;
            }
            out.push(Some(row));
        } else {
            gap = true;
        }
    }
    if gap && !out.is_empty() {
        out.push(None);
    }
    out
}

/// The compare view of one file, as the window holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompareView {
    Loading { path: String },
    Ready { path: String, rows: Vec<CompareRow> },
    Failed { path: String, reason: String },
}

impl CompareView {
    pub fn path(&self) -> &str {
        match self {
            CompareView::Loading { path } | CompareView::Ready { path, .. } | CompareView::Failed { path, .. } => path,
        }
    }

    /// Whether the view covers the code area while `current` is the active tab.
    pub fn visible(&self, current: Option<&str>) -> bool {
        current == Some(self.path())
    }
}

/// The compare view's title.
pub fn compare_title(name: &str) -> String {
    format!("{} (디스크 버전 \u{2194} 내 변경 사항)", crate::ui::editor::display_name(name))
}

pub mod view {
    //! The banners, the compare view and the auto-save hooks. Their signals live in the window's root
    //! scope and are kept here so the shared window state does not grow.

    use std::cell::{Cell, RefCell};

    use dioxus_compose::html::prelude::dioxus_core::ScopeId;
    use dioxus_compose::html::prelude::dioxus_signals::{ReadableExt, Signal, WritableExt};
    use dioxus_compose::html::prelude::*;

    use super::{
        compare_rows, compare_title, connection_banner, doc_bar, manual_save_gate, reopen_plan, should_auto_save, visible_compare_rows,
        AutoSave, AutoSaveTimer, BarTone, CompareKind, CompareView, DocAction, Reopen, SaveTrigger, AUTO_SAVE_DELAY_MS, LOST_SAVE_NOTICE,
    };
    use crate::editor_host::HostCommand;
    use crate::ui::editor::base_name;
    use crate::ui::editor_workbench::page::{icon, icon_px};
    use crate::ui::editor_workbench::Shared;

    #[derive(Clone, Copy)]
    struct Ui {
        auto_save: Signal<AutoSave>,
        compare: Signal<Option<CompareView>>,
    }

    thread_local! {
        static UI: Cell<Option<Ui>> = const { Cell::new(None) };
        static TIMER: RefCell<AutoSaveTimer> = RefCell::new(AutoSaveTimer::default());
        /// The tab that was current on the last render, for onFocusChange.
        static LAST_CURRENT: RefCell<Option<String>> = const { RefCell::new(None) };
        /// The host's reconnect count the window has already acted on.
        static RECONNECTS_SEEN: Cell<u64> = const { Cell::new(0) };
    }

    fn ui() -> Ui {
        UI.with(|cell| {
            if let Some(ui) = cell.get() {
                return ui;
            }
            let root = ScopeId::ROOT;
            let ui = Ui {
                auto_save: Signal::new_in_scope(crate::services::services().ui.prefs.get_untracked().auto_save, root),
                compare: Signal::new_in_scope(None, root),
            };
            cell.set(Some(ui));
            ui
        })
    }

    fn notice(text: impl Into<String>) {
        crate::ui::cr::notice::post(text);
    }

    fn connection_lost(s: &Shared) -> bool {
        s.host.state.lock().unwrap().lost.is_some()
    }

    fn doc_state(s: &Shared, path: &str) -> Option<(ember_editor_conn::uri::UriComponents, u64, bool, Option<ember_editor::ExternalChange>)> {
        let uri = s.uris.borrow().get(path).cloned()?;
        let bridge = s.bridge.borrow();
        let doc = bridge.document(&uri)?;
        Some((uri, doc.generation, doc.dirty, doc.external_change))
    }

    /// Send the save of `path` without any check (다시 시도, 덮어쓰기, auto-save).
    fn send_save(s: &Shared, path: &str) {
        let Some(uri) = s.uris.borrow().get(path).cloned() else { return };
        let event = s.bridge.borrow().on_save_requested(&uri, 0);
        if let Some((uri, generation, event)) = event {
            s.host.send(HostCommand::Widget(uri, generation, event));
            s.scm.refresh();
        }
    }

    /// Whether a manual save of `path` (Cmd/Ctrl+S, from the page or the widget) goes ahead; when it
    /// does not, the reason was posted.
    pub fn manual_save_allowed(s: &Shared, path: &str) -> bool {
        if connection_lost(s) {
            notice(LOST_SAVE_NOTICE);
            return false;
        }
        let external = doc_state(s, path).and_then(|d| d.3);
        match manual_save_gate(external) {
            Ok(()) => true,
            Err(text) => {
                notice(text);
                false
            }
        }
    }

    /// The person typed in `path`: with afterDelay a save is due [`AUTO_SAVE_DELAY_MS`] after the last
    /// edit. The wait runs on the app runtime and hands its number back through the host state.
    pub fn edited(s: &Shared, path: &str) {
        if *ui().auto_save.read() != AutoSave::AfterDelay {
            return;
        }
        let seq = TIMER.with(|t| t.borrow_mut().edited(path));
        let (state, mut revision, path) = (s.host.state.clone(), s.revision, path.to_owned());
        crate::services::services().rt.spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(AUTO_SAVE_DELAY_MS)).await;
            state.lock().unwrap().autosave_due.push((path, seq));
            *revision.write() += 1;
        });
    }

    /// After the window applied the session's updates: put the tabs back after a reconnect, carry out
    /// due auto-saves, save the tab left behind (onFocusChange), and take a compare read.
    pub fn after_updates(s: &Shared, current: Option<&str>) {
        let (reconnects, due, compare) = {
            let mut state = s.host.state.lock().unwrap();
            (state.reconnects, std::mem::take(&mut state.autosave_due), state.compare.take())
        };
        if RECONNECTS_SEEN.with(|c| c.replace(reconnects)) != reconnects && reconnects > 0 {
            let tabs: Vec<(String, Option<String>)> = {
                let bridge = s.bridge.borrow();
                let uris = s.uris.borrow();
                s.tabs
                    .read()
                    .paths()
                    .into_iter()
                    .map(|p| (p.to_owned(), uris.get(p).and_then(|u| bridge.document(u)).map(|d| d.text.clone())))
                    .collect()
            };
            for step in reopen_plan(&tabs, current) {
                match step {
                    Reopen::Restore { path, text } => s.host.send(HostCommand::Restore { path, text }),
                    Reopen::Open(path) => s.host.send(HostCommand::Open(path)),
                    Reopen::Focus(path) => s.host.send(HostCommand::Focus(path)),
                };
            }
        }
        let mode = *ui().auto_save.read();
        for (path, seq) in due {
            if !TIMER.with(|t| t.borrow_mut().fire(&path, seq)) || connection_lost(s) {
                continue;
            }
            if let Some((_, _, dirty, external)) = doc_state(s, &path) {
                if should_auto_save(mode, SaveTrigger::Delay, dirty, external) {
                    send_save(s, &path);
                }
            }
        }
        let previous = LAST_CURRENT.with(|l| l.replace(current.map(str::to_owned)));
        if let Some(previous) = previous.filter(|p| Some(p.as_str()) != current) {
            if !connection_lost(s) {
                if let Some((_, _, dirty, external)) = doc_state(s, &previous) {
                    if should_auto_save(mode, SaveTrigger::FocusChange, dirty, external) {
                        send_save(s, &previous);
                    }
                }
            }
        }
        if let Some((path, result)) = compare {
            let mut sig = ui().compare;
            let waiting = matches!(sig.read().as_ref(), Some(CompareView::Loading { path: p }) if *p == path);
            if waiting {
                let mine = doc_state(s, &path).and_then(|(uri, ..)| s.bridge.borrow().document(&uri).map(|d| d.text.clone()));
                let view = match (result, mine) {
                    (Ok(disk), Some(mine)) => CompareView::Ready { path, rows: compare_rows(&disk, &mine) },
                    (Ok(_), None) => CompareView::Failed { path, reason: "파일이 열려 있지 않습니다".into() },
                    (Err(error), _) => {
                        tracing::warn!(%error, "reading the disk version for compare failed");
                        CompareView::Failed { path, reason: crate::ui::editor::file_failure_reason(&error).into() }
                    }
                };
                sig.set(Some(view));
            }
        }
        // A compare of a document that is no longer in conflict (overwritten, reverted, closed) closes.
        let open = ui().compare.read().clone();
        if let Some(view) = open {
            let loading = matches!(view, CompareView::Loading { .. });
            let in_conflict = doc_state(s, view.path()).is_some_and(|d| d.3 == Some(ember_editor::ExternalChange::ChangedWhileDirty));
            if !loading && !in_conflict {
                let mut sig = ui().compare;
                sig.set(None);
            }
        }
    }

    /// Whether the compare view covers the code area for the current tab.
    pub fn compare_visible(current: Option<&str>) -> bool {
        ui().compare.read().as_ref().is_some_and(|v| v.visible(current))
    }

    fn run(s: &Shared, path: &str, action: DocAction) {
        match action {
            DocAction::Retry => {
                if connection_lost(s) {
                    notice(LOST_SAVE_NOTICE);
                } else {
                    send_save(s, path);
                }
            }
            DocAction::Overwrite => {
                if connection_lost(s) {
                    notice(LOST_SAVE_NOTICE);
                } else {
                    let mut sig = ui().compare;
                    sig.set(None);
                    send_save(s, path);
                }
            }
            DocAction::Dismiss => {
                if let Some(uri) = s.uris.borrow().get(path).cloned() {
                    s.bridge.borrow_mut().dismiss_save_error(&uri);
                }
            }
            DocAction::Compare => {
                let mut sig = ui().compare;
                sig.set(Some(CompareView::Loading { path: path.to_owned() }));
                s.host.send(HostCommand::CompareWithDisk(path.to_owned()));
            }
            DocAction::Revert => {
                let mut sig = ui().compare;
                sig.set(None);
                s.host.send(HostCommand::Reload(path.to_owned()));
            }
        }
        let mut revision = s.revision;
        *revision.write() += 1;
    }

    /// The connection banner and the bar of the current document, drawn under the breadcrumbs.
    pub fn banners(s: &Shared, current: Option<&str>) -> Element {
        let (lost, reconnecting) = {
            let state = s.host.state.lock().unwrap();
            (state.lost.clone(), state.reconnecting)
        };
        let bar = current.and_then(|path| {
            let uri = s.uris.borrow().get(path).cloned()?;
            let bridge = s.bridge.borrow();
            let doc = bridge.document(&uri)?;
            doc_bar(base_name(path), doc.external_change, doc.save_error.as_deref()).map(|b| (path.to_owned(), b))
        });
        let banner_s = s.clone();
        rsx! {
            if let Some(reason) = lost {
                {
                    let (text, button) = connection_banner(reconnecting);
                    rsx! {
                        div { role: "alert", title: "{reason}",
                            style: "display:flex; align-items:center; gap:var(--sp2); margin:var(--sp2) var(--sp3) 0; padding:var(--sp1) var(--sp3); background:var(--c-orange-soft); color:var(--text); border-radius:var(--r-sm); flex:none;",
                            {icon_px("warn", 14)}
                            div { style: "flex:1; min-width:0; white-space:normal;", "{text}" }
                            if let Some(label) = button {
                                button { class: "ed-pj", style: "background:var(--c-orange); color:var(--ink-text); height:24px;", "aria-label": "{label}",
                                    onclick: move |_| { banner_s.host.send(HostCommand::Reconnect); },
                                    "{label}" }
                            }
                        }
                    }
                }
            }
            if let Some((path, bar)) = bar {
                {
                    let (bg, role) = match bar.tone { BarTone::Error => ("var(--c-red-soft)", "alert"), BarTone::Warning => ("var(--c-orange-soft)", "alertdialog") };
                    rsx! {
                        div { role: "{role}", "aria-label": "{bar.text}",
                            style: "display:flex; align-items:center; flex-wrap:wrap; gap:var(--sp2); margin:var(--sp2) var(--sp3) 0; padding:var(--sp1) var(--sp3); background:{bg}; color:var(--text); border-radius:var(--r-sm); flex:none;",
                            div { style: "flex:1 1 240px; min-width:0; white-space:normal;", "{bar.text}" }
                            for action in bar.actions.iter().copied() {
                                {
                                    let (s, path) = (s.clone(), path.clone());
                                    let label = action.label();
                                    rsx! {
                                        button { key: "{label}", class: "ed-pj", style: "border:1px solid var(--line); background:var(--surface); color:var(--text); height:24px;",
                                            onclick: move |_| run(&s, &path, action),
                                            "{label}" }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// The compare view: the disk version against the buffer, changed lines with three lines around.
    pub fn compare_pane(s: &Shared) -> Element {
        let Some(view) = ui().compare.read().clone() else { return rsx! {} };
        let path = view.path().to_owned();
        let title = compare_title(base_name(&path));
        let (over_s, revert_s, over_path, revert_path) = (s.clone(), s.clone(), path.clone(), path.clone());
        let mut close = ui().compare;
        rsx! {
            div { style: "flex:1; min-width:0; min-height:0; display:flex; flex-direction:column;", role: "region", "aria-label": "{title}",
                div { class: "ed-tsub",
                    {icon_px("file", 14)}
                    b { "{title}" }
                    div { class: "spacer" }
                    button { class: "ed-pj", style: "border:1px solid var(--line);", onclick: move |_| run(&over_s, &over_path, DocAction::Overwrite), "덮어쓰기" }
                    button { class: "ed-pj", style: "border:1px solid var(--line);", onclick: move |_| run(&revert_s, &revert_path, DocAction::Revert), "디스크 버전으로 되돌리기" }
                    button { class: "ed-ib", title: "비교 닫기", "aria-label": "비교 닫기", onclick: move |_| close.set(None), {icon("close")} }
                }
                {match view {
                    CompareView::Loading { .. } => rsx! { div { class: "ed-empty", role: "status", p { "디스크 버전을 읽는 중입니다" } } },
                    CompareView::Failed { reason, .. } => rsx! { div { class: "ed-empty", role: "alert", b { "디스크 버전을 읽지 못했습니다" } p { "{reason}" } } },
                    CompareView::Ready { rows, .. } => {
                        let shown = visible_compare_rows(&rows, 3);
                        rsx! {
                            if shown.is_empty() {
                                div { class: "ed-empty", role: "status", p { "디스크 버전과 내용이 같습니다" } }
                            } else {
                                div { style: "flex:1; min-height:0; overflow:auto; font-family:var(--mono); font-size:var(--fs-sm); padding:var(--sp2) 0;",
                                    for (i, row) in shown.into_iter().enumerate() {
                                        {
                                            match row {
                                                None => rsx! { div { key: "gap-{i}", style: "padding:2px 8px; margin:4px 0; background:var(--surface-2); color:var(--muted); font-size:var(--fs-xs);", "\u{22ef}" } },
                                                Some(row) => {
                                                    let (bg, sign) = match row.kind {
                                                        CompareKind::Disk => ("background:var(--c-red-soft);", "-"),
                                                        CompareKind::Mine => ("background:var(--c-green-soft);", "+"),
                                                        CompareKind::Same => ("", " "),
                                                    };
                                                    let disk_no = row.disk_line.map(|n| n.to_string()).unwrap_or_default();
                                                    let mine_no = row.mine_line.map(|n| n.to_string()).unwrap_or_default();
                                                    rsx! {
                                                        div { key: "row-{i}", style: "display:flex; white-space:pre; min-height:20px; line-height:20px; {bg}",
                                                            span { style: "width:40px; flex:none; text-align:right; padding-right:6px; color:var(--muted);", "{disk_no}" }
                                                            span { style: "width:40px; flex:none; text-align:right; padding-right:6px; color:var(--muted);", "{mine_no}" }
                                                            span { style: "width:14px; flex:none; text-align:center;", "{sign}" }
                                                            span { style: "flex:1; overflow:hidden; text-overflow:ellipsis;", "{row.text}" }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }}
            }
        }
    }

    /// The 자동 저장 choice in the ⋯ menu, saved per device like the keymap.
    pub fn menu_items(s: &Shared) -> Element {
        let current = *ui().auto_save.read();
        rsx! {
            div { class: "ed-msep" }
            div { class: "ed-mlab", "자동 저장" }
            for mode in AutoSave::ALL {
                {
                    let mut menu_open = s.menu_open;
                    let mut sig = ui().auto_save;
                    let selected = mode == current;
                    rsx! {
                        button { key: "{mode.setting()}", role: "menuitemradio", "aria-checked": "{selected}", onclick: move |_| {
                            menu_open.set(false);
                            sig.set(mode);
                            crate::services::services().ui.update_prefs(|p| p.auto_save = mode);
                        },
                            span { class: "ed-mchk", if selected { {icon_px("check", 14)} } }
                            span { class: "spacer", "{mode.label()}" }
                        }
                    }
                }
            }
        }
    }
}
