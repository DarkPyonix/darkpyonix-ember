//! Editor target and tab state.

use dioxus_compose::prelude::*;
use crate::ui::keymap::{Action, Chord};


/// What the editor was opened for.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct EditorTarget {
    pub project: String,
    /// The project's folder on its computer, when the server said (`IdeLaunch.folder`).
    pub folder: String,
    /// The demo's direct `host:port` for the OSE, dialed only without [`Self::relay`].
    pub endpoint: String,
    /// The editor server's base path at the DarkPyonix manager (`ose.path`, `/ide/ose/`, FR-W4c
    /// (1), INTENT D55). Empty means the root (the demo's direct OSE).
    pub ose_path: String,
    /// The manager's ticket (`ose.ticket`), sent as `Cookie: dpx-editor=<ticket>`. A credential:
    /// it travels in the editor process's environment and never reaches a log.
    pub ticket: String,
    /// The ember server's base URL, for the node terminal relay. Empty when unknown.
    pub server_url: String,
    /// The id of the computer the project lives on. Empty when unknown.
    pub computer: String,
    /// The server's relay route to the OSE (`/api/computers/{id}/ide/relay/{launch}`, FR-W4),
    /// dialed instead of `endpoint` when set (FR-N9.1). Empty when unknown.
    pub relay: String,
    /// An old server's VS Code target URL (a target with `url` and no relay launch), which the
    /// window offers when the server is a Code-OSS commit the editor does not support (SPEC
    /// FR-E10, #32). Empty when the target has a relay launch or the server offered none.
    pub vscode_url: String,
    /// The VS Code target's relay path (`launch.ose.relay`, FR-W4c, INTENT D55), which the
    /// fallback opens in the `--vscode-window` helper on [`Self::server_url`]
    /// (`crate::ide::VsCodeFallback`). Empty when the server sent no relay launch.
    pub vscode_relay: String,
    /// The manager's path of the VS Code target's relay launch (`launch.ose.path`).
    pub vscode_path: String,
    /// The manager's ticket of the VS Code target's relay launch (`launch.ose.ticket`). A credential.
    pub vscode_ticket: String,
    /// The folder the VS Code target's relay launch opens (`launch.folder`).
    pub vscode_folder: String,
    /// The session whose VS Code launch the fallback asks for (`POST /api/sessions/{id}/ide/vscode`,
    /// FR-W4c (1c)) when VS Code was offered with no launch. Empty otherwise.
    pub vscode_session: String,
}

impl std::fmt::Debug for EditorTarget {
    // The tickets authorize the editor connection and the VS Code window, so they never reach logs.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EditorTarget")
            .field("project", &self.project)
            .field("folder", &self.folder)
            .field("endpoint", &self.endpoint)
            .field("ose_path", &self.ose_path)
            .field("ticket", &"[REDACTED]")
            .field("server_url", &self.server_url)
            .field("computer", &self.computer)
            .field("relay", &self.relay)
            .field("vscode_url", &self.vscode_url)
            .field("vscode_relay", &self.vscode_relay)
            .field("vscode_path", &self.vscode_path)
            .field("vscode_ticket", &"[REDACTED]")
            .field("vscode_folder", &self.vscode_folder)
            .field("vscode_session", &self.vscode_session)
            .finish()
    }
}

/// The one sentence the window shows instead of the editor when the server is not a supported
/// Code-OSS commit (SPEC FR-E10, Q-E2: a sentence and a button, never an automatic switch).
/// `has_vscode` says whether a VS Code target exists for the button.
pub fn fallback_sentence(fallback: &ember_editor::Fallback, has_vscode: bool) -> String {
    let ember_editor::Fallback::OpenInVsCode { server_commit } = fallback;
    let what = match server_commit {
        Some(c) => format!(
            "이 컴퓨터의 편집기 서버(Code-OSS {})는 Ember 에디터가 지원하지 않는 버전입니다.",
            c.chars().take(12).collect::<String>()
        ),
        None => "이 컴퓨터의 편집기 서버 버전을 알 수 없어 Ember 에디터로 열 수 없습니다.".to_string(),
    };
    if has_vscode {
        format!("{what} VS Code에서 열 수 있습니다.")
    } else {
        format!("{what} 이 프로젝트에는 열 수 있는 VS Code 대상도 없습니다.")
    }
}

/// What the hover card shows (SPEC FR-E3, #32 phase A step 7): the diagnostics at the position
/// first, as Code-OSS's hover does, then each provider's Markdown, drawn from [`markdown_blocks`].
/// `None` when there is nothing to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HoverCard {
    pub diagnostics: Vec<String>,
    pub contents: Vec<String>,
}

/// The card for `hover`, or `None` when it has no diagnostic and no non-empty content. Each
/// content block is the provider's Markdown source; [`markdown_blocks`] turns it into what is drawn.
pub fn hover_card(hover: &ember_editor::hover::HoverPopup) -> Option<HoverCard> {
    let diagnostics: Vec<String> = hover.diagnostics.iter().map(|d| d.message.clone()).filter(|m| !m.trim().is_empty()).collect();
    let contents: Vec<String> = hover.contents.iter().map(|m| m.value.trim().to_string()).filter(|v| !v.is_empty()).collect();
    (!diagnostics.is_empty() || !contents.is_empty()).then_some(HoverCard { diagnostics, contents })
}

/// Which position the card is anchored to: the start of the hovered range when a provider gave
/// one (the word), else where the pointer rests; with the document generation.
pub fn hover_key(generation: u64, hover: &ember_editor::hover::HoverPopup) -> (u64, u32, u32) {
    let at = hover.range.map(|r| r.start).unwrap_or(hover.pos);
    (generation, at.line, at.col)
}

/// The tallest the hover card grows, in dp; a longer card scrolls.
pub const HOVER_CARD_MAX_HEIGHT: f32 = 320.0;

/// Where the card goes: under the hovered line when the card fits below it in the window, else on
/// the side with more room, as Code-OSS's hover does. `rect` is the position's rectangle in the
/// window's dp and `window_height` the window's height in dp. A height of 0 (the renderer has not
/// reported a size yet) places the card below.
pub fn hover_card_style(rect: compose_rust::code::CodeRect, window_height: f32) -> String {
    let frame = format!(
        "position:fixed; z-index:41; left:{:.0}px; max-width:480px; max-height:{HOVER_CARD_MAX_HEIGHT:.0}px; overflow:auto; padding:var(--sp1);",
        rect.x.max(0.0)
    );
    let below_top = rect.y + rect.height + 2.0;
    let above_bottom = rect.y - 2.0;
    let space_below = window_height - below_top;
    let above = window_height > 0.0 && space_below < HOVER_CARD_MAX_HEIGHT && above_bottom > space_below;
    if above {
        format!("{frame} top:auto; bottom:calc(100vh - {above_bottom:.0}px);")
    } else {
        format!("{frame} top:{below_top:.0}px;")
    }
}

/// One run of inline Markdown in a hover card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MdSpan {
    Text(String),
    Strong(String),
    Emphasis(String),
    Code(String),
    /// A link's text and target. The card shows the text; the target is its tooltip.
    Link { text: String, href: String },
}

/// One block of Markdown in a hover card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MdBlock {
    Heading { level: u8, spans: Vec<MdSpan> },
    Paragraph(Vec<MdSpan>),
    List { ordered: bool, items: Vec<Vec<MdSpan>> },
    /// A fenced or indented code block: its info string (the language, may be empty) and text.
    Code { lang: String, text: String },
    Rule,
}

/// The blocks of `source`, the CommonMark subset language servers send in hovers: ATX headings,
/// paragraphs, bullet and numbered lists, fenced code, thematic breaks, and inline code, strong,
/// emphasis and links. Raw HTML is shown as text, never interpreted.
pub fn markdown_blocks(source: &str) -> Vec<MdBlock> {
    let mut blocks = Vec::new();
    let mut para: Vec<String> = Vec::new();
    let mut list: Option<(bool, Vec<String>)> = None;
    let mut lines = source.lines();

    fn flush(blocks: &mut Vec<MdBlock>, para: &mut Vec<String>, list: &mut Option<(bool, Vec<String>)>) {
        if !para.is_empty() {
            blocks.push(MdBlock::Paragraph(markdown_spans(&para.join(" "))));
            para.clear();
        }
        if let Some((ordered, items)) = list.take() {
            blocks.push(MdBlock::List { ordered, items: items.iter().map(|i| markdown_spans(i)).collect() });
        }
    }

    while let Some(line) = lines.next() {
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();
        if let Some(fence) = fence_marker(trimmed) {
            flush(&mut blocks, &mut para, &mut list);
            let lang = trimmed[fence.len()..].trim().to_string();
            let mut body: Vec<&str> = Vec::new();
            for next in lines.by_ref() {
                let t = next.trim_start();
                if t.starts_with(fence) && t[fence.len()..].trim().is_empty() {
                    break;
                }
                body.push(next);
            }
            blocks.push(MdBlock::Code { lang, text: body.join("\n") });
            continue;
        }
        if trimmed.is_empty() {
            flush(&mut blocks, &mut para, &mut list);
            continue;
        }
        if indent < 4 {
            if let Some((level, text)) = heading(trimmed) {
                flush(&mut blocks, &mut para, &mut list);
                blocks.push(MdBlock::Heading { level, spans: markdown_spans(text) });
                continue;
            }
            if is_rule(trimmed) {
                flush(&mut blocks, &mut para, &mut list);
                blocks.push(MdBlock::Rule);
                continue;
            }
            if let Some((ordered, text)) = list_item(trimmed) {
                if !para.is_empty() || list.as_ref().is_some_and(|(o, _)| *o != ordered) {
                    flush(&mut blocks, &mut para, &mut list);
                }
                list.get_or_insert_with(|| (ordered, Vec::new())).1.push(text.to_string());
                continue;
            }
        }
        if let Some((_, items)) = list.as_mut() {
            // A lazy continuation line belongs to the last item.
            if let Some(last) = items.last_mut() {
                last.push(' ');
                last.push_str(trimmed);
                continue;
            }
        }
        if indent >= 4 && para.is_empty() {
            let mut body = vec![&line[4..]];
            // Indented code runs until a line that is not indented (blank lines inside are kept).
            let mut rest: Vec<&str> = Vec::new();
            for next in lines.by_ref() {
                if next.trim().is_empty() {
                    rest.push("");
                    continue;
                }
                if next.len() - next.trim_start().len() >= 4 {
                    body.append(&mut rest);
                    body.push(&next[4..]);
                } else {
                    blocks.push(MdBlock::Code { lang: String::new(), text: body.join("\n") });
                    body.clear();
                    para.push(next.trim().to_string());
                    break;
                }
            }
            if !body.is_empty() {
                blocks.push(MdBlock::Code { lang: String::new(), text: body.join("\n") });
            }
            continue;
        }
        para.push(trimmed.trim_end().to_string());
    }
    flush(&mut blocks, &mut para, &mut list);
    blocks
}

/// The fence a line opens with (three or more backticks or tildes), if any.
fn fence_marker(line: &str) -> Option<&str> {
    for c in ['`', '~'] {
        let n = line.chars().take_while(|&x| x == c).count();
        if n >= 3 {
            return Some(&line[..n]);
        }
    }
    None
}

fn heading(line: &str) -> Option<(u8, &str)> {
    let n = line.chars().take_while(|&c| c == '#').count();
    if !(1..=6).contains(&n) {
        return None;
    }
    let rest = &line[n..];
    if !rest.is_empty() && !rest.starts_with(' ') && !rest.starts_with('\t') {
        return None;
    }
    let text = rest.trim().trim_end_matches('#').trim_end();
    Some((n as u8, text))
}

fn is_rule(line: &str) -> bool {
    let compact: String = line.chars().filter(|c| !c.is_whitespace()).collect();
    compact.len() >= 3 && ['-', '*', '_'].iter().any(|&m| compact.chars().all(|c| c == m))
}

fn list_item(line: &str) -> Option<(bool, &str)> {
    for marker in ["- ", "* ", "+ "] {
        if let Some(rest) = line.strip_prefix(marker) {
            return Some((false, rest.trim()));
        }
    }
    let digits = line.chars().take_while(|c| c.is_ascii_digit()).count();
    if (1..=9).contains(&digits) {
        let rest = &line[digits..];
        if let Some(text) = rest.strip_prefix(". ").or_else(|| rest.strip_prefix(") ")) {
            return Some((true, text.trim()));
        }
    }
    None
}

/// The inline runs of `text`: code spans, `**strong**` / `__strong__`, `*emphasis*` / `_emphasis_`,
/// `[text](href)` links and backslash escapes. Markers that do not close are kept as text.
pub fn markdown_spans(text: &str) -> Vec<MdSpan> {
    let chars: Vec<char> = text.chars().collect();
    let mut spans: Vec<MdSpan> = Vec::new();
    let mut plain = String::new();
    let mut i = 0;
    let push_plain = |spans: &mut Vec<MdSpan>, plain: &mut String| {
        if !plain.is_empty() {
            spans.push(MdSpan::Text(std::mem::take(plain)));
        }
    };
    let find = |from: usize, pat: &[char]| -> Option<usize> {
        (from..=chars.len().saturating_sub(pat.len())).find(|&j| chars[j..j + pat.len()] == *pat)
    };
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' && i + 1 < chars.len() && chars[i + 1].is_ascii_punctuation() {
            plain.push(chars[i + 1]);
            i += 2;
            continue;
        }
        if c == '`' {
            let run = chars[i..].iter().take_while(|&&x| x == '`').count();
            let fence: Vec<char> = vec!['`'; run];
            if let Some(end) = find(i + run, &fence) {
                push_plain(&mut spans, &mut plain);
                let code: String = chars[i + run..end].iter().collect();
                let code = if code.len() > 1 && code.starts_with(' ') && code.ends_with(' ') { code[1..code.len() - 1].to_string() } else { code };
                spans.push(MdSpan::Code(code));
                i = end + run;
                continue;
            }
        }
        if (c == '*' || c == '_') && i + 1 < chars.len() && chars[i + 1] == c {
            if let Some(end) = find(i + 2, &[c, c]).filter(|&e| e > i + 2) {
                push_plain(&mut spans, &mut plain);
                spans.push(MdSpan::Strong(spans_text(&markdown_spans(&chars[i + 2..end].iter().collect::<String>()))));
                i = end + 2;
                continue;
            }
        }
        // `_` only opens emphasis at a word boundary, so snake_case names stay as they are.
        let boundary = c == '*' || i == 0 || !chars[i - 1].is_alphanumeric();
        if (c == '*' || c == '_') && boundary && i + 1 < chars.len() && !chars[i + 1].is_whitespace() {
            let close = (i + 2..chars.len()).find(|&j| {
                chars[j] == c && !chars[j - 1].is_whitespace() && (c == '*' || j + 1 == chars.len() || !chars[j + 1].is_alphanumeric())
            });
            if let Some(end) = close {
                push_plain(&mut spans, &mut plain);
                spans.push(MdSpan::Emphasis(spans_text(&markdown_spans(&chars[i + 1..end].iter().collect::<String>()))));
                i = end + 1;
                continue;
            }
        }
        if c == '[' {
            if let Some(mid) = find(i + 1, &[']', '(']) {
                if let Some(close) = (mid + 2..chars.len()).find(|&j| chars[j] == ')') {
                    push_plain(&mut spans, &mut plain);
                    let label: String = chars[i + 1..mid].iter().collect();
                    let href: String = chars[mid + 2..close].iter().collect();
                    let href = href.split_whitespace().next().unwrap_or("").to_string();
                    spans.push(MdSpan::Link { text: spans_text(&markdown_spans(&label)), href });
                    i = close + 1;
                    continue;
                }
            }
        }
        plain.push(c);
        i += 1;
    }
    push_plain(&mut spans, &mut plain);
    spans
}

/// The text of `spans` without their markup.
pub fn spans_text(spans: &[MdSpan]) -> String {
    spans
        .iter()
        .map(|s| match s {
            MdSpan::Text(t) | MdSpan::Strong(t) | MdSpan::Emphasis(t) | MdSpan::Code(t) => t.as_str(),
            MdSpan::Link { text, .. } => text.as_str(),
        })
        .collect()
}

/// The label of the fallback button (SPEC FR-E10).
pub const FALLBACK_BUTTON: &str = "VS Code에서 열기";

/// The status bar's cursor position, "N행 M열".
pub fn position_text(line: usize, column: usize) -> String {
    format!("{line}행 {column}열")
}

/// The last path segment.
pub fn base_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// A Korean reason for a failed open or save. The server's own text is English, can carry paths and
/// control characters, and is not for the person: the caller logs it and shows this.
pub fn file_failure_reason(error: &str) -> &'static str {
    let e = error.to_lowercase();
    if e.contains("changed on disk") {
        "파일이 디스크에서 바뀌어 저장하지 않았습니다. 비교한 뒤 다시 저장하면 덮어씁니다"
    } else if e.contains("entrynotfound") || e.contains("enoent") || e.contains("not found") || e.contains("no such file") {
        "파일이 없습니다. 지워졌거나 이름이 바뀌었을 수 있습니다"
    } else if e.contains("nopermissions") || e.contains("eacces") || e.contains("eperm") || e.contains("permission") {
        "파일을 읽거나 쓸 권한이 없습니다"
    } else if e.contains("utf-8") || e.contains("utf8") {
        "UTF-8 텍스트 파일이 아니어서 열 수 없습니다"
    } else if e.contains("connection closed") || e.contains("timed out") || e.contains("io:") || e.contains("handshake") || e.contains("refused") {
        "서버와의 연결이 끊어졌습니다. 연결을 확인한 뒤 다시 시도하세요"
    } else if e.contains("filetoolarge") || e.contains("too large") {
        "파일이 너무 커서 열 수 없습니다"
    } else {
        "알 수 없는 오류입니다. 자세한 내용은 로그에 있습니다"
    }
}

/// What the window says about a [`ember_editor::FileNoticeKind`] the session reported, in Korean and
/// naming the file when the notice has one. The server's own words are logged, never shown.
pub fn file_notice_text(kind: ember_editor::FileNoticeKind, name: Option<&str>) -> String {
    use ember_editor::FileNoticeKind::{EditRejected, ReloadFailed, WatchRefused, WatcherError};
    let file = name.map(display_name);
    match (kind, file) {
        (ReloadFailed, file) => format!(
            "{}이(가) 디스크에서 바뀐 것을 다시 읽지 못했습니다. 화면의 내용이 파일과 다를 수 있습니다",
            file.as_deref().unwrap_or("파일")
        ),
        (WatchRefused, Some(file)) => format!("서버가 {file}의 변경을 감시하지 않습니다. 다른 곳에서 바뀌어도 알려 드리지 못합니다"),
        (WatchRefused, None) => "서버가 파일의 변경을 감시하지 않습니다. 다른 곳에서 바뀌어도 알려 드리지 못합니다".to_string(),
        (WatcherError, Some(file)) => format!("서버의 파일 감시에 오류가 있습니다 ({file}). 파일이 바뀌어도 알려 드리지 못할 수 있습니다"),
        (WatcherError, None) => "서버의 파일 감시에 오류가 있습니다. 파일이 바뀌어도 알려 드리지 못할 수 있습니다".to_string(),
        (EditRejected, file) => format!(
            "{}에 적용하려던 변경이 입력한 내용과 겹쳐 적용하지 못했습니다",
            file.as_deref().unwrap_or("파일")
        ),
    }
}

/// A file name as shown to the person: control characters (newlines, escapes) are dropped, so a name
/// cannot break the layout or smuggle terminal sequences, and a very long one is shortened.
pub fn display_name(name: &str) -> String {
    let clean: String = name.chars().filter(|c| !c.is_control()).collect();
    if clean.chars().count() > 80 {
        let head: String = clean.chars().take(79).collect();
        format!("{head}…")
    } else {
        clean
    }
}

/// Whether a click on a tree row ends the pending name prompt. An empty field has nothing to lose; a
/// typed name is kept (the person may have clicked by mistake), and a delete question is not a field.
pub fn row_click_closes_prompt(prompt: &TreePrompt, typed: &str) -> bool {
    match prompt {
        TreePrompt::NewFile { .. } | TreePrompt::NewDir { .. } | TreePrompt::Rename { .. } => typed.trim().is_empty(),
        TreePrompt::Delete { .. } | TreePrompt::DeleteMany { .. } => false,
    }
}

/// The row whose label the name box replaces: a rename edits the renamed row in place, as Code-OSS's
/// explorer does (#797). New file and new folder prompts, and a rename of a row that is not shown, get
/// no row and keep the box at the top of the tree.
pub fn rename_in_place_row(prompt: Option<&TreePrompt>, rows: &[TreeRow]) -> Option<usize> {
    match prompt {
        Some(TreePrompt::Rename { path }) => rows.iter().position(|r| &r.path == path),
        _ => None,
    }
}

/// Whether leaving the new-name field closes its prompt: an empty field has nothing to keep, a typed
/// name stays so the person can come back to it.
pub fn blur_closes_prompt(typed: &str) -> bool {
    typed.trim().is_empty()
}

/// The widget's colour role for a syntax role: the design system resolves it, light and dark.
pub fn colour_role(role: ember_editor::textmate::SyntaxRole) -> compose_rust::ColorRole {
    use compose_rust::ColorRole as C;
    use ember_editor::textmate::SyntaxRole as R;
    match role {
        R::Keyword => C::SyntaxKeyword,
        R::String => C::SyntaxString,
        R::Comment => C::SyntaxComment,
        R::Number => C::SyntaxNumber,
        R::Constant => C::SyntaxConstant,
        R::Type => C::SyntaxType,
        R::Function => C::SyntaxFunction,
        R::Variable => C::SyntaxVariable,
        R::Property => C::SyntaxProperty,
        R::Operator => C::SyntaxOperator,
        R::Punctuation => C::SyntaxPunctuation,
        R::Tag => C::SyntaxTag,
        R::Attribute => C::SyntaxAttribute,
        R::Escape => C::SyntaxEscape,
        R::Macro => C::SyntaxMacro,
    }
}

/// The `syntax_spans` prop for the coloured runs the session reported.
pub fn syntax_spans(syntax: &ember_editor::SyntaxRuns) -> compose_rust::code::SyntaxSpans {
    syntax_spans_at(syntax, syntax.version)
}

/// [`syntax_spans`] with the runs written against `version`, the widget's numbering of the session's
/// `syntax.version` (`DocumentWidgetState::widget_version_of`).
pub fn syntax_spans_at(syntax: &ember_editor::SyntaxRuns, version: u32) -> compose_rust::code::SyntaxSpans {
    use compose_rust::code::{CodeRange, SyntaxSpan, SyntaxSpans};
    SyntaxSpans::new(
        syntax.runs.iter().map(|r| SyntaxSpan::role(version, CodeRange::of(r.line, r.start, r.line, r.end), colour_role(r.role))),
    )
}

/// The `folds` prop for the foldable regions the session reported (FR-E6, #441).
pub fn folds(regions: &[ember_editor::FoldRegion]) -> compose_rust::code::Folds {
    folds_from(regions, 0)
}

/// [`folds`] with each version in the widget's numbering: the session's version minus the document's
/// `widget_base`.
pub fn folds_from(regions: &[ember_editor::FoldRegion], widget_base: u32) -> compose_rust::code::Folds {
    use compose_rust::code::{Fold, Folds};
    Folds::new(regions.iter().map(|f| Fold {
        version: f.version.saturating_sub(widget_base),
        start_line: f.start_line,
        end_line: f.end_line,
        collapsed: f.collapsed,
    }))
}

/// One open file in the tab strip.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Tab {
    path: String,
    /// Unsaved edits.
    dirty: bool,
    /// Pinned (Code-OSS "sticky"): kept at the front of the strip and skipped by close others, close to the
    /// right and close all.
    pinned: bool,
    /// A preview tab (Code-OSS "preview editor"): drawn upright like any tab (FR-L31.30), replaced by the next preview open, made
    /// permanent by an edit, a double-click, pinning or an explicit open.
    preview: bool,
}

/// The open files (FR-L31.5). A tab holds its unsaved, pinned and preview marks, so closing a tab drops
/// them. Pinned tabs always come first, as in Code-OSS, and a reorder never crosses that boundary.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EditorTabs {
    tabs: Vec<Tab>,
    current: Option<usize>,
    /// Tabs whose close waits on their save (저장 in the unsaved-changes question): each closes when its
    /// save succeeds and stays open and dirty when it fails, as in Code-OSS. Each holds the document
    /// version the save was asked for, so an edit typed while the save was in flight keeps the tab.
    closing: Vec<(String, Option<u32>)>,
    /// The tabs closed, for 닫은 편집기 다시 열기 (FR-L31.15).
    closed: super::editor_actions::ClosedEditors,
}

/// What closing one or more tabs needs (FR-L31.5): the tabs to close, the ones among them with unsaved
/// edits, and the tab to make active afterwards. With no dirty tab it closes at once; with one, the window
/// asks first (저장 / 저장 안 함 / 취소), as Code-OSS does, and Cancel closes none of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloseRequest {
    pub paths: Vec<String>,
    pub dirty: Vec<String>,
    pub anchor: Option<String>,
}

/// How a save a closing tab waited on ended: whether it succeeded, and the document's version and unsaved
/// mark once the session answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SaveOutcome {
    pub ok: bool,
    pub version: Option<u32>,
    pub dirty: bool,
}

/// The answer to the unsaved-changes question.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirtyChoice {
    Save,
    DontSave,
    Cancel,
}

/// What to do after the answer: save these, then close these.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CloseResolution {
    pub save: Vec<String>,
    pub close: Vec<String>,
}

impl CloseRequest {
    /// Whether the window has to ask before closing (a tab to close has unsaved edits).
    pub fn needs_confirm(&self) -> bool {
        !self.dirty.is_empty()
    }

    /// Whether there is anything to close.
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    /// The question, as Code-OSS words it: one file by name, several by count.
    pub fn confirm_title(&self) -> String {
        match self.dirty.as_slice() {
            [one] => format!("{}의 변경 내용을 저장하시겠습니까?", display_name(base_name(one))),
            many => format!("다음 {}개 파일의 변경 내용을 저장하시겠습니까?", many.len()),
        }
    }

    /// The line under the question.
    pub fn confirm_detail(&self) -> &'static str {
        "저장하지 않으면 변경 내용이 손실됩니다."
    }

    /// The save button: 저장 for one file, 모두 저장 for several (Code-OSS's Save and Save All).
    pub fn save_label(&self) -> &'static str {
        if self.dirty.len() > 1 { "모두 저장" } else { "저장" }
    }

    /// The file names listed under a question about several files.
    pub fn dirty_names(&self) -> Vec<String> {
        if self.dirty.len() > 1 { self.dirty.iter().map(|p| display_name(base_name(p))).collect() } else { Vec::new() }
    }

    /// What the answer does: Save saves the dirty tabs and closes every tab asked for, 저장 안 함 closes
    /// them all without saving, 취소 closes none.
    pub fn resolve(&self, choice: DirtyChoice) -> CloseResolution {
        match choice {
            DirtyChoice::Save => CloseResolution { save: self.dirty.clone(), close: self.paths.clone() },
            DirtyChoice::DontSave => CloseResolution { save: Vec::new(), close: self.paths.clone() },
            DirtyChoice::Cancel => CloseResolution::default(),
        }
    }
}

impl EditorTabs {
    fn index(&self, path: &str) -> Option<usize> {
        self.tabs.iter().position(|t| t.path == path)
    }

    fn pinned_count(&self) -> usize {
        self.tabs.iter().take_while(|t| t.pinned).count()
    }

    /// Run `f` on the tab list and keep the same tab current, wherever it moved.
    fn keeping_current(&mut self, f: impl FnOnce(&mut Vec<Tab>)) {
        let current = self.current_path().map(str::to_owned);
        f(&mut self.tabs);
        self.current = current.and_then(|p| self.index(&p));
    }

    /// Open a file as a permanent tab, or activate it when it is already open (a preview tab opened this
    /// way becomes permanent, as Code-OSS's pinned open does).
    pub fn open(&mut self, path: &str) {
        let i = match self.index(path) {
            Some(i) => {
                self.tabs[i].preview = false;
                i
            }
            None => {
                self.tabs.push(Tab { path: path.to_string(), dirty: false, pinned: false, preview: false });
                self.tabs.len() - 1
            }
        };
        self.current = Some(i);
    }

    /// Open a file as the preview tab (a single click in the explorer or the search results). A file that
    /// is already open is only activated. Otherwise the current preview tab, if there is one, is replaced
    /// in its place; its path is returned so the caller closes its document.
    pub fn open_preview(&mut self, path: &str) -> Option<String> {
        if let Some(i) = self.index(path) {
            self.current = Some(i);
            return None;
        }
        let tab = Tab { path: path.to_string(), dirty: false, pinned: false, preview: true };
        if let Some(i) = self.tabs.iter().position(|t| t.preview && !t.dirty && !t.pinned) {
            let old = std::mem::replace(&mut self.tabs[i], tab);
            self.current = Some(i);
            return Some(old.path);
        }
        self.tabs.push(tab);
        self.current = Some(self.tabs.len() - 1);
        None
    }

    /// Make a preview tab permanent (a double-click on it or on its file, or 열린 상태 유지).
    pub fn keep_open(&mut self, path: &str) {
        if let Some(i) = self.index(path) {
            self.tabs[i].preview = false;
        }
    }

    pub fn is_preview(&self, path: &str) -> bool {
        self.index(path).is_some_and(|i| self.tabs[i].preview)
    }

    pub fn is_pinned(&self, path: &str) -> bool {
        self.index(path).is_some_and(|i| self.tabs[i].pinned)
    }

    /// Pin a tab: it becomes permanent and moves to the end of the pinned tabs at the front.
    pub fn pin(&mut self, path: &str) {
        let Some(i) = self.index(path) else { return };
        if self.tabs[i].pinned {
            return;
        }
        let to = self.pinned_count();
        self.keeping_current(|tabs| {
            let mut tab = tabs.remove(i);
            tab.pinned = true;
            tab.preview = false;
            tabs.insert(to, tab);
        });
    }

    /// Unpin a tab: it moves to the first place after the tabs that stay pinned.
    pub fn unpin(&mut self, path: &str) {
        let Some(i) = self.index(path) else { return };
        if !self.tabs[i].pinned {
            return;
        }
        let to = self.pinned_count() - 1;
        self.keeping_current(|tabs| {
            let mut tab = tabs.remove(i);
            tab.pinned = false;
            tabs.insert(to, tab);
        });
    }

    pub fn toggle_pin(&mut self, path: &str) {
        if self.is_pinned(path) { self.unpin(path) } else { self.pin(path) }
    }

    /// Move a tab to `to`, kept inside its own group (pinned tabs among the pinned, others after them).
    /// Returns whether it moved.
    pub fn move_to(&mut self, path: &str, to: usize) -> bool {
        let Some(i) = self.index(path) else { return false };
        let pinned = self.pinned_count();
        let (lo, hi) = if self.tabs[i].pinned { (0, pinned - 1) } else { (pinned, self.tabs.len() - 1) };
        let to = to.clamp(lo, hi);
        if to == i {
            return false;
        }
        self.keeping_current(|tabs| {
            let tab = tabs.remove(i);
            tabs.insert(to, tab);
        });
        true
    }

    /// Move a tab one place left (Code-OSS's Move Editor Left), not across the pinned boundary.
    pub fn move_left(&mut self, path: &str) -> bool {
        match self.index(path) {
            Some(i) if i > 0 => self.move_to(path, i - 1),
            _ => false,
        }
    }

    /// Move a tab one place right (Code-OSS's Move Editor Right), not across the pinned boundary.
    pub fn move_right(&mut self, path: &str) -> bool {
        match self.index(path) {
            Some(i) => self.move_to(path, i + 1),
            None => false,
        }
    }

    pub fn activate(&mut self, path: &str) {
        if let Some(i) = self.index(path) {
            self.current = Some(i);
        }
    }

    /// Close a tab. Closing the current one activates the tab that takes its place, else the
    /// one before it; closing another one keeps the current tab.
    pub fn close(&mut self, path: &str) {
        let Some(i) = self.index(path) else { return };
        self.closed.push_at(path, i);
        self.tabs.remove(i);
        self.closing.retain(|(p, _)| p != path);
        self.current = match self.current {
            _ if self.tabs.is_empty() => None,
            Some(c) if c == i => Some(i.min(self.tabs.len() - 1)),
            Some(c) if c > i => Some(c - 1),
            other => other,
        };
    }

    /// Act on the answer to the unsaved-changes question. Returns the tabs to close now: every tab asked
    /// for after 저장 안 함; after 저장 the clean ones only, while each dirty one waits for its save
    /// ([`EditorTabs::save_finished`]); none after 취소.
    /// `version_of` gives each document's version when its save is asked for.
    pub fn begin_close(&mut self, request: &CloseRequest, choice: DirtyChoice, version_of: impl Fn(&str) -> Option<u32>) -> Vec<String> {
        let resolution = request.resolve(choice);
        for p in &resolution.save {
            if self.index(p).is_some() && !self.is_closing(p) {
                self.closing.push((p.clone(), version_of(p)));
            }
        }
        resolution.close.into_iter().filter(|p| !resolution.save.contains(p)).collect()
    }

    /// Whether the tab waits to close on its save.
    pub fn is_closing(&self, path: &str) -> bool {
        self.closing.iter().any(|(p, _)| p == path)
    }

    /// A save finished. Returns whether the tab is to close now: it waited on this save, the save
    /// succeeded, and the document is still at the version that was saved and not dirty again. On a
    /// failure, or after an edit typed while the save was in flight, it stays open and dirty and the
    /// pending close is dropped (Code-OSS keeps an editor open that is dirty after its save).
    pub fn save_finished(&mut self, path: &str, outcome: SaveOutcome) -> bool {
        let Some(i) = self.closing.iter().position(|(p, _)| p == path) else { return false };
        let (_, saved_version) = self.closing.remove(i);
        let edited = outcome.dirty || outcome.version != saved_version;
        if !outcome.ok || edited {
            self.set_dirty(path, true);
            return false;
        }
        self.index(path).is_some()
    }

    /// Close several tabs, then activate `anchor` when it is still open.
    pub fn close_many(&mut self, paths: &[String], anchor: Option<&str>) {
        for p in paths {
            self.close(p);
        }
        if let Some(a) = anchor {
            self.activate(a);
        }
    }

    fn request(&self, paths: Vec<String>, anchor: Option<&str>) -> CloseRequest {
        let dirty = paths.iter().filter(|p| self.is_dirty(p)).cloned().collect();
        CloseRequest { paths, dirty, anchor: anchor.map(str::to_owned) }
    }

    /// Closing one tab (its close button, 닫기, Cmd+W): a request to confirm when it is dirty.
    pub fn close_request(&self, path: &str) -> CloseRequest {
        let paths = self.index(path).map(|_| vec![path.to_string()]).unwrap_or_default();
        self.request(paths, None)
    }

    /// 다른 항목 닫기: every tab but `path`, skipping pinned tabs as Code-OSS does; `path` becomes active.
    pub fn close_others_request(&self, path: &str) -> CloseRequest {
        let paths = self.tabs.iter().filter(|t| t.path != path && !t.pinned).map(|t| t.path.clone()).collect();
        self.request(paths, Some(path))
    }

    /// 오른쪽에 있는 항목 닫기: the tabs right of `path`, skipping pinned ones; `path` becomes active.
    pub fn close_right_request(&self, path: &str) -> CloseRequest {
        let Some(i) = self.index(path) else { return self.request(Vec::new(), None) };
        let paths = self.tabs[i + 1..].iter().filter(|t| !t.pinned).map(|t| t.path.clone()).collect();
        self.request(paths, Some(path))
    }

    /// 모두 닫기: every tab that is not pinned (Code-OSS keeps pinned editors on Close All).
    pub fn close_all_request(&self) -> CloseRequest {
        let paths = self.tabs.iter().filter(|t| !t.pinned).map(|t| t.path.clone()).collect();
        self.request(paths, None)
    }

    /// A path was renamed: every tab at or below `old` takes the matching path below `new`, keeping its
    /// place and its marks. Returns the (old, new) pairs of the tabs that moved.
    pub fn rename_prefix(&mut self, old: &str, new: &str) -> Vec<(String, String)> {
        let old = old.trim_end_matches('/');
        let new = new.trim_end_matches('/');
        let mut moved = Vec::new();
        for tab in &mut self.tabs {
            if is_at_or_below(&tab.path, old) {
                let renamed = format!("{new}{}", &tab.path[old.len()..]);
                moved.push((std::mem::replace(&mut tab.path, renamed.clone()), renamed));
            }
        }
        moved
    }

    /// Mark a tab's unsaved edits. An edit makes a preview tab permanent, as in Code-OSS.
    pub fn set_dirty(&mut self, path: &str, dirty: bool) {
        if let Some(i) = self.index(path) {
            self.tabs[i].dirty = dirty;
            if dirty {
                self.tabs[i].preview = false;
            }
        }
    }

    pub fn is_dirty(&self, path: &str) -> bool {
        self.index(path).is_some_and(|i| self.tabs[i].dirty)
    }

    pub fn has_unsaved(&self) -> bool {
        self.tabs.iter().any(|t| t.dirty)
    }

    pub fn paths(&self) -> Vec<&str> {
        self.tabs.iter().map(|t| t.path.as_str()).collect()
    }

    pub fn current_path(&self) -> Option<&str> {
        self.current.map(|i| self.tabs[i].path.as_str())
    }

    /// Activate the next tab in the strip, wrapping around. Returns the new current path.
    pub fn next_tab(&mut self) -> Option<String> {
        let len = self.tabs.len();
        if len <= 1 {
            return None;
        }
        let next = match self.current {
            Some(i) => (i + 1) % len,
            None => 0,
        };
        self.current = Some(next);
        Some(self.tabs[next].path.clone())
    }

    /// Activate the previous tab in the strip, wrapping around. Returns the new current path.
    pub fn prev_tab(&mut self) -> Option<String> {
        let len = self.tabs.len();
        if len <= 1 {
            return None;
        }
        let prev = match self.current {
            Some(i) => (i + len - 1) % len,
            None => len - 1,
        };
        self.current = Some(prev);
        Some(self.tabs[prev].path.clone())
    }

    /// Close every tab except the currently active one and the pinned ones, without asking, returning the
    /// paths of the tabs that closed. The window asks first through [`EditorTabs::close_others_request`].
    pub fn close_others(&mut self) -> Vec<String> {
        let Some(cur) = self.current_path().map(str::to_owned) else { return Vec::new() };
        let closed = self.close_others_request(&cur).paths;
        self.close_many(&closed, Some(&cur));
        closed
    }

    /// 닫은 편집기 다시 열기 (`workbench.action.reopenClosedEditor`): open the tab closed last
    /// that is not open again, and return its path.
    pub fn reopen_closed(&mut self) -> Option<String> {
        let open: Vec<String> = self.tabs.iter().map(|t| t.path.clone()).collect();
        let (path, index) = self.closed.pop_at(|p| open.iter().any(|o| o == p))?;
        // Code-OSS puts a reopened editor back at the index it had, or last when fewer tabs are left.
        // It comes back unpinned, so never inside the pinned group (FR-L31.5).
        let pinned = self.tabs.iter().filter(|t| t.pinned).count();
        let at = index.clamp(pinned, self.tabs.len());
        self.tabs.insert(at, Tab { path: path.clone(), dirty: false, pinned: false, preview: false });
        self.current = Some(at);
        Some(path)
    }

    /// A path was deleted: its tabs are not reopened.
    pub fn forget_closed_below(&mut self, path: &str) {
        self.closed.forget_below(path);
    }
}

/// The CSS class of a tab with its pinned and preview marks (Ember additions to the mockup's `ed-tab`:
/// `.pinned` draws the pin in place of the close button; `.preview` marks the tab and has no rule of
/// its own, because an italic label is cut on the HTML path, FR-L31.30).
pub fn tab_class_with(active: bool, modified: bool, pinned: bool, preview: bool) -> String {
    let mut class = tab_class(active, modified).to_string();
    if pinned {
        class.push_str(" pinned");
    }
    if preview {
        class.push_str(" preview");
    }
    class
}

/// The tab context menu (FR-L31.5), in Code-OSS's order with Ember's move items for the reorder that drag
/// cannot do yet (dioxus-compose sends no drag events).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabMenuItem {
    Close,
    CloseOthers,
    CloseRight,
    CloseAll,
    KeepOpen,
    Pin,
    Unpin,
    MoveLeft,
    MoveRight,
}

impl TabMenuItem {
    pub fn label(self) -> &'static str {
        match self {
            TabMenuItem::Close => "닫기",
            TabMenuItem::CloseOthers => "다른 항목 닫기",
            TabMenuItem::CloseRight => "오른쪽에 있는 항목 닫기",
            TabMenuItem::CloseAll => "모두 닫기",
            TabMenuItem::KeepOpen => "열린 상태 유지",
            TabMenuItem::Pin => "고정",
            TabMenuItem::Unpin => "고정 해제",
            TabMenuItem::MoveLeft => "왼쪽으로 이동",
            TabMenuItem::MoveRight => "오른쪽으로 이동",
        }
    }
}

/// The items of the context menu of the tab `path`, with whether each one can run (a close item with
/// nothing to close, or a move at the edge of its group, is shown disabled, as Code-OSS does).
pub fn tab_menu_items(tabs: &EditorTabs, path: &str) -> Vec<(TabMenuItem, bool)> {
    let Some(i) = tabs.index(path) else { return Vec::new() };
    let pinned = tabs.is_pinned(path);
    let pc = tabs.pinned_count();
    let (lo, hi) = if pinned { (0, pc.saturating_sub(1)) } else { (pc, tabs.tabs.len() - 1) };
    let mut items = vec![
        (TabMenuItem::Close, true),
        (TabMenuItem::CloseOthers, !tabs.close_others_request(path).is_empty()),
        (TabMenuItem::CloseRight, !tabs.close_right_request(path).is_empty()),
        (TabMenuItem::CloseAll, !tabs.close_all_request().is_empty()),
    ];
    if tabs.is_preview(path) {
        items.push((TabMenuItem::KeepOpen, true));
    }
    items.push((if pinned { TabMenuItem::Unpin } else { TabMenuItem::Pin }, true));
    items.push((TabMenuItem::MoveLeft, i > lo));
    items.push((TabMenuItem::MoveRight, i < hi));
    items
}

/// Whether a key on the focused tab strip opens the tab menu: Shift+F10 or the ContextMenu key, as in
/// Code-OSS, beside the right-click menu (`oncontextmenu`, compose-rust#417).
pub fn tab_menu_key(key: &str, ctrl: bool, shift: bool, alt: bool, meta: bool) -> bool {
    match key {
        "F10" => shift && !ctrl && !alt && !meta,
        "ContextMenu" => !ctrl && !shift && !alt && !meta,
        _ => false,
    }
}

/// The tab strip's reorder keys (Code-OSS's Move Editor Left and Right): Ctrl+Shift+PageUp and
/// Ctrl+Shift+PageDown, the same on every OS. `Some(true)` moves right, `Some(false)` left.
pub fn tab_move_key(key: &str, ctrl: bool, shift: bool, alt: bool, meta: bool) -> Option<bool> {
    if !ctrl || !shift || alt || meta {
        return None;
    }
    match key {
        "PageUp" => Some(false),
        "PageDown" => Some(true),
        _ => None,
    }
}

/// The ancestor directories (from `root` down to the parent of `file_path`) to expand in the explorer
/// tree so that `file_path` becomes visible (`열린 파일 찾기`). Returns an empty list when `file_path`
/// is not strictly inside `root`.
pub fn ancestor_dirs_to_expand(root: &str, file_path: &str) -> Vec<String> {
    let root = root.trim_end_matches('/');
    if root.is_empty() || !is_at_or_below(file_path, root) || file_path.trim_end_matches('/') == root {
        return Vec::new();
    }
    let rel = file_path[root.len()..].trim_start_matches('/');
    let parts: Vec<&str> = rel.split('/').filter(|s| !s.is_empty()).collect();
    if parts.is_empty() {
        return Vec::new();
    }
    let mut dirs = vec![root.to_string()];
    let mut cur = root.to_string();
    for dir_seg in &parts[..parts.len() - 1] {
        cur = super::editor_workbench::join_path(&cur, dir_seg);
        dirs.push(cur.clone());
    }
    dirs
}

/// The two shortcut hint pairs `(hint, label)` shown inside `.ed-empty` when no file is open:
/// `(find_hint, find_label)` and `(project_hint, "프로젝트 창")`.
pub fn empty_state_hints(
    keymap: crate::ui::keymap::Keymap,
    os: crate::ui::keymap::Os,
) -> ((String, &'static str), (String, &'static str)) {
    (
        (keymap.hint(Action::Find, os), keymap.find_label()),
        (keymap.hint(Action::Project, os), "프로젝트 창"),
    )
}

/// The CSS class of a tree row (`ed-tn`, with `.sel` when chosen and `.mod` when modified in git).
pub fn tree_row_class(selected: bool, modified: bool) -> &'static str {
    match (selected, modified) {
        (true, true) => "ed-tn sel mod",
        (true, false) => "ed-tn sel",
        (false, true) => "ed-tn mod",
        (false, false) => "ed-tn",
    }
}

/// The CSS class of a file tab (`ed-tab`, with `.on` when active and `.vc-mod` when modified in git).
pub fn tab_class(active: bool, modified: bool) -> &'static str {
    match (active, modified) {
        (true, true) => "ed-tab on vc-mod",
        (true, false) => "ed-tab on",
        (false, true) => "ed-tab vc-mod",
        (false, false) => "ed-tab",
    }
}

/// The status bar tab width label (`"4칸"` when `tab_width` is 0, else `"{tab_width}칸"`).
pub fn tab_width_text(tab_width: u32) -> String {
    let w = if tab_width == 0 { 4 } else { tab_width };
    format!("{w}칸")
}

/// The path segments the breadcrumbs show: the path below the project folder when it lies
/// inside it, split on `/` with empty segments dropped.
pub fn breadcrumb_segments(path: &str, folder: &str) -> Vec<String> {
    let folder = folder.trim_end_matches('/');
    let rest = match path.strip_prefix(folder) {
        Some(r) if !folder.is_empty() && r.starts_with('/') => r,
        _ => path,
    };
    rest.split('/')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// One row of the explorer tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeRow {
    pub path: String,
    pub name: String,
    pub depth: u32,
    pub is_dir: bool,
    pub open: bool,
}

/// The rows of the explorer: the folder's entries with directories first, then files by name,
/// and below an expanded directory its own entries once they have been listed.
pub fn tree_rows(
    root: &str,
    listings: &std::collections::HashMap<String, Vec<(String, u32)>>,
    expanded: &std::collections::HashSet<String>,
) -> Vec<TreeRow> {
    fn walk(
        dir: &str,
        depth: u32,
        listings: &std::collections::HashMap<String, Vec<(String, u32)>>,
        expanded: &std::collections::HashSet<String>,
        rows: &mut Vec<TreeRow>,
    ) {
        let Some(entries) = listings.get(dir) else { return };
        let is_dir = |kind: u32| kind & ember_editor_conn::remote_fs::file_type::DIRECTORY != 0;
        let mut sorted: Vec<&(String, u32)> = entries.iter().collect();
        sorted.sort_by_key(|(name, kind)| (!is_dir(*kind), name.to_lowercase()));
        for (name, kind) in sorted {
            let path = super::editor_workbench::join_path(dir, name);
            let dir_row = is_dir(*kind);
            let open = dir_row && expanded.contains(&path);
            rows.push(TreeRow { path: path.clone(), name: name.clone(), depth, is_dir: dir_row, open });
            if open {
                walk(&path, depth + 1, listings, expanded, rows);
            }
        }
    }
    let mut rows = Vec::new();
    walk(root, 0, listings, expanded, &mut rows);
    rows
}

/// The editor page's grid tracks as inline style. An open tool window adds nothing: the
/// stylesheet's tracks apply, including its width breakpoints. A closed left column is 0 wide and
/// a closed bottom row is 0 high (the mockup's `.no-left` and `.no-bottom`).
pub fn editor_grid_style(has_left: bool, has_bottom: bool) -> String {
    let mut style = String::new();
    if !has_left {
        style.push_str("grid-template-columns:var(--stw) 0 minmax(0,1fr) var(--stw); ");
    }
    if !has_bottom {
        style.push_str("grid-template-rows:52px minmax(0,1fr) 0 var(--sbh); ");
    }
    style
}

/// The page's size rule, the same on every system: the page is the viewport. The viewport is the
/// box the window gives its content (the whole window under FullSize on macOS, the area below the
/// native title bar elsewhere), and the HTML runtime lays the page out again on every dp change
/// of that box, so the page follows the window both when it grows and when it shrinks (#301,
/// #320). No title bar height is taken out here: the viewport already excludes it.
pub fn editor_page_size_style() -> &'static str {
    "width:100vw; height:100vh;"
}

/// The padding of the page's error message, which is drawn with no top bar above it. The
/// message starts below the title strip the window reports (`strip_top`, 0 until the first
/// report and where the window manager draws the title bar) on every system (FR-L33).
pub fn editor_error_padding(strip_top: f32) -> String {
    let top = 20.0 + strip_top.max(0.0);
    format!("padding:20px; padding-top:{top}px;")
}

/// The top bar's padding past the window controls, exactly as the window reports them on every
/// system (FR-L33): the leading set (macOS traffic lights) at the left, the trailing set (Windows
/// caption buttons, a Linux desktop's own) at the right. Nothing for a zero edge.
pub fn editor_strip_padding(leading: f32, trailing: f32) -> String {
    let mut s = String::new();
    if leading > 0.0 {
        s.push_str(&format!("padding-left:{leading}px;"));
    }
    if trailing > 0.0 {
        s.push_str(&format!("padding-right:{trailing}px;"));
    }
    s
}

/// The page grid while the bottom tool window is maximized (the mockup's `.ed.max`): the editor row
/// is 0 high and the bottom window takes its row and the one below.
pub fn editor_grid_style_maximized() -> String {
    "grid-template-rows:52px 0 minmax(0,1fr) var(--sbh);".to_string()
}

/// The label of the find pill in the top bar (`editor.js:74`), the same in both keymaps; the hover
/// names the keymap's action (`Keymap::find_label`) and its shortcut.
pub const FIND_PILL_LABEL: &str = "파일, 동작, 기호 찾기";

/// The breadcrumb line: segments joined with a right-angle quote.
pub fn breadcrumb_text(path: &str, folder: &str) -> String {
    breadcrumb_segments(path, folder).join(" \u{203A} ")
}

/// How an item was placed on the clipboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardKind {
    Copy,
    Cut,
}

/// Paths held on the explorer clipboard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeClipboard {
    pub kind: ClipboardKind,
    pub paths: Vec<(String, bool)>,
}

/// A pending explorer edit the tree is asking a name or a confirmation for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreePrompt {
    NewFile { parent: String },
    NewDir { parent: String },
    Rename { path: String },
    Delete { path: String, is_dir: bool },
    DeleteMany { items: Vec<(String, bool)> },
}

/// The icon name for a tree prompt row.
pub fn tree_prompt_icon(prompt: &TreePrompt) -> &'static str {
    match prompt {
        TreePrompt::NewDir { .. } => "folder",
        _ => "file",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptKeyOutcome {
    Submit,
    Cancel,
    Ignore,
}

/// The action a key in a tree prompt row takes.
pub fn prompt_key_action(prompt: &TreePrompt, key: &str) -> PromptKeyOutcome {
    match prompt {
        TreePrompt::NewFile { .. } | TreePrompt::NewDir { .. } | TreePrompt::Rename { .. } => {
            match key {
                "Enter" => PromptKeyOutcome::Submit,
                "Escape" => PromptKeyOutcome::Cancel,
                _ => PromptKeyOutcome::Ignore,
            }
        }
        TreePrompt::Delete { .. } | TreePrompt::DeleteMany { .. } => {
            match key {
                "Enter" | "Escape" => PromptKeyOutcome::Cancel,
                _ => PromptKeyOutcome::Ignore,
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptSubmitOutcome {
    CreateFile { parent: String, name: String },
    CreateDir { parent: String, name: String },
    Rename { path: String, new_name: String },
    Delete { path: String, is_dir: bool },
    DeleteMany { items: Vec<(String, bool)> },
    BlockedDirty { path: String, dirty_files: Vec<String> },
    EmptyName,
    Unchanged,
}

/// Drops sources that are already in `dest_dir`, are `dest_dir` itself, or would move a folder into
/// one of its own descendants.
pub fn filter_move_sources(sources: Vec<String>, dest_dir: &str) -> Vec<String> {
    let dest = dest_dir.trim_end_matches('/');
    let dest = if dest.is_empty() { "/" } else { dest };
    sources
        .into_iter()
        .filter(|src| {
            let trimmed = src.trim_end_matches('/');
            if trimmed == dest || is_at_or_below(dest, trimmed) {
                return false;
            }
            let parent = match trimmed.rfind('/') {
                Some(0) => "/",
                Some(i) => &trimmed[..i],
                None => "",
            };
            parent != dest
        })
        .collect()
}

/// Evaluates submitting a tree prompt: checks empty names and unsaved dirty buffers on rename.
pub fn evaluate_prompt_submit(
    prompt: &TreePrompt,
    typed_name: &str,
    unsaved_paths: &[String],
) -> PromptSubmitOutcome {
    let name = typed_name.trim();
    if matches!(prompt, TreePrompt::NewFile { .. } | TreePrompt::NewDir { .. } | TreePrompt::Rename { .. }) && name.is_empty() {
        return PromptSubmitOutcome::EmptyName;
    }
    match prompt {
        TreePrompt::NewFile { parent } => PromptSubmitOutcome::CreateFile {
            parent: parent.clone(),
            name: name.to_string(),
        },
        TreePrompt::NewDir { parent } => PromptSubmitOutcome::CreateDir {
            parent: parent.clone(),
            name: name.to_string(),
        },
        TreePrompt::Rename { path } => {
            if name == base_name(path) {
                return PromptSubmitOutcome::Unchanged;
            }
            let blocked: Vec<String> = unsaved_paths
                .iter()
                .filter(|u| is_at_or_below(u, path))
                .cloned()
                .collect();
            if !blocked.is_empty() {
                PromptSubmitOutcome::BlockedDirty {
                    path: path.clone(),
                    dirty_files: blocked,
                }
            } else {
                PromptSubmitOutcome::Rename {
                    path: path.clone(),
                    new_name: name.to_string(),
                }
            }
        }
        TreePrompt::Delete { path, is_dir } => PromptSubmitOutcome::Delete {
            path: path.clone(),
            is_dir: *is_dir,
        },
        TreePrompt::DeleteMany { items } => PromptSubmitOutcome::DeleteMany {
            items: items.clone(),
        },
    }
}

/// Remaps expanded directory paths after `old` was renamed to `new`, returning the updated set
/// and the list of remapped directory paths that should be listed again.
pub fn remap_expanded_paths(
    expanded: &std::collections::HashSet<String>,
    old: &str,
    new: &str,
) -> (std::collections::HashSet<String>, Vec<String>) {
    let mut updated = expanded.clone();
    let old_trimmed = old.trim_end_matches('/');
    let new_trimmed = new.trim_end_matches('/');
    let mut remapped = Vec::new();
    for p in expanded {
        if is_at_or_below(p, old) {
            updated.remove(p);
            let next = format!("{}{}", new_trimmed, &p[old_trimmed.len()..]);
            updated.insert(next.clone());
            remapped.push(next);
        }
    }
    remapped.sort();
    (updated, remapped)
}

/// Updates the explorer selection based on shift (range) or toggle (Cmd/Ctrl) modifiers.
pub fn update_tree_selection(
    current: &[(String, bool)],
    clicked: (String, bool),
    rows: &[TreeRow],
    shift: bool,
    toggle: bool,
) -> Vec<(String, bool)> {
    if shift {
        if let Some(anchor) = current.first() {
            let anchor_idx = rows.iter().position(|r| r.path == anchor.0);
            let clicked_idx = rows.iter().position(|r| r.path == clicked.0);
            if let (Some(a), Some(c)) = (anchor_idx, clicked_idx) {
                let (start, end) = if a <= c { (a, c) } else { (c, a) };
                return rows[start..=end]
                    .iter()
                    .map(|r| (r.path.clone(), r.is_dir))
                    .collect();
            }
        }
        vec![clicked]
    } else if toggle {
        if let Some(pos) = current.iter().position(|(p, _)| p == &clicked.0) {
            let mut next = current.to_vec();
            next.remove(pos);
            next
        } else {
            let mut next = current.to_vec();
            next.push(clicked);
            next
        }
    } else {
        vec![clicked]
    }
}

/// Removes any item whose ancestor directory is also in `items`.
pub fn prune_redundant_selection(items: &[(String, bool)]) -> Vec<(String, bool)> {
    items
        .iter()
        .filter(|(p, _)| {
            !items
                .iter()
                .any(|(other, is_dir)| *is_dir && other != p && is_at_or_below(p, other))
        })
        .cloned()
        .collect()
}

/// Selects every visible row in the explorer tree (FR-L31.4), returning `(multi_selection, primary_selection)`.
pub fn select_all_tree_rows(rows: &[TreeRow]) -> (Vec<(String, bool)>, Option<(String, bool)>) {
    let all: Vec<(String, bool)> = rows.iter().map(|r| (r.path.clone(), r.is_dir)).collect();
    let last = all.last().cloned();
    (all, last)
}

/// Builds a cut [`TreeClipboard`] from either the multi-selection (when `row_path` belongs to it)
/// or `(row_path, is_dir)` alone, pruning redundant descendant paths (FR-L31.4).
pub fn cut_selection(
    multi_sel: &[(String, bool)],
    row_path: &str,
    is_dir: bool,
) -> Option<TreeClipboard> {
    let items = if multi_sel.iter().any(|(p, _)| p == row_path) && !multi_sel.is_empty() {
        multi_sel.to_vec()
    } else {
        vec![(row_path.to_string(), is_dir)]
    };
    let pruned = prune_redundant_selection(&items);
    if pruned.is_empty() {
        None
    } else {
        Some(TreeClipboard {
            kind: ClipboardKind::Cut,
            paths: pruned,
        })
    }
}

/// Formats the status badge (`"복사됨 N개"` / `"잘라냄 N개"`) shown on the explorer root row when the
/// tree clipboard holds items (FR-L31.4).
pub fn clipboard_badge_text(clipboard: Option<&TreeClipboard>) -> Option<String> {
    let clip = clipboard.filter(|c| !c.paths.is_empty())?;
    let verb = match clip.kind {
        ClipboardKind::Copy => "복사됨",
        ClipboardKind::Cut => "잘라냄",
    };
    Some(format!("{verb} {}개", clip.paths.len()))
}

/// The directory a new file or folder goes in: the selected directory, the directory of the
/// selected file, or the project folder when nothing is selected.
pub fn new_item_parent(selected: Option<(&str, bool)>, root: &str) -> String {
    match selected {
        Some((path, true)) => path.to_string(),
        Some((path, false)) => match path.trim_end_matches('/').rsplit_once('/') {
            Some(("", _)) | None => "/".to_string(),
            Some((parent, _)) => parent.to_string(),
        },
        None => root.to_string(),
    }
}

/// What the delete confirmation says. There is no trash, so it says the deletion cannot be undone,
/// a directory goes with everything in it, and which open files have unsaved changes that go too.
pub fn delete_confirmation(path: &str, folder: &str, is_dir: bool, unsaved: &[String]) -> String {
    let name = breadcrumb_segments(path, folder).join("/");
    let mut text = if is_dir {
        format!("폴더 {name}와 그 안의 모든 파일을 삭제할까요? 휴지통이 없어 되돌릴 수 없습니다.")
    } else {
        format!("파일 {name}을 삭제할까요? 휴지통이 없어 되돌릴 수 없습니다.")
    };
    if !unsaved.is_empty() {
        let names: Vec<String> = unsaved.iter().map(|p| breadcrumb_segments(p, folder).join("/")).collect();
        text.push_str(&format!(" 저장하지 않은 변경이 있는 파일: {}. 삭제하면 그 편집도 사라집니다.", names.join(", ")));
    }
    text
}

/// What the delete confirmation says when multiple items are selected.
pub fn delete_many_confirmation(items: &[(String, bool)], folder: &str, unsaved: &[String]) -> String {
    if items.len() == 1 {
        return delete_confirmation(&items[0].0, folder, items[0].1, unsaved);
    }
    let count = items.len();
    let mut text = format!("선택한 항목 {count}개를 삭제할까요? 휴지통이 없어 되돌릴 수 없습니다.");
    if !unsaved.is_empty() {
        let names: Vec<String> = unsaved.iter().map(|p| breadcrumb_segments(p, folder).join("/")).collect();
        text.push_str(&format!(" 저장하지 않은 변경이 있는 파일: {}. 삭제하면 그 편집도 사라집니다.", names.join(", ")));
    }
    text
}

/// Why an explorer edit failed, in Korean: the server's own English text is not shown, only what kind
/// of failure it was.
pub fn edit_failure_text(action: &str, detail: &str) -> String {
    let d = detail.to_lowercase();
    let reason = if d.contains("exists") || d.contains("eexist") {
        "같은 이름이 이미 있습니다"
    } else if d.contains("not found") || d.contains("enoent") || d.contains("does not exist") {
        "대상을 찾을 수 없습니다"
    } else if d.contains("permission") || d.contains("eacces") || d.contains("eperm") {
        "권한이 없습니다"
    } else if d.contains("not empty") || d.contains("enotempty") {
        "폴더가 비어 있지 않습니다"
    } else if d.contains("closed") || d.contains("timed out") || d.contains("connection") {
        "서버와의 연결이 끊겼습니다"
    } else {
        "서버가 요청을 처리하지 못했습니다"
    };
    format!("{action} 실패: {reason}")
}

/// The delete button answers only once the question has been on screen for 600 ms, so a stray double
/// click that opened it cannot also confirm it.
pub const DELETE_GUARD_MS: u64 = 600;

pub fn delete_click_allowed(opened_ms: u64, now_ms: u64) -> bool {
    now_ms.saturating_sub(opened_ms) >= DELETE_GUARD_MS
}

/// Whether closing `tab` is needed after `gone` was deleted or renamed: it is that path or below it.
pub fn is_at_or_below(tab: &str, gone: &str) -> bool {
    let gone = gone.trim_end_matches('/');
    tab == gone || tab.strip_prefix(gone).is_some_and(|rest| rest.starts_with('/'))
}

/// The accessible name of a tab: the file name, and that its edits are not saved.
pub fn tab_aria_label(name: &str, dirty: bool) -> String {
    if dirty { format!("{name} (저장 안 됨)") } else { name.to_string() }
}

/// The accessible name of a tree row: the name, and that it is a folder.
pub fn tree_item_aria_label(name: &str, is_dir: bool) -> String {
    if is_dir { format!("{name} 폴더") } else { name.to_string() }
}

/// The accessible name of a problem row: what it is, what it says, and where.
pub fn problem_aria_label(severity: &str, message: &str, location: &str) -> String {
    format!("{severity}: {message} ({location})")
}

/// The problems tab's label: the tool window's name, with the count when there is one.
pub fn problems_tab_label(count: usize) -> String {
    if count == 0 { ToolWindow::Problems.label().to_string() } else { format!("{} {count}", ToolWindow::Problems.label()) }
}

/// Maximum number of problems rendered in the DOM before offering a load-more button.
pub const MAX_VISIBLE_PROBLEMS: usize = 200;

/// Maximum number of workbench log entries kept in memory.
pub const MAX_WORKBENCH_LOGS: usize = 500;

/// Append a workbench log line, keeping at most [`MAX_WORKBENCH_LOGS`] newest lines.
pub fn push_workbench_log(logs: &mut Vec<String>, line: String) {
    if logs.len() >= MAX_WORKBENCH_LOGS {
        let excess = logs.len() + 1 - MAX_WORKBENCH_LOGS;
        logs.drain(0..excess);
    }
    logs.push(line);
}

/// Return visible problems slice and overflow count.
pub fn paginate_problems<'a, T>(items: &'a [T], limit: usize) -> (&'a [T], usize) {
    let cap = limit.min(items.len());
    (&items[..cap], items.len().saturating_sub(cap))
}

/// Where a problem is, as a row shows it: the file below the project folder and the 1-based line.
pub fn problem_location(path: &str, folder: &str, line: u32) -> String {
    format!("{}:{}", breadcrumb_segments(path, folder).join("/"), line + 1)
}

/// What an arrow, Home, End, or Delete/Close key does in the tab strip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TabKeyOutcome {
    /// Activate and focus this tab path.
    Activate(String),
    /// Close this tab path.
    Close(String),
    /// Refuse to close this dirty tab via keyboard Delete/Backspace without saving first.
    RefuseDirty(String),
}

/// Roving `tabindex` values for the explorer tree: `(container_tabindex, root_tabindex, row_tabindices)`.
/// The tree container (`role="tree"`) is always `"-1"` so it is never a duplicate tab stop;
/// when a child row is active (matching `selected`, else the first item in `multi_sel` present in `rows`),
/// that single child row has `"0"` and all other rows (including the root folder row) have `"-1"`.
/// When no child row is selected, the root folder row (`role="treeitem"`) has `"0"`.
pub fn tree_roving_tabindex(
    rows: &[TreeRow],
    selected: Option<(&str, bool)>,
    multi_sel: &[(String, bool)],
) -> (&'static str, &'static str, Vec<&'static str>) {
    let active_idx = selected
        .and_then(|(p, _)| rows.iter().position(|r| r.path == p))
        .or_else(|| {
            rows.iter()
                .position(|r| multi_sel.iter().any(|(mp, _)| mp == &r.path))
        });
    let root_ti = if active_idx.is_none() { "0" } else { "-1" };
    let row_tis = (0..rows.len())
        .map(|i| if active_idx == Some(i) { "0" } else { "-1" })
        .collect();
    ("-1", root_ti, row_tis)
}

/// Roving `tabindex` values for the tab strip: `(strip_tabindex, tab_tabindices)`.
/// The tablist container (`role="tablist"`) is always `"-1"`; when tabs exist, only the active tab
/// (or the first tab if `current` does not match any open tab) has `"0"`.
pub fn tab_strip_roving_tabindex(
    paths: &[String],
    current: Option<&str>,
) -> (&'static str, Vec<&'static str>) {
    if paths.is_empty() {
        return ("-1", Vec::new());
    }
    let active_idx = current
        .and_then(|c| paths.iter().position(|p| p == c))
        .unwrap_or(0);
    let tab_tis = (0..paths.len())
        .map(|i| if i == active_idx { "0" } else { "-1" })
        .collect();
    ("-1", tab_tis)
}

/// Polite Korean notice shown when keyboard `Delete` or `Backspace` is pressed on an unsaved tab.
pub fn dirty_tab_close_notice(path: &str) -> String {
    let name = display_name(base_name(path));
    format!("{name} 파일에 저장하지 않은 변경이 있어 바로 닫지 않았습니다. 먼저 저장하세요.")
}

/// Handles ArrowLeft, ArrowRight, Home, End, Enter, Space, and Delete/Backspace on the tab strip,
/// refusing to close a dirty tab when `dirty[i]` is true.
pub fn tab_strip_key_with_dirty(
    paths: &[String],
    dirty: &[bool],
    current: Option<&str>,
    key: &str,
) -> Option<TabKeyOutcome> {
    if paths.is_empty() {
        return None;
    }
    match key {
        "Delete" | "Backspace" => {
            let idx = current.and_then(|c| paths.iter().position(|p| p == c))?;
            let path = paths[idx].clone();
            if dirty.get(idx).copied().unwrap_or(false) {
                Some(TabKeyOutcome::RefuseDirty(path))
            } else {
                Some(TabKeyOutcome::Close(path))
            }
        }
        _ => tab_strip_key(paths, current, key),
    }
}

/// Handles ArrowLeft, ArrowRight, Home, End, Enter, Space, and Delete/Backspace on the tab strip.
pub fn tab_strip_key(paths: &[String], current: Option<&str>, key: &str) -> Option<TabKeyOutcome> {
    if paths.is_empty() {
        return None;
    }
    match key {
        "ArrowRight" => {
            let idx = current.and_then(|c| paths.iter().position(|p| p == c));
            let next = match idx {
                Some(i) => (i + 1) % paths.len(),
                None => 0,
            };
            Some(TabKeyOutcome::Activate(paths[next].clone()))
        }
        "ArrowLeft" => {
            let idx = current.and_then(|c| paths.iter().position(|p| p == c));
            let prev = match idx {
                Some(i) => {
                    if i == 0 {
                        paths.len() - 1
                    } else {
                        i - 1
                    }
                }
                None => paths.len() - 1,
            };
            Some(TabKeyOutcome::Activate(paths[prev].clone()))
        }
        "Home" => Some(TabKeyOutcome::Activate(paths[0].clone())),
        "End" => Some(TabKeyOutcome::Activate(paths.last().unwrap().clone())),
        "Enter" | " " | "Space" => {
            let chosen = current
                .filter(|c| paths.iter().any(|p| p == *c))
                .unwrap_or(&paths[0]);
            Some(TabKeyOutcome::Activate(chosen.to_string()))
        }
        "Delete" | "Backspace" => {
            current
                .filter(|c| paths.iter().any(|p| p == *c))
                .map(|c| TabKeyOutcome::Close(c.to_string()))
        }
        _ => None,
    }
}

/// What an arrow, Home, End, Enter, or Space key does in the explorer tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeKeyOutcome {
    /// Select the row `(path, is_dir)`.
    Select(String, bool),
    /// Expand a directory and list its contents.
    Expand(String),
    /// Collapse an expanded directory.
    Collapse(String),
    /// Open a file in the editor.
    OpenFile(String),
    /// Start renaming the row (`F2`, as Code-OSS's `renameFile` keybinding).
    Rename(String),
}

/// Whether the explorer tree container should handle key events (`Escape`, `Cmd/Ctrl+C/X/V`,
/// `Delete`/`Backspace`, and arrow navigation). While an inline prompt (`NewFile`, `NewDir`,
/// `Rename`, `Delete`, `DeleteMany`) is open inside `.ed-tree`, the tree container must ignore
/// keys so `Backspace`, `Delete`, and `Cmd/Ctrl+C/X/V` inside the inline `<input>` are not
/// intercepted by the tree.
pub fn tree_handles_key(prompt: Option<&TreePrompt>) -> bool {
    prompt.is_none()
}

/// Handles WAI-ARIA treeview keyboard navigation (`ArrowDown`, `ArrowUp`, `ArrowRight`, `ArrowLeft`, `Home`, `End`, `Enter`, `" "`)
/// and `F2` (rename the selected row, never the root folder)
/// over `rows`, given `selected` (`Option<(&str, bool)>`) and `root`.
pub fn tree_key_action(
    rows: &[TreeRow],
    selected: Option<(&str, bool)>,
    root: &str,
    key: &str,
) -> Option<TreeKeyOutcome> {
    if rows.is_empty() {
        return None;
    }
    let current_idx = selected.and_then(|(sel_path, _)| rows.iter().position(|r| r.path == sel_path));

    match key {
        "ArrowDown" => {
            let next_idx = match current_idx {
                Some(i) => (i + 1).min(rows.len() - 1),
                None => 0,
            };
            Some(TreeKeyOutcome::Select(rows[next_idx].path.clone(), rows[next_idx].is_dir))
        }
        "ArrowUp" => {
            let prev_idx = match current_idx {
                Some(i) => i.saturating_sub(1),
                None => 0,
            };
            Some(TreeKeyOutcome::Select(rows[prev_idx].path.clone(), rows[prev_idx].is_dir))
        }
        "Home" => Some(TreeKeyOutcome::Select(rows[0].path.clone(), rows[0].is_dir)),
        "End" => {
            let last = rows.last().unwrap();
            Some(TreeKeyOutcome::Select(last.path.clone(), last.is_dir))
        }
        "ArrowRight" => {
            let i = current_idx?;
            let row = &rows[i];
            if row.is_dir && !row.open {
                Some(TreeKeyOutcome::Expand(row.path.clone()))
            } else if row.is_dir && row.open {
                if i + 1 < rows.len() && rows[i + 1].depth == row.depth + 1 {
                    Some(TreeKeyOutcome::Select(rows[i + 1].path.clone(), rows[i + 1].is_dir))
                } else {
                    None
                }
            } else {
                None
            }
        }
        "ArrowLeft" => {
            let i = current_idx?;
            let row = &rows[i];
            if row.is_dir && row.open {
                Some(TreeKeyOutcome::Collapse(row.path.clone()))
            } else if row.depth > 0 {
                let parent_path = new_item_parent(Some((&row.path, false)), root);
                if let Some(parent_row) = rows.iter().find(|r| r.path == parent_path && r.is_dir) {
                    Some(TreeKeyOutcome::Select(parent_row.path.clone(), true))
                } else {
                    (0..i)
                        .rev()
                        .find(|&idx| rows[idx].depth == row.depth - 1 && rows[idx].is_dir)
                        .map(|idx| TreeKeyOutcome::Select(rows[idx].path.clone(), true))
                }
            } else {
                None
            }
        }
        "F2" => {
            let i = current_idx?;
            let row = &rows[i];
            (row.path != root).then(|| TreeKeyOutcome::Rename(row.path.clone()))
        }
        "Enter" | " " | "Space" => {
            let i = current_idx?;
            let row = &rows[i];
            if row.is_dir {
                if row.open {
                    Some(TreeKeyOutcome::Collapse(row.path.clone()))
                } else {
                    Some(TreeKeyOutcome::Expand(row.path.clone()))
                }
            } else {
                Some(TreeKeyOutcome::OpenFile(row.path.clone()))
            }
        }
        _ => None,
    }
}

/// The tree row that takes keyboard focus after `outcome`: the row an arrow key, `Home` or `End`
/// selected. Focus follows the selection, as in Code-OSS, so the next `Enter` or `Space` reaches
/// the selected row instead of the row that had focus before the arrow key.
pub fn tree_key_focus(outcome: &TreeKeyOutcome) -> Option<&str> {
    match outcome {
        TreeKeyOutcome::Select(path, _) => Some(path),
        _ => None,
    }
}

/// The index of the tab that takes keyboard focus after `outcome`: the tab an arrow key, `Home` or
/// `End` activated, so focus follows the selection in the strip as it does in the tree.
pub fn tab_key_focus(paths: &[String], outcome: &TabKeyOutcome) -> Option<usize> {
    match outcome {
        TabKeyOutcome::Activate(path) => paths.iter().position(|p| p == path),
        _ => None,
    }
}

/// What path to focus in the editor when returning focus from a tool window or tree, if any.
pub fn editor_focus_target(current: Option<&str>) -> Option<String> {
    current.map(str::to_string)
}

/// The status bar's agent dot: orange while waiting for approval, blue while running, green otherwise.
pub fn agent_dot_class(state: &str) -> &'static str {
    match state {
        "승인 대기" => "dot wait",
        "실행 중" => "dot run",
        _ => "dot ok",
    }
}

/// The status bar's path: the folder's name, then the directories of the open file below it.
pub fn status_crumbs(folder: &str, current: Option<&str>) -> String {
    let name = folder.trim_end_matches('/').rsplit('/').next().unwrap_or(folder);
    let mut parts = vec![name.to_string()];
    if let Some(path) = current {
        let mut dirs = breadcrumb_segments(path, folder);
        dirs.pop();
        parts.extend(dirs);
    }
    parts.join(" \u{203A} ")
}

/// The status bar's agent field.
pub fn agent_status_text(state: &str) -> String {
    format!("에이전트 {state}")
}

/// The chord a pressed key makes. `key` is the key's name as the keymap table writes it
/// (`P`, `F12`, `Esc`); a single letter is upper-cased so Shift does not change the match.
pub fn chord_from_key(key: &str, ctrl: bool, shift: bool, alt: bool, meta: bool) -> Chord {
    let key = if key.chars().count() == 1 { key.to_uppercase() } else { key.to_string() };
    Chord::Keys { ctrl, shift, alt, meta, key }
}

/// The action a pressed key triggers under a keymap, if any. `key` is the key's name as the
/// keymap table writes it.
pub fn action_for_key(
    keymap: crate::ui::keymap::Keymap,
    os: crate::ui::keymap::Os,
    key: &str,
    ctrl: bool,
    shift: bool,
    alt: bool,
    meta: bool,
) -> Option<Action> {
    keymap.action_for(&chord_from_key(key, ctrl, shift, alt, meta), os)
}

/// How long after a Shift press a second one still counts as a double press.
pub const DOUBLE_SHIFT_MS: u64 = 500;

/// Finds Shift pressed twice in quick succession, the JetBrains chord for search everywhere.
/// Feed it every key down event; it answers true on the second press of a pair.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DoubleShift {
    last_press_ms: Option<u64>,
}

impl DoubleShift {
    /// One key down event at `now_ms`. `repeat` is the operating system's auto repeat for a held
    /// key, which is not a new press. Any key but a bare Shift breaks a pair that was started.
    pub fn press(&mut self, key: &str, repeat: bool, ctrl: bool, alt: bool, meta: bool, now_ms: u64) -> bool {
        if key != "Shift" || ctrl || alt || meta {
            self.last_press_ms = None;
            return false;
        }
        if repeat {
            return false;
        }
        match self.last_press_ms.take() {
            Some(first) if now_ms.saturating_sub(first) <= DOUBLE_SHIFT_MS => true,
            _ => {
                self.last_press_ms = Some(now_ms);
                false
            }
        }
    }
}

pub fn tooltip_text(label: &str, hint: &str) -> String {
    if hint.is_empty() {
        label.to_string()
    } else {
        format!("{label}  {hint}")
    }
}

/// Where a stripe sits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
    Bottom,
}

/// The tool windows the stripes open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolWindow {
    Project,
    /// Search across files and replace (FR-E7).
    Search,
    Commit,
    Git,
    /// Installed extensions, Open VSX search, install, uninstall, enable and disable (FR-L31.14).
    Extensions,
    Agent,
    Notifications,
    Terminal,
    Run,
    Problems,
    Output,
}

impl ToolWindow {
    pub const ALL: [ToolWindow; 11] = [
        ToolWindow::Project,
        ToolWindow::Search,
        ToolWindow::Commit,
        ToolWindow::Git,
        ToolWindow::Extensions,
        ToolWindow::Agent,
        ToolWindow::Notifications,
        ToolWindow::Terminal,
        ToolWindow::Run,
        ToolWindow::Problems,
        ToolWindow::Output,
    ];
    pub const LEFT: [ToolWindow; 6] =
        [ToolWindow::Project, ToolWindow::Search, ToolWindow::Commit, ToolWindow::Git, ToolWindow::Extensions, ToolWindow::Agent];
    pub const RIGHT: [ToolWindow; 1] = [ToolWindow::Notifications];
    /// The buttons of the right stripe: 에이전트 (its window opens in the left slot, #766) and 알림.
    pub const RIGHT_STRIPE: [ToolWindow; 2] = [ToolWindow::Agent, ToolWindow::Notifications];
    pub const BOTTOM: [ToolWindow; 4] =
        [ToolWindow::Terminal, ToolWindow::Run, ToolWindow::Problems, ToolWindow::Output];

    /// The buttons of the left stripe, exactly the mockup's (`editor.js:51`): 프로젝트, 커밋, Git.
    /// 검색 opens from the top find pill and ⇧⌘F, 확장 from its shortcut and the palette; their
    /// windows still take the left side (owner placement rule, 2026-11-13, `docs/DESIGN.md` 6.3).
    pub const LEFT_STRIPE: [ToolWindow; 3] = [ToolWindow::Project, ToolWindow::Commit, ToolWindow::Git];
    /// The buttons of the bottom stripe, exactly the mockup's: 터미널, 실행, 문제. 출력 is a tab of
    /// the bottom tool window beside 문제, not a stripe button.
    pub const BOTTOM_STRIPE: [ToolWindow; 3] = [ToolWindow::Terminal, ToolWindow::Run, ToolWindow::Problems];

    pub fn label(self) -> &'static str {
        match self {
            ToolWindow::Project => "프로젝트",
            ToolWindow::Search => "검색",
            ToolWindow::Commit => "커밋",
            ToolWindow::Git => "Git",
            ToolWindow::Extensions => "확장",
            ToolWindow::Agent => "에이전트",
            ToolWindow::Notifications => "알림",
            ToolWindow::Terminal => "터미널",
            ToolWindow::Run => "실행",
            ToolWindow::Problems => "문제",
            ToolWindow::Output => "출력",
        }
    }

    pub fn side(self) -> Side {
        match self {
            ToolWindow::Project
            | ToolWindow::Search
            | ToolWindow::Commit
            | ToolWindow::Git
            | ToolWindow::Extensions
            | ToolWindow::Agent => Side::Left,
            ToolWindow::Notifications => Side::Right,
            ToolWindow::Terminal | ToolWindow::Run | ToolWindow::Problems | ToolWindow::Output => Side::Bottom,
        }
    }

    /// The keymap action that opens it.
    pub fn action(self) -> Option<Action> {
        match self {
            ToolWindow::Project => Some(Action::Project),
            ToolWindow::Search => Some(Action::FindInFiles),
            ToolWindow::Commit => Some(Action::Commit),
            ToolWindow::Git => Some(Action::Git),
            ToolWindow::Extensions => Some(Action::Extensions),
            ToolWindow::Agent => Some(Action::Ask),
            ToolWindow::Notifications => None,
            ToolWindow::Terminal => Some(Action::Terminal),
            ToolWindow::Run => Some(Action::Run),
            ToolWindow::Problems => Some(Action::Problems),
            ToolWindow::Output => None,
        }
    }

    pub fn from_action(action: Action) -> Option<ToolWindow> {
        ToolWindow::ALL
            .into_iter()
            .find(|w| w.action() == Some(action))
    }

    /// The nearest glyph the pinned toolkit has. TODO(#166: 임의 아이콘 세트): terminal, play,
    /// branch and bell glyphs once `Icon` takes an SVG asset.
    pub fn icon(self) -> IconRole {
        match self {
            ToolWindow::Project => IconRole::Library,
            ToolWindow::Search => IconRole::Search,
            ToolWindow::Commit => IconRole::Check,
            ToolWindow::Git => IconRole::History,
            ToolWindow::Extensions => IconRole::Library,
            ToolWindow::Agent => IconRole::Compose,
            ToolWindow::Notifications => IconRole::Inbox,
            ToolWindow::Terminal => IconRole::Menu,
            ToolWindow::Run => IconRole::Forward,
            ToolWindow::Problems => IconRole::Close,
            ToolWindow::Output => IconRole::Menu,
        }
    }
}

/// Which tool window each stripe shows. Selecting the selected one hides it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stripes {
    left: Option<ToolWindow>,
    right: Option<ToolWindow>,
    bottom: Option<ToolWindow>,
}

impl Default for Stripes {
    fn default() -> Self {
        Stripes {
            left: Some(ToolWindow::Project),
            right: None,
            bottom: Some(ToolWindow::Terminal),
        }
    }
}

/// Width in px at and below which the mockup lays the left tool window over the editor island
/// and opens with it closed (`editor.css` `@media (max-width:900px)`, `editor.js` `onHash`).
pub const NARROW_WINDOW_PX: f32 = 900.0;

impl Stripes {
    /// The stripes a window opened `width` px wide starts with (FR-L31.41): the default, less the
    /// left tool window when the window is narrow, because there it would cover the code. A width
    /// not known yet (0, nothing reported and no opened size) keeps the default.
    pub fn opening(width: f32) -> Stripes {
        let mut s = Stripes::default();
        if width > 0.0 && width <= NARROW_WINDOW_PX {
            s.left = None;
        }
        s
    }

    pub fn selected(&self, side: Side) -> Option<ToolWindow> {
        match side {
            Side::Left => self.left,
            Side::Right => self.right,
            Side::Bottom => self.bottom,
        }
    }

    pub fn is_selected(&self, w: ToolWindow) -> bool {
        self.selected(w.side()) == Some(w)
    }

    pub fn toggle(&mut self, w: ToolWindow) {
        let next = if self.is_selected(w) { None } else { Some(w) };
        match w.side() {
            Side::Left => self.left = next,
            Side::Right => self.right = next,
            Side::Bottom => self.bottom = next,
        }
    }

    pub fn hide(&mut self, side: Side) {
        match side {
            Side::Left => self.left = None,
            Side::Right => self.right = None,
            Side::Bottom => self.bottom = None,
        }
    }

    pub fn show(&mut self, w: ToolWindow) {
        match w.side() {
            Side::Left => self.left = Some(w),
            Side::Right => self.right = Some(w),
            Side::Bottom => self.bottom = Some(w),
        }
    }
}


/// Whether the editor does something for `action` today ([`apply_action`] returns true for it
/// when it can act). The palette shows the others as not ready instead of as live commands.
pub fn is_implemented(action: Action) -> bool {
    // The notification tool window has no panel in the window yet: toggling it shows nothing.
    ToolWindow::from_action(action).is_some_and(|w| w.side() != Side::Right)
        || matches!(
            action,
            Action::Close
                | Action::HideTool
                | Action::FindInFile
                | Action::ReplaceInFiles
                | Action::Recent
                | Action::GotoLine
                | Action::Save
                | Action::MaximizeTool
                | Action::ReopenClosed
                | Action::Palette
                | Action::Find
                | Action::ZoomIn
                | Action::ZoomOut
                | Action::ZoomReset
                | Action::Settings
                | Action::SettingsJson
                | Action::KeybindingsJson
                | Action::Format
                | Action::NewTerminal
                | Action::NextTab
                | Action::PrevTab
        )
}

/// Do what a keymap action does. Returns false for an action the editor cannot do yet (the
/// caller says "준비 중입니다"). The key handler calls this with `Keymap::action_for`.
pub fn apply_action(action: Action, tabs: &mut EditorTabs, stripes: &mut Stripes) -> bool {
    if let Some(w) = ToolWindow::from_action(action) {
        if w.side() == Side::Right {
            return false;
        }
        stripes.toggle(w);
        return true;
    }
    match action {
        Action::NewTerminal => {
            stripes.show(ToolWindow::Terminal);
            true
        }
        Action::NextTab => {
            tabs.next_tab();
            true
        }
        Action::PrevTab => {
            tabs.prev_tab();
            true
        }
        Action::Close => match tabs.current_path().map(str::to_string) {
            Some(p) => {
                tabs.close(&p);
                true
            }
            None => false,
        },
        Action::HideTool => {
            stripes.hide(Side::Bottom);
            true
        }
        _ => false,
    }
}

#[component]
pub fn EditorScreen(target: EditorTarget) -> Element {
    // The editor runs as its own HTML-mode window (`editor_workbench::page::app`); this screen
    // is only reached when a Compose root asks for it, and draws nothing.
    let _ = target;
    rsx! {}
}

/// The kind of change for a line in a diff against HEAD.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffLineKind {
    Added,
    Modified,
    Deleted,
}

/// A line diff marker for the editor gutter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiffLineMarker {
    pub line: u32,
    pub kind: DiffLineKind,
}

/// Maximum number of LCS matrix cells (`old_mid.len() * new_mid.len()`) allocated by [`compute_line_diffs`].
/// When the changed middle region exceeds this cap, [`compute_line_diffs`] pairs lines in O(n) memory.
pub const MAX_DIFF_MATRIX_CELLS: usize = 250_000;

/// Compute line-level diff markers (added, modified, deleted lines) comparing new_text against old_text.
pub fn compute_line_diffs(old_text: &str, new_text: &str) -> Vec<DiffLineMarker> {
    if old_text == new_text {
        return Vec::new();
    }
    let norm_old = old_text.replace("\r\n", "\n").replace('\r', "\n");
    let norm_new = new_text.replace("\r\n", "\n").replace('\r', "\n");

    let old_lines: Vec<&str> = if norm_old.is_empty() {
        Vec::new()
    } else {
        norm_old.split('\n').collect()
    };
    let new_lines: Vec<&str> = if norm_new.is_empty() {
        Vec::new()
    } else {
        norm_new.split('\n').collect()
    };

    if old_lines.is_empty() {
        return new_lines
            .iter()
            .enumerate()
            .map(|(i, _)| DiffLineMarker {
                line: i as u32,
                kind: DiffLineKind::Added,
            })
            .collect();
    }
    if new_lines.is_empty() {
        return vec![DiffLineMarker {
            line: 0,
            kind: DiffLineKind::Deleted,
        }];
    }

    // 1. Common prefix
    let mut prefix = 0;
    while prefix < old_lines.len()
        && prefix < new_lines.len()
        && old_lines[prefix] == new_lines[prefix]
    {
        prefix += 1;
    }

    // 2. Common suffix
    let mut suffix = 0;
    while suffix < (old_lines.len() - prefix)
        && suffix < (new_lines.len() - prefix)
        && old_lines[old_lines.len() - 1 - suffix] == new_lines[new_lines.len() - 1 - suffix]
    {
        suffix += 1;
    }

    let old_mid = &old_lines[prefix..old_lines.len() - suffix];
    let new_mid = &new_lines[prefix..new_lines.len() - suffix];

    let mut markers = Vec::new();

    if old_mid.is_empty() {
        for i in 0..new_mid.len() {
            markers.push(DiffLineMarker {
                line: (prefix + i) as u32,
                kind: DiffLineKind::Added,
            });
        }
        return markers;
    }

    if new_mid.is_empty() {
        let del_line = prefix.min(new_lines.len().saturating_sub(1));
        markers.push(DiffLineMarker {
            line: del_line as u32,
            kind: DiffLineKind::Deleted,
        });
        return markers;
    }

    let m = old_mid.len();
    let n = new_mid.len();
    if m.saturating_mul(n) > MAX_DIFF_MATRIX_CELLS {
        let mod_count = m.min(n);
        for idx in 0..n {
            let kind = if idx < mod_count { DiffLineKind::Modified } else { DiffLineKind::Added };
            markers.push(DiffLineMarker { line: (prefix + idx) as u32, kind });
        }
        return markers;
    }
    let mut dp = vec![vec![0u32; n + 1]; m + 1];

    for i in 0..m {
        for j in 0..n {
            if old_mid[i] == new_mid[j] {
                dp[i + 1][j + 1] = dp[i][j] + 1;
            } else {
                dp[i + 1][j + 1] = dp[i + 1][j].max(dp[i][j + 1]);
            }
        }
    }

    #[derive(Debug, PartialEq, Eq)]
    enum Edit {
        Equal(usize, usize),
        Delete(usize),
        Insert(usize),
    }

    let mut edits = Vec::new();
    let mut i = m;
    let mut j = n;
    while i > 0 || j > 0 {
        if i > 0 && j > 0 && old_mid[i - 1] == new_mid[j - 1] {
            edits.push(Edit::Equal(i - 1, j - 1));
            i -= 1;
            j -= 1;
        } else if j > 0 && (i == 0 || dp[i][j - 1] >= dp[i - 1][j]) {
            edits.push(Edit::Insert(j - 1));
            j -= 1;
        } else {
            edits.push(Edit::Delete(i - 1));
            i -= 1;
        }
    }
    edits.reverse();

    let mut k = 0;
    while k < edits.len() {
        match edits[k] {
            Edit::Equal(_, _) => {
                k += 1;
            }
            Edit::Delete(_) | Edit::Insert(_) => {
                let mut del_count = 0;
                let mut ins_indices = Vec::new();
                while k < edits.len() {
                    match edits[k] {
                        Edit::Delete(_) => {
                            del_count += 1;
                            k += 1;
                        }
                        Edit::Insert(new_idx) => {
                            ins_indices.push(new_idx);
                            k += 1;
                        }
                        Edit::Equal(_, _) => break,
                    }
                }
                if del_count == 0 {
                    for idx in ins_indices {
                        markers.push(DiffLineMarker {
                            line: (prefix + idx) as u32,
                            kind: DiffLineKind::Added,
                        });
                    }
                } else if ins_indices.is_empty() {
                    let line_in_new = if k < edits.len() {
                        if let Edit::Equal(_, new_idx) = edits[k] {
                            prefix + new_idx
                        } else {
                            prefix + new_mid.len()
                        }
                    } else {
                        prefix + new_mid.len()
                    };
                    let del_line = line_in_new.min(new_lines.len().saturating_sub(1));
                    markers.push(DiffLineMarker {
                        line: del_line as u32,
                        kind: DiffLineKind::Deleted,
                    });
                } else {
                    let mod_count = del_count.min(ins_indices.len());
                    for (idx_pos, &idx) in ins_indices.iter().enumerate() {
                        let kind = if idx_pos < mod_count {
                            DiffLineKind::Modified
                        } else {
                            DiffLineKind::Added
                        };
                        markers.push(DiffLineMarker {
                            line: (prefix + idx) as u32,
                            kind,
                        });
                    }
                }
            }
        }
    }

    markers
}

/// Convert diff line markers into compose_rust GutterDecorations for CodeEditor.
pub fn diff_gutter_decorations(version: u32, markers: &[DiffLineMarker]) -> compose_rust::code::GutterDecorations {
    use compose_rust::code::GutterDecoration;
    use compose_rust::ColorRole;
    let list = markers.iter().map(|m| match m.kind {
        DiffLineKind::Added => GutterDecoration::bar(version, m.line, 0).with_color(ColorRole::DiffAdded),
        DiffLineKind::Modified => GutterDecoration::bar(version, m.line, 0).with_color(ColorRole::DiffModified),
        DiffLineKind::Deleted => GutterDecoration::dot(version, m.line, 0).with_color(ColorRole::DiffRemoved),
    });
    compose_rust::code::GutterDecorations::new(list)
}

/// The tints over a diff's added and changed lines (FR-L31.10, compose-rust#542): one
/// `Decoration::line_background` per run of neighbouring lines of the same kind. A removed line has
/// no line of its own in the working text, so it keeps its gutter dot and gets no tint.
pub fn diff_line_tints(version: u32, markers: &[DiffLineMarker]) -> Vec<compose_rust::code::Decoration> {
    use compose_rust::code::Decoration;
    use compose_rust::ColorRole;
    let mut runs: Vec<(u32, u32, DiffLineKind)> = Vec::new();
    for m in markers.iter().filter(|m| m.kind != DiffLineKind::Deleted) {
        match runs.last_mut() {
            Some((_, end, kind)) if *kind == m.kind && *end + 1 == m.line => *end = m.line,
            _ => runs.push((m.line, m.line, m.kind)),
        }
    }
    runs.into_iter()
        .map(|(start, end, kind)| {
            let role = if kind == DiffLineKind::Added { ColorRole::DiffAdded } else { ColorRole::DiffModified };
            Decoration::line_background(version, start, end, role)
        })
        .collect()
}

/// The document's own decorations with the diff's line tints after them.
pub fn with_diff_line_tints(decorations: compose_rust::code::Decorations, version: u32, markers: &[DiffLineMarker]) -> compose_rust::code::Decorations {
    let tints = diff_line_tints(version, markers);
    if tints.is_empty() {
        return decorations;
    }
    compose_rust::code::Decorations::new(decorations.decorations().into_iter().chain(tints))
}

/// The 0-indexed line number of the first change between old and new text.
pub fn first_diff_line(old_text: &str, new_text: &str) -> Option<u32> {
    compute_line_diffs(old_text, new_text).first().map(|m| m.line)
}


#[cfg(test)]
mod tests {

    fn hover(range: Option<ember_editor::WidgetRange>, diag: &[&str], contents: &[&str]) -> ember_editor::hover::HoverPopup {
        ember_editor::hover::HoverPopup {
            doc: "d".into(),
            generation: 3,
            pos: ember_editor::WidgetPos::new(1, 9),
            range,
            diagnostics: diag
                .iter()
                .map(|m| ember_editor::diagnostics::HoverDiagnostic { owner: "o".into(), severity: ember_editor::Severity::Error, message: (*m).into(), source: None, code: None })
                .collect(),
            contents: contents.iter().map(|v| ember_editor::hover::MarkdownString { value: (*v).into(), is_trusted: false, support_theme_icons: false, support_html: false, raw: serde_json::Value::Null }).collect(),
            complete: true,
        }
    }

    /// SPEC FR-E3 (#32 phase A step 7): the card lists diagnostics first, then the providers' text;
    /// it anchors at the hovered word's start when a range came back, else at the pointer; it goes
    /// under the line near the top of the window and above it lower down.
    #[test]
    fn fr_e3_hover_card_text_anchor_and_placement() {
        assert_eq!(hover_card(&hover(None, &[], &["  ", ""])), None, "nothing to show");
        let h = hover(Some(ember_editor::WidgetRange::new(ember_editor::WidgetPos::new(1, 6), ember_editor::WidgetPos::new(1, 11))), &["expected ','"], &["**probe**", ""]);
        assert_eq!(hover_card(&h), Some(HoverCard { diagnostics: vec!["expected ','".into()], contents: vec!["**probe**".into()] }));
        assert_eq!(hover_key(3, &h), (3, 1, 6));
        assert_eq!(hover_key(3, &hover(None, &[], &["x"])), (3, 1, 9));
        let high = hover_card_style(compose_rust::code::CodeRect::new(100.0, 80.0, 2.0, 18.0), 800.0);
        assert!(high.contains("left:100px;") && high.contains("top:100px;"), "{high}");
        let low = hover_card_style(compose_rust::code::CodeRect::new(-5.0, 600.0, 2.0, 18.0), 800.0);
        assert!(low.contains("left:0px;") && low.contains("bottom:calc(100vh - 598px);"), "{low}");
    }

    /// SPEC FR-E3: the card flips above the line only when it does not fit below in the window
    /// and there is more room above; the decision reads the window's height, not a fixed line.
    #[test]
    fn fr_e3_hover_card_flips_by_window_height() {
        let at = |y: f32, h: f32| hover_card_style(compose_rust::code::CodeRect::new(10.0, y, 2.0, 18.0), h);
        // 400 dp tall: 80 dp below, 298 above: above.
        assert!(at(300.0, 400.0).contains("bottom:calc(100vh - 298px);"));
        // The same line in a 1200 dp window: 800 dp below: below, where the old fixed 360 rule flipped it.
        assert!(at(380.0, 1200.0).contains("top:400px;"));
        assert!(at(500.0, 1200.0).contains("top:520px;"));
        // Too little room either way: the side with more room.
        assert!(at(100.0, 300.0).contains("top:120px;"));
        assert!(at(180.0, 300.0).contains("bottom:calc(100vh - 178px);"));
        // Size not reported yet: below.
        assert!(at(700.0, 0.0).contains("top:720px;"));
    }

    /// SPEC FR-E3: provider Markdown is drawn as headings, paragraphs, lists, code and inline
    /// styles, not as its source.
    #[test]
    fn fr_e3_hover_markdown_blocks_and_spans() {
        use MdBlock as B;
        use MdSpan as S;
        let md = "## Heading ##\n\nUse **bold**, *italic*, `a_b()` and [docs](https://example.com \"t\").\nsnake_case_name stays.\n\n- one\n  more\n- two\n\n1. first\n2) second\n\n---\n```rust\nlet x = 1;\n\nlet y = 2;\n```\n\n    indented()\n\\*not em\\*";
        let blocks = markdown_blocks(md);
        assert_eq!(blocks[0], B::Heading { level: 2, spans: vec![S::Text("Heading".into())] });
        assert_eq!(
            blocks[1],
            B::Paragraph(vec![
                S::Text("Use ".into()),
                S::Strong("bold".into()),
                S::Text(", ".into()),
                S::Emphasis("italic".into()),
                S::Text(", ".into()),
                S::Code("a_b()".into()),
                S::Text(" and ".into()),
                S::Link { text: "docs".into(), href: "https://example.com".into() },
                S::Text(". snake_case_name stays.".into()),
            ])
        );
        assert_eq!(blocks[2], B::List { ordered: false, items: vec![vec![S::Text("one more".into())], vec![S::Text("two".into())]] });
        assert_eq!(blocks[3], B::List { ordered: true, items: vec![vec![S::Text("first".into())], vec![S::Text("second".into())]] });
        assert_eq!(blocks[4], B::Rule);
        assert_eq!(blocks[5], B::Code { lang: "rust".into(), text: "let x = 1;\n\nlet y = 2;".into() });
        assert_eq!(blocks[6], B::Code { lang: String::new(), text: "indented()".into() });
        assert_eq!(blocks[7], B::Paragraph(vec![S::Text("*not em*".into())]));
        assert_eq!(blocks.len(), 8);
        // Unclosed markers and HTML stay as text.
        assert_eq!(markdown_spans("a ** b `c <b>x</b>"), vec![S::Text("a ** b `c <b>x</b>".into())]);
        assert_eq!(markdown_blocks("#nospace"), vec![B::Paragraph(vec![S::Text("#nospace".into())])]);
    }

    /// SPEC FR-E10, Q-E2 (#32 phase A step 6): one sentence for the fallback, in 합니다체, naming a
    /// short commit or saying it is unknown, and offering VS Code only when a target exists.
    #[test]
    fn fr_e10_fallback_sentence_names_the_commit_and_the_way_out() {
        use ember_editor::Fallback;
        let known = Fallback::OpenInVsCode { server_commit: Some("0123456789abcdef0123".into()) };
        assert_eq!(
            fallback_sentence(&known, true),
            "이 컴퓨터의 편집기 서버(Code-OSS 0123456789ab)는 Ember 에디터가 지원하지 않는 버전입니다. VS Code에서 열 수 있습니다."
        );
        let unknown = Fallback::OpenInVsCode { server_commit: None };
        assert_eq!(
            fallback_sentence(&unknown, false),
            "이 컴퓨터의 편집기 서버 버전을 알 수 없어 Ember 에디터로 열 수 없습니다. 이 프로젝트에는 열 수 있는 VS Code 대상도 없습니다."
        );
        assert_eq!(FALLBACK_BUTTON, "VS Code에서 열기");
    }

    use super::*;

    #[test]
    fn fr_e6_every_syntax_role_has_its_own_widget_colour_role() {
        use ember_editor::textmate::SyntaxRole as R;
        let roles = [
            R::Keyword, R::String, R::Comment, R::Number, R::Constant, R::Type, R::Function, R::Variable, R::Property, R::Operator,
            R::Punctuation, R::Tag, R::Attribute, R::Escape, R::Macro,
        ];
        let mut seen = std::collections::HashSet::new();
        for r in roles {
            assert!(seen.insert(format!("{:?}", colour_role(r))), "{r:?} shares a colour role");
        }
        assert_eq!(seen.len(), 15);
    }

    #[test]
    fn fr_e6_the_syntax_spans_prop_carries_each_run_with_the_version_it_was_written_against() {
        use ember_editor::textmate::{DocRun, SyntaxRole};
        let syntax = ember_editor::SyntaxRuns {
            version: 7,
            runs: vec![DocRun { line: 2, start: 1, end: 5, role: SyntaxRole::Keyword }, DocRun { line: 3, start: 0, end: 2, role: SyntaxRole::Comment }],
        };
        let spans = syntax_spans(&syntax).spans();
        assert_eq!(spans.len(), 2);
        assert!(spans.iter().all(|s| s.version == 7));
        assert!(syntax_spans(&ember_editor::SyntaxRuns::default()).is_empty());
    }

    #[test]
    fn fr_l31_6_a3_window_syntax_runs_and_folds_are_written_in_the_widget_numbering() {
        use ember_editor::textmate::{DocRun, SyntaxRole};
        let syntax = ember_editor::SyntaxRuns { version: 12, runs: vec![DocRun { line: 0, start: 0, end: 2, role: SyntaxRole::Keyword }] };
        // The session is at 12 and the widget was mounted again at 10: it knows this text as version 2.
        assert!(syntax_spans_at(&syntax, 2).spans().iter().all(|s| s.version == 2));
        let regions = [ember_editor::FoldRegion { version: 12, start_line: 1, end_line: 4, collapsed: false }];
        assert_eq!(folds_from(&regions, 10).folds()[0].version, 2);
        assert_eq!(folds_from(&regions, 0).folds()[0].version, 12, "a widget that never restarted keeps the session's numbers");
    }

    #[test]
    fn fr_l31_failure_reasons_are_korean_and_never_the_server_text() {
        assert!(file_failure_reason("remote error: EntryNotFound (FileSystemError)").contains("없습니다"));
        assert!(file_failure_reason("NoPermissions (FileSystemError): /root/x").contains("권한"));
        assert!(file_failure_reason("file is not UTF-8").contains("UTF-8"));
        assert!(file_failure_reason("connection closed").contains("연결"));
        assert!(file_failure_reason("timed out: readFile").contains("연결"));
        let other = file_failure_reason("weird \u{1b}[31m thing at /secret/path");
        assert!(!other.contains("secret") && !other.contains('\u{1b}'));
    }

    #[test]
    fn fr_l31_display_name_drops_control_characters_and_shortens() {
        assert_eq!(display_name("a\nb\u{1b}[2Jc.txt"), "ab[2Jc.txt");
        assert_eq!(display_name(&"x".repeat(200)).chars().count(), 80);
        assert_eq!(display_name("README.md"), "README.md");
    }

    #[test]
    fn fr_l31_4_rename_box_replaces_the_renamed_rows_label() {
        let row = |path: &str, depth| TreeRow { path: path.into(), name: base_name_for_test(path), depth, is_dir: false, open: false };
        let rows = vec![row("/w/src", 0), row("/w/src/a.rs", 1), row("/w/b.md", 0)];
        assert_eq!(rename_in_place_row(Some(&TreePrompt::Rename { path: "/w/src/a.rs".into() }), &rows), Some(1));
        assert_eq!(rename_in_place_row(Some(&TreePrompt::Rename { path: "/w/b.md".into() }), &rows), Some(2));
        assert_eq!(rename_in_place_row(Some(&TreePrompt::Rename { path: "/w/gone".into() }), &rows), None);
        assert_eq!(rename_in_place_row(Some(&TreePrompt::NewFile { parent: "/w/src".into() }), &rows), None);
        assert_eq!(rename_in_place_row(None, &rows), None);
    }

    fn base_name_for_test(path: &str) -> String { path.rsplit('/').next().unwrap_or(path).to_string() }

    #[test]
    fn fr_l31_row_click_keeps_a_typed_name_and_closes_an_empty_field() {
        let new_file = TreePrompt::NewFile { parent: "/w".into() };
        assert!(row_click_closes_prompt(&new_file, ""));
        assert!(row_click_closes_prompt(&new_file, "  "));
        assert!(!row_click_closes_prompt(&new_file, "notes.md"));
        assert!(!row_click_closes_prompt(&TreePrompt::Rename { path: "/w/a".into() }, "b"));
        assert!(!row_click_closes_prompt(&TreePrompt::Delete { path: "/w/a".into(), is_dir: false }, ""));
    }

    #[test]
    fn fr_l31_leaving_the_name_field_closes_it_only_when_empty() {
        assert!(blur_closes_prompt(""));
        assert!(blur_closes_prompt("   "));
        assert!(!blur_closes_prompt("notes.md"));
        assert!(!blur_closes_prompt(" a "));
    }
    use crate::ui::keymap::{Keymap, Os};

    #[test]
    fn fr_l31_is_implemented_matches_what_apply_action_does() {
        for action in Action::ALL {
            // The palette, find, find-in-file, replace-in-files, recent files and goto-line open sheets and bars in the window:
            // they are not tab or tool window changes, so `apply_action` does not do them.
            // Save, maximize and reopen act on the window's session, panel size and close history (FR-L31.15).
            // The settings view and the two settings files open a tab through the window (FR-L31.17).
            // Format document goes to the language features (FR-L31.13), not to a tab or a tool window.
            if matches!(
                action,
                Action::Palette
                    | Action::Find
                    | Action::FindInFile
                    | Action::ReplaceInFiles
                    | Action::Recent
                    | Action::GotoLine
                    | Action::Save
                    | Action::MaximizeTool
                    | Action::ReopenClosed
                    | Action::ZoomIn
                    | Action::ZoomOut
                    | Action::ZoomReset
                    | Action::Settings
                    | Action::SettingsJson
                    | Action::KeybindingsJson
                    | Action::Format
            ) {
                continue;
            }
            let mut tabs = EditorTabs::default();
            tabs.open("/w/p/a.rs");
            let mut stripes = Stripes::default();
            assert_eq!(apply_action(action, &mut tabs, &mut stripes), is_implemented(action), "{action:?}");
        }
    }

    #[test]
    fn fr_l31_41_a_narrow_window_opens_with_the_left_tool_window_closed() {
        for w in [390.0, 768.0, 900.0] {
            let s = Stripes::opening(w);
            assert_eq!(s.selected(Side::Left), None, "{w}");
            assert_eq!(s.selected(Side::Bottom), Some(ToolWindow::Terminal), "{w}");
            assert_eq!(s.selected(Side::Right), None, "{w}");
        }
        for w in [0.0, 901.0, 1024.0, 1440.0] {
            assert_eq!(Stripes::opening(w), Stripes::default(), "{w}");
        }
        assert_eq!(Stripes::default().selected(Side::Left), Some(ToolWindow::Project));
    }

    #[test]
    fn fr_l31_41_the_window_opens_its_stripes_for_its_width() {
        let page = include_str!("editor_workbench.rs");
        assert!(page.contains("Stripes::opening(crate::ui::window_size::window_size_untracked().width_dp)"));
        let lib = include_str!("../lib.rs");
        let editor = &lib[lib.find("Separate IDE window (FR-L31)").unwrap()..lib.find("ui::cr::gallery::requested()").unwrap()];
        assert!(editor.contains("ui::window_size::record_opened(f32::from(w), f32::from(h));"));
    }

    #[test]
    fn fr_l31_15_reopen_closed_editor_reopens_the_tab_closed_last() {
        let mut tabs = EditorTabs::default();
        for p in ["/w/a", "/w/b", "/w/c"] {
            tabs.open(p);
        }
        tabs.close("/w/b");
        assert_eq!(tabs.reopen_closed().as_deref(), Some("/w/b"));
        assert_eq!(tabs.current_path(), Some("/w/b"));
        assert_eq!(tabs.paths(), vec!["/w/a", "/w/b", "/w/c"], "a reopened tab goes back to its old place");
        tabs.activate("/w/a");
        assert_eq!(tabs.close_others(), vec!["/w/b".to_string(), "/w/c".to_string()]);
        assert_eq!(tabs.reopen_closed().as_deref(), Some("/w/c"));
        tabs.forget_closed_below("/w/b");
        assert_eq!(tabs.reopen_closed(), None);
        assert_eq!(tabs.paths(), vec!["/w/a", "/w/c"]);
    }

    #[test]
    fn fr_l31_actions_toggle_tool_windows_and_close_tabs() {
        let mut tabs = EditorTabs::default();
        let mut stripes = Stripes::default();
        assert!(apply_action(Action::Terminal, &mut tabs, &mut stripes));
        assert_eq!(stripes.selected(Side::Bottom), None);
        assert!(apply_action(Action::Problems, &mut tabs, &mut stripes));
        assert!(stripes.is_selected(ToolWindow::Problems));
        assert!(!apply_action(Action::Close, &mut tabs, &mut stripes));
        tabs.open("a.rs");
        assert!(apply_action(Action::Close, &mut tabs, &mut stripes));
        assert_eq!(tabs.current_path(), None);
        assert!(!apply_action(Action::Format, &mut tabs, &mut stripes));
    }

    #[test]
    fn fr_l31_tooltip_and_base_name() {
        assert_eq!(tooltip_text("저장", ""), "저장");
        assert_eq!(tooltip_text("저장", "Ctrl+S"), "저장  Ctrl+S");
        assert_eq!(base_name("ember/app/src/lib.rs"), "lib.rs");
        assert_eq!(base_name("lib.rs"), "lib.rs");
    }

    #[test]
    fn fr_l31_every_stripe_window_belongs_to_one_list() {
        for w in ToolWindow::ALL {
            let n = [&ToolWindow::LEFT[..], &ToolWindow::RIGHT[..], &ToolWindow::BOTTOM[..]]
                .iter()
                .filter(|l| l.contains(&w))
                .count();
            assert_eq!(n, 1, "{w:?}");
        }
    }

    #[test]
    fn fr_l31_key_chords_drive_tabs_and_stripes() {
        let mut tabs = EditorTabs::default();
        let mut stripes = Stripes::default();
        tabs.open("a.rs");
        tabs.open("b.rs");

        let close = action_for_key(Keymap::VsCode, Os::Other, "w", true, false, false, false);
        assert_eq!(close, Some(Action::Close));
        assert!(apply_action(close.unwrap(), &mut tabs, &mut stripes));
        assert_eq!(tabs.paths(), vec!["a.rs"]);

        assert!(stripes.is_selected(ToolWindow::Terminal));
        let terminal = action_for_key(Keymap::VsCode, Os::Other, "`", true, false, false, false);
        assert_eq!(terminal, Some(Action::Terminal));
        assert!(apply_action(terminal.unwrap(), &mut tabs, &mut stripes));
        assert_eq!(stripes.selected(Side::Bottom), None);

        assert_eq!(action_for_key(Keymap::VsCode, Os::Other, "w", false, false, false, false), None);
    }

    #[test]
    fn fr_l31_editor_target_debug_redacts_the_tickets() {
        let target = EditorTarget {
            project: "p".into(),
            folder: "/f".into(),
            endpoint: "127.0.0.1:9000".into(),
            ose_path: "/ide/ose/".into(),
            ticket: "s3cret-token".into(),
            server_url: "http://srv:1".into(),
            computer: "node-a".into(),
            relay: "/api/computers/node-a/ide/relay/l1".into(),
            vscode_url: String::new(),
            vscode_relay: String::new(),
            vscode_path: String::new(),
            vscode_ticket: "vs-s3cret".into(),
            vscode_folder: String::new(),
            vscode_session: String::new(),
        };
        let text = format!("{target:?}");
        assert!(text.contains("/api/computers/node-a/ide/relay/l1"), "{text}");
        assert!(!text.contains("s3cret-token") && !text.contains("vs-s3cret"), "{text}");
        assert!(text.contains("[REDACTED]"));
        assert!(text.contains("127.0.0.1:9000"));
        assert!(text.contains("http://srv:1") && text.contains("node-a"));
    }

    #[test]
    fn fr_l31_2_compute_line_diffs_identifies_added_modified_and_deleted_lines() {
        let old = "line 1\nline 2\nline 3\nline 4";
        let new = "line 1\nline 2 mod\nline 3\nline 4\nline 5 add";
        let markers = compute_line_diffs(old, new);
        assert_eq!(markers, vec![
            DiffLineMarker { line: 1, kind: DiffLineKind::Modified },
            DiffLineMarker { line: 4, kind: DiffLineKind::Added },
        ]);
        assert_eq!(first_diff_line(old, new), Some(1));

        let deleted_new = "line 1\nline 3\nline 4";
        let del_markers = compute_line_diffs(old, deleted_new);
        assert_eq!(del_markers, vec![
            DiffLineMarker { line: 1, kind: DiffLineKind::Deleted },
        ]);

        let identical = compute_line_diffs(old, old);
        assert!(identical.is_empty());
        assert_eq!(first_diff_line(old, old), None);

        let brand_new = compute_line_diffs("", "a\nb");
        assert_eq!(brand_new, vec![
            DiffLineMarker { line: 0, kind: DiffLineKind::Added },
            DiffLineMarker { line: 1, kind: DiffLineKind::Added },
        ]);

        let all_deleted = compute_line_diffs("a\nb", "");
        assert_eq!(all_deleted, vec![
            DiffLineMarker { line: 0, kind: DiffLineKind::Deleted },
        ]);
    }

    #[test]
    fn fr_l31_2_compute_line_diffs_caps_large_middle_region_without_quadratic_table() {
        let old_lines: Vec<String> = (0..20_000).map(|i| format!("old_{i}")).collect();
        let new_lines: Vec<String> = (0..20_005).map(|i| format!("new_{i}")).collect();
        let old = old_lines.join("\n");
        let new = new_lines.join("\n");
        let markers = compute_line_diffs(&old, &new);
        assert_eq!(markers.len(), 20_005);
        assert_eq!(markers[0], DiffLineMarker { line: 0, kind: DiffLineKind::Modified });
        assert_eq!(markers[19_999], DiffLineMarker { line: 19_999, kind: DiffLineKind::Modified });
        assert_eq!(markers[20_000], DiffLineMarker { line: 20_000, kind: DiffLineKind::Added });
        assert_eq!(markers[20_004], DiffLineMarker { line: 20_004, kind: DiffLineKind::Added });
    }

    #[test]
    fn fr_l31_2_diff_gutter_decorations_maps_to_color_roles() {
        use compose_rust::{ColorRole, GutterDecorationKind};

        let markers = vec![
            DiffLineMarker { line: 0, kind: DiffLineKind::Added },
            DiffLineMarker { line: 2, kind: DiffLineKind::Modified },
            DiffLineMarker { line: 5, kind: DiffLineKind::Deleted },
        ];
        let decs = diff_gutter_decorations(4, &markers);
        assert!(!decs.is_empty());
        let list = decs.decorations();
        assert_eq!(list.len(), 3);

        assert_eq!(list[0].line, 0);
        assert_eq!(list[0].version, 4);
        assert_eq!(list[0].kind, GutterDecorationKind::Bar);
        assert_eq!(list[0].color, Some(ColorRole::DiffAdded));

        assert_eq!(list[1].line, 2);
        assert_eq!(list[1].version, 4);
        assert_eq!(list[1].kind, GutterDecorationKind::Bar);
        assert_eq!(list[1].color, Some(ColorRole::DiffModified));

        assert_eq!(list[2].line, 5);
        assert_eq!(list[2].version, 4);
        assert_eq!(list[2].kind, GutterDecorationKind::Dot);
        assert_eq!(list[2].color, Some(ColorRole::DiffRemoved));

        let empty = diff_gutter_decorations(1, &[]);
        assert!(empty.is_empty());
    }

    #[test]
    fn fr_l31_10_added_and_changed_lines_are_tinted_in_runs() {
        use compose_rust::{ColorRole, DecorationKind};

        let markers = vec![
            DiffLineMarker { line: 0, kind: DiffLineKind::Added },
            DiffLineMarker { line: 1, kind: DiffLineKind::Added },
            DiffLineMarker { line: 2, kind: DiffLineKind::Modified },
            DiffLineMarker { line: 4, kind: DiffLineKind::Deleted },
            DiffLineMarker { line: 6, kind: DiffLineKind::Modified },
        ];
        let tints = diff_line_tints(3, &markers);
        assert_eq!(tints.len(), 3, "two added lines are one run, the removed line has no tint");
        assert!(tints.iter().all(|t| t.kind == DecorationKind::LineBackground && t.version == 3));
        assert_eq!((tints[0].range.start.line, tints[0].range.end.line, tints[0].color), (0, 1, Some(ColorRole::DiffAdded)));
        assert_eq!((tints[1].range.start.line, tints[1].range.end.line, tints[1].color), (2, 2, Some(ColorRole::DiffModified)));
        assert_eq!((tints[2].range.start.line, tints[2].range.end.line, tints[2].color), (6, 6, Some(ColorRole::DiffModified)));

        let own = compose_rust::code::Decorations::new([compose_rust::code::Decoration::line_background(3, 9, 9, ColorRole::DiffRemoved)]);
        let merged = with_diff_line_tints(own.clone(), 3, &markers).decorations();
        assert_eq!(merged.len(), 4, "the document's own decorations stay first");
        assert_eq!(merged[0].range.start.line, 9);
        assert_eq!(with_diff_line_tints(own.clone(), 3, &[]), own, "no change sends the same list");
    }

    #[test]
    fn tabs_next_and_prev_cycle_and_wrap() {
        let mut tabs = EditorTabs::default();
        assert_eq!(tabs.next_tab(), None);
        assert_eq!(tabs.prev_tab(), None);

        tabs.open("/w/a.rs");
        assert_eq!(tabs.next_tab(), None);
        assert_eq!(tabs.prev_tab(), None);

        tabs.open("/w/b.rs");
        tabs.open("/w/c.rs");
        assert_eq!(tabs.current_path(), Some("/w/c.rs"));

        assert_eq!(tabs.next_tab().as_deref(), Some("/w/a.rs"));
        assert_eq!(tabs.next_tab().as_deref(), Some("/w/b.rs"));
        assert_eq!(tabs.prev_tab().as_deref(), Some("/w/a.rs"));
        assert_eq!(tabs.prev_tab().as_deref(), Some("/w/c.rs"));

        let mut stripes = Stripes::default();
        assert!(is_implemented(Action::NextTab));
        assert!(is_implemented(Action::PrevTab));
        assert!(apply_action(Action::NextTab, &mut tabs, &mut stripes));
        assert_eq!(tabs.current_path(), Some("/w/a.rs"));
        assert!(apply_action(Action::PrevTab, &mut tabs, &mut stripes));
        assert_eq!(tabs.current_path(), Some("/w/c.rs"));
    }

    #[test]
    fn filter_move_sources_drops_same_parent_self_and_descendant() {
        assert_eq!(
            filter_move_sources(vec!["/w/src/a.rs".into()], "/w/src"),
            Vec::<String>::new(),
            "dropping a file into its own parent is a no-op"
        );
        assert_eq!(
            filter_move_sources(vec!["/w/src".into()], "/w/src"),
            Vec::<String>::new(),
            "dropping a folder onto itself is a no-op"
        );
        assert_eq!(
            filter_move_sources(vec!["/w/src".into()], "/w/src/sub"),
            Vec::<String>::new(),
            "dropping a folder into its own descendant is rejected"
        );
        assert_eq!(
            filter_move_sources(vec!["/w/src/a.rs".into(), "/w/docs/b.md".into()], "/w/src"),
            vec!["/w/docs/b.md".to_string()],
            "only items from another directory remain"
        );
    }

    #[test]
    fn rename_prompt_submit_unchanged_name_is_noop() {
        let prompt = TreePrompt::Rename { path: "/w/src/a.rs".into() };
        assert_eq!(
            evaluate_prompt_submit(&prompt, "a.rs", &[]),
            PromptSubmitOutcome::Unchanged
        );
        assert_eq!(
            evaluate_prompt_submit(&prompt, "  a.rs  ", &[]),
            PromptSubmitOutcome::Unchanged
        );
        assert_eq!(
            evaluate_prompt_submit(&prompt, "b.rs", &[]),
            PromptSubmitOutcome::Rename {
                path: "/w/src/a.rs".into(),
                new_name: "b.rs".into(),
            }
        );
    }
}
