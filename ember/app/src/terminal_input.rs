//! Paste and IME input for the editor's terminal panel (FR-L31.21, #335).
//!
//! Behaviour follows Code-OSS 1.139's integrated terminal (xterm.js and
//! `terminalInstance.ts`), because the editor's functions equal Code-OSS (D38):
//!
//! - The paste chord is ⌘V or Ctrl+Shift+V. Both are accepted on every OS, because one UI runs on
//!   every OS (no OS branch); Ctrl+V alone stays the shell's (literal next character).
//! - Pasted text has its line breaks turned into `\r` (xterm.js `prepareTextForTerminal`) and, while
//!   the program enabled bracketed paste (`ESC[?2004h`), is wrapped in `ESC[200~` and `ESC[201~`
//!   (xterm.js `bracketTextForPaste`).
//! - `terminal.integrated.enableMultiLinePasteWarning` (`auto` by default) asks before a multi-line
//!   paste; `auto` skips the question when bracketed paste is on or the text is one line plus a
//!   trailing line break.
//! - IME: only committed text (`compositionend`) reaches the PTY. The preedit of
//!   `compositionstart` and `compositionupdate`, and key presses while a composition is open,
//!   are not sent, as xterm.js's `CompositionHelper` does.
//!
//! Pure, so it is tested without a window.

use crate::terminal_stream::text_bytes;

/// Bracketed paste start and end markers.
pub const PASTE_START: &[u8] = b"\x1b[200~";
pub const PASTE_END: &[u8] = b"\x1b[201~";

/// True when the key press is the terminal's paste chord: ⌘V, or Ctrl+Shift+V.
pub fn is_paste_chord(name: &str, ctrl: bool, alt: bool, shift: bool, meta: bool) -> bool {
    if alt || !name.eq_ignore_ascii_case("v") {
        return false;
    }
    (meta && !ctrl && !shift) || (ctrl && shift && !meta)
}

/// Bytes for pasted text. Line breaks become `\r`; with `bracketed` the text is wrapped in the
/// bracketed paste markers. An end marker inside the text is removed so the paste cannot end the
/// bracket early and run what follows as typed commands.
pub fn paste_bytes(text: &str, bracketed: bool) -> Vec<u8> {
    if !bracketed {
        return text_bytes(text);
    }
    let inner = text.replace("\x1b[201~", "");
    let mut out = PASTE_START.to_vec();
    out.extend_from_slice(&text_bytes(&inner));
    out.extend_from_slice(PASTE_END);
    out
}

/// `terminal.integrated.enableMultiLinePasteWarning`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MultiLinePasteWarning {
    /// Warn, except with bracketed paste on or for one line plus a trailing line break.
    #[default]
    Auto,
    Always,
    Never,
}

/// The lines the warning counts, split on `\r\n`, `\n` or `\r`.
fn lines(text: &str) -> Vec<&str> {
    text.split("\r\n").flat_map(|l| l.split('\n')).flat_map(|l| l.split('\r')).collect()
}

/// True when pasting `text` should first ask (Code-OSS `shouldPasteTerminalText`).
pub fn needs_multiline_warning(text: &str, setting: MultiLinePasteWarning, bracketed: bool) -> bool {
    let lines = lines(text);
    if setting == MultiLinePasteWarning::Never || lines.len() <= 1 {
        return false;
    }
    if setting == MultiLinePasteWarning::Auto {
        if bracketed {
            return false;
        }
        if lines.len() == 2 && lines[1].trim().is_empty() {
            return false;
        }
    }
    true
}

/// The question the warning asks.
pub fn multiline_warning_text(text: &str) -> String {
    format!("터미널에 텍스트 {}줄을 붙여넣으시겠습니까?", lines(text).len())
}

/// Buttons of the warning, in Code-OSS's order.
pub const WARN_PASTE: &str = "붙여넣기";
pub const WARN_SINGLE_LINE: &str = "한 줄로 붙여넣기";
pub const WARN_CANCEL: &str = "취소";

/// What the user chose in the warning.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PasteChoice {
    Paste,
    /// Code-OSS's "Paste as one line": line breaks are removed.
    SingleLine,
    Cancel,
}

/// The text to send for a choice, or `None` for cancel.
pub fn resolve_paste(text: &str, choice: PasteChoice) -> Option<String> {
    match choice {
        PasteChoice::Paste => Some(text.to_owned()),
        PasteChoice::SingleLine => Some(text.replace(['\r', '\n'], "")),
        PasteChoice::Cancel => None,
    }
}

/// IME composition filter: only committed text goes to the PTY.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImeInput {
    composing: bool,
}

impl ImeInput {
    /// True while a composition is open.
    pub fn composing(&self) -> bool {
        self.composing
    }

    /// `compositionstart`: nothing is sent.
    pub fn start(&mut self) {
        self.composing = true;
    }

    /// `compositionupdate` carries the preedit, which is never sent.
    pub fn update(&mut self, _preedit: &str) {
        self.composing = true;
    }

    /// `compositionend` carries the committed text; its bytes are returned (empty when the
    /// composition was cancelled).
    pub fn end(&mut self, committed: &str) -> Vec<u8> {
        self.composing = false;
        text_bytes(committed)
    }

    /// True when a key press must be dropped: a composition is open, the toolkit flags the
    /// event as composing, or the key is the IME's `Process` placeholder.
    pub fn drops_key(&self, name: &str, is_composing: bool) -> bool {
        self.composing || is_composing || name == "Process"
    }
}

/// Result of handling a clipboard read for a terminal paste chord (FR-L31.21).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PasteAction {
    /// Send these bytes to the terminal immediately.
    Send(Vec<u8>),
    /// Ask the user to confirm before pasting this multi-line text.
    Confirm(String),
    /// Nothing to paste (empty clipboard text).
    Empty,
}

/// Decides whether pasted clipboard text goes straight to the terminal or waits for multi-line
/// confirmation (FR-L31.21).
pub fn plan_paste(text: &str, setting: MultiLinePasteWarning, bracketed: bool) -> PasteAction {
    if text.is_empty() {
        return PasteAction::Empty;
    }
    if needs_multiline_warning(text, setting, bracketed) {
        PasteAction::Confirm(text.to_owned())
    } else {
        PasteAction::Send(paste_bytes(text, bracketed))
    }
}

/// Pending multi-line paste confirmation state in the editor terminal panel (FR-L31.21).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PendingPaste {
    text: Option<String>,
}

impl PendingPaste {
    /// The text waiting for confirmation, if any.
    pub fn text(&self) -> Option<&str> {
        self.text.as_deref()
    }

    /// True while a multi-line paste confirmation is open.
    pub fn is_open(&self) -> bool {
        self.text.is_some()
    }

    /// Stores `text` for confirmation, replacing any earlier pending paste.
    pub fn ask(&mut self, text: String) {
        self.text = Some(text);
    }

    /// Resolves the pending paste with `choice`, clearing the pending state and returning the bytes
    /// to send to the terminal (or `None` when cancelled or nothing was pending).
    pub fn decide(&mut self, choice: PasteChoice, bracketed: bool) -> Option<Vec<u8>> {
        let pending = self.text.take()?;
        let resolved = resolve_paste(&pending, choice)?;
        Some(paste_bytes(&resolved, bracketed))
    }
}

/// Korean notice when clipboard text exceeds the toolkit's single-read limit
/// (`compose_rust::clipboard::MAX_TEXT_BYTES`).
pub const PASTE_TOO_LONG: &str = "클립보드 텍스트가 너무 길어 붙여넣을 수 없습니다.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr_l31_21_single_line_pastes_immediately_and_multiline_confirms() {
        // Single-line text sends bytes directly without asking.
        assert_eq!(plan_paste("ls -la", MultiLinePasteWarning::Auto, false), PasteAction::Send(b"ls -la".to_vec()));
        assert_eq!(plan_paste("ls\n", MultiLinePasteWarning::Auto, false), PasteAction::Send(b"ls\r".to_vec()));
        assert_eq!(plan_paste("", MultiLinePasteWarning::Auto, false), PasteAction::Empty);
        // Multi-line text without bracketed paste asks for confirmation.
        assert_eq!(
            plan_paste("echo a\necho b", MultiLinePasteWarning::Auto, false),
            PasteAction::Confirm("echo a\necho b".to_string()),
        );
        // With bracketed paste on in Auto mode, multi-line text sends bracketed bytes directly.
        assert_eq!(
            plan_paste("echo a\necho b", MultiLinePasteWarning::Auto, true),
            PasteAction::Send(b"\x1b[200~echo a\recho b\x1b[201~".to_vec()),
        );
        // Notice for over-long clipboard text is in Korean 합니다체.
        assert!(PASTE_TOO_LONG.ends_with("없습니다."), "{PASTE_TOO_LONG}");
    }

    #[test]
    fn fr_l31_21_pending_paste_state_transitions() {
        let mut pending = PendingPaste::default();
        assert!(!pending.is_open());
        assert_eq!(pending.text(), None);
        assert_eq!(pending.decide(PasteChoice::Paste, false), None);

        // Confirm as multi-line: sends bytes and clears state.
        pending.ask("a\nb".to_string());
        assert!(pending.is_open());
        assert_eq!(pending.text(), Some("a\nb"));
        assert_eq!(pending.decide(PasteChoice::Paste, false), Some(b"a\rb".to_vec()));
        assert!(!pending.is_open());

        // Confirm as single-line: strips line breaks, sends bytes, clears state.
        pending.ask("a\r\nb".to_string());
        assert_eq!(pending.decide(PasteChoice::SingleLine, true), Some(b"\x1b[200~ab\x1b[201~".to_vec()));
        assert!(!pending.is_open());

        // Cancel: sends nothing and clears state.
        pending.ask("x\ny".to_string());
        assert_eq!(pending.decide(PasteChoice::Cancel, false), None);
        assert!(!pending.is_open());
    }

    #[test]
    fn fr_l31_21_paste_chord_is_cmd_v_or_ctrl_shift_v() {
        assert!(is_paste_chord("v", false, false, false, true));
        assert!(is_paste_chord("V", true, false, true, false));
        assert!(!is_paste_chord("v", true, false, false, false), "Ctrl+V stays the shell's");
        assert!(!is_paste_chord("v", false, true, false, true));
        assert!(!is_paste_chord("c", false, false, false, true));
    }

    #[test]
    fn fr_l31_21_bracketed_paste_wraps_only_when_enabled() {
        assert_eq!(paste_bytes("ls\n", false), b"ls\r".to_vec());
        assert_eq!(paste_bytes("a\r\nb", true), b"\x1b[200~a\rb\x1b[201~".to_vec());
        assert_eq!(paste_bytes("x\x1b[201~rm", true), b"\x1b[200~xrm\x1b[201~".to_vec());
    }

    #[test]
    fn fr_l31_21_session_tracks_bracketed_paste_mode() {
        use crate::terminal_stream::{TermEvent, TerminalSession};
        let mut s = TerminalSession::new();
        assert!(!s.state.bracketed_paste);
        s.apply_event(TermEvent::Output { data: b"\x1b[?2004h$ ".to_vec() });
        assert!(s.state.bracketed_paste);
        s.apply_event(TermEvent::Output { data: b"\x1b[?2004l".to_vec() });
        assert!(!s.state.bracketed_paste);
    }

    #[test]
    fn fr_l31_21_multiline_warning_rule() {
        use MultiLinePasteWarning::*;
        assert!(!needs_multiline_warning("ls", Always, false));
        assert!(needs_multiline_warning("a\nb", Auto, false));
        assert!(!needs_multiline_warning("a\nb", Auto, true), "bracketed paste skips it in auto");
        assert!(!needs_multiline_warning("ls\n", Auto, false), "one line plus a trailing break");
        assert!(needs_multiline_warning("ls\n", Always, false));
        assert!(needs_multiline_warning("a\nb", Always, true));
        assert!(!needs_multiline_warning("a\nb\nc", Never, false));
        assert_eq!(MultiLinePasteWarning::default(), Auto);
        assert_eq!(multiline_warning_text("a\r\nb\nc"), "터미널에 텍스트 3줄을 붙여넣으시겠습니까?");
        assert_eq!(resolve_paste("a\r\nb", PasteChoice::SingleLine).as_deref(), Some("ab"));
        assert_eq!(resolve_paste("a\nb", PasteChoice::Paste).as_deref(), Some("a\nb"));
        assert_eq!(resolve_paste("a\nb", PasteChoice::Cancel), None);
    }

    #[test]
    fn fr_l31_21_ime_sends_committed_text_only() {
        let mut ime = ImeInput::default();
        ime.start();
        ime.update("ㅎ");
        ime.update("하");
        assert!(ime.composing());
        assert!(ime.drops_key("ㅎ", false), "keys during a composition are not sent");
        assert_eq!(ime.end("한"), "한".as_bytes().to_vec());
        assert!(!ime.composing());
        assert!(!ime.drops_key("a", false));
        assert!(ime.drops_key("Process", false));
        assert!(ime.drops_key("a", true));
        ime.start();
        assert!(ime.end("").is_empty(), "a cancelled composition sends nothing");
    }
}
