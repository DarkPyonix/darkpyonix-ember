//! The Terminal side of a Session pane (FR-L18, FR-P2): a read-only view of a member's terminal.
//!
//! Pure model only: line buffering, the scrollback cap, line colours, the honest states and the
//! demo screen. The widget is `PaneTerminal` in `cr/session.rs`. The text arrives already decoded
//! by the VT emulator (`terminal_stream::TerminalState::lines`), so colouring here is by line
//! shape (as the mockup's `.term` classes), not by SGR codes; [`strip_ansi`] only guards sources
//! that were not decoded.

use std::collections::VecDeque;

use crate::terminal_stream::{TerminalState, TerminalStatus};

/// Scrollback kept for one pane (FR-P2 asks for at least 10,000 lines).
pub const SCROLLBACK_LINES: usize = 10_000;
/// Lines drawn at once; the older ones stay in the buffer. TODO(compose-rust): a lazy list
/// would draw all of them.
pub const RENDER_LINES: usize = 500;

pub const CONNECTING: &str = "터미널에 연결하는 중입니다";
pub const NO_TERMINAL: &str = "이 팀원에게는 볼 수 있는 터미널이 없습니다";
pub const FAILED: &str = "터미널에 연결하지 못했습니다. 잠시 뒤에 다시 시도해 주세요";
pub const DROPPED: &str = "터미널 연결이 끊어졌습니다";
pub const EMPTY: &str = "터미널에 아직 출력이 없습니다";

/// How a line is drawn (the mockup's `.tb .tu .ta .tt .tw .tsel .tadd .tdel .tm`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TermRole {
    Plain,
    Banner,
    User,
    Agent,
    Tool,
    Warn,
    Selected,
    Add,
    Del,
    Muted,
}

/// Remove CSI and OSC escape sequences and bare control characters (except tab).
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\u{1b}' {
            match it.next() {
                Some('[') => {
                    for n in it.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&n) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    while let Some(n) = it.next() {
                        if n == '\u{7}' {
                            break;
                        }
                        if n == '\u{1b}' {
                            it.next();
                            break;
                        }
                    }
                }
                _ => {}
            }
        } else if c == '\t' || c == '\n' || c == '\r' || !c.is_control() {
            out.push(c);
        }
    }
    out
}

/// Colour role of one decoded line.
pub fn classify_line(line: &str) -> TermRole {
    let t = line.trim_start();
    if t.is_empty() {
        return TermRole::Plain;
    }
    if t.starts_with("╭") || t.starts_with("╰") || t.starts_with('│') || t.starts_with(">_") || t.starts_with('▌') {
        return TermRole::Banner;
    }
    if t.starts_with("❯") {
        return TermRole::Selected;
    }
    if t.starts_with("⏺ Bash(") || t.starts_with("⏺ Update(") || t.starts_with("• Ran ") || t.starts_with("• Edited ") {
        return TermRole::Tool;
    }
    if t.starts_with("Allow command?") || t.starts_with("Bash command:") || t.starts_with("Do you want to proceed?") || t.contains("esc to interrupt") {
        return TermRole::Warn;
    }
    if t.starts_with("⏺") || t.starts_with('•') {
        return TermRole::Agent;
    }
    if t.starts_with("> ") || t.starts_with("› ") || t == ">" {
        return TermRole::User;
    }
    if t.starts_with("⎿") || t.starts_with("└") || t.starts_with("? for") || t.starts_with("⏎") {
        return TermRole::Muted;
    }
    if line.starts_with("     +") {
        return TermRole::Add;
    }
    if line.starts_with("     -") {
        return TermRole::Del;
    }
    TermRole::Plain
}

/// Line buffer with a scrollback cap. Text is pushed in chunks; a chunk may end mid-line, and the
/// open line is completed by the next chunk. `\r\n` and `\n` end a line, a lone `\r` returns to
/// the start of the open line.
#[derive(Debug, Clone, Default)]
pub struct ScrollBuffer {
    lines: VecDeque<String>,
    open: String,
    cap: usize,
}

impl ScrollBuffer {
    pub fn new(cap: usize) -> Self {
        Self { lines: VecDeque::new(), open: String::new(), cap: cap.max(1) }
    }

    pub fn push_text(&mut self, chunk: &str) {
        let clean = strip_ansi(&chunk.replace("\r\n", "\n"));
        for c in clean.chars() {
            match c {
                '\n' => {
                    let line = std::mem::take(&mut self.open);
                    self.lines.push_back(line);
                    while self.lines.len() > self.cap {
                        self.lines.pop_front();
                    }
                }
                '\r' => self.open.clear(),
                c => self.open.push(c),
            }
        }
    }

    /// Finished lines plus the open one, if it has text.
    pub fn all(&self) -> Vec<String> {
        let mut v: Vec<String> = self.lines.iter().cloned().collect();
        if !self.open.is_empty() {
            v.push(self.open.clone());
        }
        v
    }

    pub fn len(&self) -> usize {
        self.lines.len() + usize::from(!self.open.is_empty())
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// What the Terminal toggle shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TermView {
    Connecting,
    /// The member has no terminal stream to attach to.
    NoTerminal,
    /// A Korean sentence, never the transport's text.
    Failed(&'static str),
    Lines { rows: Vec<(TermRole, String)>, notice: Option<String> },
}

/// The last `max` lines, each with its role. Older lines stay in the buffer.
pub fn render_rows(lines: &[String], max: usize) -> Vec<(TermRole, String)> {
    let start = lines.len().saturating_sub(max);
    lines[start..].iter().map(|l| (classify_line(l), strip_ansi(l))).collect()
}

/// Map the stream state to what the pane shows. The raw `error` text is dropped on purpose.
pub fn view_from_state(state: &TerminalState) -> TermView {
    if state.error.is_some() {
        return TermView::Failed(FAILED);
    }
    match state.status {
        TerminalStatus::Connecting if state.lines.is_empty() => TermView::Connecting,
        TerminalStatus::Disconnected if state.lines.is_empty() && state.exit.is_none() => {
            TermView::Failed(DROPPED)
        }
        _ => {
            let mut lines = state.lines.clone();
            if lines.is_empty() {
                lines.push(EMPTY.to_owned());
            }
            let notice = state.exit_text().or_else(|| {
                (state.status == TerminalStatus::Disconnected).then(|| DROPPED.to_owned())
            });
            TermView::Lines { rows: render_rows(&lines, RENDER_LINES), notice }
        }
    }
}

/// Which view a member's Terminal toggle gets. Without a stream for the member (`state` is
/// `None`) outside the demo there is no terminal, and the pane says so.
pub fn view_for_member(
    demo: bool,
    state: Option<&TerminalState>,
    agent: &str,
    model: &str,
    project: &str,
    task: &str,
) -> TermView {
    if demo {
        return TermView::Lines {
            rows: render_rows(&demo_lines(agent, model, project, task), RENDER_LINES),
            notice: None,
        };
    }
    match state {
        Some(s) => view_from_state(s),
        None => TermView::NoTerminal,
    }
}

/// Sample screen for `EMBER_DEMO_DATA`, shaped like the mockup's `termHtml`.
pub fn demo_lines(agent: &str, model: &str, project: &str, task: &str) -> Vec<String> {
    let dir = format!("~/{project}");
    let a = agent.to_lowercase();
    let mut v = Vec::new();
    if a.contains("codex") {
        v.push(">_ OpenAI Codex".to_owned());
        v.push(format!("model: {model}   directory: {dir}"));
        v.push(String::new());
        v.push(format!("› {task}"));
        v.push("• Ran cargo test --locked".to_owned());
        v.push("  └ test result: ok. 42 passed".to_owned());
        v.push("• 테스트가 모두 통과했습니다.".to_owned());
        v.push(String::new());
        v.push("▌ Ask Codex to do anything".to_owned());
        v.push("  ⏎ send   ⌃J newline   ⌃C quit".to_owned());
    } else {
        let name = if a.contains("gemini") { "Gemini CLI" } else { "Claude Code" };
        v.push("╭────────────────────────────────────╮".to_owned());
        v.push(format!("│ ✻ Welcome to {name}"));
        v.push(format!("│   cwd: {dir}"));
        v.push("╰────────────────────────────────────╯".to_owned());
        v.push(String::new());
        v.push(format!("> {task}"));
        v.push("⏺ Bash(cargo test --locked)".to_owned());
        v.push("  ⎿  test result: ok. 42 passed".to_owned());
        v.push("⏺ 테스트가 모두 통과했습니다.".to_owned());
        v.push(String::new());
        v.push("> ".to_owned());
        v.push("  ? for shortcuts".to_owned());
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(status: TerminalStatus, lines: &[&str]) -> TerminalState {
        TerminalState { status, lines: lines.iter().map(|s| s.to_string()).collect(), ..Default::default() }
    }

    #[test]
    fn fr_l18_terminal_buffer_joins_partial_lines() {
        let mut b = ScrollBuffer::new(100);
        b.push_text("hel");
        b.push_text("lo\r\nwor");
        assert_eq!(b.all(), vec!["hello", "wor"]);
        b.push_text("ld\n");
        assert_eq!(b.all(), vec!["hello", "world"]);
    }

    #[test]
    fn fr_l18_terminal_buffer_carriage_return_overwrites_open_line() {
        let mut b = ScrollBuffer::new(100);
        b.push_text("10%\r50%\r100%\n");
        assert_eq!(b.all(), vec!["100%"]);
    }

    #[test]
    fn fr_l18_terminal_scrollback_cap_drops_oldest() {
        let mut b = ScrollBuffer::new(3);
        for i in 0..10 {
            b.push_text(&format!("l{i}\n"));
        }
        assert_eq!(b.all(), vec!["l7", "l8", "l9"]);
        assert!(SCROLLBACK_LINES >= 10_000);
    }

    #[test]
    fn fr_l18_terminal_strips_ansi() {
        assert_eq!(strip_ansi("\u{1b}[31mred\u{1b}[0m ok\u{1b}]0;title\u{7}"), "red ok");
        let mut b = ScrollBuffer::new(5);
        b.push_text("\u{1b}[1mbold\u{1b}[0m\n");
        assert_eq!(b.all(), vec!["bold"]);
    }

    #[test]
    fn fr_l18_terminal_line_roles() {
        assert_eq!(classify_line("╭──╮"), TermRole::Banner);
        assert_eq!(classify_line("> fix it"), TermRole::User);
        assert_eq!(classify_line("⏺ Bash(ls)"), TermRole::Tool);
        assert_eq!(classify_line("⏺ done"), TermRole::Agent);
        assert_eq!(classify_line("  ⎿  out"), TermRole::Muted);
        assert_eq!(classify_line("  ❯ 1. Yes"), TermRole::Selected);
        assert_eq!(classify_line("     +new"), TermRole::Add);
        assert_eq!(classify_line("     -old"), TermRole::Del);
        assert_eq!(classify_line("plain"), TermRole::Plain);
    }

    #[test]
    fn fr_l18_terminal_render_rows_keep_the_tail() {
        let lines: Vec<String> = (0..10).map(|i| format!("l{i}")).collect();
        let rows = render_rows(&lines, 3);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].1, "l7");
    }

    #[test]
    fn fr_l18_terminal_connecting_state() {
        let v = view_from_state(&state(TerminalStatus::Connecting, &[]));
        assert_eq!(v, TermView::Connecting);
    }

    #[test]
    fn fr_l18_terminal_failure_never_shows_raw_error() {
        let mut s = state(TerminalStatus::Disconnected, &[]);
        s.error = Some("connection refused (os error 61)".into());
        assert_eq!(view_from_state(&s), TermView::Failed(FAILED));
        assert!(!FAILED.contains("refused"));
    }

    #[test]
    fn fr_l18_terminal_lines_and_exit_notice() {
        let mut s = state(TerminalStatus::Connected, &["$ ls", "a b"]);
        s.exit = Some((Some(0), None));
        match view_from_state(&s) {
            TermView::Lines { rows, notice } => {
                assert_eq!(rows.len(), 2);
                assert!(notice.unwrap().contains("종료"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn fr_l18_terminal_no_stream_outside_demo_says_so() {
        assert_eq!(view_for_member(false, None, "claude", "m", "p", "t"), TermView::NoTerminal);
    }

    #[test]
    fn fr_l18_terminal_demo_shows_sample_output() {
        match view_for_member(true, None, "codex", "gpt", "ember", "고쳐 줘") {
            TermView::Lines { rows, .. } => assert!(rows.iter().any(|(_, l)| l.contains("고쳐 줘"))),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn fr_l18_terminal_strings_are_clean() {
        for s in [CONNECTING, NO_TERMINAL, FAILED, DROPPED, EMPTY] {
            assert!(!s.contains('\u{2014}') && !s.contains("준비 중"));
        }
    }
}
