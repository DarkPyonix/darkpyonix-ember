//! The editor window's search tool window (SPEC FR-E7, #443): search across the project's files and replace.
//!
//! The search runs in the OSE's extension host (ripgrep) through `ember_editor`; this module keeps what the
//! panel shows ([`SearchResults`], filled on the host's runtime from the session's updates) and draws the
//! panel ([`view::panel`]). The mockup has no search window, so it is a left island tool window like the
//! project tree, in the mockup's tokens.

use std::sync::atomic::{AtomicU64, Ordering};

use ember_editor::search::{FileHits, Hit, Matcher, ReplaceReport, ReplaceRequest, SearchEnd, SearchOptions, SearchRequest, Target};
use ember_editor::SessionUpdate;
use ember_editor_conn::uri::UriComponents;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// A fresh id for a search or a replace, unique in the process.
pub fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

/// How long typing must pause before the query is searched (Code-OSS waits 300 ms too).
pub const SEARCH_DEBOUNCE_MS: u64 = 300;

/// What the search panel shows: the current search's matches as they arrive and how it ended, and the
/// last replace's report.
#[derive(Debug, Clone, Default)]
pub struct SearchResults {
    /// The search the matches belong to; updates for any other id are dropped.
    pub id: u64,
    pub request: Option<SearchRequest>,
    files: Vec<FileHits>,
    pub running: bool,
    pub end: Option<SearchEnd>,
    /// The replace in progress.
    pub replace_id: Option<u64>,
    pub report: Option<ReplaceReport>,
    /// A replace finished: the search runs again so the list shows what is left.
    refresh: bool,
}

/// Which matches a replace takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplaceScope {
    All,
    File(UriComponents),
    One(UriComponents, Target),
}

impl SearchResults {
    /// A new search `id` starts: the old matches go.
    pub fn start(&mut self, id: u64, request: SearchRequest) {
        self.id = id;
        self.request = Some(request);
        self.files.clear();
        self.running = true;
        self.end = None;
    }

    /// The query was emptied: nothing to show.
    pub fn clear(&mut self) {
        self.id = 0;
        self.request = None;
        self.files.clear();
        self.running = false;
        self.end = None;
        self.report = None;
    }

    /// The user stopped search `id` (it may not have reached the session yet).
    pub fn cancel_local(&mut self, id: u64) {
        if self.id == id && self.running {
            self.running = false;
            self.end = Some(SearchEnd::Cancelled);
        }
    }

    pub fn begin_replace(&mut self, id: u64) {
        self.replace_id = Some(id);
        self.report = None;
    }

    /// Take a session update that belongs to search or replace; `false` for any other update.
    pub fn apply(&mut self, update: &SessionUpdate) -> bool {
        match update {
            SessionUpdate::SearchHits { id, files } => {
                if *id == self.id && self.id != 0 {
                    self.merge(files.clone());
                }
                true
            }
            SessionUpdate::SearchDone { id, end } => {
                if *id == self.id && self.id != 0 {
                    self.running = false;
                    // A stop the user asked for may already be shown; keep the first word.
                    if self.end.is_none() {
                        self.end = Some(end.clone());
                    }
                }
                true
            }
            SessionUpdate::ReplaceDone { id, report } => {
                if self.replace_id == Some(*id) {
                    self.replace_id = None;
                    self.report = Some(report.clone());
                    self.refresh = self.request.is_some();
                }
                true
            }
            _ => false,
        }
    }

    fn merge(&mut self, files: Vec<FileHits>) {
        for f in files {
            match self.files.iter_mut().find(|x| x.uri == f.uri) {
                Some(x) => x.hits.extend(f.hits),
                None => self.files.push(f),
            }
        }
    }

    /// After a replace: the search to run again, under a new id.
    pub fn take_refresh(&mut self) -> Option<(u64, SearchRequest)> {
        if !std::mem::take(&mut self.refresh) {
            return None;
        }
        let request = self.request.clone()?;
        let id = next_id();
        self.start(id, request.clone());
        Some((id, request))
    }

    /// The files with matches, by path, each with its matches in order.
    pub fn files(&self) -> Vec<FileHits> {
        let mut files = self.files.clone();
        files.sort_by(|a, b| a.uri.path.cmp(&b.uri.path));
        for f in &mut files {
            f.hits.sort_by_key(|h| (h.line, h.start));
            f.hits.dedup_by_key(|h| (h.line, h.start, h.end));
        }
        files
    }

    pub fn match_count(&self) -> usize {
        self.files.iter().map(|f| f.hits.len()).sum()
    }

    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    pub fn limit_hit(&self) -> bool {
        matches!(self.end, Some(SearchEnd::Complete { limit_hit: true }))
    }

    /// The replace a scope asks for, with the options of the search the matches came from.
    pub fn replace_request(&self, scope: &ReplaceScope, replacement: &str) -> Option<ReplaceRequest> {
        let options = self.request.as_ref()?.options.clone();
        let targets = |f: &FileHits| -> Vec<Target> { f.hits.iter().filter_map(Hit::target).collect() };
        let files: Vec<(UriComponents, Vec<Target>)> = match scope {
            ReplaceScope::All => self.files().iter().map(|f| (f.uri.clone(), targets(f))).collect(),
            ReplaceScope::File(uri) => self.files.iter().filter(|f| &f.uri == uri).map(|f| (f.uri.clone(), targets(f))).collect(),
            ReplaceScope::One(uri, t) => vec![(uri.clone(), vec![*t])],
        };
        let files: Vec<_> = files.into_iter().filter(|(_, t)| !t.is_empty()).collect();
        (!files.is_empty()).then(|| ReplaceRequest { options, replacement: replacement.to_string(), files })
    }

    /// The line under the fields: how the search stands. `None` before any search.
    pub fn status_text(&self) -> Option<String> {
        if self.request.is_none() {
            return None;
        }
        let (n, files) = (self.match_count(), self.file_count());
        let found = if n == 0 { "결과가 없습니다.".to_string() } else { format!("{files}개 파일에서 결과 {n}개를 찾았습니다.") };
        if self.running {
            return Some(if n == 0 { "검색 중입니다…".to_string() } else { format!("검색 중입니다… 지금까지 {files}개 파일에서 {n}개를 찾았습니다.") });
        }
        Some(match &self.end {
            Some(SearchEnd::Complete { limit_hit: true }) => format!("{found} {LIMIT_NOTE}"),
            Some(SearchEnd::Complete { .. }) | None => found,
            Some(SearchEnd::Cancelled) => format!("검색을 중지했습니다. 그때까지 찾은 결과 {n}개만 표시합니다."),
            Some(SearchEnd::Failed(reason)) => failure_text(reason),
            Some(SearchEnd::Unavailable) => "검색 기능이 아직 준비되지 않았습니다. 잠시 후 다시 시도하세요.".to_string(),
        })
    }
}

/// The note when the provider stopped at the cap (FR-E7).
pub const LIMIT_NOTE: &str = "결과가 많아 일부만 표시합니다.";

/// Why a search failed, in Korean; the provider's own text goes to the log, not the screen.
pub fn failure_text(reason: &str) -> String {
    tracing::warn!("search failed: {reason}");
    let lower = reason.to_lowercase();
    if lower.contains("regex") || lower.contains("regular expression") || lower.contains("parse error") {
        "정규식을 해석할 수 없습니다. 식을 확인하세요.".to_string()
    } else if lower.contains("closed") || lower.contains("connection") || lower.contains("timed out") {
        "서버와 연결이 끊겨 검색하지 못했습니다.".to_string()
    } else {
        "검색하지 못했습니다. 검색어와 파일 필터를 확인하세요.".to_string()
    }
}

/// The question before 모두 바꾸기.
pub fn confirm_text(files: usize, matches: usize, replacement: &str, limit_hit: bool) -> String {
    let what = if replacement.is_empty() {
        format!("{files}개 파일에서 결과 {matches}개를 지웁니다.")
    } else {
        format!("{files}개 파일에서 결과 {matches}개를 \u{201C}{replacement}\u{201D}(으)로 바꿉니다.")
    };
    let mut text = format!("{what} 바꾼 파일은 바로 저장됩니다.");
    if limit_hit {
        text.push_str(" 결과가 많아 표시된 결과만 바꿉니다.");
    }
    text
}

/// What a replace did, in one line.
pub fn report_text(report: &ReplaceReport) -> String {
    let mut parts = Vec::new();
    if report.replaced > 0 {
        parts.push(format!("{}개 파일에서 {}개를 바꿨습니다.", report.files, report.replaced));
    } else {
        parts.push("바꾼 결과가 없습니다.".to_string());
    }
    if report.skipped > 0 {
        parts.push(format!("검색한 뒤 내용이 달라졌거나 저장하지 못해 {}개는 건너뛰었습니다.", report.skipped));
    }
    if let Some((path, error)) = report.failures.first() {
        let name = crate::ui::editor::display_name(crate::ui::editor::base_name(path));
        let reason = crate::ui::editor::file_failure_reason(error);
        if report.failures.len() == 1 {
            parts.push(format!("{name} 파일을 저장하지 못했습니다: {reason}"));
        } else {
            parts.push(format!("{name} 등 파일 {}개를 저장하지 못했습니다: {reason}", report.failures.len()));
        }
    }
    parts.join(" ")
}

/// `path` as the panel names it: relative to the project folder.
pub fn relative_path(folder: &str, path: &str) -> String {
    ember_editor::search::relative_to(folder, path).unwrap_or(path).to_string()
}

/// The directory part of a relative path ("" at the top).
pub fn dir_part(relative: &str) -> &str {
    relative.rsplit_once('/').map(|(d, _)| d).unwrap_or("")
}

fn byte_at_utf16(s: &str, col: u32) -> usize {
    let mut acc = 0u32;
    for (b, c) in s.char_indices() {
        if acc >= col {
            return b;
        }
        acc += c.len_utf16() as u32;
    }
    s.len()
}

/// How many characters of the line before a match a row keeps.
const PREVIEW_BEFORE: usize = 30;
/// How many characters after it.
const PREVIEW_AFTER: usize = 200;

/// A row's preview in three parts: before the match (leading blanks dropped, cut to the last
/// [`PREVIEW_BEFORE`] characters with an ellipsis), the match, and after it.
pub fn preview_parts(hit: &Hit) -> (String, String, String) {
    let p = &hit.preview;
    let s = byte_at_utf16(p, hit.preview_start);
    let e = byte_at_utf16(p, hit.preview_end).max(s);
    let before = p[..s].trim_start();
    let count = before.chars().count();
    let before = if count > PREVIEW_BEFORE {
        let skip = count - PREVIEW_BEFORE;
        format!("\u{2026}{}", before.chars().skip(skip).collect::<String>())
    } else {
        before.to_string()
    };
    let after: String = p[e..].chars().take(PREVIEW_AFTER).collect();
    (before, p[s..e].to_string(), after)
}

/// The text a match becomes, for the preview: the replacement itself, or for a regex the replacement with the
/// match's groups (found again in the preview line with `matcher`).
pub fn replaced_text(options: &SearchOptions, matcher: Option<&Matcher>, replacement: &str, hit: &Hit) -> String {
    if !options.regex {
        return replacement.to_string();
    }
    if let Some(m) = matcher {
        for caps in m.captures(&hit.preview) {
            if let Some((s, e)) = caps.pos(0) {
                let (cs, ce) = (ember_editor::search::utf16_col(&hit.preview, s), ember_editor::search::utf16_col(&hit.preview, e));
                if cs == hit.preview_start && ce == hit.preview_end {
                    return ember_editor::search::expand_replacement(replacement, &caps);
                }
            }
        }
    }
    replacement.to_string()
}

/// The accessible name of a match row.
pub fn hit_label(hit: &Hit) -> String {
    let (before, matched, after) = preview_parts(hit);
    format!("{}행: {before}{matched}{after}", hit.line + 1)
}

/// The accessible name of a file group.
pub fn file_label(relative: &str, count: usize) -> String {
    format!("{relative}, 결과 {count}개")
}

pub mod view {
    //! The panel. Its field values and toggles are signals of the window's root scope, kept here so the
    //! shared window state does not grow for one tool window.

    use std::cell::Cell;
    use std::collections::HashSet;

    use dioxus_compose::html::prelude::dioxus_core::ScopeId;
    use dioxus_compose::html::prelude::dioxus_elements::point_interaction::ModifiersInteraction;
    use dioxus_compose::html::prelude::dioxus_elements::{Key, Modifiers};
    use dioxus_compose::html::prelude::dioxus_signals::{ReadableExt, Signal, WritableExt};
    use dioxus_compose::html::prelude::*;
    use ember_editor::search::{Matcher, SearchOptions, SearchRequest};

    use super::{confirm_text, dir_part, file_label, hit_label, next_id, preview_parts, relative_path, replaced_text, report_text, ReplaceScope};
    use crate::editor_host::HostCommand;
    use crate::ui::editor_workbench::page::{icon, icon_px};
    use crate::ui::editor_workbench::{FindCmd, Shared};
    use crate::ui::find::Match;

    #[derive(Clone, Copy)]
    struct Ui {
        query: Signal<String>,
        replacement: Signal<String>,
        include: Signal<String>,
        exclude: Signal<String>,
        case_sensitive: Signal<bool>,
        whole_word: Signal<bool>,
        regex: Signal<bool>,
        use_ignore: Signal<bool>,
        show_replace: Signal<bool>,
        show_filters: Signal<bool>,
        collapsed: Signal<HashSet<String>>,
        confirm: Signal<bool>,
        focus_seq: Signal<u64>,
        /// A match to select once its file is the active editor: (path, match).
        reveal: Signal<Option<(String, Match)>>,
    }

    thread_local! {
        static UI: Cell<Option<Ui>> = const { Cell::new(None) };
        /// The file of the pending reveal was already active on the last render (its editor is mounted).
        static REVEAL_SEEN: Cell<bool> = const { Cell::new(false) };
    }

    fn ui() -> Ui {
        UI.with(|cell| {
            if let Some(ui) = cell.get() {
                return ui;
            }
            let root = ScopeId::ROOT;
            let ui = Ui {
                query: Signal::new_in_scope(String::new(), root),
                replacement: Signal::new_in_scope(String::new(), root),
                include: Signal::new_in_scope(String::new(), root),
                exclude: Signal::new_in_scope(String::new(), root),
                case_sensitive: Signal::new_in_scope(false, root),
                whole_word: Signal::new_in_scope(false, root),
                regex: Signal::new_in_scope(false, root),
                use_ignore: Signal::new_in_scope(true, root),
                show_replace: Signal::new_in_scope(false, root),
                show_filters: Signal::new_in_scope(false, root),
                collapsed: Signal::new_in_scope(HashSet::new(), root),
                confirm: Signal::new_in_scope(false, root),
                focus_seq: Signal::new_in_scope(0, root),
                reveal: Signal::new_in_scope(None, root),
            };
            cell.set(Some(ui));
            ui
        })
    }

    fn notice(text: impl Into<String>) {
        crate::ui::cr::notice::post(text);
    }

    fn request(ui: Ui) -> SearchRequest {
        SearchRequest {
            options: SearchOptions {
                pattern: ui.query.read().clone(),
                regex: *ui.regex.read(),
                case_sensitive: *ui.case_sensitive.read(),
                whole_word: *ui.whole_word.read(),
            },
            include: ui.include.read().clone(),
            exclude: ui.exclude.read().clone(),
            use_ignore_files: *ui.use_ignore.read(),
        }
    }

    /// Search what the fields say: at once, or after a pause in typing. An empty query clears the list.
    fn run_search(s: &Shared, debounce: bool) {
        let ui = ui();
        let req = request(ui);
        let mut state = s.host.state.lock().unwrap();
        let running = state.search.running.then_some(state.search.id);
        if req.options.pattern.is_empty() {
            state.search.clear();
            drop(state);
            if let Some(id) = running {
                s.host.send(HostCommand::CancelSearch(id));
            }
            return;
        }
        let id = next_id();
        state.search.start(id, req.clone());
        drop(state);
        s.host.send(HostCommand::Search { id, request: req, debounce });
    }

    fn stop_search(s: &Shared) {
        let mut state = s.host.state.lock().unwrap();
        if !state.search.running {
            return;
        }
        let id = state.search.id;
        state.search.cancel_local(id);
        drop(state);
        s.host.send(HostCommand::CancelSearch(id));
    }

    fn replace(s: &Shared, scope: ReplaceScope) {
        let ui = ui();
        let replacement = ui.replacement.read().clone();
        let mut state = s.host.state.lock().unwrap();
        if state.search.replace_id.is_some() {
            drop(state);
            notice("바꾸는 중입니다. 끝난 뒤 다시 시도하세요.");
            return;
        }
        let Some(request) = state.search.replace_request(&scope, &replacement) else { return };
        let id = next_id();
        state.search.begin_replace(id);
        drop(state);
        s.host.send(HostCommand::Replace { id, request });
    }

    /// Show the search window and put the cursor in its field; `with_replace` also shows the replace field.
    pub fn open(s: &Shared, with_replace: bool) {
        let ui = ui();
        let mut stripes = s.stripes;
        if !stripes.read().is_selected(crate::ui::editor::ToolWindow::Search) {
            stripes.write().toggle(crate::ui::editor::ToolWindow::Search);
        }
        if with_replace {
            let mut show = ui.show_replace;
            show.set(true);
        }
        let mut focus = ui.focus_seq;
        let next = *focus.read() + 1;
        focus.set(next);
    }

    /// After a click on a match: once its file is the active editor (and was on the render before, so the
    /// editor is there), select the match through the find bar's command. Called by the page on every render.
    pub fn apply_pending_reveal(s: &Shared) {
        let ui = ui();
        let Some((path, m)) = ui.reveal.read().clone() else { return };
        let active = s.bridge.borrow().active_document().map(|d| d.uri.path.clone());
        if active.as_deref() != Some(path.as_str()) {
            REVEAL_SEEN.with(|c| c.set(false));
            return;
        }
        if !REVEAL_SEEN.with(|c| c.replace(true)) {
            // One more render, so the editor of this file is mounted before it is asked to select.
            let mut revision = s.revision;
            *revision.write() += 1;
            return;
        }
        REVEAL_SEEN.with(|c| c.set(false));
        let mut reveal = ui.reveal;
        reveal.set(None);
        let (mut cmd, mut next) = (s.find.cmd, s.find.next_id);
        let id = *next.read();
        next.set(id + 1);
        cmd.set(Some(FindCmd { id, goto: Some(m), replacements: Vec::new() }));
    }

    /// Queue a match or caret location in `path` to be revealed as soon as `path` is the mounted active editor.
    pub fn schedule_reveal(path: &str, m: Match) {
        let mut reveal = ui().reveal;
        reveal.set(Some((path.to_string(), m)));
        REVEAL_SEEN.with(|c| c.set(false));
    }

    fn open_hit(s: &Shared, path: &str, m: Match) {
        let mut tabs = s.tabs;
        tabs.write().open(path);
        s.open_errors.borrow_mut().remove(path);
        s.host.send(HostCommand::Open(path.to_string()));
        s.host.send(HostCommand::Focus(path.to_string()));
        schedule_reveal(path, m);
    }

    const FIELD: &str = "flex:1; min-width:0; border:0; background:none; outline:none; color:var(--text); font:inherit;";
    const BOX: &str = "width:auto; height:30px; cursor:text;";

    fn toggle(label: &'static str, text: &'static str, on: bool, mut flag: Signal<bool>, s: &Shared) -> Element {
        let s = s.clone();
        let class = if on { "ed-ib on" } else { "ed-ib" };
        let style = if on { "width:auto; min-width:24px; height:22px; padding:0 4px; background:var(--sel); color:var(--text); font-size:var(--fs-xs);" } else { "width:auto; min-width:24px; height:22px; padding:0 4px; font-size:var(--fs-xs);" };
        rsx! {
            button { class, style, title: "{label}", "aria-label": "{label}", "aria-pressed": "{on}",
                onclick: move |_| { flag.set(!on); run_search(&s, false); },
                "{text}"
            }
        }
    }

    /// The search tool window.
    pub fn panel(s: &Shared) -> Element {
        let ui = ui();
        let (mut query, mut replacement, mut include, mut exclude) = (ui.query, ui.replacement, ui.include, ui.exclude);
        let (mut show_replace, mut show_filters, mut collapsed, mut confirm) = (ui.show_replace, ui.show_filters, ui.collapsed, ui.confirm);
        let mut stripes = s.stripes;
        let focus_seq = *ui.focus_seq.read();
        let (files, status, running, report, replacing, limit_hit, match_count, options) = {
            let state = s.host.state.lock().unwrap();
            let r = &state.search;
            (
                r.files(),
                r.status_text(),
                r.running,
                r.report.as_ref().map(report_text),
                r.replace_id.is_some(),
                r.limit_hit(),
                r.match_count(),
                r.request.as_ref().map(|q| q.options.clone()),
            )
        };
        let q = query.read().clone();
        let rep = replacement.read().clone();
        let replace_on = *show_replace.read();
        let filters_on = *show_filters.read();
        let preview_replace = replace_on && options.is_some();
        // One compiled matcher for every row's regex replacement preview.
        let matcher = options.as_ref().filter(|o| o.regex && preview_replace).and_then(|o| Matcher::new(o).ok());
        let folder = s.folder.clone();
        let (case_on, word_on, regex_on, ignore_on) = (*ui.case_sensitive.read(), *ui.whole_word.read(), *ui.regex.read(), *ui.use_ignore.read());
        let can_replace = replace_on && match_count > 0 && !replacing && !running;
        let file_total = files.len();
        let all_paths: HashSet<String> = files.iter().map(|f| f.uri.path.clone()).collect();
        let (key_s, enter_s, stop_s, refresh_s, confirm_s, inc_s, exc_s, ign_s) =
            (s.clone(), s.clone(), s.clone(), s.clone(), s.clone(), s.clone(), s.clone(), s.clone());
        let (case_t, word_t, regex_t) = (toggle("대소문자 구분", "Aa", case_on, ui.case_sensitive, s), toggle("단어 단위", "단어", word_on, ui.whole_word, s), toggle("정규식", ".*", regex_on, ui.regex, s));
        let confirm_message = confirm_text(file_total, match_count, &rep, limit_hit);
        rsx! {
            section { class: "ed-tw ed-left", "aria-label": "검색",
                div { class: "ed-twh",
                    b { class: "ed-twt", "검색" }
                    div { class: "spacer" }
                    if running {
                        button { class: "ed-ib", title: "검색 중지", "aria-label": "검색 중지", onclick: move |_| stop_search(&stop_s), {icon("pause")} }
                    }
                    button { class: "ed-ib", title: "다시 검색", "aria-label": "다시 검색", onclick: move |_| run_search(&refresh_s, false), {icon("refresh")} }
                    button { class: "ed-ib", title: "모두 접기", "aria-label": "모두 접기", onclick: move |_| collapsed.set(all_paths.clone()), {icon_px("chev", 16)} }
                    button { class: "ed-ib", title: "숨기기", "aria-label": "검색 창 숨기기", onclick: move |_| stripes.write().hide(crate::ui::editor::Side::Left), {icon("close")} }
                }
                div { role: "search", "aria-label": "파일에서 찾기", style: "display:flex; flex-direction:column; gap:var(--sp1); padding:var(--sp2) var(--sp2) var(--sp1); flex:none;",
                    div { style: "display:flex; align-items:center; gap:var(--sp1);",
                        button { class: "ed-ib", style: "width:22px; height:22px;", title: "바꾸기 표시", "aria-label": "바꾸기 표시", "aria-expanded": "{replace_on}",
                            onclick: move |_| show_replace.set(!replace_on),
                            {icon_px(if replace_on { "chev-dn" } else { "chev" }, 12)}
                        }
                        div { class: "ed-find", style: "{BOX}",
                            input { key: "search-query-{focus_seq}", style: "{FIELD}", "aria-label": "검색어", placeholder: "검색", value: "{q}", autofocus: true,
                                oninput: move |event| { query.set(event.value()); run_search(&key_s, true); },
                                onkeydown: move |event| {
                                    match event.key() {
                                        Key::Enter => { event.stop_propagation(); run_search(&enter_s, false); }
                                        Key::Escape => { event.stop_propagation(); stop_search(&enter_s); }
                                        _ => {}
                                    }
                                },
                            }
                            {case_t}
                            {word_t}
                            {regex_t}
                        }
                    }
                    if replace_on {
                        div { style: "display:flex; align-items:center; gap:var(--sp1); padding-left:26px;",
                            div { class: "ed-find", style: "{BOX}",
                                input { style: "{FIELD}", "aria-label": "바꿀 내용", placeholder: "바꾸기", value: "{rep}",
                                    oninput: move |event| replacement.set(event.value()),
                                    // ⌥⌘Enter / Ctrl+Alt+Enter is Code-OSS's replace all; it asks first here too.
                                    onkeydown: move |event| {
                                        let m = event.modifiers();
                                        if matches!(event.key(), Key::Enter) && m.contains(Modifiers::ALT) && (m.contains(Modifiers::META) || m.contains(Modifiers::CONTROL)) {
                                            event.stop_propagation();
                                            confirm.set(true);
                                        }
                                    },
                                }
                            }
                            button { class: "ed-pj", style: "border:1px solid var(--line); height:30px;", disabled: !can_replace, "aria-label": "모두 바꾸기", title: "모두 바꾸기",
                                onclick: move |_| confirm.set(true), "모두 바꾸기" }
                        }
                    }
                    div { style: "display:flex; align-items:center; gap:var(--sp1); padding-left:26px;",
                        button { class: "ed-pj", style: "height:24px; padding:0 var(--sp2); font-size:var(--fs-xs); color:var(--text-2);", "aria-expanded": "{filters_on}", "aria-label": "파일 필터",
                            onclick: move |_| show_filters.set(!filters_on),
                            {icon_px(if filters_on { "chev-dn" } else { "chev" }, 11)} "파일 필터"
                        }
                    }
                    if filters_on {
                        div { style: "display:flex; flex-direction:column; gap:var(--sp1); padding-left:26px;",
                            div { class: "ed-find", style: "{BOX}",
                                input { style: "{FIELD}", "aria-label": "포함할 파일", placeholder: "포함할 파일 (예: *.rs, src/**)", value: "{include}",
                                    oninput: move |event| { include.set(event.value()); run_search(&inc_s, true); } }
                            }
                            div { class: "ed-find", style: "{BOX}",
                                input { style: "{FIELD}", "aria-label": "제외할 파일", placeholder: "제외할 파일 (예: target, *.min.js)", value: "{exclude}",
                                    oninput: move |event| { exclude.set(event.value()); run_search(&exc_s, true); } }
                            }
                            button { class: if ignore_on { "ed-pj on" } else { "ed-pj" }, style: "height:26px; font-size:var(--fs-xs); justify-content:flex-start; padding:0 var(--sp2);",
                                role: "checkbox", "aria-checked": "{ignore_on}", "aria-label": "제외 설정 및 무시 파일 사용",
                                onclick: move |_| { let mut flag = ui.use_ignore; flag.set(!ignore_on); run_search(&ign_s, false); },
                                {icon_px(if ignore_on { "check" } else { "close" }, 12)} "제외 설정 및 무시 파일 사용"
                            }
                        }
                    }
                    if let Some(text) = status {
                        div { role: "status", style: "padding:var(--sp1) var(--sp1) 0 26px; color:var(--muted); font-size:var(--fs-xs); white-space:normal;", "{text}" }
                    }
                    if replacing {
                        div { role: "status", style: "padding:0 var(--sp1) 0 26px; color:var(--muted); font-size:var(--fs-xs);", "바꾸는 중입니다…" }
                    }
                    if let Some(text) = report {
                        div { role: "status", style: "padding:0 var(--sp1) 0 26px; color:var(--text-2); font-size:var(--fs-xs); white-space:normal;", "{text}" }
                    }
                }
                if *confirm.read() {
                    div { class: "ed-tn", role: "alertdialog", "aria-label": "모두 바꾸기 확인", tabindex: "0",
                        style: "--d:0; height:auto; flex-wrap:wrap; margin:0 var(--sp2) var(--sp2); padding:var(--sp2); white-space:normal; background:var(--c-orange-soft); color:var(--text); cursor:default;",
                        onkeydown: move |event| { if matches!(event.key(), Key::Enter | Key::Escape) { event.stop_propagation(); confirm.set(false); } },
                        div { style: "flex:1 1 100%;", "{confirm_message}" }
                        button { class: "ed-pj", style: "border:1px solid var(--line); background:var(--surface); color:var(--text);", autofocus: true, onclick: move |_| confirm.set(false), "취소" }
                        button { class: "ed-pj", style: "background:var(--ink); color:var(--ink-text);", onclick: move |_| { confirm.set(false); replace(&confirm_s, ReplaceScope::All); }, "모두 바꾸기" }
                    }
                }
                div { class: "ed-tree", role: "tree", "aria-label": "검색 결과",
                    for f in files {
                        {
                            let relative = relative_path(&folder, &f.uri.path);
                            let name = crate::ui::editor::display_name(crate::ui::editor::base_name(&relative));
                            let dir = dir_part(&relative).to_string();
                            let key = f.uri.path.clone();
                            let open = !collapsed.read().contains(&key);
                            let count = f.hits.len();
                            let label = file_label(&relative, count);
                            let toggle_key = key.clone();
                            let file_uri = f.uri.clone();
                            let file_s = s.clone();
                            let file_title = format!("이 파일에서 모두 바꾸기: {name}");
                            let rows = f.hits.clone();
                            let path = f.uri.path.clone();
                            rsx! {
                                div { key: "{key}", class: "ed-tn", role: "treeitem", "aria-label": "{label}", "aria-expanded": "{open}", style: "--d:0;",
                                    onclick: move |_| { let mut c = collapsed.write(); if !c.remove(&toggle_key) { c.insert(toggle_key.clone()); } },
                                    if open { {icon_px("chev-dn", 12)} } else { {icon_px("chev", 12)} }
                                    {icon_px("file", 15)}
                                    span { style: "font-weight:var(--fw-medium);", "{name}" }
                                    span { style: "color:var(--muted); font-size:var(--fs-xs); overflow:hidden; text-overflow:ellipsis; min-width:0;", "{dir}" }
                                    div { class: "spacer" }
                                    if preview_replace && can_replace {
                                        button { class: "ed-ib", style: "width:22px; height:22px;", title: "{file_title}", "aria-label": "{file_title}",
                                            onclick: move |event| { event.stop_propagation(); replace(&file_s, ReplaceScope::File(file_uri.clone())); },
                                            {icon_px("edit", 13)}
                                        }
                                    }
                                    span { class: "ed-sc", "aria-hidden": "true", "{count}" }
                                }
                                if open {
                                    for (i, hit) in rows.into_iter().enumerate() {
                                        {
                                            let (before, matched, after) = preview_parts(&hit);
                                            let new_text = if preview_replace { options.as_ref().map(|o| replaced_text(o, matcher.as_ref(), &rep, &hit)) } else { None };
                                            let row_label = hit_label(&hit);
                                            let m = Match { line: hit.line, start: hit.start, end: if hit.end_line == hit.line { hit.end } else { hit.start } };
                                            let target = hit.target();
                                            let (click_s, one_s) = (s.clone(), s.clone());
                                            let (click_path, one_uri) = (path.clone(), f.uri.clone());
                                            let line_no = hit.line + 1;
                                            let one_title = "이 결과 바꾸기";
                                            rsx! {
                                                div { key: "{key}:{i}", class: "ed-tn", role: "treeitem", "aria-label": "{row_label}", style: "--d:1; gap:var(--sp2);",
                                                    onclick: move |_| open_hit(&click_s, &click_path, m),
                                                    span { style: "color:var(--muted); font-size:var(--fs-2xs); font-variant-numeric:tabular-nums; flex:none;", "{line_no}" }
                                                    span { style: "flex:1; min-width:0; overflow:hidden; text-overflow:ellipsis; white-space:pre; font-family:var(--mono); font-size:var(--fs-xs);",
                                                        span { "{before}" }
                                                        if let Some(new_text) = new_text.clone() {
                                                            span { style: "background:var(--c-red-soft); color:var(--c-red); text-decoration:line-through;", "{matched}" }
                                                            span { style: "background:var(--c-green-soft); color:var(--c-green);", "{new_text}" }
                                                        } else {
                                                            span { style: "background:var(--c-orange-soft); color:var(--text); border-radius:2px;", "{matched}" }
                                                        }
                                                        span { "{after}" }
                                                    }
                                                    if preview_replace && can_replace && target.is_some() {
                                                        button { class: "ed-ib", style: "width:22px; height:22px;", title: "{one_title}", "aria-label": "{one_title}",
                                                            onclick: move |event| {
                                                                event.stop_propagation();
                                                                if let Some(t) = target { replace(&one_s, ReplaceScope::One(one_uri.clone(), t)); }
                                                            },
                                                            {icon_px("edit", 13)}
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
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uri(path: &str) -> UriComponents {
        UriComponents::remote("h", path)
    }

    fn hit(line: u32, start: u32, end: u32, preview: &str) -> Hit {
        Hit { line, start, end_line: line, end, preview: preview.into(), preview_start: start, preview_end: end }
    }

    fn request(pattern: &str, regex: bool) -> SearchRequest {
        SearchRequest {
            options: SearchOptions { pattern: pattern.into(), regex, case_sensitive: true, whole_word: false },
            include: String::new(),
            exclude: String::new(),
            use_ignore_files: true,
        }
    }

    fn hits(id: u64, path: &str, list: Vec<Hit>) -> SessionUpdate {
        SessionUpdate::SearchHits { id, files: vec![FileHits { uri: uri(path), hits: list }] }
    }

    #[test]
    fn fr_e7_results_group_by_file_sorted_by_path_and_line() {
        let mut r = SearchResults::default();
        r.start(1, request("foo", false));
        assert!(r.apply(&hits(1, "/w/b.rs", vec![hit(3, 0, 3, "foo")])));
        assert!(r.apply(&hits(1, "/w/a.rs", vec![hit(9, 0, 3, "foo"), hit(2, 0, 3, "foo")])));
        assert!(r.apply(&hits(1, "/w/b.rs", vec![hit(1, 0, 3, "foo")])));
        let files = r.files();
        assert_eq!(files.iter().map(|f| f.uri.path.as_str()).collect::<Vec<_>>(), vec!["/w/a.rs", "/w/b.rs"]);
        assert_eq!(files[0].hits.iter().map(|h| h.line).collect::<Vec<_>>(), vec![2, 9]);
        assert_eq!(files[1].hits.len(), 2);
        assert_eq!((r.file_count(), r.match_count()), (2, 4));
        assert_eq!(r.status_text().as_deref(), Some("검색 중입니다… 지금까지 2개 파일에서 4개를 찾았습니다."));
    }

    #[test]
    fn fr_e7_updates_of_an_older_search_are_dropped() {
        let mut r = SearchResults::default();
        r.start(2, request("foo", false));
        assert!(r.apply(&hits(1, "/w/a.rs", vec![hit(0, 0, 3, "foo")])));
        assert!(r.apply(&SessionUpdate::SearchDone { id: 1, end: SearchEnd::Complete { limit_hit: false } }));
        assert_eq!(r.match_count(), 0);
        assert!(r.running);
        assert!(!r.apply(&SessionUpdate::ConnectionLost { reason: "x".into() }));
    }

    #[test]
    fn fr_e7_the_cap_note_is_honest() {
        let mut r = SearchResults::default();
        r.start(1, request("foo", false));
        r.apply(&hits(1, "/w/a.rs", vec![hit(0, 0, 3, "foo")]));
        r.apply(&SessionUpdate::SearchDone { id: 1, end: SearchEnd::Complete { limit_hit: true } });
        assert!(r.limit_hit());
        assert_eq!(r.status_text().unwrap(), "1개 파일에서 결과 1개를 찾았습니다. 결과가 많아 일부만 표시합니다.");
        assert_eq!(LIMIT_NOTE, "결과가 많아 일부만 표시합니다.");
    }

    #[test]
    fn fr_e7_status_texts_for_every_end() {
        let mut r = SearchResults::default();
        assert_eq!(r.status_text(), None);
        r.start(1, request("foo", false));
        assert_eq!(r.status_text().unwrap(), "검색 중입니다…");
        r.apply(&SessionUpdate::SearchDone { id: 1, end: SearchEnd::Complete { limit_hit: false } });
        assert_eq!(r.status_text().unwrap(), "결과가 없습니다.");
        r.start(2, request("foo", false));
        r.cancel_local(2);
        r.apply(&SessionUpdate::SearchDone { id: 2, end: SearchEnd::Failed("late".into()) });
        assert!(r.status_text().unwrap().starts_with("검색을 중지했습니다."), "a stop the user asked for stays");
        r.start(3, request("(", true));
        r.apply(&SessionUpdate::SearchDone { id: 3, end: SearchEnd::Failed("regex parse error: unclosed group".into()) });
        assert_eq!(r.status_text().unwrap(), "정규식을 해석할 수 없습니다. 식을 확인하세요.");
        r.start(4, request("foo", false));
        r.apply(&SessionUpdate::SearchDone { id: 4, end: SearchEnd::Unavailable });
        assert!(r.status_text().unwrap().contains("준비되지 않았습니다"));
        r.clear();
        assert_eq!(r.status_text(), None);
    }

    #[test]
    fn fr_e7_replace_requests_take_all_one_file_or_one_match() {
        let mut r = SearchResults::default();
        r.start(1, request("foo", false));
        r.apply(&hits(1, "/w/a.rs", vec![hit(0, 0, 3, "foo"), hit(1, 4, 7, "let foo")]));
        r.apply(&hits(1, "/w/b.rs", vec![hit(5, 0, 3, "foo")]));
        let all = r.replace_request(&ReplaceScope::All, "bar").unwrap();
        assert_eq!(all.files.len(), 2);
        assert_eq!(all.replacement, "bar");
        assert_eq!(all.options.pattern, "foo");
        let file = r.replace_request(&ReplaceScope::File(uri("/w/a.rs")), "bar").unwrap();
        assert_eq!(file.files, vec![(uri("/w/a.rs"), vec![Target { line: 0, start: 0, end: 3 }, Target { line: 1, start: 4, end: 7 }])]);
        let one = r.replace_request(&ReplaceScope::One(uri("/w/b.rs"), Target { line: 5, start: 0, end: 3 }), "").unwrap();
        assert_eq!(one.files.len(), 1);
        assert!(SearchResults::default().replace_request(&ReplaceScope::All, "x").is_none());
    }

    #[test]
    fn fr_e7_a_finished_replace_reports_and_searches_again() {
        let mut r = SearchResults::default();
        r.start(1, request("foo", false));
        r.begin_replace(10);
        assert!(r.apply(&SessionUpdate::ReplaceDone { id: 10, report: ReplaceReport { replaced: 3, skipped: 1, files: 2, failures: vec![] } }));
        assert_eq!(r.replace_id, None);
        let (id, req) = r.take_refresh().expect("search again");
        assert_eq!(req.options.pattern, "foo");
        assert_eq!(r.id, id);
        assert!(r.running);
        assert!(r.take_refresh().is_none());
        assert_eq!(report_text(r.report.as_ref().unwrap()), "2개 파일에서 3개를 바꿨습니다. 검색한 뒤 내용이 달라졌거나 저장하지 못해 1개는 건너뛰었습니다.");
    }

    #[test]
    fn fr_e7_confirmation_and_report_texts() {
        assert_eq!(confirm_text(2, 5, "bar", false), "2개 파일에서 결과 5개를 \u{201C}bar\u{201D}(으)로 바꿉니다. 바꾼 파일은 바로 저장됩니다.");
        assert_eq!(confirm_text(1, 1, "", true), "1개 파일에서 결과 1개를 지웁니다. 바꾼 파일은 바로 저장됩니다. 결과가 많아 표시된 결과만 바꿉니다.");
        let failed = ReplaceReport { replaced: 0, skipped: 2, files: 0, failures: vec![("/w/a.rs".into(), "NoPermissions (FileSystemError)".into())] };
        let text = report_text(&failed);
        assert!(text.starts_with("바꾼 결과가 없습니다."));
        assert!(text.contains("a.rs 파일을 저장하지 못했습니다"));
        assert!(text.contains("권한"));
    }

    #[test]
    fn fr_e7_previews_cut_long_prefixes_and_keep_the_match() {
        let h = hit(0, 44, 47, &format!("        {}foo();", "x".repeat(36)));
        let (before, matched, after) = preview_parts(&h);
        assert_eq!(matched, "foo");
        assert_eq!(after, "();");
        assert!(before.starts_with('\u{2026}'));
        assert_eq!(before.chars().count(), 31);
        let short = hit(0, 4, 7, "    foo bar");
        assert_eq!(preview_parts(&short), (String::new(), "foo".into(), " bar".into()));
        assert_eq!(hit_label(&short), "1행: foo bar");
        // UTF-16 columns past a non-BMP character.
        let wide = Hit { line: 0, start: 2, end_line: 0, end: 5, preview: "😀foo".into(), preview_start: 2, preview_end: 5 };
        assert_eq!(preview_parts(&wide).1, "foo");
    }

    #[test]
    fn fr_e7_regex_replacement_preview_uses_the_groups() {
        let o = SearchOptions { pattern: r"v(\d)".into(), regex: true, case_sensitive: true, whole_word: false };
        let m = Matcher::new(&o).ok();
        let h = hit(0, 3, 5, "v1 v2");
        assert_eq!(replaced_text(&o, m.as_ref(), "version-$1", &h), "version-2");
        let literal = SearchOptions { regex: false, ..o.clone() };
        assert_eq!(replaced_text(&literal, None, "$1", &h), "$1");
    }

    #[test]
    fn fr_e7_paths_are_relative_to_the_folder() {
        assert_eq!(relative_path("/w/p", "/w/p/src/a.rs"), "src/a.rs");
        assert_eq!(relative_path("/w/p", "/elsewhere/a.rs"), "/elsewhere/a.rs");
        assert_eq!(dir_part("src/ui/a.rs"), "src/ui");
        assert_eq!(dir_part("a.rs"), "");
        assert_eq!(file_label("src/a.rs", 3), "src/a.rs, 결과 3개");
    }

    #[test]
    fn fr_e7_sentences_are_korean_and_polite() {
        // NFR-L4: whole sentences end in 합니다체, never a plain 다.
        let mut r = SearchResults::default();
        r.start(1, request("foo", false));
        let mut texts = vec![
            LIMIT_NOTE.to_string(),
            confirm_text(1, 1, "x", true),
            report_text(&ReplaceReport { replaced: 1, skipped: 1, files: 1, failures: vec![] }),
            failure_text("regex"),
            failure_text("connection closed"),
            failure_text("something else"),
        ];
        for end in [SearchEnd::Complete { limit_hit: false }, SearchEnd::Cancelled, SearchEnd::Unavailable] {
            r.start(2, request("foo", false));
            r.apply(&SessionUpdate::SearchDone { id: 2, end });
            texts.push(r.status_text().unwrap());
        }
        for t in texts {
            for sentence in t.split(". ").map(|s| s.trim_end_matches('.')) {
                assert!(!sentence.ends_with('다') || sentence.ends_with("니다"), "{t}");
            }
            assert!(t.chars().any(|c| ('\u{AC00}'..='\u{D7A3}').contains(&c)), "{t}");
        }
    }

    #[test]
    fn fr_e7_ids_are_unique() {
        let a = next_id();
        let b = next_id();
        assert!(b > a);
    }
}
