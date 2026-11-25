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
    /// Find (and replace) in the open file.
    FindInFile,
    /// Search across the project's files (FR-E7).
    FindInFiles,
    /// Search across the project's files with the replace field shown (FR-E7).
    ReplaceInFiles,
    /// Reopen the editor closed last (FR-L31.15).
    ReopenClosed,
    /// View: Zoom In, Zoom Out, Reset Zoom (FR-L31.15).
    ZoomIn,
    ZoomOut,
    ZoomReset,
    /// Show the extensions window (FR-L31.14).
    Extensions,
    /// The settings view (FR-L31.17).
    Settings,
    /// The user's settings.json as a document (FR-L31.17).
    SettingsJson,
    /// The user's keybindings.json as a document (FR-L31.17).
    KeybindingsJson,
    /// Switch to the next open editor tab.
    NextTab,
    /// Switch to the previous open editor tab.
    PrevTab,
}

impl Action {
    pub const ALL: [Action; 32] = [
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
        Action::FindInFile,
        Action::FindInFiles,
        Action::ReplaceInFiles,
        Action::ReopenClosed,
        Action::ZoomIn,
        Action::ZoomOut,
        Action::ZoomReset,
        Action::Extensions,
        Action::Settings,
        Action::SettingsJson,
        Action::KeybindingsJson,
        Action::NextTab,
        Action::PrevTab,
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
            Action::FindInFile => "문서에서 찾기",
            Action::FindInFiles => "파일에서 찾기",
            Action::ReplaceInFiles => "파일에서 바꾸기",
            Action::ReopenClosed => "닫은 편집기 다시 열기",
            Action::ZoomIn => "확대",
            Action::ZoomOut => "축소",
            Action::ZoomReset => "원래 크기",
            Action::Extensions => "확장",
            Action::Settings => "설정",
            Action::SettingsJson => "사용자 설정 열기 (JSON)",
            Action::KeybindingsJson => "키보드 단축키 열기 (JSON)",
            Action::NextTab => "다음 탭",
            Action::PrevTab => "이전 탭",
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

const VSCODE: [Row; 32] = [
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
    (Action::FindInFile, "\u{2318}F", "Ctrl+F"),
    (Action::FindInFiles, "\u{21E7}\u{2318}F", "Ctrl+Shift+F"),
    (Action::ReplaceInFiles, "\u{21E7}\u{2318}H", "Ctrl+Shift+H"),
    (Action::ReopenClosed, "\u{21E7}\u{2318}T", "Ctrl+Shift+T"),
    (Action::ZoomIn, "\u{2318}=", "Ctrl+="),
    (Action::ZoomOut, "\u{2318}-", "Ctrl+-"),
    (Action::ZoomReset, "\u{2318}0", "Ctrl+0"),
    // Code-OSS: workbench.view.extensions.
    (Action::Extensions, "\u{21E7}\u{2318}X", "Ctrl+Shift+X"),
    (Action::Settings, "\u{2318},", "Ctrl+,"),
    // Code-OSS binds neither JSON file by default (the keyboard shortcuts editor is a two-stroke chord).
    (Action::SettingsJson, "", ""),
    (Action::KeybindingsJson, "", ""),
    (Action::NextTab, "\u{2303}Tab", "Ctrl+Tab"),
    (Action::PrevTab, "\u{2303}\u{21E7}Tab", "Ctrl+Shift+Tab"),
];

const JETBRAINS: [Row; 32] = [
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
    (Action::FindInFile, "\u{2318}F", "Ctrl+F"),
    (Action::FindInFiles, "\u{21E7}\u{2318}F", "Ctrl+Shift+F"),
    (Action::ReplaceInFiles, "\u{21E7}\u{2318}R", "Ctrl+Shift+R"),
    // IntelliJ's Reopen Closed Tab is ⇧⌘T on macOS; elsewhere its Ctrl+Shift+T is the new-terminal row.
    (Action::ReopenClosed, "\u{21E7}\u{2318}T", ""),
    // ⌘0 is the commit window here, so the IDE zoom takes IntelliJ's own Zoom IDE chords.
    (Action::ZoomIn, "\u{2303}\u{21E7}\u{2325}=", "Ctrl+Shift+Alt+="),
    (Action::ZoomOut, "\u{2303}\u{21E7}\u{2325}-", "Ctrl+Shift+Alt+-"),
    (Action::ZoomReset, "\u{2303}\u{21E7}\u{2325}0", "Ctrl+Shift+Alt+0"),
    // The JetBrains keymap has no default chord for Plugins.
    (Action::Extensions, "", ""),
    (Action::Settings, "\u{2318},", "Ctrl+Alt+S"),
    (Action::SettingsJson, "", ""),
    (Action::KeybindingsJson, "", ""),
    (Action::NextTab, "\u{2303}Tab", "Ctrl+Tab"),
    (Action::PrevTab, "\u{2303}\u{21E7}Tab", "Ctrl+Shift+Tab"),
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

    fn table(self) -> &'static [Row; 32] {
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

/// Code-OSS's `DEFAULT_COMMANDS_TO_SKIP_SHELL` (`terminal.integrated.commandsToSkipShell`,
/// `src/vs/workbench/contrib/terminal/common/terminal.ts` at 04c0d99f, with the `TerminalCommandId` and
/// `AccessibilityCommandId` values written out): a key pressed in the terminal that runs one of these
/// goes to the workbench instead of the shell. The list's last entry spreads
/// `defaultTerminalContribCommandsToSkipShell` (the terminal's own find, history, suggest and
/// accessibility commands); Ember has none of those, so they are not copied.
pub const COMMANDS_TO_SKIP_SHELL: [&str; 159] = [
    "workbench.action.terminal.clearSelection",
    "workbench.action.terminal.clear",
    "workbench.action.terminal.copyAndClearSelection",
    "workbench.action.terminal.copySelection",
    "workbench.action.terminal.copySelectionAsHtml",
    "workbench.action.terminal.copyLastCommand",
    "workbench.action.terminal.copyLastCommandOutput",
    "workbench.action.terminal.copyLastCommandAndLastCommandOutput",
    "workbench.action.terminal.deleteToLineStart",
    "workbench.action.terminal.deleteWordLeft",
    "workbench.action.terminal.deleteWordRight",
    "workbench.action.terminal.focusNextPane",
    "workbench.action.terminal.focusNext",
    "workbench.action.terminal.focusPreviousPane",
    "workbench.action.terminal.focusPrevious",
    "workbench.action.terminal.focus",
    "workbench.action.terminal.sizeToContentWidth",
    "workbench.action.terminal.kill",
    "workbench.action.terminal.killEditor",
    "workbench.action.terminal.moveToEditor",
    "workbench.action.terminal.moveToLineEnd",
    "workbench.action.terminal.moveToLineStart",
    "workbench.action.terminal.moveToTerminalPanel",
    "workbench.action.terminal.newInActiveWorkspace",
    "workbench.action.terminal.new",
    "workbench.action.terminal.newInNewWindow",
    "workbench.action.terminal.paste",
    "workbench.action.terminal.pastePwsh",
    "workbench.action.terminal.pasteSelection",
    "workbench.action.terminal.resizePaneDown",
    "workbench.action.terminal.resizePaneLeft",
    "workbench.action.terminal.resizePaneRight",
    "workbench.action.terminal.resizePaneUp",
    "workbench.action.terminal.runActiveFile",
    "workbench.action.terminal.runSelectedText",
    "workbench.action.terminal.scrollDown",
    "workbench.action.terminal.scrollDownPage",
    "workbench.action.terminal.scrollToBottom",
    "workbench.action.terminal.scrollToNextCommand",
    "workbench.action.terminal.scrollToPreviousCommand",
    "workbench.action.terminal.scrollToTop",
    "workbench.action.terminal.scrollUp",
    "workbench.action.terminal.scrollUpPage",
    "workbench.action.terminal.sendSequence",
    "workbench.action.terminal.selectAll",
    "workbench.action.terminal.selectToNextCommand",
    "workbench.action.terminal.selectToNextLine",
    "workbench.action.terminal.selectToPreviousCommand",
    "workbench.action.terminal.selectToPreviousLine",
    "workbench.action.terminal.splitInActiveWorkspace",
    "workbench.action.terminal.split",
    "workbench.action.terminal.toggleTerminal",
    "workbench.action.terminal.focusHover",
    "editor.action.accessibilityHelp",
    "workbench.action.terminal.stopVoice",
    "workbench.action.terminal.sendSignal",
    "workbench.action.tasks.rerunForActiveTerminal",
    "editor.action.toggleTabFocusMode",
    "notifications.hideList",
    "notifications.hideToasts",
    "workbench.action.closeQuickOpen",
    "workbench.action.quickOpen",
    "workbench.action.quickOpenPreviousEditor",
    "workbench.action.showCommands",
    "workbench.action.tasks.build",
    "workbench.action.tasks.restartTask",
    "workbench.action.tasks.runTask",
    "workbench.action.tasks.reRunTask",
    "workbench.action.tasks.showLog",
    "workbench.action.tasks.showTasks",
    "workbench.action.tasks.terminate",
    "workbench.action.tasks.test",
    "workbench.action.toggleFullScreen",
    "workbench.action.terminal.focusAtIndex1",
    "workbench.action.terminal.focusAtIndex2",
    "workbench.action.terminal.focusAtIndex3",
    "workbench.action.terminal.focusAtIndex4",
    "workbench.action.terminal.focusAtIndex5",
    "workbench.action.terminal.focusAtIndex6",
    "workbench.action.terminal.focusAtIndex7",
    "workbench.action.terminal.focusAtIndex8",
    "workbench.action.terminal.focusAtIndex9",
    "workbench.action.focusSecondEditorGroup",
    "workbench.action.focusThirdEditorGroup",
    "workbench.action.focusFourthEditorGroup",
    "workbench.action.focusFifthEditorGroup",
    "workbench.action.focusSixthEditorGroup",
    "workbench.action.focusSeventhEditorGroup",
    "workbench.action.focusEighthEditorGroup",
    "workbench.action.focusNextPart",
    "workbench.action.focusPreviousPart",
    "workbench.action.nextPanelView",
    "workbench.action.previousPanelView",
    "workbench.action.nextSideBarView",
    "workbench.action.previousSideBarView",
    "workbench.action.debug.disconnect",
    "workbench.action.debug.start",
    "workbench.action.debug.stop",
    "workbench.action.debug.run",
    "workbench.action.debug.restart",
    "workbench.action.debug.continue",
    "workbench.action.debug.pause",
    "workbench.action.debug.stepInto",
    "workbench.action.debug.stepOut",
    "workbench.action.debug.stepOver",
    "sessions.goBack",
    "sessions.goForward",
    "sessions.focusActiveSession",
    "sessions.focusSessionInGrid1",
    "sessions.focusSessionInGrid2",
    "sessions.focusSessionInGrid3",
    "sessions.focusSessionInGrid4",
    "sessions.focusSessionInGrid5",
    "sessions.focusSessionInGrid6",
    "sessions.focusSessionInGrid7",
    "sessions.focusSessionInGrid8",
    "sessions.focusSessionInGrid9",
    "sessionsViewPane.navigatePreviousSession",
    "sessionsViewPane.navigateNextSession",
    "workbench.action.nextEditor",
    "workbench.action.previousEditor",
    "workbench.action.nextEditorInGroup",
    "workbench.action.previousEditorInGroup",
    "workbench.action.openNextRecentlyUsedEditor",
    "workbench.action.openPreviousRecentlyUsedEditor",
    "workbench.action.openNextRecentlyUsedEditorInGroup",
    "workbench.action.openPreviousRecentlyUsedEditorInGroup",
    "workbench.action.quickOpenPreviousRecentlyUsedEditor",
    "workbench.action.quickOpenLeastRecentlyUsedEditor",
    "workbench.action.quickOpenPreviousRecentlyUsedEditorInGroup",
    "workbench.action.quickOpenLeastRecentlyUsedEditorInGroup",
    "workbench.action.focusActiveEditorGroup",
    "workbench.action.focusFirstEditorGroup",
    "workbench.action.focusLastEditorGroup",
    "workbench.action.firstEditorInGroup",
    "workbench.action.lastEditorInGroup",
    "workbench.action.navigateUp",
    "workbench.action.navigateDown",
    "workbench.action.navigateRight",
    "workbench.action.navigateLeft",
    "workbench.action.togglePanel",
    "workbench.action.quickOpenView",
    "workbench.action.toggleMaximizedPanel",
    "workbench.action.zoomIn",
    "workbench.action.zoomOut",
    "workbench.action.zoomReset",
    "notification.acceptPrimaryAction",
    "runCommands",
    "workbench.action.terminal.chat.start",
    "workbench.action.terminal.chat.close",
    "workbench.action.terminal.chat.discard",
    "workbench.action.terminal.chat.makeRequest",
    "workbench.action.terminal.chat.cancel",
    "workbench.action.terminal.chat.feedbackHelpful",
    "workbench.action.terminal.chat.feedbackUnhelpful",
    "workbench.action.terminal.chat.feedbackReportIssue",
    "workbench.action.terminal.chat.runCommand",
    "workbench.action.terminal.chat.insertCommand",
    "workbench.action.terminal.chat.viewInChat",

];

/// The Code-OSS command of each Ember action that is in [`COMMANDS_TO_SKIP_SHELL`] (FR-L31.21, #1133).
pub const TERMINAL_SKIPS_SHELL: [(Action, &str); 13] = [
    (Action::Find, "workbench.action.quickOpen"),
    (Action::Palette, "workbench.action.showCommands"),
    (Action::Terminal, "workbench.action.terminal.toggleTerminal"),
    (Action::NewTerminal, "workbench.action.terminal.new"),
    (Action::HideTool, "workbench.action.togglePanel"),
    (Action::MaximizeTool, "workbench.action.toggleMaximizedPanel"),
    (Action::ZoomIn, "workbench.action.zoomIn"),
    (Action::ZoomOut, "workbench.action.zoomOut"),
    (Action::ZoomReset, "workbench.action.zoomReset"),
    (Action::NextTab, "workbench.action.nextEditor"),
    (Action::PrevTab, "workbench.action.previousEditor"),
    (Action::RunConfig, "workbench.action.debug.run"),
    (Action::Stop, "workbench.action.debug.stop"),
];

/// What a key pressed in the terminal would do if it reached the page's key handler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PageKey {
    /// The key runs nothing on the page.
    Nothing,
    /// The key runs this keymap action (after the user's `keybindings.json`).
    Action(Action),
    /// The key runs this `keybindings.json` command, which is not a keymap action.
    Command(String),
    /// A language chord of the code editor (FR-L31.13). Code-OSS binds those only while the editor
    /// has focus, so from the terminal they never run.
    Editor,
}

/// True for a chord the shell reads as a control byte that Ember's JetBrains keymap binds to an action
/// Code-OSS skips the shell for: bare Ctrl with a letter (⌃R, ⌃D). Code-OSS's own keymaps bind no
/// such chord to run or debug, so this exception is the JetBrains keymap's only.
fn shell_control_chord(keymap: Keymap, chord: &Chord) -> bool {
    keymap == Keymap::JetBrains
        && matches!(chord, Chord::Keys { ctrl: true, shift: false, alt: false, meta: false, key }
            if key.len() == 1 && key.as_bytes()[0].is_ascii_alphabetic())
}

/// The one rule for keys pressed in the integrated terminal (FR-L31.21, #1133), as Code-OSS's terminal
/// does it: true when the terminal keeps the key (it stops the key there and sends the shell its bytes,
/// if it has any), false when the key goes on to the page.
///
/// * A key whose action or command is in [`COMMANDS_TO_SKIP_SHELL`] goes on, except a
///   [`shell_control_chord`] under the JetBrains keymap, which stays with the shell (⌃R, ⌃D).
/// * A code editor chord never runs from the terminal.
/// * Any other key stays with the shell when the shell has bytes for it, and goes on when it has none,
///   as xterm.js leaves a key it cannot encode to the workbench (⌘S saves, a bare Shift still counts
///   toward Shift Shift).
pub fn terminal_keeps_key(keymap: Keymap, chord: &Chord, page: &PageKey, shell_bytes: bool) -> bool {
    let skips_shell = match page {
        PageKey::Action(action) => TERMINAL_SKIPS_SHELL.iter().any(|(a, _)| a == action),
        PageKey::Command(id) => COMMANDS_TO_SKIP_SHELL.contains(&id.as_str()),
        PageKey::Editor => return true,
        PageKey::Nothing => false,
    };
    if skips_shell && !shell_control_chord(keymap, chord) {
        return false;
    }
    shell_bytes
}

/// An option of the find bar (FR-L31.12). Code-OSS binds these only while the find widget or the editor
/// has focus, so they live beside the window table rather than in it (JetBrains' ⌥⌘L, 코드 서식, would
/// otherwise collide with 선택 영역에서 찾기); they are the same in both keymaps, as Code-OSS's own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindOption {
    Regex,
    InSelection,
    PreserveCase,
}

impl FindOption {
    pub const ALL: [FindOption; 3] = [FindOption::Regex, FindOption::InSelection, FindOption::PreserveCase];

    fn text(self, os: Os) -> &'static str {
        match (self, os) {
            (FindOption::Regex, Os::Mac) => "\u{2325}\u{2318}R",
            (FindOption::InSelection, Os::Mac) => "\u{2325}\u{2318}L",
            (FindOption::PreserveCase, Os::Mac) => "\u{2325}\u{2318}P",
            (FindOption::Regex, Os::Other) => "Alt+R",
            (FindOption::InSelection, Os::Other) => "Alt+L",
            (FindOption::PreserveCase, Os::Other) => "Alt+P",
        }
    }

    pub fn chord(self, os: Os) -> Option<Chord> {
        Chord::parse(os, self.text(os))
    }

    /// The chord as the OS writes it, for the button's tooltip.
    pub fn hint(self, os: Os) -> String {
        self.chord(os).map(|c| c.display(os)).unwrap_or_default()
    }

    /// The option a chord pressed in the find bar toggles.
    pub fn for_chord(chord: &Chord, os: Os) -> Option<FindOption> {
        FindOption::ALL.into_iter().find(|o| o.chord(os).as_ref() == Some(chord))
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

    fn keys(ctrl: bool, shift: bool, meta: bool, key: &str) -> Chord {
        Chord::Keys { ctrl, shift, alt: false, meta, key: key.into() }
    }

    fn page_key(keymap: Keymap, os: Os, chord: &Chord) -> PageKey {
        keymap.action_for(chord, os).map_or(PageKey::Nothing, PageKey::Action)
    }

    /// Whether the shell has bytes for `chord` (the terminal's own encoding).
    fn shell_bytes(chord: &Chord) -> bool {
        match chord {
            Chord::Keys { ctrl, shift, alt, meta, key } => {
                !*meta && crate::terminal_stream::key_bytes(key, crate::terminal_stream::Modifiers { ctrl: *ctrl, alt: *alt, shift: *shift }).is_some()
            }
            Chord::DoubleShift => false,
        }
    }

    fn keeps(keymap: Keymap, os: Os, chord: &Chord) -> bool {
        terminal_keeps_key(keymap, chord, &page_key(keymap, os, chord), shell_bytes(chord))
    }

    // #1133: ⌃R, Ctrl+E and Ctrl+D typed in the terminal stay with the shell under both keymaps on both
    // systems, even where the keymap binds them: JetBrains ⌃R 다시 실행 on macOS (on the skip list, kept
    // by the control chord exception), Ctrl+E 최근 파일 elsewhere, VS Code ⌃R 최근 파일 on macOS.
    #[test]
    fn fr_l31_21_shell_keys_typed_in_the_terminal_run_no_page_action() {
        for keymap in Keymap::ALL {
            for os in [Os::Mac, Os::Other] {
                for key in ["R", "E", "D"] {
                    assert!(keeps(keymap, os, &keys(true, false, false, key)), "{keymap:?} {os:?} Ctrl+{key}");
                }
            }
        }
        assert_eq!(Keymap::JetBrains.action_for(&keys(true, false, false, "R"), Os::Mac), Some(Action::RunConfig));
        assert_eq!(Keymap::JetBrains.action_for(&keys(true, false, false, "E"), Os::Other), Some(Action::Recent));
        // A code editor chord never runs from the terminal, with or without bytes.
        assert!(terminal_keeps_key(Keymap::VsCode, &keys(true, false, false, "."), &PageKey::Editor, false));
        // Escape runs nothing but has bytes: the shell gets it.
        assert!(terminal_keeps_key(Keymap::VsCode, &keys(false, false, false, "Esc"), &PageKey::Nothing, true));
        // A bare Shift has no bytes: it goes on, so Shift Shift still counts.
        assert!(!terminal_keeps_key(Keymap::JetBrains, &keys(false, true, false, "Shift"), &PageKey::Nothing, false));
    }

    // #1133: the palette and quick open are in Code-OSS's default commandsToSkipShell, so their chords
    // reach the page from the terminal, also where they are Ctrl chords the shell could read.
    #[test]
    fn fr_l31_21_palette_and_quick_open_still_run_from_the_terminal() {
        let cases = [
            (Keymap::VsCode, Os::Mac, keys(false, true, true, "P"), Action::Palette),
            (Keymap::VsCode, Os::Other, keys(true, true, false, "P"), Action::Palette),
            (Keymap::VsCode, Os::Mac, keys(false, false, true, "P"), Action::Find),
            (Keymap::VsCode, Os::Other, keys(true, false, false, "P"), Action::Find),
            (Keymap::JetBrains, Os::Mac, keys(false, true, true, "A"), Action::Palette),
            (Keymap::JetBrains, Os::Other, keys(true, true, false, "A"), Action::Palette),
            (Keymap::JetBrains, Os::Mac, Chord::DoubleShift, Action::Find),
        ];
        for (keymap, os, chord, action) in cases {
            assert_eq!(page_key(keymap, os, &chord), PageKey::Action(action), "{keymap:?} {os:?} {chord:?}");
            assert!(!keeps(keymap, os, &chord), "{keymap:?} {os:?} {chord:?} goes on to the page");
        }
    }

    // #1133: the run and debug commands Code-OSS skips the shell for (Ctrl+F5 debug.run, Shift+F5
    // debug.stop) reach the page from the terminal, though the shell has bytes for F5.
    #[test]
    fn fr_l31_21_run_and_stop_run_from_the_terminal() {
        for os in [Os::Mac, Os::Other] {
            let run = keys(true, false, false, "F5");
            let stop = keys(false, true, false, "F5");
            assert!(shell_bytes(&run) && shell_bytes(&stop));
            assert_eq!(page_key(Keymap::VsCode, os, &run), PageKey::Action(Action::RunConfig), "{os:?}");
            assert_eq!(page_key(Keymap::VsCode, os, &stop), PageKey::Action(Action::Stop), "{os:?}");
            assert!(!keeps(Keymap::VsCode, os, &run), "{os:?} Ctrl+F5");
            assert!(!keeps(Keymap::VsCode, os, &stop), "{os:?} Shift+F5");
        }
        // JetBrains 중지 (⌘F2 on macOS, Ctrl+F2 elsewhere) and 다시 실행 elsewhere (Shift+F10) go on too.
        assert!(!keeps(Keymap::JetBrains, Os::Mac, &keys(false, false, true, "F2")));
        assert!(!keeps(Keymap::JetBrains, Os::Other, &keys(true, false, false, "F2")));
        assert!(!keeps(Keymap::JetBrains, Os::Other, &keys(false, true, false, "F10")));
    }

    // #1133: a key the shell has no bytes for reaches the page as in Code-OSS (xterm.js leaves it to the
    // workbench): ⌘S saves and ⌘W closes on macOS. Ctrl+S elsewhere has bytes and stays with the shell.
    #[test]
    fn fr_l31_21_keys_the_shell_cannot_read_still_run_their_actions() {
        for keymap in Keymap::ALL {
            let save = keys(false, false, true, "S");
            assert_eq!(page_key(keymap, Os::Mac, &save), PageKey::Action(Action::Save));
            assert!(!keeps(keymap, Os::Mac, &save), "{keymap:?} ⌘S saves from the terminal");
            assert!(!keeps(keymap, Os::Mac, &keys(false, false, true, "W")), "{keymap:?} ⌘W");
            assert!(keeps(keymap, Os::Other, &keys(true, false, false, "S")), "{keymap:?} Ctrl+S stays with the shell");
        }
        assert!(!keeps(Keymap::JetBrains, Os::Mac, &keys(false, false, true, "1")), "⌘1 프로젝트");
    }

    #[test]
    fn fr_l31_21_terminal_allow_list_is_code_oss_commands_to_skip_shell() {
        for (action, command) in TERMINAL_SKIPS_SHELL {
            assert!(COMMANDS_TO_SKIP_SHELL.contains(&command), "{action:?} {command}");
            if let Some(mapped) = crate::ui::editor_settings::command_of(action) {
                assert_eq!(mapped, command, "{action:?} has one Code-OSS command");
            }
        }
        for command in ["workbench.action.debug.start", "workbench.action.terminal.toggleTerminal", "workbench.action.showCommands"] {
            assert!(COMMANDS_TO_SKIP_SHELL.contains(&command), "{command}");
        }
        for action in [Action::Recent, Action::Save, Action::Close, Action::Project] {
            assert!(!TERMINAL_SKIPS_SHELL.iter().any(|(a, _)| *a == action), "{action:?}");
        }
        // A keybindings.json command on the list goes on; one off it with shell bytes stays.
        let chord = keys(true, true, false, "B");
        assert!(!terminal_keeps_key(Keymap::VsCode, &chord, &PageKey::Command("workbench.action.tasks.build".into()), true));
        assert!(terminal_keeps_key(Keymap::VsCode, &chord, &PageKey::Command("editor.action.formatDocument".into()), true));
    }

    #[test]
    fn ctrl_tab_switches_tabs_in_every_keymap_and_os() {
        let next = Chord::Keys { ctrl: true, shift: false, alt: false, meta: false, key: "Tab".into() };
        let prev = Chord::Keys { ctrl: true, shift: true, alt: false, meta: false, key: "Tab".into() };
        for keymap in Keymap::ALL {
            for os in [Os::Mac, Os::Other] {
                assert_eq!(keymap.action_for(&next, os), Some(Action::NextTab), "{keymap:?} {os:?}");
                assert_eq!(keymap.action_for(&prev, os), Some(Action::PrevTab), "{keymap:?} {os:?}");
            }
        }
    }
}
