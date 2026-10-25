//! The editor keymap (FR-L30, `docs/INTENT.md` D32): VS Code or JetBrains.
//!
//! One table maps every action to a key chord per keymap and per OS (the primary modifier is
//! Command on macOS and Ctrl elsewhere). The hints the editor shows and the dispatch of a
//! pressed chord both read this table, so no hint names a key the keymap does not handle. The
//! defaults are each product's real ones (the mockup's `editor.js` table).
//!
//! The pinned dioxus-compose reports only `Key::Enter` in a key event, so chords cannot be
//! matched against real key presses yet. [`Keymap::action_for`] is the dispatch the editor will
//! call once the toolkit reports letters and function keys; until then only the hints show.
//! TODO(#166: 단축키 이벤트): wire `on_key_down` to [`Keymap::action_for`].

use serde::{Deserialize, Serialize};

/// The user's choice. Default VS Code (FR-L30).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Keymap {
    #[default]
    VsCode,
    JetBrains,
}

/// Which modifier set the chords are written for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Os {
    Mac,
    Other,
}

impl Os {
    pub fn current() -> Os {
        if cfg!(target_os = "macos") {
            Os::Mac
        } else {
            Os::Other
        }
    }
}

/// Everything a chord can trigger in the editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    Project,
    Commit,
    Git,
    Run,
    Problems,
    Terminal,
    Close,
    Save,
    Find,
    Recent,
    Palette,
    RunConfig,
    Stop,
    GotoLine,
    Format,
    HideTool,
    Ask,
    NewTerminal,
    MaximizeTool,
}

impl Action {
    pub const ALL: [Action; 19] = [
        Action::Project,
        Action::Commit,
        Action::Git,
        Action::Run,
        Action::Problems,
        Action::Terminal,
        Action::Close,
        Action::Save,
        Action::Find,
        Action::Recent,
        Action::Palette,
        Action::RunConfig,
        Action::Stop,
        Action::GotoLine,
        Action::Format,
        Action::HideTool,
        Action::Ask,
        Action::NewTerminal,
        Action::MaximizeTool,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Action::Project => "프로젝트",
            Action::Commit => "커밋",
            Action::Git => "Git",
            Action::Run => "실행",
            Action::Problems => "문제",
            Action::Terminal => "터미널",
            Action::Close => "탭 닫기",
            Action::Save => "저장",
            Action::Find => "파일, 동작, 기호 찾기",
            Action::Recent => "최근 파일",
            Action::Palette => "명령 팔레트",
            Action::RunConfig => "다시 실행",
            Action::Stop => "중지",
            Action::GotoLine => "줄로 이동",
            Action::Format => "코드 정리",
            Action::HideTool => "숨기기",
            Action::Ask => "에이전트에게 묻기",
            Action::NewTerminal => "새 터미널",
            Action::MaximizeTool => "최대화",
        }
    }
}

/// A key chord in one OS's notation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Chord {
    Keys {
        ctrl: bool,
        shift: bool,
        alt: bool,
        /// Command on macOS.
        meta: bool,
        /// `P`, `F12`, `` ` ``, `Esc`.
        key: String,
    },
    /// Shift pressed twice (JetBrains search everywhere).
    DoubleShift,
}

impl Chord {
    /// Parse a chord written like the table: `⇧⌘P` on macOS, `Ctrl+Shift+P` elsewhere.
    /// An empty string is no chord.
    pub fn parse(os: Os, text: &str) -> Option<Chord> {
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        if text == "\u{21E7}\u{21E7}" || text == "Shift Shift" {
            return Some(Chord::DoubleShift);
        }
        let (mut ctrl, mut shift, mut alt, mut meta) = (false, false, false, false);
        let key;
        match os {
            Os::Mac => {
                let mut rest = text;
                while let Some(c) = rest.chars().next() {
                    match c {
                        '\u{2318}' => meta = true,
                        '\u{2303}' => ctrl = true,
                        '\u{2325}' => alt = true,
                        '\u{21E7}' => shift = true,
                        _ => break,
                    }
                    rest = &rest[c.len_utf8()..];
                }
                key = if rest == "\u{238B}" { "Esc" } else { rest };
            }
            Os::Other => {
                let parts: Vec<&str> = text.split('+').collect();
                let (last, mods) = parts.split_last()?;
                for m in mods {
                    match *m {
                        "Ctrl" => ctrl = true,
                        "Shift" => shift = true,
                        "Alt" => alt = true,
                        "Meta" | "Win" => meta = true,
                        _ => return None,
                    }
                }
                key = last;
            }
        }
        if key.is_empty() {
            return None;
        }
        Some(Chord::Keys {
            ctrl,
            shift,
            alt,
            meta,
            key: key.to_string(),
        })
    }

    /// The chord as the OS writes it: `⌃⇧⌥⌘` order on macOS, `Ctrl+Shift+Alt` elsewhere.
    pub fn display(&self, os: Os) -> String {
        match self {
            Chord::DoubleShift => match os {
                Os::Mac => "\u{21E7}\u{21E7}".to_string(),
                Os::Other => "Shift Shift".to_string(),
            },
            Chord::Keys {
                ctrl,
                shift,
                alt,
                meta,
                key,
            } => match os {
                Os::Mac => {
                    let mut out = String::new();
                    for (on, sym) in [
                        (ctrl, '\u{2303}'),
                        (shift, '\u{21E7}'),
                        (alt, '\u{2325}'),
                        (meta, '\u{2318}'),
                    ] {
                        if *on {
                            out.push(sym);
                        }
                    }
                    out.push_str(if key == "Esc" { "\u{238B}" } else { key });
                    out
                }
                Os::Other => {
                    let mut parts: Vec<&str> = Vec::new();
                    for (on, name) in [
                        (ctrl, "Ctrl"),
                        (shift, "Shift"),
                        (alt, "Alt"),
                        (meta, "Meta"),
                    ] {
                        if *on {
                            parts.push(name);
                        }
                    }
                    parts.push(key);
                    parts.join("+")
                }
            },
        }
    }
}

/// `(action, macOS chord, other chord)`; an empty chord means no default binding.
type Row = (Action, &'static str, &'static str);

const VSCODE: [Row; 19] = [
    (Action::Project, "\u{21E7}\u{2318}E", "Ctrl+Shift+E"),
    // VS Code has no default for a commit window; the source control view shares Git's chord.
    (Action::Commit, "", ""),
    (Action::Git, "\u{2303}\u{21E7}G", "Ctrl+Shift+G"),
    (Action::Run, "\u{21E7}\u{2318}U", "Ctrl+Shift+U"),
    (Action::Problems, "\u{21E7}\u{2318}M", "Ctrl+Shift+M"),
    (Action::Terminal, "\u{2303}`", "Ctrl+`"),
    (Action::Close, "\u{2318}W", "Ctrl+W"),
    (Action::Save, "\u{2318}S", "Ctrl+S"),
    (Action::Find, "\u{2318}P", "Ctrl+P"),
    (Action::Recent, "\u{2303}R", "Ctrl+R"),
    (Action::Palette, "\u{21E7}\u{2318}P", "Ctrl+Shift+P"),
    (Action::RunConfig, "\u{2303}F5", "Ctrl+F5"),
    (Action::Stop, "\u{21E7}F5", "Shift+F5"),
    (Action::GotoLine, "\u{2303}G", "Ctrl+G"),
    (Action::Format, "\u{21E7}\u{2325}F", "Shift+Alt+F"),
    (Action::HideTool, "\u{2318}J", "Ctrl+J"),
    (Action::Ask, "\u{2303}\u{2318}I", "Ctrl+Alt+I"),
    (Action::NewTerminal, "\u{2303}\u{21E7}`", "Ctrl+Shift+`"),
    (Action::MaximizeTool, "", ""),
];

const JETBRAINS: [Row; 19] = [
    (Action::Project, "\u{2318}1", "Alt+1"),
    (Action::Commit, "\u{2318}0", "Alt+0"),
    (Action::Git, "\u{2318}9", "Alt+9"),
    (Action::Run, "\u{2318}4", "Alt+4"),
    (Action::Problems, "\u{2318}6", "Alt+6"),
    (Action::Terminal, "\u{2325}F12", "Alt+F12"),
    (Action::Close, "\u{2318}W", "Ctrl+F4"),
    (Action::Save, "\u{2318}S", "Ctrl+S"),
    (Action::Find, "\u{21E7}\u{21E7}", "Shift Shift"),
    (Action::Recent, "\u{2318}E", "Ctrl+E"),
    (Action::Palette, "\u{21E7}\u{2318}A", "Ctrl+Shift+A"),
    (Action::RunConfig, "\u{2303}R", "Shift+F10"),
    (Action::Stop, "\u{2318}F2", "Ctrl+F2"),
    (Action::GotoLine, "\u{2318}L", "Ctrl+G"),
    (Action::Format, "\u{2325}\u{2318}L", "Ctrl+Alt+L"),
    (Action::HideTool, "\u{21E7}\u{238B}", "Shift+Esc"),
    (Action::Ask, "", ""),
    (Action::NewTerminal, "\u{2318}T", "Ctrl+Shift+T"),
    (Action::MaximizeTool, "\u{21E7}\u{2318}'", "Ctrl+Shift+'"),
];

impl Keymap {
    pub const ALL: [Keymap; 2] = [Keymap::VsCode, Keymap::JetBrains];

    pub fn label(self) -> &'static str {
        match self {
            Keymap::VsCode => "VS Code",
            Keymap::JetBrains => "JetBrains",
        }
    }

    /// The name of the "find anything" action in this keymap's own words.
    pub fn find_label(self) -> &'static str {
        match self {
            Keymap::VsCode => "파일로 이동",
            Keymap::JetBrains => "모든 곳에서 찾기",
        }
    }

    pub fn palette_label(self) -> &'static str {
        match self {
            Keymap::VsCode => "명령 팔레트",
            Keymap::JetBrains => "동작 찾기",
        }
    }

    fn table(self) -> &'static [Row; 19] {
        match self {
            Keymap::VsCode => &VSCODE,
            Keymap::JetBrains => &JETBRAINS,
        }
    }

    /// The chord bound to `action`, if the keymap binds one on this OS.
    pub fn chord(self, action: Action, os: Os) -> Option<Chord> {
        let row = self.table().iter().find(|r| r.0 == action)?;
        Chord::parse(os, if os == Os::Mac { row.1 } else { row.2 })
    }

    /// The hint text for `action` (empty when unbound), read from the same table as dispatch.
    pub fn hint(self, action: Action, os: Os) -> String {
        self.chord(action, os)
            .map(|c| c.display(os))
            .unwrap_or_default()
    }

    /// The action a pressed chord triggers in this keymap.
    pub fn action_for(self, chord: &Chord, os: Os) -> Option<Action> {
        Action::ALL
            .into_iter()
            .find(|a| self.chord(*a, os).as_ref() == Some(chord))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_action_appears_once_per_table() {
        for keymap in Keymap::ALL {
            for a in Action::ALL {
                assert_eq!(
                    keymap.table().iter().filter(|r| r.0 == a).count(),
                    1,
                    "{a:?}"
                );
            }
        }
    }
}
