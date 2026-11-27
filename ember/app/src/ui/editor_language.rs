//! Language features in the editor window (SPEC FR-L31.13, #442): the completion list, go to
//! definition, references, rename, code actions, format document, the parameter hint and the
//! symbols in the breadcrumbs.
//!
//! The session (`ember_editor::language`) asks the extension host's providers; this module keeps
//! what the window shows ([`LanguageState`], filled on the host's runtime from the session's
//! updates) and draws it ([`view`]).
//!
//! The pinned toolkit (compose-rust v0.0.11, dioxus-compose 94ade333) gives the code editor what
//! these need, and the window uses it:
//! * compose-rust#553: a list opens at the caret the editor reports (`on_caret_rect`,
//!   [`view::set_caret_rect`]) inside the editor pane ([`popup_placement`]), and falls back to the sheet over the window, like the command
//!   palette, until the editor has reported one.
//!   A completion or code actions list asked for with the caret scrolled away first has the
//!   editor reveal the caret (`CodeEditorHandle::reveal`, #895) and opens at its new rectangle.
//! * compose-rust#554: the chords are taken from the focused code (`on_key_preview`,
//!   [`code_key_action`]) and consumed, so F12, F2, Ctrl+Space and the rest work while typing.
//! * compose-rust#555: Cmd (macOS) or Ctrl (elsewhere) and a click on a name goes to its
//!   definition (`on_code_pointer` with `pointer_kinds`, [`pointer_definition`]).
//! * compose-rust#556: snippet completions are forwarded as `WidgetCommand::InsertSnippet` and
//!   opened in the widget's snippet session via `CodeEditorHandle::insert_snippet`.
//! * While a completion or code actions list is open, `on_key_preview` routes `ArrowUp`,
//!   `ArrowDown`, `Enter`, `Tab` and `Escape` directly to the list (`list_key_action`), so focus
//!   stays in the code editor.
//! * The parameter hint anchors above the caret (`hint_style`) once the editor reports its
//!   rectangle, falling back to the bottom of the editor area (`hint_box::STYLE`) before the first
//!   caret report.

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
// The page's key handler and the code editor's `on_key_preview` ([`code_key_action`]) both use it.
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

/// The chord a key offered by the focused code editor (`on_key_preview`, compose-rust#554) asks for.
/// `text` is the DOM key string the event carries; F2 and F12 are named keys and are read from `key`.
pub fn code_key_action(key: compose_rust::Key, text: &str, ctrl: bool, shift: bool, alt: bool, meta: bool) -> Option<LangAction> {
    let name = match key {
        compose_rust::Key::F2 => "F2",
        compose_rust::Key::F12 => "F12",
        _ => text,
    };
    action_for_key(name, ctrl, shift, alt, meta)
}

/// What a key offered by the focused code editor (`on_key_preview`) does while a completion or
/// code actions list is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListKeyAction {
    MoveUp,
    MoveDown,
    Accept,
    Close,
}

/// Whether `sheet` is a completion or code actions list whose navigation keys are routed from the
/// focused code editor's `on_key_preview`.
pub fn is_code_routed_list(sheet: &Sheet) -> bool {
    matches!(sheet, Sheet::Completion { .. } | Sheet::Actions { .. })
}

/// Maps a key from `on_key_preview` to a list navigation, accept or close action while a
/// completion or code actions list is open. Keys with Ctrl, Alt or Meta modifiers, and Shift+Tab,
/// are left alone.
pub fn list_key_action(key: compose_rust::Key, ctrl: bool, shift: bool, alt: bool, meta: bool) -> Option<ListKeyAction> {
    if ctrl || alt || meta {
        return None;
    }
    match key {
        compose_rust::Key::ArrowUp if !shift => Some(ListKeyAction::MoveUp),
        compose_rust::Key::ArrowDown if !shift => Some(ListKeyAction::MoveDown),
        compose_rust::Key::Enter if !shift => Some(ListKeyAction::Accept),
        compose_rust::Key::Tab if !shift => Some(ListKeyAction::Accept),
        compose_rust::Key::Escape => Some(ListKeyAction::Close),
        _ => None,
    }
}

/// Next selected row index for `MoveUp` / `MoveDown` in a list of `count` rows.
pub fn step_selection(current: usize, count: usize, action: ListKeyAction) -> usize {
    if count == 0 {
        return 0;
    }
    let clamped = current.min(count - 1);
    match action {
        ListKeyAction::MoveDown => (clamped + 1) % count,
        ListKeyAction::MoveUp => (clamped + count - 1) % count,
        ListKeyAction::Accept | ListKeyAction::Close => clamped,
    }
}

/// Where a press in the code goes to a definition (compose-rust#555): a primary-button `Down` with
/// Cmd on macOS or Ctrl elsewhere, as Code-OSS does, over a character of the text.
pub fn pointer_definition(pointer: &compose_rust::code::CodePointer, mac: bool) -> Option<WidgetPos> {
    let event = &pointer.event;
    let primary = if mac { event.meta_key() && !event.ctrl_key() } else { event.ctrl_key() && !event.meta_key() };
    let down = event.kind() == compose_rust::PointerKind::Down && event.button() == compose_rust::pointer_button::PRIMARY;
    if !(down && primary && !event.alt_key() && !event.shift_key()) {
        return None;
    }
    pointer.position.map(|p| WidgetPos::new(p.line, p.column))
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

/// The parameter hint's box (#800): shrink-to-fit at the editor area's bottom left, so it is as wide
/// as its text and wraps only past the area less the margins. dioxus-compose sizes an absolutely
/// positioned box with an auto width shrink-to-fit against the room its insets leave
/// (dioxus-compose#227, PR #251), so nothing is measured in Rust.
/// The list as a sheet over the window, like the command palette: before the editor reported a caret.
pub const SHEET_STYLE: &str = "position:fixed; z-index:40; top:60px; left:calc(50vw - min(280px, calc(50vw - 12px))); width:min(560px, calc(100vw - 24px)); right:auto; padding:var(--sp1);";

/// The editor pane's box in the window's px (its left, top, width and height), as the page laid it
/// out: the room a list that belongs to the caret is kept inside.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PaneBox {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// A list's width and the most its rows take, as `popup` draws them.
const LIST_WIDTH: f32 = 440.0;
const LIST_ROWS_MAX: f32 = 320.0;
/// One row's height, and what the title and the query row above the rows take.
const LIST_ROW: f32 = 34.0;
const LIST_CHROME: f32 = 76.0;
/// The gap between the caret line and the list, and the list and the pane's edge.
const LIST_GAP: f32 = 2.0;
const PANE_MARGIN: f32 = 4.0;

/// Where a list opens and how tall its rows may grow.
#[derive(Clone, Debug, PartialEq)]
pub struct Placement {
    pub style: String,
    pub rows_max: f32,
}

/// Where a list opens (compose-rust#553), as Code-OSS's suggest widget does: under the caret line
/// when it fits inside the editor pane, else above it, else on the side with more room with its rows
/// cut to that room, so it never covers the bottom panel. The caret and the pane are in the window's
/// px. With no caret, no pane, or a caret outside the pane (scrolled away), it is the sheet, because
/// a list placed from a caret nobody sees floats unattached. `rows` is how many rows it shows.
pub fn popup_placement(caret: Option<compose_rust::code::CodeRect>, pane: Option<PaneBox>, rows: usize) -> Placement {
    let sheet = Placement { style: SHEET_STYLE.to_string(), rows_max: LIST_ROWS_MAX };
    let (Some(caret), Some(pane)) = (caret, pane) else { return sheet };
    let (top, bottom) = (pane.y, pane.y + pane.height);
    if !caret_in_pane(caret, pane) || pane.width <= 0.0 || pane.height <= 0.0 {
        return sheet;
    }
    let rows_h = (rows.max(1) as f32 * LIST_ROW).min(LIST_ROWS_MAX);
    let height = LIST_CHROME + rows_h;
    let below = bottom - PANE_MARGIN - (caret.bottom() + LIST_GAP);
    let above = (caret.y - LIST_GAP) - (top + PANE_MARGIN);
    let width = LIST_WIDTH.min(pane.width - 2.0 * PANE_MARGIN).max(0.0);
    let left = caret.x.min(pane.x + pane.width - PANE_MARGIN - width).max(pane.x + PANE_MARGIN);
    let frame = format!("position:fixed; z-index:40; left:{left:.0}px; width:{width:.0}px; right:auto; padding:var(--sp1);");
    let (open_below, room) = if below >= height {
        (true, height)
    } else if above >= height {
        (false, height)
    } else {
        (below >= above, below.max(above))
    };
    let rows_max = (rows_h - (height - room)).clamp(0.0, LIST_ROWS_MAX);
    let style = if open_below {
        format!("{frame} top:{:.0}px; bottom:auto;", caret.bottom() + LIST_GAP)
    } else {
        // Anchored by its bottom edge, so a shorter list still sits right above the caret line.
        format!("{frame} top:auto; bottom:calc(100vh - {:.0}px);", caret.y - LIST_GAP)
    };
    Placement { style, rows_max }
}

/// Whether the caret line is whole inside the editor pane (not scrolled above, below or aside).
pub fn caret_in_pane(caret: compose_rust::code::CodeRect, pane: PaneBox) -> bool {
    caret.y >= pane.y && caret.bottom() <= pane.y + pane.height && caret.x >= pane.x && caret.x <= pane.x + pane.width
}

/// Whether a list that is about to open first scrolls the caret into view (#895), as Code-OSS's
/// suggest and quick-fix widgets do (`revealPosition` before `showAt`): a completion or code
/// actions list, with a caret the editor reported in a pane that was laid out, and that caret
/// outside the pane. The list then waits for the caret's new rectangle and opens at it. Locations
/// and rename are not anchored lists, and with no caret or pane there is nothing to reveal.
pub fn list_reveals_caret(sheet: &Sheet, caret: Option<compose_rust::code::CodeRect>, pane: Option<PaneBox>) -> bool {
    let anchored = matches!(sheet, Sheet::Completion { .. } | Sheet::Actions { .. });
    match (caret, pane) {
        (Some(caret), Some(pane)) => anchored && !caret_in_pane(caret, pane),
        _ => false,
    }
}

/// The room the page leaves above the code editor's box inside the pane (`--sp2`, FR-L31.27).
pub const CODE_TOP_ROOM: f32 = 8.0;

/// Whether a caret the reader just moved (#301, FR-L31.40) has to be scrolled into view: its line
/// is not whole between the top of the code editor's box (the pane less `CODE_TOP_ROOM`) and the
/// pane's bottom edge. Before compose-rust#841 (v0.0.11, `CodeEditorSurface.kt`), the toolkit's
/// own scroll on an arrow key stopped about a line short of the bottom (measured: line 31 at y
/// 603.92 to 625.92 in a box that ends at 591), so the window asks the editor for the nearest
/// reveal, as Code-OSS reveals the cursor after every move. Only the vertical extent counts: the
/// reveal scrolls rows.
pub fn moved_caret_needs_reveal(caret: compose_rust::code::CodeRect, pane: PaneBox) -> bool {
    const SLACK: f32 = 0.5;
    caret.y < pane.y + CODE_TOP_ROOM - SLACK || caret.bottom() > pane.y + pane.height + SLACK
}

/// The range the editor is asked to reveal for the caret at `(line, col)`, both 1-based as the
/// window keeps them: the empty range at that place, in the editor's 0-based positions.
pub fn caret_reveal_range(line: usize, col: usize) -> compose_rust::code::CodeRange {
    let (line, col) = (line.saturating_sub(1) as u32, col.saturating_sub(1) as u32);
    compose_rust::code::CodeRange::of(line, col, line, col)
}

pub mod hint_box {
    /// Shared visual frame of the parameter hint box.
    pub const CHROME_STYLE: &str = "box-sizing:border-box; padding:8px 12px; display:flex; align-items:center; gap:8px; background:var(--surface); border:1px solid var(--line); border-radius:var(--r); box-shadow:var(--shadow-lg);";
    /// Fallback box: anchored at the editor area's bottom left before the first caret report.
    pub const STYLE: &str = "position:absolute; z-index:15; left:12px; bottom:12px; max-width:calc(100% - 24px); box-sizing:border-box; padding:8px 12px; display:flex; align-items:center; gap:8px; background:var(--surface); border:1px solid var(--line); border-radius:var(--r); box-shadow:var(--shadow-lg);";
    /// The text: one line while it fits, wrapped anywhere past that.
    pub const TEXT_STYLE: &str = "flex:0 1 auto; min-width:0; font-family:var(--mono); font-size:13px; line-height:20px; white-space:pre-wrap; overflow-wrap:anywhere; color:var(--muted);";
}

/// Minimum vertical room needed above the caret to open the parameter hint above the line.
const HINT_MIN_ABOVE: f32 = 44.0;
/// Minimum horizontal room kept to the right edge of the pane when clamping the hint's left edge.
const HINT_MIN_WIDTH: f32 = 240.0;
/// Window edge margin when anchoring the parameter hint before the pane box is measured.
const HINT_WINDOW_MARGIN: f32 = 12.0;

/// Inline style for the parameter hint: anchored above the caret (clamped inside the editor pane
/// or window) once the editor reports `caret`, flipping below when there is no room above, and
/// falling back to [`hint_box::STYLE`] before the first caret report or when the caret is scrolled
/// out of the pane.
pub fn hint_style(caret: Option<compose_rust::code::CodeRect>, pane: Option<PaneBox>) -> String {
    let Some(caret) = caret else { return hint_box::STYLE.to_string() };
    if let Some(pane) = pane {
        if !caret_in_pane(caret, pane) || pane.width <= 0.0 || pane.height <= 0.0 {
            return hint_box::STYLE.to_string();
        }
        let max_w = (pane.width - 2.0 * PANE_MARGIN).max(0.0);
        let min_w = HINT_MIN_WIDTH.min(max_w);
        let min_left = pane.x + PANE_MARGIN;
        let max_left = (pane.x + pane.width - PANE_MARGIN - min_w).max(min_left);
        let left = caret.x.clamp(min_left, max_left);
        let avail_w = (pane.x + pane.width - PANE_MARGIN - left).max(0.0);
        let above = (caret.y - LIST_GAP) - (pane.y + PANE_MARGIN);
        let below = (pane.y + pane.height - PANE_MARGIN) - (caret.bottom() + LIST_GAP);
        let open_above = above >= HINT_MIN_ABOVE || above >= below;
        let vert = if open_above {
            let bottom_y = (caret.y - LIST_GAP).max(pane.y + PANE_MARGIN);
            format!("top:auto; bottom:calc(100vh - {bottom_y:.0}px);")
        } else {
            let top_y = (caret.bottom() + LIST_GAP).min(pane.y + pane.height - PANE_MARGIN);
            format!("top:{top_y:.0}px; bottom:auto;")
        };
        return format!(
            "position:fixed; z-index:40; left:{left:.0}px; right:auto; {vert} max-width:{avail_w:.0}px; {}",
            hint_box::CHROME_STYLE
        );
    }
    // Caret reported before pane_box() is known: anchor to the caret clamped inside the window.
    let left = caret.x.max(HINT_WINDOW_MARGIN);
    let open_above = caret.y - LIST_GAP >= HINT_MIN_ABOVE + HINT_WINDOW_MARGIN;
    let vert = if open_above {
        format!("top:auto; bottom:calc(100vh - {:.0}px);", caret.y - LIST_GAP)
    } else {
        format!("top:{:.0}px; bottom:auto;", (caret.bottom() + LIST_GAP).max(HINT_WINDOW_MARGIN))
    };
    format!(
        "position:fixed; z-index:40; left:{left:.0}px; right:auto; {vert} max-width:calc(100vw - {left:.0}px - {HINT_WINDOW_MARGIN:.0}px); {}",
        hint_box::CHROME_STYLE
    )
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
    use crate::ui::editor_workbench::page::{icon, icon_tone};
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
        /// Where the code editor last drew its caret, in the window's dp (compose-rust#553).
        static CARET_RECT: Cell<Option<compose_rust::code::CodeRect>> = const { Cell::new(None) };
        /// The element the editor pane is drawn in, whose box a list is kept inside.
        static PANE_MOUNT: RefCell<Option<std::rc::Rc<dioxus_elements::MountedData>>> = const { RefCell::new(None) };
        /// The editor is to scroll the caret into view on its next draw (#895).
        static REVEAL_CARET: Cell<bool> = const { Cell::new(false) };
        /// The sheet (its `sheet_seq`) that asked for the reveal and waits for the caret's new
        /// rectangle before it opens.
        static AWAIT_CARET: Cell<Option<u64>> = const { Cell::new(None) };
        /// The last sheet that asked for a reveal: each sheet asks once, so a caret that stays
        /// out of view after it falls back to the sheet over the window.
        static REVEALED_FOR: Cell<Option<u64>> = const { Cell::new(None) };
        /// The reader moved the caret since its rectangle was last reported (#301, FR-L31.40). A
        /// rectangle that changes with no move (the reader scrolled) never brings the caret back.
        static CARET_MOVED: Cell<bool> = const { Cell::new(false) };
    }

    /// The code editor's selection changed: the next caret rectangle it reports is checked against
    /// the pane, and a caret left out of view is revealed (#301, FR-L31.40).
    pub fn caret_moved() {
        CARET_MOVED.with(|cell| cell.set(true));
    }

    /// The code editor's `on_caret_rect`: kept for the next list to open under it. Nothing is
    /// redrawn, because a list is placed when it opens, unless a list is waiting for the caret it
    /// asked to be revealed (#895): then the window draws again and the list opens at it.
    pub fn set_caret_rect(s: &Shared, rect: compose_rust::code::CodeRect) {
        CARET_RECT.with(|cell| cell.set(Some(rect)));
        if CARET_MOVED.with(|cell| cell.replace(false)) && pane_box().is_some_and(|pane| super::moved_caret_needs_reveal(rect, pane)) {
            REVEAL_CARET.with(|cell| cell.set(true));
            redraw(s);
        }
        if AWAIT_CARET.with(|cell| cell.take()).is_some() {
            redraw(s);
        }
        if s.host.state.lock().unwrap().language.signature.is_some() {
            redraw(s);
        }
    }

    /// Whether the editor is to reveal the caret now (#895). Read once by the editor pane, which
    /// sends `CodeEditorHandle::reveal` for the caret when it is.
    pub fn take_caret_reveal() -> bool {
        REVEAL_CARET.with(|cell| cell.replace(false))
    }

    /// The editor pane's element (its `onmounted`): a list is kept inside the box it was last laid out in.
    pub fn set_pane_mount(mounted: std::rc::Rc<dioxus_elements::MountedData>) {
        PANE_MOUNT.with(|cell| *cell.borrow_mut() = Some(mounted));
    }

    /// The editor pane's box from the last layout, or None before it was laid out.
    fn pane_box() -> Option<super::PaneBox> {
        use futures_util::FutureExt;
        PANE_MOUNT.with(|cell| {
            let r = cell.borrow().as_ref()?.get_client_rect().now_or_never()?.ok()?;
            let pane = super::PaneBox { x: r.origin.x as f32, y: r.origin.y as f32, width: r.size.width as f32, height: r.size.height as f32 };
            (pane.width > 0.0 && pane.height > 0.0).then_some(pane)
        })
    }

    /// The code editor's `on_key_preview` (compose-rust#554): when a completion or code actions
    /// list is open, `ArrowUp`, `ArrowDown`, `Enter`, `Tab` and `Escape` drive the list directly so
    /// focus can stay in the code editor; otherwise Escape closes the parameter hint and language
    /// chords run their features. Any other key goes on to the editor.
    pub fn code_key(s: &Shared, event: &compose_rust::KeyEvent) {
        // FR-L31.42: Alt switches the debug hover to this hover, another key hides it.
        crate::ui::editor_debug_hover::view::code_key(s, event);
        let open_list = {
            let state = s.host.state.lock().unwrap();
            state
                .language
                .sheet
                .as_ref()
                .filter(|sheet| super::is_code_routed_list(sheet))
                .cloned()
                .map(|sheet| (sheet, state.language.sheet_seq))
        };
        if let Some((sheet, seq)) = open_list {
            if let Some(list_action) = super::list_key_action(
                event.key(),
                event.ctrl_key(),
                event.shift_key(),
                event.alt_key(),
                event.meta_key(),
            ) {
                event.consume();
                let ui = ui();
                let (mut query, mut sel, mut seen) = (ui.query, ui.sel, ui.seq);
                if *seen.peek() != seq {
                    seen.set(seq);
                    sel.set(0);
                    query.set(String::new());
                }
                let text = query.peek().clone();
                let shown: Vec<_> = rows(&sheet, &text, &s.folder).into_iter().take(MAX_ROWS).collect();
                let count = shown.len();
                let chosen = (*sel.peek()).min(count.saturating_sub(1));
                match list_action {
                    super::ListKeyAction::Close => cancel(s),
                    super::ListKeyAction::MoveUp | super::ListKeyAction::MoveDown => {
                        if count > 0 {
                            sel.set(super::step_selection(chosen, count, list_action));
                            redraw(s);
                        }
                    }
                    super::ListKeyAction::Accept => {
                        if let Some(index) = shown.get(chosen).filter(|row| row.enabled).map(|row| row.index) {
                            choose(s, index);
                        }
                    }
                }
                return;
            }
        }
        if event.key() == compose_rust::Key::Escape && dismiss_hint(s) {
            event.consume();
            return;
        }
        let action = super::code_key_action(event.key(), event.text(), event.ctrl_key(), event.shift_key(), event.alt_key(), event.meta_key());
        if let Some(action) = action {
            event.consume();
            run(s, action);
            return;
        }
        // F5 and Shift+F5 (FR-L31.42): the code editor keeps keys it does not pass on, so the
        // page's key handler would never see them while the code has focus.
        crate::ui::editor_debug::code_key(s, event);
    }

    /// The code editor's `on_code_pointer` (compose-rust#555): Cmd or Ctrl and a click on a name asks
    /// for its definition there, and the press does not move the caret.
    pub fn code_pointer(s: &Shared, pointer: &compose_rust::code::CodePointer) {
        // FR-L31.42: a press in the editor hides the debug hover (Code-OSS `onEditorMouseDown`).
        crate::ui::editor_debug_hover::view::hide(s);
        let mac = crate::ui::keymap::Os::current() == crate::ui::keymap::Os::Mac;
        let Some(pos) = super::pointer_definition(pointer, mac) else { return };
        let Some(uri) = current(s) else { return };
        pointer.event.consume();
        send(s, LanguageRequest::Definition { uri, pos: Some(pos) });
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
                {icon_tone("chev", 11, "muted")}
                // The mockup's symbol crumb (`.ed-crumbs .fn`): `--text-2` in the code font.
                span { key: "sym{i}", class: "fn", "{name}" }
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

    /// The parameter hint: the signature with the parameter being typed set off. Anchored above the
    /// caret once the editor reports its rectangle, falling back to the bottom of the editor area
    /// before the first caret report.
    fn hint(s: &Shared, help: &ember_editor::language::SignatureHelp) -> Element {
        use super::hint_box;
        let Some((before, active, after)) = signature_parts(help) else { return rsx! {} };
        let count = help.signatures.len();
        let close_s = s.clone();
        let (caret_rect, pane) = (CARET_RECT.with(|cell| cell.get()), pane_box());
        let style = super::hint_style(caret_rect, pane);
        rsx! {
            div { role: "status", "aria-label": "매개변수 정보", style: "{style}",
                code { style: hint_box::TEXT_STYLE,
                    "{before}"
                    span { style: "font-weight:var(--fw-semibold); color:var(--text);", "{active}" }
                    "{after}"
                }
                if count > 1 { span { class: "muted", style: "flex:none; font-size:var(--fs-xs);", "외 {count - 1}개" } }
                button { class: "ed-ib", style: "flex:none;", title: "닫기", "aria-label": "매개변수 정보 닫기", onclick: move |_| { dismiss_hint(&close_s); }, {icon("close")} }
            }
        }
    }

    /// The parameter hint inside the editor area (`.ed-main`, positioned), when no sheet is open: it
    /// stays above the bottom panel and sizes to its text up to the area's width (#800).
    pub fn hint_in_editor(s: &Shared) -> Element {
        capture_hook(s);
        let (sheet_open, signature) = {
            let state = s.host.state.lock().unwrap();
            (state.language.sheet.is_some(), state.language.signature.clone())
        };
        match signature {
            Some(help) if !sheet_open => hint(s, &help),
            _ => rsx! {},
        }
    }

    /// The list over the window, when there is one to show: under the caret once the editor has
    /// reported it (compose-rust#553), else as a sheet like the command palette. Completion and
    /// code actions lists leave focus in the code editor and take navigation keys from
    /// `on_key_preview`.
    pub fn overlay(s: &Shared) -> Element {
        capture_hook(s);
        let ui = ui();
        let (sheet, seq, signature) = {
            let state = s.host.state.lock().unwrap();
            (state.language.sheet.clone(), state.language.sheet_seq, state.language.signature.clone())
        };
        // With no sheet, the parameter hint is drawn inside the editor area (`hint_in_editor`, #800).
        let _ = signature;
        let Some(sheet) = sheet else { return rsx! {} };
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
        let is_code_list = super::is_code_routed_list(&sheet);
        let list = rows(&sheet, &text, &s.folder);
        let total = list.len();
        let shown: Vec<_> = list.into_iter().take(MAX_ROWS).collect();
        let chosen = (*sel.read()).min(shown.len().saturating_sub(1));
        let enter = shown.get(chosen).filter(|row| row.enabled).map(|row| row.index);
        let count = shown.len();
        let (title, placeholder, loading) = (sheet.title(), sheet.placeholder(), sheet.loading());
        let (scrim_s, key_s) = (s.clone(), s.clone());
        let rows_drawn = if is_rename { 0 } else if loading { count + 1 } else { count.max(1) };
        let (caret_rect, pane) = (CARET_RECT.with(|cell| cell.get()), pane_box());
        // #895: a completion or code actions list asked for with the caret scrolled away first has
        // the editor reveal the caret, then opens at the rectangle the editor reports for it.
        if AWAIT_CARET.with(|cell| cell.get()) == Some(seq) {
            return rsx! {};
        }
        if super::list_reveals_caret(&sheet, caret_rect, pane) && REVEALED_FOR.with(|cell| cell.get()) != Some(seq) {
            REVEALED_FOR.with(|cell| cell.set(Some(seq)));
            AWAIT_CARET.with(|cell| cell.set(Some(seq)));
            REVEAL_CARET.with(|cell| cell.set(true));
            redraw(s);
            return rsx! {};
        }
        let placement = super::popup_placement(caret_rect, pane, rows_drawn);
        rsx! {
            div { style: "position:fixed; left:0; top:0; right:0; bottom:0; z-index:39;", onclick: move |_| cancel(&scrim_s) }
            div { class: "ed-menu", role: "dialog", "aria-label": "{title}",
                style: "{placement.style}",
                div { style: "padding:var(--sp1) var(--sp3) 0; color:var(--muted); font-size:var(--fs-xs);", "{title}" }
                // FR-L31.29: the query row is a row of the popover, not a framed field inside the frame.
                div { class: "ed-palq", style: "margin:var(--sp1) calc(var(--sp1) * -1) var(--sp1);",
                    {icon_tone(if is_rename { "edit" } else { "search" }, 16, "muted")}
                    input {
                        "aria-label": "{title}", placeholder: "{placeholder}", value: "{text}", autofocus: !is_code_list,
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
                                Key::Tab if !is_rename => {
                                    event.stop_propagation();
                                    event.prevent_default();
                                    if let Some(index) = enter {
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
                    div { style: "max-height:{placement.rows_max:.0}px; overflow:auto;", role: "listbox",
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
    #[test]
    fn fr_l31_13_parameter_hint_sizes_to_its_text_within_the_editor_area() {
        // #800: the hint was fixed to the window, then stacked one glyph per line, then sized in Rust
        // from an estimated advance. dioxus-compose#227 (PR #251) sizes an absolute box with an auto
        // width shrink-to-fit, so the stylesheet alone sizes it.
        use super::hint_box::*;
        assert!(STYLE.contains("position:absolute;") && STYLE.contains("left:12px;") && STYLE.contains("bottom:12px;"));
        assert!(STYLE.contains("max-width:calc(100% - 24px);"), "the hint stops at the area less the margins");
        assert!(!STYLE.replace("max-width:", "").contains("width:"), "no width is fixed: {STYLE}");
        assert!(!STYLE.contains("height:") && !STYLE.contains("fixed") && !STYLE.contains("top:"));
        assert!(TEXT_STYLE.contains("min-width:0;") && !TEXT_STYLE.replace("min-width:", "").contains("width:"), "{TEXT_STYLE}");
        let src = include_str!("editor_language.rs");
        let production = &src[..src.find("#[cfg(test)]").expect("tests")];
        assert!(!production.contains(concat!("TODO(dioxus-compose#", "227)")), "the hint width workaround is gone");
        assert!(!production.contains(concat!("TODO(dioxus-compose#", "207)")), "no width is read, so no resize stand-in is left");
        assert!(!production.contains(concat!("fn editor_area", "_width")), "the area's width is not measured");
        let page = include_str!("editor_workbench.rs");
        let main = page.find(r#"section { class: "ed-main""#).expect("editor area");
        let hint = page.find("editor_language::view::hint_in_editor(&s)").expect("hint call");
        let right = page.find("stripe_column(&s, sel, keymap, Strip::Right)").expect("right stripe");
        assert!(main < hint && hint < right, "the hint is drawn inside the editor area");
        assert!(!page.contains(concat!("mount_editor", "_area(")), "the editor area is not measured for the hint");
    }

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

    // #477: a symbol crumb is the mockup's `<span class="fn">`, which editor.css sets in `--text-2`
    // and the code font; without the class it was a muted path segment.
    #[test]
    fn fr_l31_symbol_crumbs_carry_the_mockup_fn_class() {
        let src = include_str!("editor_language.rs");
        let production = &src[..src.find("#[cfg(test)]").expect("tests")];
        assert!(production.contains("span { key: \"sym{i}\", class: \"fn\", \"{name}\" }"), "a symbol crumb lost its fn class");
        let css = include_str!("../../assets/editor/editor.css");
        assert!(css.contains(".ed-crumbs .fn{color:var(--text-2);font-family:var(--mono)}"), "editor.css no longer styles .ed-crumbs .fn");
    }

    #[test]
    fn fr_l31_13_the_focused_code_offers_the_chords() {
        use compose_rust::Key;
        // compose-rust#554: F2 and F12 are named keys, the rest come as their DOM key string.
        assert_eq!(code_key_action(Key::F12, "F12", false, false, false, false), Some(LangAction::Definition));
        assert_eq!(code_key_action(Key::F12, "", false, true, false, false), Some(LangAction::References));
        assert_eq!(code_key_action(Key::F2, "", false, false, false, false), Some(LangAction::Rename));
        assert_eq!(code_key_action(Key::Other, " ", true, false, false, false), Some(LangAction::Suggest));
        assert_eq!(code_key_action(Key::Other, ".", false, false, false, true), Some(LangAction::QuickFix));
        assert_eq!(code_key_action(Key::Other, "a", false, false, false, false), None, "typing goes to the editor");
        assert_eq!(code_key_action(Key::Enter, "Enter", false, false, false, false), None);
    }

    #[test]
    fn fr_l31_13_cmd_or_ctrl_click_goes_to_the_definition() {
        use compose_rust::code::{CodePointer, Position};
        use compose_rust::{pointer_button, PointerEvent, PointerKind, PointerType, WheelDeltaMode};
        // Modifier bits as the wire carries them: shift 1, ctrl 2, alt 4, meta 8.
        let press = |kind: PointerKind, button: u32, modifiers: u32, at: Option<Position>| CodePointer {
            event: PointerEvent::new(kind, 1, PointerType::Mouse, 0.0, 0.0, button, button, modifiers, 1, 0.0, 0.0, WheelDeltaMode::Pixel, 0),
            position: at,
        };
        let at = Some(Position::new(4, 7));
        let primary = pointer_button::PRIMARY;
        assert_eq!(pointer_definition(&press(PointerKind::Down, primary, 8, at), true), Some(WidgetPos::new(4, 7)), "Cmd+click on macOS");
        assert_eq!(pointer_definition(&press(PointerKind::Down, primary, 2, at), false), Some(WidgetPos::new(4, 7)), "Ctrl+click elsewhere");
        assert_eq!(pointer_definition(&press(PointerKind::Down, primary, 2, at), true), None, "Ctrl+click on macOS is the context menu");
        assert_eq!(pointer_definition(&press(PointerKind::Down, primary, 0, at), false), None, "a plain click moves the caret");
        assert_eq!(pointer_definition(&press(PointerKind::Down, primary, 2 | 1, at), false), None);
        assert_eq!(pointer_definition(&press(PointerKind::Up, primary, 2, at), false), None);
        assert_eq!(pointer_definition(&press(PointerKind::Down, pointer_button::SECONDARY, 2, at), false), None);
        assert_eq!(pointer_definition(&press(PointerKind::Down, primary, 2, None), false), None, "over the gutter");
    }

    #[test]
    fn fr_l31_13_a_list_opens_under_the_caret() {
        use compose_rust::code::CodeRect;
        // The editor pane: 800 x 600 at (200, 100), so its bottom is at 700.
        let pane = Some(PaneBox { x: 200.0, y: 100.0, width: 800.0, height: 600.0 });
        let sheet = Placement { style: SHEET_STYLE.to_string(), rows_max: 320.0 };
        assert_eq!(popup_placement(None, pane, 5), sheet, "before the editor reports a caret, the sheet");
        assert_eq!(popup_placement(Some(CodeRect::new(300.0, 120.0, 2.0, 22.0)), None, 5), sheet, "no pane box, the sheet");

        // Below: 5 rows (76 + 170 = 246 px) fit under a caret on the first lines.
        let below = popup_placement(Some(CodeRect::new(300.0, 120.0, 2.0, 22.0)), pane, 5);
        assert!(below.style.contains("top:144px;") && below.style.contains("left:300px;") && below.style.contains("width:440px;"), "{below:?}");
        assert_eq!(below.rows_max, 170.0);

        // Above: the caret line ends at 680, 16 px over the pane's bottom; the list ends 2 px over the line.
        let above = popup_placement(Some(CodeRect::new(900.0, 658.0, 2.0, 22.0)), pane, 5);
        assert!(above.style.contains("top:auto;") && above.style.contains("bottom:calc(100vh - 656px);"), "{above:?}");
        assert!(above.style.contains("left:556px;"), "kept inside the pane's right edge: {above:?}");
        assert_eq!(above.rows_max, 170.0);

        // Clamped: a 200 px pane fits neither side; the side with more room, with the rows cut to it.
        let short = Some(PaneBox { x: 200.0, y: 100.0, width: 800.0, height: 200.0 });
        let clamped = popup_placement(Some(CodeRect::new(300.0, 130.0, 2.0, 22.0)), short, 20);
        let room = 300.0 - 4.0 - 154.0; // the pane's bottom less the margin, less the line's bottom and gap
        assert!(clamped.style.contains("top:154px;"), "{clamped:?}");
        assert_eq!(clamped.rows_max, room - 76.0, "the list ends inside the pane, not over the bottom panel");
        let high = popup_placement(Some(CodeRect::new(300.0, 250.0, 2.0, 22.0)), short, 20);
        assert!(high.style.contains("bottom:calc(100vh - 248px);"), "{high:?}");
        assert_eq!(high.rows_max, (248.0 - 104.0) - 76.0);

        // Caret off screen: scrolled above the pane, below it, or out to the side.
        for caret in [CodeRect::new(300.0, 60.0, 2.0, 22.0), CodeRect::new(300.0, 690.0, 2.0, 22.0), CodeRect::new(-40.0, 300.0, 2.0, 22.0)] {
            assert_eq!(popup_placement(Some(caret), pane, 5), sheet, "{caret:?}");
        }
    }

    #[test]
    fn fr_l31_13_the_code_editor_takes_keys_pointer_and_caret() {
        let page = include_str!("editor_workbench.rs");
        let pane = &page[page.find("fn editor_pane()").unwrap()..page.find("pub mod page {").unwrap()];
        assert!(pane.contains("on_key_preview:") && pane.contains("editor_language::view::code_key("), "compose-rust#554");
        assert!(pane.contains("pointer_kinds: Some(compose_rust::PointerKinds::DOWN)") && pane.contains("editor_language::view::code_pointer("), "compose-rust#555");
        assert!(pane.contains("on_caret_rect:") && pane.contains("editor_language::view::set_caret_rect(&caret_s, e.rect)"), "compose-rust#553");
        let src = include_str!("editor_language.rs");
        let production = &src[..src.find("#[cfg(test)]").unwrap()];
        assert!(production.contains("let (caret_rect, pane) = (CARET_RECT.with(|cell| cell.get()), pane_box());"));
        assert!(production.contains("super::popup_placement(caret_rect, pane, rows_drawn)"), "the list is placed from the caret and the pane");
        assert!(page.contains("editor_language::view::set_pane_mount("), "the pane's box is known");
        for issue in ["553", "554", "555"] {
            assert!(!production.contains(&format!("TODO(compose-rust#{issue}")), "#{issue} is in use");
            assert!(!page.contains(&format!("TODO(compose-rust#{issue}")), "#{issue} is in use");
        }
    }

    #[test]
    fn fr_l31_13_a_list_reveals_a_caret_scrolled_out_of_view_first() {
        use compose_rust::code::{CodeRange, CodeRect};
        let uri = uri("/w/src/main.rs");
        let pane = Some(PaneBox { x: 200.0, y: 100.0, width: 800.0, height: 600.0 });
        let completion = LanguageState::sample("completion", &uri).unwrap();
        let actions = LanguageState::sample("actions", &uri).unwrap();
        let references = LanguageState::sample("references", &uri).unwrap();
        let rename = LanguageState::sample("rename", &uri).unwrap();
        let away = Some(CodeRect::new(300.0, 60.0, 2.0, 22.0));
        let seen = Some(CodeRect::new(300.0, 120.0, 2.0, 22.0));
        // Code-OSS reveals the caret, then shows the suggest and quick-fix widgets at it.
        assert!(list_reveals_caret(&completion, away, pane));
        assert!(list_reveals_caret(&actions, away, pane));
        assert!(!list_reveals_caret(&completion, seen, pane), "a caret on screen opens the list at once");
        assert!(!list_reveals_caret(&references, away, pane), "a locations list is not anchored at the caret");
        assert!(!list_reveals_caret(&rename, away, pane));
        assert!(!list_reveals_caret(&completion, None, pane), "no caret reported yet: the sheet");
        assert!(!list_reveals_caret(&completion, away, None), "no pane box: the sheet");
        // The window's caret is 1-based; the editor is asked for the empty range at it, 0-based.
        assert_eq!(caret_reveal_range(12, 5), CodeRange::of(11, 4, 11, 4));
        assert_eq!(caret_reveal_range(0, 0), CodeRange::of(0, 0, 0, 0));
    }

    #[test]
    fn fr_l31_13_the_editor_pane_sends_the_caret_reveal() {
        // What is sent to the toolkit: `CodeEditorHandle::reveal` for the caret, the nearest scroll
        // (Code-OSS's `revealPosition`), when a list asked for it, and the list waits for the caret.
        let page = include_str!("editor_workbench.rs");
        let pane = &page[page.find("fn editor_pane()").unwrap()..page.find("pub mod page {").unwrap()];
        assert!(pane.contains("if crate::ui::editor_language::view::take_caret_reveal() {"));
        assert!(pane.contains("handle.reveal(version, crate::ui::editor_language::caret_reveal_range(line, col), compose_rust::schema::RevealAlign::Nearest);"));
        let src = include_str!("editor_language.rs");
        let production = &src[..src.find("#[cfg(test)]").unwrap()];
        assert!(production.contains("if super::list_reveals_caret(&sheet, caret_rect, pane) && REVEALED_FOR.with(|cell| cell.get()) != Some(seq) {"));
        assert!(production.contains("if AWAIT_CARET.with(|cell| cell.take()).is_some() {"), "the caret's new rectangle opens the list");
    }

    #[test]
    fn fr_l31_40_a_moved_caret_left_out_of_view_is_revealed() {
        use compose_rust::code::CodeRect;
        // The window's numbers at 1440x900: pane 1054x456 at 337,135, the code editor's box from 143.
        let pane = PaneBox { x: 337.0, y: 135.0, width: 1054.0, height: 456.0 };
        let line = |y: f32| CodeRect::new(393.0, y, 2.0, 22.0);
        assert!(!moved_caret_needs_reveal(line(142.92), pane), "the first line, at the box's top");
        assert!(!moved_caret_needs_reveal(line(568.92), pane), "a line that ends at the pane's bottom");
        assert!(moved_caret_needs_reveal(line(582.92), pane), "line 21, cut at the bottom");
        assert!(moved_caret_needs_reveal(line(603.92), pane), "the caret under the bottom panel");
        assert!(moved_caret_needs_reveal(line(130.0), pane), "a line cut at the top");
        assert!(!moved_caret_needs_reveal(CodeRect::new(2000.0, 300.0, 2.0, 22.0), pane), "only rows are revealed");
        // Wired: a selection change marks the move, and the next caret rectangle outside the pane
        // has the editor pane reveal the caret (the nearest scroll of FR-L31.13's reveal).
        let page = include_str!("editor_workbench.rs");
        let editor = &page[page.find("fn editor_pane()").unwrap()..page.find("pub mod page {").unwrap()];
        assert!(editor.contains("crate::ui::editor_language::view::caret_moved();"));
        let src = include_str!("editor_language.rs");
        let production = &src[..src.find("#[cfg(test)]").unwrap()];
        assert!(production.contains("if CARET_MOVED.with(|cell| cell.replace(false)) && pane_box().is_some_and(|pane| super::moved_caret_needs_reveal(rect, pane)) {"));
    }

    #[test]
    fn fr_l31_13_on_key_preview_routes_list_navigation_accept_and_escape() {
        use compose_rust::Key;
        let u = uri("/w/src/main.rs");
        assert!(is_code_routed_list(&LanguageState::sample("completion", &u).unwrap()));
        assert!(is_code_routed_list(&LanguageState::sample("actions", &u).unwrap()));
        assert!(!is_code_routed_list(&LanguageState::sample("references", &u).unwrap()));
        assert!(!is_code_routed_list(&LanguageState::sample("rename", &u).unwrap()));

        assert_eq!(list_key_action(Key::ArrowUp, false, false, false, false), Some(ListKeyAction::MoveUp));
        assert_eq!(list_key_action(Key::ArrowDown, false, false, false, false), Some(ListKeyAction::MoveDown));
        assert_eq!(list_key_action(Key::Enter, false, false, false, false), Some(ListKeyAction::Accept));
        assert_eq!(list_key_action(Key::Tab, false, false, false, false), Some(ListKeyAction::Accept));
        assert_eq!(list_key_action(Key::Escape, false, false, false, false), Some(ListKeyAction::Close));
        assert_eq!(list_key_action(Key::Tab, false, true, false, false), None, "Shift+Tab is not accept");
        assert_eq!(list_key_action(Key::ArrowDown, true, false, false, false), None, "modified arrow stays with editor");
        assert_eq!(list_key_action(Key::Other, false, false, false, false), None, "typing stays with editor");

        assert_eq!(step_selection(0, 3, ListKeyAction::MoveDown), 1);
        assert_eq!(step_selection(2, 3, ListKeyAction::MoveDown), 0);
        assert_eq!(step_selection(0, 3, ListKeyAction::MoveUp), 2);
        assert_eq!(step_selection(2, 3, ListKeyAction::MoveUp), 1);
        assert_eq!(step_selection(5, 3, ListKeyAction::MoveDown), 0, "clamped before wrap");
        assert_eq!(step_selection(0, 0, ListKeyAction::MoveDown), 0, "empty list");

        let src = include_str!("editor_language.rs");
        let production = &src[..src.find("#[cfg(test)]").unwrap()];
        assert!(production.contains("super::is_code_routed_list(sheet)"));
        assert!(production.contains("super::list_key_action("));
        assert!(production.contains("autofocus: !is_code_list"), "completion/action lists leave focus in the code editor");
    }

    #[test]
    fn fr_l31_13_parameter_hint_anchors_above_caret_with_bottom_fallback() {
        use compose_rust::code::CodeRect;
        let pane = Some(PaneBox { x: 200.0, y: 100.0, width: 800.0, height: 600.0 });

        // Before the first caret report: falls back to the bottom of the editor area.
        assert_eq!(hint_style(None, pane), hint_box::STYLE);
        assert_eq!(hint_style(None, None), hint_box::STYLE);

        // With room above the caret: anchors above the caret line.
        let above = hint_style(Some(CodeRect::new(320.0, 260.0, 2.0, 22.0)), pane);
        assert!(above.contains("position:fixed;") && above.contains("left:320px;"), "{above}");
        assert!(above.contains("top:auto; bottom:calc(100vh - 258px);"), "{above}");
        assert!(above.contains("max-width:676px;"), "{above}");

        // Near the top of the pane with no room above: flips below the caret line.
        let below = hint_style(Some(CodeRect::new(320.0, 110.0, 2.0, 22.0)), pane);
        assert!(below.contains("top:134px; bottom:auto;"), "{below}");

        // Near the right edge of the pane: clamps left inside the pane so it stays readable.
        let right_clamped = hint_style(Some(CodeRect::new(960.0, 260.0, 2.0, 22.0)), pane);
        assert!(right_clamped.contains("left:756px;") && right_clamped.contains("max-width:240px;"), "{right_clamped}");

        // Caret scrolled outside the pane: falls back to the bottom of the editor area.
        assert_eq!(hint_style(Some(CodeRect::new(320.0, 60.0, 2.0, 22.0)), pane), hint_box::STYLE);

        // Caret reported before pane_box() is measured: still anchors above the caret in the window.
        let no_pane = hint_style(Some(CodeRect::new(320.0, 260.0, 2.0, 22.0)), None);
        assert!(no_pane.contains("position:fixed;") && no_pane.contains("left:320px;") && no_pane.contains("bottom:calc(100vh - 258px);"), "{no_pane}");
    }
}
