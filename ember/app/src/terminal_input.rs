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

impl MultiLinePasteWarning {
    /// The setting's value. Code-OSS still takes the old boolean: `true` is `auto`, `false` is
    /// `never` (`terminalConfiguration.ts` migration).
    pub fn from_setting(value: Option<&serde_json::Value>) -> Self {
        match value {
            Some(serde_json::Value::String(v)) if v == "always" => Self::Always,
            Some(serde_json::Value::String(v)) if v == "never" => Self::Never,
            Some(serde_json::Value::Bool(false)) => Self::Never,
            _ => Self::Auto,
        }
    }
}

/// What a paste does with the clipboard's text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PastePlan {
    /// Send these bytes now.
    Send(Vec<u8>),
    /// Ask first ([`multiline_warning_text`]); the text waits for the answer.
    Ask(String),
}

/// The plan for pasting `text` (Code-OSS `terminalInstance.paste`): ask when the warning applies,
/// else send it, bracketed while the program asked for bracketed paste.
pub fn plan_paste(text: &str, setting: MultiLinePasteWarning, bracketed: bool) -> PastePlan {
    if needs_multiline_warning(text, setting, bracketed) {
        PastePlan::Ask(text.to_owned())
    } else {
        PastePlan::Send(paste_bytes(text, bracketed))
    }
}

/// The notice when the clipboard holds more text than one read may carry: nothing is pasted,
/// rather than a part of it.
pub const PASTE_TOO_LONG: &str = "클립보드의 텍스트가 너무 길어 붙여넣지 않았습니다.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr_l31_21_paste_sends_or_asks_as_code_oss_does() {
        use serde_json::json;
        let auto = MultiLinePasteWarning::from_setting(None);
        assert_eq!(auto, MultiLinePasteWarning::Auto);
        assert_eq!(MultiLinePasteWarning::from_setting(Some(&json!("always"))), MultiLinePasteWarning::Always);
        assert_eq!(MultiLinePasteWarning::from_setting(Some(&json!("never"))), MultiLinePasteWarning::Never);
        assert_eq!(MultiLinePasteWarning::from_setting(Some(&json!(false))), MultiLinePasteWarning::Never);
        assert_eq!(MultiLinePasteWarning::from_setting(Some(&json!(true))), MultiLinePasteWarning::Auto);
        assert_eq!(plan_paste("ls -la", auto, false), PastePlan::Send(b"ls -la".to_vec()));
        assert_eq!(plan_paste("make\n", auto, false), PastePlan::Send(b"make\r".to_vec()));
        assert_eq!(plan_paste("a\nb", auto, false), PastePlan::Ask("a\nb".into()));
        assert_eq!(plan_paste("a\nb", auto, true), PastePlan::Send(b"\x1b[200~a\rb\x1b[201~".to_vec()));
        assert_eq!(plan_paste("a\nb", MultiLinePasteWarning::Always, true), PastePlan::Ask("a\nb".into()));
        assert_eq!(plan_paste("a\nb", MultiLinePasteWarning::Never, false), PastePlan::Send(b"a\rb".to_vec()));
    }

    #[test]
    fn fr_l31_21_the_terminal_paste_chord_reads_the_clipboard() {
        // The chord in the panel reads the clipboard and follows the plan; it no longer only
        // posts a notice.
        let page = include_str!("ui/editor_workbench.rs");
        let chord = &page[page.find("is_paste_chord(&name").unwrap()..];
        let chord = &chord[..chord.find("return;").unwrap()];
        assert!(chord.contains("compose_rust::clipboard::read_text"), "{chord}");
        assert!(page.contains("plan_paste("));
        assert!(page.contains("WARN_SINGLE_LINE") && page.contains("WARN_CANCEL"), "no multi-line confirmation");
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
