//! Language features in the editor window (SPEC FR-L31.13, #442): the completion list, go to
//! definition, references, rename, code actions, format document, the parameter hint and the
//! symbols in the breadcrumbs.
//!
//! The session (`ember_editor::language`) asks the extension host's providers; this module keeps
//! what the window shows ([`LanguageState`], filled on the host's runtime from the session's
//! updates) and draws it ([`view`]).
//!
//! The lists are drawn as a sheet over the window, like the command palette, and not as a popup at
//! the caret: the `CodeEditor` widget does not say where the caret is on screen and keeps every key
//! to itself while it has the focus (see `docs/design/EDITOR-PARITY.md`, row "Language features").
//! So a list opens on its command (Ctrl+Space, F12, Shift+F12, F2, Ctrl/Cmd+.), takes the focus,
//! and is filtered by typing in its own field.

use std::collections::HashMap;

use ember_editor::language::completion::{filter, kind_name};
use ember_editor::language::navigation::symbol_path;
use ember_editor::language::{CodeAction, CompletionItem, DocumentSymbol, EditSummary, Feature, LanguageUpdate, Location, SignatureHelp};
use ember_editor::{SessionUpdate, WidgetPos};
use ember_editor_conn::uri::UriComponents;

/// What a key or a menu asks of the language features.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LangAction {
    Suggest,
    Definition,
    References,
    Rename,
    QuickFix,
    Format,
    ParameterHints,
}

/// The Code-OSS chord for a language feature, the same on every system: F12 definition, Shift+F12
/// references, F2 rename, Ctrl+Space suggestions, Ctrl+Shift+Space (or Cmd+Shift+Space) parameter
/// hints, Ctrl+. (or Cmd+.) quick fix. Format document is in the keymap table (`Action::Format`).
pub fn action_for_key(key: &str, ctrl: bool, shift: bool, alt: bool, meta: bool) -> Option<LangAction> {
    let primary = ctrl != meta;
    match key {
        "F12" if !ctrl && !alt && !meta => Some(if shift { LangAction::References } else { LangAction::Definition }),
        "F2" if !ctrl && !alt && !meta && !shift => Some(LangAction::Rename),
        " " if ctrl && !alt && !meta && !shift => Some(LangAction::Suggest),
        " " if primary && shift && !alt => Some(LangAction::ParameterHints),
        "." if primary && !shift && !alt => Some(LangAction::QuickFix),
        _ => None,
    }
}

/// The language features in the editor menu: the action, its name and its chord.
pub const MENU: [(LangAction, &str, &str); 6] = [
    (LangAction::Suggest, "자동 완성", "Ctrl+Space"),
    (LangAction::Definition, "정의로 이동", "F12"),
    (LangAction::References, "참조 찾기", "Shift+F12"),
    (LangAction::Rename, "이름 바꾸기", "F2"),
    (LangAction::QuickFix, "코드 작업", "Ctrl+."),
    (LangAction::ParameterHints, "매개변수 정보", "Ctrl+Shift+Space"),
];

/// The list or box now open over the window.
#[derive(Debug, Clone, PartialEq)]
pub enum Sheet {
    Completion { uri: UriComponents, items: Vec<CompletionItem>, complete: bool },
    Locations { feature: Feature, locations: Vec<Location> },
    Actions { uri: UriComponents, actions: Vec<CodeAction>, complete: bool },
    Rename { uri: UriComponents, pos: WidgetPos, placeholder: String },
}

impl Sheet {
    pub fn title(&self) -> &'static str {
        match self {
            Self::Completion { .. } => "자동 완성",
            Self::Locations { feature: Feature::Definition, .. } => "정의",
            Self::Locations { .. } => "참조",
            Self::Actions { .. } => "코드 작업",
            Self::Rename { .. } => "이름 바꾸기",
        }
    }

    pub fn placeholder(&self) -> &'static str {
        match self {
            Self::Completion { .. } => "제안을 걸러 냅니다",
            Self::Locations { .. } => "파일 이름으로 걸러 냅니다",
            Self::Actions { .. } => "코드 작업을 걸러 냅니다",
            Self::Rename { .. } => "새 이름을 입력합니다",
        }
    }

    /// The providers have not all answered yet.
    pub fn loading(&self) -> bool {
        matches!(self, Self::Completion { complete: false, .. } | Self::Actions { complete: false, .. })
    }
}

/// One row of a sheet's list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// The entry's place in the sheet's own list, which is what choosing it names.
    pub index: usize,
    pub title: String,
    pub detail: String,
    pub hint: String,
    pub enabled: bool,
}

fn relative(folder: &str, path: &str) -> String {
    let folder = folder.trim_end_matches('/');
    match path.strip_prefix(folder) {
        Some(rest) if !folder.is_empty() && rest.starts_with('/') => rest[1..].to_string(),
        _ => path.to_string(),
    }
}

/// The name a code action's kind is shown by.
pub fn action_kind_label(kind: Option<&str>) -> &'static str {
    match kind.unwrap_or_default() {
        k if k.starts_with("quickfix") => "빠른 수정",
        k if k.starts_with("refactor") => "리팩터링",
        k if k.starts_with("source") => "소스 작업",
        _ => "",
    }
}

/// The rows a sheet lists for what is typed in its field. A rename has none.
pub fn rows(sheet: &Sheet, query: &str, folder: &str) -> Vec<Row> {
    let needle = query.to_lowercase();
    match sheet {
        Sheet::Completion { items, .. } => filter(items, query)
            .into_iter()
            .map(|index| {
                let item = &items[index];
                Row {
                    index,
                    title: format!("{}{}", item.label, item.label_detail.as_deref().unwrap_or_default()),
                    detail: item.description.clone().or_else(|| item.detail.clone()).unwrap_or_default(),
                    hint: kind_name(item.kind).to_string(),
                    enabled: true,
                }
            })
            .collect(),
        Sheet::Locations { locations, .. } => locations
            .iter()
            .enumerate()
            .map(|(index, location)| {
                let at = location.target().start;
                Row {
                    index,
                    title: format!("{}:{}", relative(folder, &location.uri.path), at.line + 1),
                    detail: location.preview.clone().unwrap_or_default(),
                    hint: format!("{}열", at.col + 1),
                    enabled: true,
                }
            })
            .filter(|row| needle.is_empty() || row.title.to_lowercase().contains(&needle))
            .collect(),
        Sheet::Actions { actions, .. } => {
            let mut list: Vec<Row> = actions
                .iter()
                .enumerate()
                .filter(|(_, action)| needle.is_empty() || action.title.to_lowercase().contains(&needle))
                .map(|(index, action)| Row {
                    index,
                    title: action.title.clone(),
                    detail: action.disabled.clone().unwrap_or_default(),
                    hint: action_kind_label(action.kind.as_deref()).to_string(),
                    enabled: action.disabled.is_none(),
                })
                .collect();
            // The fix the provider prefers comes first, as in Code-OSS's menu; the rest keep their order.
            list.sort_by_key(|row| !actions[row.index].preferred);
            list
        }
        Sheet::Rename { .. } => Vec::new(),
    }
}

/// What the window says when no provider offers `feature` for the file. The breadcrumbs simply
/// show no symbols, so they say nothing.
pub fn unavailable_text(feature: Feature) -> Option<&'static str> {
    match feature {
        Feature::Completion => Some("이 파일에서는 자동 완성을 사용할 수 없습니다"),
        Feature::Definition => Some("이 파일에서는 정의로 이동할 수 없습니다"),
        Feature::References => Some("이 파일에서는 참조를 찾을 수 없습니다"),
        Feature::Format => Some("이 파일에 맞는 코드 정리 도구가 없습니다"),
        Feature::Rename => Some("이 파일에서는 이름을 바꿀 수 없습니다"),
        Feature::CodeActions => Some("이 파일에서는 코드 작업을 사용할 수 없습니다"),
        Feature::SignatureHelp => Some("이 파일에서는 매개변수 정보를 볼 수 없습니다"),
        Feature::DocumentSymbols => None,
    }
}

fn feature_label(feature: Feature) -> &'static str {
    match feature {
        Feature::Completion => "자동 완성",
        Feature::Definition => "정의로 이동",
        Feature::References => "참조 찾기",
        Feature::Format => "코드 정리",
        Feature::Rename => "이름 바꾸기",
        Feature::CodeActions => "코드 작업",
        Feature::SignatureHelp => "매개변수 정보",
        Feature::DocumentSymbols => "문서 기호",
    }
}

/// What a finished workspace edit says: what changed, what was left out, what failed.
pub fn summary_text(feature: Feature, summary: &EditSummary) -> String {
    let mut text = if summary.edits == 0 {
        format!("{}: 바꾼 곳이 없습니다", feature_label(feature))
    } else {
        format!("{}: 파일 {}개에서 {}곳을 바꿨습니다", feature_label(feature), summary.files, summary.edits)
    };
    if summary.unsupported > 0 {
        text.push_str(&format!(". 파일 만들기, 이동, 삭제 {}건은 적용하지 않았습니다", summary.unsupported));
    }
    if let Some((path, _)) = summary.failures.first() {
        text.push_str(&format!(". 바꾸지 못한 파일 {}개: {}", summary.failures.len(), super::editor::base_name(path)));
    }
    text
}

/// The parameter hint as three pieces of the signature's label: before the active parameter, the
/// parameter, after it. No active parameter leaves the last two empty.
pub fn signature_parts(help: &SignatureHelp) -> Option<(String, String, String)> {
    let (signature, active) = help.active()?;
    let span = active.and_then(|i| signature.parameters[i].span);
    let label = &signature.label;
    let cut = |units: u32| -> Option<usize> {
        let mut seen = 0u32;
        for (at, ch) in label.char_indices() {
            if seen == units {
                return Some(at);
            }
            seen += ch.len_utf16() as u32;
        }
        (seen == units).then_some(label.len())
    };
    match span.and_then(|(start, end)| Some((cut(start)?, cut(end)?))) {
        Some((a, b)) if a <= b => Some((label[..a].to_string(), label[a..b].to_string(), label[b..].to_string())),
        _ => Some((label.clone(), String::new(), String::new())),
    }
}

/// The names of the symbols around a caret (1-based line and column, as the status bar has them),
/// outermost first.
pub fn symbol_names(symbols: &[DocumentSymbol], line: usize, col: usize) -> Vec<String> {
    let pos = WidgetPos::new(line.saturating_sub(1) as u32, col.saturating_sub(1) as u32);
    symbol_path(symbols, pos).into_iter().map(|s| s.name.clone()).collect()
}

/// What the language features show, kept beside the session's other state.
#[derive(Debug, Clone, Default)]
pub struct LanguageState {
    pub sheet: Option<Sheet>,
    /// Goes up when another sheet opens, so the window starts its field and selection afresh.
    pub sheet_seq: u64,
    /// Messages for the window to show once.
    pub notices: Vec<String>,
    /// A place for the window to go to once.
    pub jump: Option<Location>,
    pub signature: Option<SignatureHelp>,
    /// The symbols of each open file, by path.
    pub symbols: HashMap<String, Vec<DocumentSymbol>>,
}

impl LanguageState {
    fn open(&mut self, sheet: Sheet) {
        let same_kind = self.sheet.as_ref().is_some_and(|old| std::mem::discriminant(old) == std::mem::discriminant(&sheet));
        if !same_kind {
            self.sheet_seq += 1;
        }
        self.sheet = Some(sheet);
    }

    /// Close the sheet (the reader chose, or pressed Escape).
    pub fn close_sheet(&mut self) -> Option<Sheet> {
        self.sheet.take()
    }

    /// Take a session update that belongs to the language features; `false` for any other update.
    pub fn apply(&mut self, update: &SessionUpdate) -> bool {
        if let SessionUpdate::Closed { uri } = update {
            self.symbols.remove(&uri.path);
            return false;
        }
        let SessionUpdate::Language(update) = update else { return false };
        match update.clone() {
            LanguageUpdate::Unavailable { feature, .. } => self.notices.extend(unavailable_text(feature).map(str::to_string)),
            LanguageUpdate::Failed { feature, error, .. } => {
                tracing::warn!(?feature, %error, "a language feature failed");
                if feature != Feature::DocumentSymbols {
                    self.notices.push(format!("{}: 확장이 응답하지 못했습니다", feature_label(feature)));
                }
            }
            LanguageUpdate::Completions { uri, items, complete, .. } => {
                if complete && items.is_empty() {
                    self.sheet = None;
                    self.notices.push("제안이 없습니다".to_string());
                } else {
                    self.open(Sheet::Completion { uri, items, complete });
                }
            }
            LanguageUpdate::CompletionApplied { .. } => {}
            LanguageUpdate::Locations { feature, locations, .. } => match (feature, locations.len()) {
                (Feature::Definition, 0) => self.notices.push("정의를 찾지 못했습니다".to_string()),
                (_, 0) => self.notices.push("참조를 찾지 못했습니다".to_string()),
                // One definition: go there, as F12 does in Code-OSS.
                (Feature::Definition, 1) => self.jump = locations.into_iter().next(),
                _ => self.open(Sheet::Locations { feature, locations }),
            },
            LanguageUpdate::Formatted { edits, .. } => {
                if edits == 0 {
                    self.notices.push("이미 정리되어 있습니다".to_string());
                }
            }
            LanguageUpdate::RenameReady { uri, pos, placeholder, .. } => self.open(Sheet::Rename { uri, pos, placeholder }),
            LanguageUpdate::RenameRejected { reason, .. } => {
                self.notices.push(reason.unwrap_or_else(|| "이 위치의 요소는 이름을 바꿀 수 없습니다".to_string()));
            }
            LanguageUpdate::WorkspaceEditApplied { feature, summary } => self.notices.push(summary_text(feature, &summary)),
            LanguageUpdate::CodeActions { uri, actions, complete, .. } => {
                if complete && actions.is_empty() {
                    self.sheet = None;
                    self.notices.push("사용할 수 있는 코드 작업이 없습니다".to_string());
                } else {
                    self.open(Sheet::Actions { uri, actions, complete });
                }
            }
            LanguageUpdate::SignatureHelp { help, .. } => {
                if help.is_none() && self.signature.is_none() {
                    self.notices.push("매개변수 정보가 없습니다".to_string());
                }
                self.signature = help;
            }
            LanguageUpdate::DocumentSymbols { uri, symbols, .. } => {
                self.symbols.insert(uri.path, symbols);
            }
        }
        true
    }

    /// A sheet filled with sample entries, for captures of a state no installed extension gives
    /// (debug builds, `EMBER_EDITOR_LANG_SHEET`).
    pub fn sample(kind: &str, uri: &UriComponents) -> Option<Sheet> {
        use serde_json::json;
        let range = json!({"startLineNumber": 3, "startColumn": 5, "endLineNumber": 3, "endColumn": 9});
        let uri_value = serde_json::to_value(uri).ok()?;
        let there = |path: &str, preview: &str| {
            let mut location = Location::from_json(&json!({"uri": uri_value, "range": range}))?;
            location.uri.path = path.to_string();
            location.preview = Some(preview.to_string());
            Some(location)
        };
        match kind {
            "completion" => {
                let at = ember_editor::WidgetRange::empty(WidgetPos::default());
                let items = [
                    json!({"a": {"label": "push", "detail": "(value)"}, "b": 0, "c": "fn push(&mut self, value: T)"}),
                    json!({"a": {"label": "pop", "detail": "()"}, "b": 0, "c": "fn pop(&mut self) -> Option<T>"}),
                    json!({"a": "len", "b": 9, "c": "usize"}),
                    json!({"a": "println!", "b": 27, "h": "println!(\"$0\")", "i": 4}),
                ]
                .iter()
                .filter_map(|v| ember_editor::language::completion::inflate_item(0, v, (at, at)))
                .collect();
                Some(Sheet::Completion { uri: uri.clone(), items, complete: true })
            }
            "references" => Some(Sheet::Locations {
                feature: Feature::References,
                locations: [there("/w/src/main.rs", "let total = count(items);"), there("/w/src/lib.rs", "pub fn count(items: &[Item]) -> usize {")]
                    .into_iter()
                    .flatten()
                    .collect(),
            }),
            "actions" => {
                let actions = [
                    json!({"title": "누락된 가져오기 추가", "kind": "quickfix", "isPreferred": true}),
                    json!({"title": "함수로 추출", "kind": "refactor.extract"}),
                    json!({"title": "변수로 추출", "kind": "refactor.extract", "disabled": "식을 선택해야 합니다"}),
                ]
                .iter()
                .filter_map(|v| CodeAction::from_json(0, v))
                .collect();
                Some(Sheet::Actions { uri: uri.clone(), actions, complete: true })
            }
            "rename" => Some(Sheet::Rename { uri: uri.clone(), pos: WidgetPos::default(), placeholder: "total".to_string() }),
            _ => None,
        }
    }

    /// A sample parameter hint, for the same captures.
    pub fn sample_signature() -> Option<SignatureHelp> {
        ember_editor::language::assist::parse_signature_help(&serde_json::json!({
            "id": 0, "activeSignature": 0, "activeParameter": 1,
            "signatures": [{"label": "insert(key: K, value: V) -> Option<V>", "parameters": [{"label": "key: K"}, {"label": "value: V"}]}]
        }))
    }
}

pub mod view {
    //! The sheet, the parameter hint and the breadcrumb symbols. The sheet's field and selection are
    //! signals of the window's root scope, kept here so the shared window state does not grow.

    use std::cell::{Cell, RefCell};

    use dioxus_compose::html::prelude::dioxus_core::ScopeId;
    use dioxus_compose::html::prelude::dioxus_elements::Key;
    use dioxus_compose::html::prelude::dioxus_signals::{ReadableExt, Signal, WritableExt};
    use dioxus_compose::html::prelude::*;
    use ember_editor::language::{LanguageRequest, Location};
    use ember_editor::{WidgetPos, WidgetRange};
    use ember_editor_conn::uri::UriComponents;

    use super::{rows, signature_parts, symbol_names, LangAction, LanguageState, Sheet};
    use crate::editor_host::HostCommand;
    use crate::ui::editor_workbench::page::{icon, icon_px};
    use crate::ui::editor_workbench::Shared;
    use crate::ui::find::Match;

    /// How many rows a sheet draws; the field narrows a longer list.
    const MAX_ROWS: usize = 50;

    #[derive(Clone, Copy)]
    struct Ui {
        query: Signal<String>,
        sel: Signal<usize>,
        /// The `sheet_seq` the field and selection belong to.
        seq: Signal<u64>,
    }

    thread_local! {
        static UI: Cell<Option<Ui>> = const { Cell::new(None) };
        /// The file whose symbols were last asked for.
        static SYMBOLS_FOR: RefCell<Option<String>> = const { RefCell::new(None) };
        /// The capture hook ran (it opens its sample once).
        static SAMPLE_SHOWN: Cell<bool> = const { Cell::new(false) };
    }

    fn ui() -> Ui {
        UI.with(|cell| {
            if let Some(ui) = cell.get() {
                return ui;
            }
            let root = ScopeId::ROOT;
            let ui = Ui { query: Signal::new_in_scope(String::new(), root), sel: Signal::new_in_scope(0, root), seq: Signal::new_in_scope(0, root) };
            cell.set(Some(ui));
            ui
        })
    }

    fn notice(text: impl Into<String>) {
        crate::ui::cr::notice::post(text);
    }

    fn redraw(s: &Shared) {
        let mut revision = s.revision;
        *revision.write() += 1;
    }

    /// The file in the active tab as the session knows it.
    fn current(s: &Shared) -> Option<UriComponents> {
        let path = s.tabs.read().current_path().map(str::to_owned)?;
        s.uris.borrow().get(&path).cloned()
    }

    /// The caret as the editor last reported it.
    fn caret(s: &Shared) -> WidgetPos {
        let (line, col) = *s.position.read();
        WidgetPos::new(line.saturating_sub(1) as u32, col.saturating_sub(1) as u32)
    }

    fn send(s: &Shared, request: LanguageRequest) {
        s.host.send(HostCommand::Language(request));
    }

    /// Start a language feature at the caret of the active file.
    pub fn run(s: &Shared, action: LangAction) {
        let Some(uri) = current(s) else {
            notice("열린 파일이 없습니다");
            return;
        };
        let pos = Some(caret(s));
        send(
            s,
            match action {
                LangAction::Suggest => LanguageRequest::Complete { uri, pos, trigger: None },
                LangAction::Definition => LanguageRequest::Definition { uri, pos },
                LangAction::References => LanguageRequest::References { uri, pos },
                LangAction::Rename => LanguageRequest::PrepareRename { uri, pos },
                LangAction::QuickFix => LanguageRequest::CodeActions { uri, range: Some(WidgetRange::empty(caret(s))) },
                LangAction::Format => LanguageRequest::Format { uri, range: None },
                LangAction::ParameterHints => LanguageRequest::SignatureHelp { uri, pos, trigger: None },
            },
        );
    }

    /// Open the file of `location` and select the place.
    fn go_to(s: &Shared, location: &Location) {
        let path = location.uri.path.clone();
        let target = location.target();
        let end = if target.end.line == target.start.line { target.end.col } else { target.start.col };
        let mut tabs = s.tabs;
        tabs.write().open(&path);
        s.open_errors.borrow_mut().remove(&path);
        s.host.send(HostCommand::Open(path.clone()));
        s.host.send(HostCommand::Focus(path.clone()));
        crate::ui::editor_search::view::schedule_reveal(&path, Match { line: target.start.line, start: target.start.col, end });
        redraw(s);
    }

    /// Escape closes the parameter hint. `true` when there was one.
    pub fn dismiss_hint(s: &Shared) -> bool {
        let had = s.host.state.lock().unwrap().language.signature.take().is_some();
        if had {
            if let Some(uri) = current(s) {
                send(s, LanguageRequest::DismissSignatureHelp { uri });
            }
        }
        had
    }

    /// Run once per render of the page, after the session's updates were applied: show the
    /// messages, go where a definition is, and ask for the symbols of a file that became active
    /// (and again after a save, when they may have changed).
    pub fn after_updates(s: &Shared, saved: bool) {
        let (notices, jump) = {
            let mut state = s.host.state.lock().unwrap();
            (std::mem::take(&mut state.language.notices), state.language.jump.take())
        };
        for text in notices {
            notice(text);
        }
        if let Some(location) = jump {
            go_to(s, &location);
        }
        if let Some(uri) = current(s) {
            let fresh = SYMBOLS_FOR.with(|last| last.borrow().as_deref() != Some(uri.path.as_str()));
            if fresh || saved {
                SYMBOLS_FOR.with(|last| *last.borrow_mut() = Some(uri.path.clone()));
                send(s, LanguageRequest::DocumentSymbols { uri });
            }
        }
    }

    /// The symbols around the caret, drawn after the path in the breadcrumbs.
    pub fn symbol_crumbs(s: &Shared, path: &str) -> Element {
        let (line, col) = *s.position.read();
        let names = {
            let state = s.host.state.lock().unwrap();
            state.language.symbols.get(path).map(|symbols| symbol_names(symbols, line, col)).unwrap_or_default()
        };
        rsx! {
            for (i, name) in names.iter().enumerate() {
                {icon_px("chev", 11)}
                span { key: "sym{i}", "{name}" }
            }
        }
    }

    fn close(s: &Shared) -> Option<Sheet> {
        let sheet = s.host.state.lock().unwrap().language.close_sheet();
        redraw(s);
        sheet
    }

    /// Escape or a click outside: the list is dropped and the session releases it.
    fn cancel(s: &Shared) {
        match close(s) {
            Some(Sheet::Completion { uri, .. }) => send(s, LanguageRequest::CancelCompletion { uri }),
            Some(Sheet::Actions { uri, .. }) => send(s, LanguageRequest::DismissCodeActions { uri }),
            _ => {}
        }
    }

    /// Enter or a click on a row.
    fn choose(s: &Shared, index: usize) {
        match close(s) {
            Some(Sheet::Completion { uri, .. }) => send(s, LanguageRequest::AcceptCompletion { uri, index }),
            Some(Sheet::Actions { uri, .. }) => send(s, LanguageRequest::ApplyCodeAction { uri, index }),
            Some(Sheet::Locations { locations, .. }) => {
                if let Some(location) = locations.get(index) {
                    go_to(s, location);
                }
            }
            _ => {}
        }
    }

    fn rename(s: &Shared, new_name: &str) {
        let name = new_name.trim();
        if let Some(Sheet::Rename { uri, pos, placeholder }) = close(s) {
            if !name.is_empty() && name != placeholder {
                send(s, LanguageRequest::Rename { uri, pos, new_name: name.to_string() });
            }
        }
    }

    /// The capture hook: `EMBER_EDITOR_LANG_SHEET=completion|references|actions|rename|signature`
    /// opens that state with sample entries once a file is open (debug builds only).
    fn capture_hook(s: &Shared) {
        if !cfg!(debug_assertions) || SAMPLE_SHOWN.with(|shown| shown.get()) {
            return;
        }
        let Ok(kind) = std::env::var("EMBER_EDITOR_LANG_SHEET") else { return };
        let Some(uri) = current(s) else { return };
        SAMPLE_SHOWN.with(|shown| shown.set(true));
        let mut state = s.host.state.lock().unwrap();
        if kind == "signature" {
            state.language.signature = LanguageState::sample_signature();
        } else if let Some(sheet) = LanguageState::sample(&kind, &uri) {
            state.language.sheet = Some(sheet);
            state.language.sheet_seq += 1;
        }
    }

    /// The parameter hint: the signature with the parameter being typed set off. It sits at the
    /// bottom of the editor area because the widget does not say where the caret is on screen.
    fn hint(s: &Shared, help: &ember_editor::language::SignatureHelp) -> Element {
        let Some((before, active, after)) = signature_parts(help) else { return rsx! {} };
        let count = help.signatures.len();
        let close_s = s.clone();
        rsx! {
            div { class: "ed-menu", role: "status", "aria-label": "매개변수 정보",
                style: "position:fixed; z-index:38; left:calc(50vw - min(280px, calc(50vw - 12px))); bottom:44px; top:auto; right:auto; max-width:min(560px, calc(100vw - 24px)); padding:var(--sp2) var(--sp3); display:flex; align-items:center; gap:var(--sp2);",
                code { style: "font-family:var(--mono); font-size:var(--fs-sm); white-space:pre-wrap; color:var(--muted);",
                    "{before}"
                    span { style: "font-weight:var(--fw-semibold); color:var(--text);", "{active}" }
                    "{after}"
                }
                if count > 1 { span { class: "muted", style: "font-size:var(--fs-xs);", "외 {count - 1}개" } }
                button { class: "ed-ib", title: "닫기", "aria-label": "매개변수 정보 닫기", onclick: move |_| { dismiss_hint(&close_s); }, {icon("close")} }
            }
        }
    }

    /// The sheet over the window and the parameter hint, when there is one to show.
    pub fn overlay(s: &Shared) -> Element {
        capture_hook(s);
        let ui = ui();
        let (sheet, seq, signature) = {
            let state = s.host.state.lock().unwrap();
            (state.language.sheet.clone(), state.language.sheet_seq, state.language.signature.clone())
        };
        let Some(sheet) = sheet else {
            return match signature {
                Some(help) => hint(s, &help),
                None => rsx! {},
            };
        };
        let (mut query, mut sel, mut seen) = (ui.query, ui.sel, ui.seq);
        if *seen.peek() != seq {
            // Another sheet: its field starts empty, or with the name a rename offers.
            seen.set(seq);
            sel.set(0);
            query.set(match &sheet {
                Sheet::Rename { placeholder, .. } => placeholder.clone(),
                _ => String::new(),
            });
        }
        let text = query.read().clone();
        let is_rename = matches!(sheet, Sheet::Rename { .. });
        let list = rows(&sheet, &text, &s.folder);
        let total = list.len();
        let shown: Vec<_> = list.into_iter().take(MAX_ROWS).collect();
        let chosen = (*sel.read()).min(shown.len().saturating_sub(1));
        let enter = shown.get(chosen).filter(|row| row.enabled).map(|row| row.index);
        let count = shown.len();
        let (title, placeholder, loading) = (sheet.title(), sheet.placeholder(), sheet.loading());
        let (scrim_s, key_s) = (s.clone(), s.clone());
        rsx! {
            div { style: "position:fixed; left:0; top:0; right:0; bottom:0; z-index:39;", onclick: move |_| cancel(&scrim_s) }
            div { class: "ed-menu", role: "dialog", "aria-label": "{title}",
                style: "position:fixed; z-index:40; top:60px; left:calc(50vw - min(280px, calc(50vw - 12px))); width:min(560px, calc(100vw - 24px)); right:auto; padding:var(--sp1);",
                div { style: "padding:var(--sp1) var(--sp3) 0; color:var(--muted); font-size:var(--fs-xs);", "{title}" }
                div { class: "ed-find", style: "width:auto; margin:var(--sp1);",
                    {icon(if is_rename { "edit" } else { "search" })}
                    input { style: "flex:1; min-width:0; border:0; background:none; outline:none; color:var(--text); font:inherit;",
                        "aria-label": "{title}", placeholder: "{placeholder}", value: "{text}", autofocus: true,
                        oninput: move |event| { query.set(event.value()); sel.set(0); },
                        onkeydown: move |event| {
                            match event.key() {
                                Key::Escape => { event.stop_propagation(); cancel(&key_s); }
                                Key::ArrowDown if count > 0 => { event.stop_propagation(); sel.set((chosen + 1) % count); }
                                Key::ArrowUp if count > 0 => { event.stop_propagation(); sel.set((chosen + count - 1) % count); }
                                Key::Enter => {
                                    event.stop_propagation();
                                    if is_rename {
                                        let name = query.peek().clone();
                                        rename(&key_s, &name);
                                    } else if let Some(index) = enter {
                                        choose(&key_s, index);
                                    }
                                }
                                _ => {}
                            }
                        },
                    }
                }
                if loading { div { style: "padding:var(--sp1) var(--sp3); color:var(--muted); font-size:var(--fs-xs);", "확장의 답을 기다리는 중입니다…" } }
                if is_rename {
                    div { style: "padding:var(--sp1) var(--sp3) var(--sp2); color:var(--muted); font-size:var(--fs-xs);", "Enter로 바꾸고 Esc로 취소합니다. 열려 있지 않은 파일은 바로 저장됩니다." }
                } else {
                    div { style: "max-height:320px; overflow:auto;", role: "listbox",
                        if shown.is_empty() && !loading { div { style: "padding:var(--sp3); color:var(--muted);", "일치하는 항목이 없습니다" } }
                        for (row_at, row) in shown.iter().enumerate() {
                            {
                                let (index, enabled) = (row.index, row.enabled);
                                let click_s = s.clone();
                                let class = if row_at == chosen { "ed-tn sel" } else { "ed-tn" };
                                let style = if enabled { "--d:0; height:34px; gap:var(--sp2);" } else { "--d:0; height:34px; gap:var(--sp2); color:var(--muted);" };
                                rsx! { div { key: "{index}", class, role: "option", "aria-selected": "{row_at == chosen}", "aria-disabled": "{!enabled}", style: "{style}",
                                    onclick: move |_| { if enabled { choose(&click_s, index); } },
                                    span { style: "white-space:nowrap;", "{row.title}" }
                                    span { class: "muted", style: "min-width:0; overflow:hidden; text-overflow:ellipsis; white-space:nowrap; font-size:var(--fs-xs);", "{row.detail}" }
                                    div { class: "spacer" }
                                    if !row.hint.is_empty() { kbd { class: "ed-sc", "{row.hint}" } }
                                } }
                            }
                        }
                        if total > count { div { style: "padding:var(--sp1) var(--sp3); color:var(--muted); font-size:var(--fs-xs);", "{total - count}개가 더 있습니다. 입력해서 좁혀 주세요." } }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ember_editor::language::completion::inflate_item;
    use ember_editor::WidgetRange;
    use serde_json::json;

    fn uri(path: &str) -> UriComponents {
        UriComponents::remote("h", path)
    }

    fn lang(update: LanguageUpdate) -> SessionUpdate {
        SessionUpdate::Language(update)
    }

    fn item(label: &str) -> CompletionItem {
        let at = WidgetRange::empty(WidgetPos::default());
        inflate_item(1, &json!({"a": label, "b": 1}), (at, at)).unwrap()
    }

    fn location(path: &str, line: u32) -> Location {
        let range = json!({"startLineNumber": line, "startColumn": 3, "endLineNumber": line, "endColumn": 8});
        let mut found = Location::from_json(&json!({"uri": serde_json::to_value(uri(path)).unwrap(), "range": range})).unwrap();
        found.preview = Some("let total = 1;".into());
        found
    }

    #[test]
    fn fr_l31_13_chords_are_code_oss_and_leave_other_keys_alone() {
        assert_eq!(action_for_key("F12", false, false, false, false), Some(LangAction::Definition));
        assert_eq!(action_for_key("F12", false, true, false, false), Some(LangAction::References));
        // Alt+F12 is the JetBrains terminal chord.
        assert_eq!(action_for_key("F12", false, false, true, false), None);
        assert_eq!(action_for_key("F2", false, false, false, false), Some(LangAction::Rename));
        assert_eq!(action_for_key("F2", true, false, false, false), None);
        assert_eq!(action_for_key(" ", true, false, false, false), Some(LangAction::Suggest));
        assert_eq!(action_for_key(" ", false, false, false, false), None);
        assert_eq!(action_for_key(" ", true, true, false, false), Some(LangAction::ParameterHints));
        assert_eq!(action_for_key(" ", false, true, false, true), Some(LangAction::ParameterHints));
        assert_eq!(action_for_key(".", true, false, false, false), Some(LangAction::QuickFix));
        assert_eq!(action_for_key(".", false, false, false, true), Some(LangAction::QuickFix));
        assert_eq!(action_for_key(".", false, false, false, false), None);
    }

    #[test]
    fn fr_l31_13_every_menu_chord_dispatches_to_its_action() {
        for (action, label, hint) in MENU {
            let keys: Vec<&str> = hint.split('+').collect();
            let key = match *keys.last().unwrap() {
                "Space" => " ",
                other => other,
            };
            assert_eq!(action_for_key(key, keys.contains(&"Ctrl"), keys.contains(&"Shift"), keys.contains(&"Alt"), false), Some(action), "{label}");
        }
    }

    #[test]
    fn fr_l31_13_completions_open_a_sheet_and_an_empty_answer_says_so() {
        let mut state = LanguageState::default();
        let update = |items: Vec<CompletionItem>, complete| {
            lang(LanguageUpdate::Completions { uri: uri("/w/a.rs"), version: 0, pos: WidgetPos::default(), items, incomplete: false, complete })
        };
        // Still waiting with nothing yet: the sheet opens and says it is loading.
        assert!(state.apply(&update(vec![], false)));
        assert!(state.sheet.as_ref().unwrap().loading());
        let seq = state.sheet_seq;
        assert!(state.apply(&update(vec![item("push"), item("pop")], true)));
        // The same list filling up keeps the field and selection.
        assert_eq!(state.sheet_seq, seq);
        let sheet = state.sheet.clone().unwrap();
        assert_eq!(rows(&sheet, "", "/w").iter().map(|r| r.title.as_str()).collect::<Vec<_>>(), ["pop", "push"]);
        assert_eq!(rows(&sheet, "pu", "/w").iter().map(|r| (r.index, r.hint.as_str())).collect::<Vec<_>>(), [(0, "함수")]);

        assert!(state.apply(&update(vec![], true)));
        assert!(state.sheet.is_none());
        assert_eq!(state.notices, ["제안이 없습니다"]);
        assert!(!state.apply(&SessionUpdate::ConnectionLost { reason: "x".into() }));
    }

    #[test]
    fn fr_l31_13_one_definition_jumps_and_several_or_references_list() {
        let mut state = LanguageState::default();
        let found = |feature, locations| lang(LanguageUpdate::Locations { uri: uri("/w/a.rs"), feature, origin: WidgetPos::default(), locations });
        state.apply(&found(Feature::Definition, vec![location("/w/src/lib.rs", 4)]));
        assert!(state.sheet.is_none());
        assert_eq!(state.jump.as_ref().unwrap().uri.path, "/w/src/lib.rs");

        state.apply(&found(Feature::References, vec![location("/w/src/lib.rs", 4)]));
        let sheet = state.sheet.clone().unwrap();
        assert_eq!(sheet.title(), "참조");
        let list = rows(&sheet, "", "/w");
        assert_eq!((list[0].title.as_str(), list[0].detail.as_str(), list[0].hint.as_str()), ("src/lib.rs:4", "let total = 1;", "3열"));
        assert!(rows(&sheet, "main", "/w").is_empty());

        state.apply(&found(Feature::Definition, vec![location("/w/a.rs", 1), location("/w/b.rs", 2)]));
        assert_eq!(state.sheet.as_ref().unwrap().title(), "정의");
        state.notices.clear();
        state.apply(&found(Feature::Definition, vec![]));
        state.apply(&found(Feature::References, vec![]));
        assert_eq!(state.notices, ["정의를 찾지 못했습니다", "참조를 찾지 못했습니다"]);
    }

    #[test]
    fn fr_l31_13_code_actions_put_the_preferred_fix_first_and_mark_disabled_ones() {
        let actions: Vec<CodeAction> = [
            json!({"title": "Extract", "kind": "refactor.extract", "disabled": "Select an expression"}),
            json!({"title": "Sort imports", "kind": "source.organizeImports"}),
            json!({"title": "Add import", "kind": "quickfix", "isPreferred": true}),
        ]
        .iter()
        .filter_map(|v| CodeAction::from_json(1, v))
        .collect();
        let mut state = LanguageState::default();
        state.apply(&lang(LanguageUpdate::CodeActions { uri: uri("/w/a.rs"), version: 0, actions, complete: true }));
        let list = rows(state.sheet.as_ref().unwrap(), "", "/w");
        assert_eq!(list.iter().map(|r| (r.index, r.title.as_str(), r.hint.as_str(), r.enabled)).collect::<Vec<_>>(), [
            (2, "Add import", "빠른 수정", true),
            (0, "Extract", "리팩터링", false),
            (1, "Sort imports", "소스 작업", true),
        ]);
        assert_eq!(list[1].detail, "Select an expression");

        state.apply(&lang(LanguageUpdate::CodeActions { uri: uri("/w/a.rs"), version: 0, actions: vec![], complete: true }));
        assert!(state.sheet.is_none());
        assert_eq!(state.notices, ["사용할 수 있는 코드 작업이 없습니다"]);
    }

    #[test]
    fn fr_l31_13_rename_opens_a_box_and_reports_what_it_did() {
        let mut state = LanguageState::default();
        let at = WidgetRange::empty(WidgetPos::new(2, 4));
        state.apply(&lang(LanguageUpdate::RenameReady { uri: uri("/w/a.rs"), pos: WidgetPos::new(2, 5), range: at, placeholder: "total".into() }));
        assert!(matches!(state.sheet, Some(Sheet::Rename { ref placeholder, .. }) if placeholder == "total"));
        assert!(rows(state.sheet.as_ref().unwrap(), "", "/w").is_empty());
        assert!(matches!(state.close_sheet(), Some(Sheet::Rename { .. })));

        state.apply(&lang(LanguageUpdate::RenameRejected { uri: uri("/w/a.rs"), reason: None }));
        state.apply(&lang(LanguageUpdate::RenameRejected { uri: uri("/w/a.rs"), reason: Some("키워드는 바꿀 수 없습니다".into()) }));
        let summary = EditSummary { files: 3, edits: 7, unsupported: 1, failures: vec![("/w/src/gone.rs".into(), "ENOENT".into())] };
        state.apply(&lang(LanguageUpdate::WorkspaceEditApplied { feature: Feature::Rename, summary }));
        assert_eq!(state.notices, [
            "이 위치의 요소는 이름을 바꿀 수 없습니다",
            "키워드는 바꿀 수 없습니다",
            "이름 바꾸기: 파일 3개에서 7곳을 바꿨습니다. 파일 만들기, 이동, 삭제 1건은 적용하지 않았습니다. 바꾸지 못한 파일 1개: gone.rs",
        ]);
        assert_eq!(summary_text(Feature::CodeActions, &EditSummary::default()), "코드 작업: 바꾼 곳이 없습니다");
    }

    #[test]
    fn fr_l31_13_unavailable_features_say_so_once_and_symbols_stay_quiet() {
        let mut state = LanguageState::default();
        for feature in [Feature::Format, Feature::DocumentSymbols, Feature::Completion] {
            state.apply(&lang(LanguageUpdate::Unavailable { uri: uri("/w/a.rs"), feature }));
        }
        assert_eq!(state.notices, ["이 파일에 맞는 코드 정리 도구가 없습니다", "이 파일에서는 자동 완성을 사용할 수 없습니다"]);
        state.notices.clear();
        state.apply(&lang(LanguageUpdate::Formatted { uri: uri("/w/a.rs"), edits: 3 }));
        assert!(state.notices.is_empty());
        state.apply(&lang(LanguageUpdate::Formatted { uri: uri("/w/a.rs"), edits: 0 }));
        assert_eq!(state.notices, ["이미 정리되어 있습니다"]);
    }

    #[test]
    fn fr_l31_13_parameter_hint_splits_the_label_at_the_active_parameter() {
        let help = LanguageState::sample_signature().unwrap();
        assert_eq!(signature_parts(&help), Some(("insert(key: K, ".to_string(), "value: V".to_string(), ") -> Option<V>".to_string())));
        let mut state = LanguageState::default();
        state.apply(&lang(LanguageUpdate::SignatureHelp { uri: uri("/w/a.rs"), help: Some(help) }));
        assert!(state.signature.is_some() && state.notices.is_empty());
        // The provider has nothing any more: the hint goes without a message.
        state.apply(&lang(LanguageUpdate::SignatureHelp { uri: uri("/w/a.rs"), help: None }));
        assert!(state.signature.is_none() && state.notices.is_empty());
        // Asked for with nothing shown and nothing found: say so.
        state.apply(&lang(LanguageUpdate::SignatureHelp { uri: uri("/w/a.rs"), help: None }));
        assert_eq!(state.notices, ["매개변수 정보가 없습니다"]);
    }

    #[test]
    fn fr_l31_13_breadcrumb_symbols_follow_the_caret_and_go_when_the_file_closes() {
        let symbols = ember_editor::language::navigation::parse_document_symbols(&json!([{
            "name": "Stack", "detail": "", "kind": 22, "tags": [],
            "range": {"startLineNumber": 1, "startColumn": 1, "endLineNumber": 9, "endColumn": 2},
            "selectionRange": {"startLineNumber": 1, "startColumn": 8, "endLineNumber": 1, "endColumn": 13},
            "children": [{"name": "push", "detail": "", "kind": 5, "tags": [],
                "range": {"startLineNumber": 2, "startColumn": 5, "endLineNumber": 4, "endColumn": 6},
                "selectionRange": {"startLineNumber": 2, "startColumn": 8, "endLineNumber": 2, "endColumn": 12}}]
        }]));
        assert_eq!(symbol_names(&symbols, 3, 9), ["Stack", "push"]);
        assert_eq!(symbol_names(&symbols, 7, 1), ["Stack"]);
        assert!(symbol_names(&symbols, 20, 1).is_empty());

        let mut state = LanguageState::default();
        state.apply(&lang(LanguageUpdate::DocumentSymbols { uri: uri("/w/a.rs"), version: 2, symbols }));
        assert_eq!(state.symbols["/w/a.rs"].len(), 1);
        // A closed file's symbols are dropped; the update still goes on to the window.
        assert!(!state.apply(&SessionUpdate::Closed { uri: uri("/w/a.rs") }));
        assert!(state.symbols.is_empty());
    }

    #[test]
    fn fr_l31_13_capture_samples_cover_every_sheet() {
        let u = uri("/w/a.rs");
        for kind in ["completion", "references", "actions"] {
            let sheet = LanguageState::sample(kind, &u).unwrap_or_else(|| panic!("{kind}"));
            assert!(!rows(&sheet, "", "/w").is_empty(), "{kind}");
        }
        assert!(matches!(LanguageState::sample("rename", &u), Some(Sheet::Rename { .. })));
        assert!(LanguageState::sample("nothing", &u).is_none());
    }
}
