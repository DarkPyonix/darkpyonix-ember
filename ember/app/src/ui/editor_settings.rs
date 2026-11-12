//! Settings of the Ember 에디터 window (SPEC FR-L31.17, `docs/INTENT.md` D46).
//!
//! The pure part of the settings lives in `ember_editor::settings`. This module is what the window
//! adds: where the user's files are on this machine, the state the host keeps, writing one setting
//! into `settings.json`, the user's `keybindings.json` against Ember's actions, and the settings
//! view (a tab in the editor area, as in Code-OSS).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use ember_editor::settings::{
    parse_keybindings, parse_layer, set_setting, user_data_uri, ExcludeMatcher, KeybindingRule, SettingKind, SettingSchema,
    SettingsLayer, SettingsScope, SettingsScopes,
};
use ember_editor::{EditorSession, EditorSettings, ServerSettingsFile, SessionUpdate};
use serde_json::Value;

use crate::editor_host::HostState;
use crate::ui::keymap::{Action, Chord};

/// The tab of the settings view. It is not a file: the host never opens, focuses or closes it.
pub const SETTINGS_TAB: &str = "ember-settings:/설정";

/// Whether a tab path is the settings view.
pub fn is_settings_tab(path: &str) -> bool {
    path == SETTINGS_TAB
}

/// What a new `keybindings.json` starts with (Code-OSS's own first line).
pub const KEYBINDINGS_TEMPLATE: &str = "// Place your key bindings in this file to override the defaults\n[\n]\n";
/// What a new `settings.json` starts with.
pub const SETTINGS_TEMPLATE: &str = "{\n}\n";

/// A file the settings view opens as a document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsFile {
    /// The user's `settings.json` on this machine.
    User,
    /// `.vscode/settings.json` in the project folder on the server.
    Workspace,
    /// The user's `keybindings.json` on this machine.
    Keybindings,
}

/// The user's settings files on this machine: `<data dir>/editor/User/settings.json` and
/// `keybindings.json` beside it. `None` when the OS reports no data directory.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UserFiles {
    pub settings: Option<PathBuf>,
    pub keybindings: Option<PathBuf>,
}

impl UserFiles {
    pub fn in_dir(dir: Option<PathBuf>) -> Self {
        match dir {
            Some(dir) => Self { settings: Some(dir.join("settings.json")), keybindings: Some(dir.join("keybindings.json")) },
            None => Self::default(),
        }
    }

    /// The files the session may open as local documents.
    pub fn paths(&self) -> Vec<PathBuf> {
        self.settings.iter().chain(self.keybindings.iter()).cloned().collect()
    }

    /// The tab path of a file: the path of its `vscode-userdata:` URI.
    pub fn tab_path(file: &Option<PathBuf>) -> Option<String> {
        file.as_deref().map(|p| user_data_uri(p).path)
    }
}

/// A file's text; a file that is not there reads as empty.
fn read_local(path: &std::path::Path) -> Result<String, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e.to_string()),
    }
}

fn write_local(path: &std::path::Path, text: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, text).map_err(|e| e.to_string())
}

/// The message for a settings file that does not parse.
pub fn parse_error_text(file: &str, error: &impl std::fmt::Display) -> String {
    format!("{file}을(를) 읽을 수 없습니다 ({error}). 파일을 고친 뒤 저장하세요.")
}

/// What the host knows about the settings.
#[derive(Debug, Clone, Default)]
pub struct SettingsState {
    pub scopes: SettingsScopes,
    /// Every setting the view lists.
    pub catalogue: Arc<Vec<SettingSchema>>,
    /// Tab paths of the files, when they exist for this window.
    pub user_path: Option<String>,
    pub workspace_path: Option<String>,
    pub keybindings_path: Option<String>,
    /// Why the user's or the workspace's file is not applied (it does not parse).
    pub user_error: Option<String>,
    pub workspace_error: Option<String>,
    pub keybindings_error: Option<String>,
    /// The user's `keybindings.json`, in file order.
    pub keybindings: Arc<Vec<KeybindingRule>>,
    /// Counts every change, so the view can tell a write has landed.
    pub revision: u64,
}

impl SettingsState {
    /// The settings the editor itself acts on, for a document of `language`.
    pub fn editor(&self, language: Option<&str>) -> EditorSettings {
        EditorSettings::resolve(&self.scopes, language)
    }

    /// The explorer's filter from `files.exclude`.
    pub fn exclude_matcher(&self) -> ExcludeMatcher {
        ExcludeMatcher::new(&self.editor(None).files_exclude)
    }
}

/// The user scope as read from this machine before the session connects, and why it is empty when
/// the file does not parse.
pub fn read_user_layer(files: &UserFiles) -> (SettingsLayer, Option<String>) {
    let Some(path) = &files.settings else { return (SettingsLayer::default(), None) };
    match read_local(path) {
        Ok(text) => match parse_layer(&text) {
            Ok(layer) => (layer, None),
            Err(e) => (SettingsLayer::default(), Some(parse_error_text("settings.json", &e))),
        },
        Err(e) => (SettingsLayer::default(), Some(parse_error_text("settings.json", &e))),
    }
}

/// The user's key bindings as read from this machine.
pub fn read_keybindings(files: &UserFiles) -> (Vec<KeybindingRule>, Option<String>) {
    let Some(path) = &files.keybindings else { return (Vec::new(), None) };
    match read_local(path).and_then(|text| parse_keybindings(&text).map_err(|e| e.to_string())) {
        Ok(rules) => (rules, None),
        Err(e) => (Vec::new(), Some(parse_error_text("keybindings.json", &e))),
    }
}

/// The state right after the session connected.
pub fn start_state(session: &EditorSession, files: &UserFiles, user_error: Option<String>) -> SettingsState {
    let (keybindings, keybindings_error) = read_keybindings(files);
    SettingsState {
        scopes: session.initial_settings().clone(),
        catalogue: Arc::new(session.setting_catalogue().to_vec()),
        user_path: UserFiles::tab_path(&files.settings),
        workspace_path: session.server_settings_path(ServerSettingsFile::Workspace),
        keybindings_path: UserFiles::tab_path(&files.keybindings),
        user_error,
        workspace_error: None,
        keybindings_error,
        keybindings: Arc::new(keybindings),
        revision: 1,
    }
}

/// Take a new text of one scope's file: a text that parses replaces the scope in the session (which
/// tells the extension host) and in the state; one that does not keeps the last good scope and
/// records why.
fn take_scope_text(session: &EditorSession, shared: &Arc<Mutex<HostState>>, scope: SettingsScope, text: &str) {
    let parsed = parse_layer(text);
    let mut state = shared.lock().unwrap();
    let settings = &mut state.settings;
    let error = match parsed {
        Ok(layer) => {
            let _ = session.set_settings(scope, layer.clone());
            *settings.scopes.layer_mut(scope) = layer;
            None
        }
        Err(e) => Some(parse_error_text(if scope == SettingsScope::User { "settings.json" } else { ".vscode/settings.json" }, &e)),
    };
    match scope {
        SettingsScope::User => settings.user_error = error,
        SettingsScope::Workspace => settings.workspace_error = error,
        _ => {}
    }
    settings.revision += 1;
}

/// Write one setting into the user's or the workspace's `settings.json` (`value` `None` removes
/// the key), reload the file if it is open, and apply the new scope. A failure is shown once.
pub async fn write_setting(
    session: &EditorSession,
    shared: &Arc<Mutex<HostState>>,
    files: &UserFiles,
    scope: SettingsScope,
    key: &str,
    value: Option<Value>,
) {
    let result: Result<String, String> = async {
        match scope {
            SettingsScope::User => {
                let path = files.settings.as_deref().ok_or("이 기기에는 사용자 설정을 둘 폴더가 없습니다")?;
                let text = read_local(path)?;
                let next = set_setting(&text, key, value.as_ref()).map_err(|e| parse_error_text("settings.json", &e))?;
                write_local(path, &next)?;
                let _ = session.reload(user_data_uri(path));
                Ok(next)
            }
            SettingsScope::Workspace => {
                let existing = session.read_server_settings(ServerSettingsFile::Workspace).await.map_err(|e| e.to_string())?;
                let created = existing.is_none();
                let next = set_setting(&existing.unwrap_or_default(), key, value.as_ref())
                    .map_err(|e| parse_error_text(".vscode/settings.json", &e))?;
                session.write_workspace_settings(&next).await.map_err(|e| e.to_string())?;
                if let Some(path) = session.server_settings_path(ServerSettingsFile::Workspace) {
                    let _ = session.reload(session.uri(&path));
                }
                if created {
                    // The `.vscode` folder may be new: list the project folder again.
                    let root = session.workspace_uri();
                    let listed = session.read_dir(&root).await;
                    shared.lock().unwrap().set_dir_listing(&root.path, listed);
                }
                Ok(next)
            }
            _ => Err("이 범위에는 설정을 쓸 수 없습니다".to_string()),
        }
    }
    .await;
    match result {
        Ok(text) => take_scope_text(session, shared, scope, &text),
        Err(error) => {
            tracing::warn!(%key, %error, "writing a setting failed");
            shared.lock().unwrap().edit_errors.push(format!("설정을 저장하지 못했습니다: {error}"));
        }
    }
}

/// Make sure a settings file exists, so it opens as a document instead of failing as missing.
pub async fn ensure_file(session: &EditorSession, files: &UserFiles, file: SettingsFile) -> Result<(), String> {
    let local = |path: &Option<PathBuf>, template: &str| -> Result<(), String> {
        let path = path.as_deref().ok_or("이 기기에는 사용자 설정을 둘 폴더가 없습니다")?;
        if path.exists() { Ok(()) } else { write_local(path, template) }
    };
    match file {
        SettingsFile::User => local(&files.settings, SETTINGS_TEMPLATE),
        SettingsFile::Keybindings => local(&files.keybindings, KEYBINDINGS_TEMPLATE),
        SettingsFile::Workspace => match session.read_server_settings(ServerSettingsFile::Workspace).await {
            Ok(Some(_)) => Ok(()),
            Ok(None) => session.write_workspace_settings(SETTINGS_TEMPLATE).await.map_err(|e| e.to_string()),
            Err(e) => Err(e.to_string()),
        },
    }
}

/// The tab path of a settings file.
pub fn file_tab_path(settings: &SettingsState, file: SettingsFile) -> Option<String> {
    match file {
        SettingsFile::User => settings.user_path.clone(),
        SettingsFile::Workspace => settings.workspace_path.clone(),
        SettingsFile::Keybindings => settings.keybindings_path.clone(),
    }
}

/// Which settings file a saved document is, if any. The user's files are told from a server file of
/// the same path by their scheme.
pub fn saved_settings_file(settings: &SettingsState, update: &SessionUpdate) -> Option<SettingsFile> {
    let SessionUpdate::Saved { uri } = update else { return None };
    let local = uri.scheme == ember_editor::settings::SCHEME_USER_DATA;
    let is = |path: &Option<String>| path.as_deref() == Some(uri.path.as_str());
    if local && is(&settings.user_path) {
        Some(SettingsFile::User)
    } else if local && is(&settings.keybindings_path) {
        Some(SettingsFile::Keybindings)
    } else if !local && is(&settings.workspace_path) {
        Some(SettingsFile::Workspace)
    } else {
        None
    }
}

/// After a settings file was saved as a document: read it again and apply it.
pub async fn reload_after_save(session: &EditorSession, shared: &Arc<Mutex<HostState>>, files: &UserFiles, file: SettingsFile) {
    match file {
        SettingsFile::User => {
            let Some(path) = &files.settings else { return };
            match read_local(path) {
                Ok(text) => take_scope_text(session, shared, SettingsScope::User, &text),
                Err(e) => shared.lock().unwrap().settings.user_error = Some(parse_error_text("settings.json", &e)),
            }
        }
        SettingsFile::Workspace => match session.read_server_settings(ServerSettingsFile::Workspace).await {
            Ok(text) => take_scope_text(session, shared, SettingsScope::Workspace, &text.unwrap_or_default()),
            Err(e) => shared.lock().unwrap().settings.workspace_error = Some(parse_error_text(".vscode/settings.json", &e)),
        },
        SettingsFile::Keybindings => {
            let (rules, error) = read_keybindings(files);
            let mut state = shared.lock().unwrap();
            // A file that does not parse keeps the last good bindings.
            if error.is_none() {
                state.settings.keybindings = Arc::new(rules);
            }
            state.settings.keybindings_error = error;
            state.settings.revision += 1;
        }
    }
}

// ---- one row of the settings view ---------------------------------------------------------------

/// What the view shows for one setting when edits go to `target`.
#[derive(Debug, Clone, PartialEq)]
pub struct RowValue {
    /// The value in effect: every scope merged, or the registered default.
    pub value: Value,
    /// `target`'s own file writes this key, so it can be reset there.
    pub set_in_target: bool,
    /// A stronger scope than `target` writes the key: an edit in `target` would not take effect.
    pub overridden_by: Option<SettingsScope>,
}

pub fn row_value(scopes: &SettingsScopes, schema: &SettingSchema, target: SettingsScope) -> RowValue {
    let value = scopes.value(&schema.key, None).unwrap_or_else(|| schema.default.clone());
    let set_in_target = scopes.layer(target).values.iter().any(|(k, _)| k == &schema.key);
    let overridden_by = scopes.winning_scope(&schema.key).filter(|winner| *winner > target);
    RowValue { value, set_in_target, overridden_by }
}

/// The text a field shows for a value: a string as it is, anything else as JSON.
pub fn value_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// The value a typed text stands for, by the setting's kind.
pub fn parse_input(kind: &SettingKind, text: &str) -> Result<Value, &'static str> {
    let text = text.trim();
    match kind {
        SettingKind::Integer => text.parse::<i64>().map(Value::from).map_err(|_| "정수를 입력하세요"),
        SettingKind::Number => text
            .parse::<f64>()
            .ok()
            .filter(|n| n.is_finite())
            .and_then(|n| if n.fract() == 0.0 && n.abs() < 9e15 { Some(Value::from(n as i64)) } else { serde_json::Number::from_f64(n).map(Value::Number) })
            .ok_or("숫자를 입력하세요"),
        SettingKind::String => Ok(Value::String(text.to_owned())),
        SettingKind::Enum(options) => {
            if options.iter().any(|o| o == text) { Ok(Value::String(text.to_owned())) } else { Err("목록에 있는 값을 고르세요") }
        }
        SettingKind::Boolean => match text {
            "true" => Ok(Value::Bool(true)),
            "false" => Ok(Value::Bool(false)),
            _ => Err("true 또는 false를 입력하세요"),
        },
        SettingKind::Other => Err("settings.json에서 편집하세요"),
    }
}

/// The label of a scope in the view.
pub fn scope_label(scope: SettingsScope) -> &'static str {
    match scope {
        SettingsScope::Default => "기본값",
        SettingsScope::User => "사용자",
        SettingsScope::Remote => "원격",
        SettingsScope::Workspace => "작업 영역",
    }
}

/// How many settings the view lists before 더 보기.
pub const SETTINGS_PAGE: usize = 100;

/// The explorer rows `files.exclude` leaves: a row is dropped when a pattern matches it or a folder
/// above it. Paths are taken relative to `root`.
pub fn visible_tree_rows(rows: Vec<crate::ui::editor::TreeRow>, root: &str, matcher: &ExcludeMatcher) -> Vec<crate::ui::editor::TreeRow> {
    if matcher.is_empty() {
        return rows;
    }
    let root = root.trim_end_matches('/');
    rows.into_iter()
        .filter(|row| {
            let relative = row.path.strip_prefix(root).unwrap_or(&row.path).trim_start_matches('/');
            !matcher.hides(relative)
        })
        .collect()
}

// ---- keybindings.json against Ember's actions ---------------------------------------------------

/// The Code-OSS command of an Ember action, for `keybindings.json`.
pub fn command_of(action: Action) -> Option<&'static str> {
    Some(match action {
        Action::Project => "workbench.view.explorer",
        Action::Git => "workbench.view.scm",
        Action::Problems => "workbench.actions.view.problems",
        Action::Terminal => "workbench.action.terminal.toggleTerminal",
        Action::NewTerminal => "workbench.action.terminal.new",
        Action::Close => "workbench.action.closeActiveEditor",
        Action::Save => "workbench.action.files.save",
        Action::Find => "workbench.action.quickOpen",
        Action::Palette => "workbench.action.showCommands",
        Action::GotoLine => "workbench.action.gotoLine",
        Action::Format => "editor.action.formatDocument",
        Action::HideTool => "workbench.action.togglePanel",
        Action::MaximizeTool => "workbench.action.toggleMaximizedPanel",
        Action::FindInFile => "actions.find",
        Action::FindInFiles => "workbench.action.findInFiles",
        Action::ReplaceInFiles => "workbench.action.replaceInFiles",
        Action::Settings => "workbench.action.openSettings",
        Action::SettingsJson => "workbench.action.openSettingsJson",
        Action::KeybindingsJson => "workbench.action.openGlobalKeybindingsFile",
        _ => return None,
    })
}

/// The Ember action of a Code-OSS command.
pub fn action_of(command: &str) -> Option<Action> {
    Action::ALL.into_iter().find(|a| command_of(*a) == Some(command))
}

/// The chord a `keybindings.json` key stands for: `cmd+shift+p`, `ctrl+,`, `alt+f12`. `None` for a
/// two-stroke key (`ctrl+k ctrl+s`), which Ember does not take yet, and for a key it cannot read.
pub fn chord_of_binding(key: &str) -> Option<Chord> {
    let key = key.trim().to_lowercase();
    if key.is_empty() || key.split_whitespace().count() != 1 {
        return None;
    }
    let (mut ctrl, mut shift, mut alt, mut meta) = (false, false, false, false);
    // `ctrl++` binds the plus key: the last part is then empty and the key is `+`.
    let (mods, last) = match key.strip_suffix("++") {
        Some(mods) => (mods.to_owned(), "+".to_owned()),
        None => match key.rsplit_once('+') {
            Some((mods, last)) => (mods.to_owned(), last.to_owned()),
            None => (String::new(), key.clone()),
        },
    };
    for part in mods.split('+').filter(|p| !p.is_empty()) {
        match part {
            "ctrl" => ctrl = true,
            "shift" => shift = true,
            "alt" | "option" => alt = true,
            "cmd" | "meta" | "win" => meta = true,
            _ => return None,
        }
    }
    let name = match last.as_str() {
        "" => return None,
        "escape" => "Esc".to_owned(),
        "enter" => "Enter".to_owned(),
        "tab" => "Tab".to_owned(),
        "space" => " ".to_owned(),
        "backspace" => "Backspace".to_owned(),
        "delete" => "Delete".to_owned(),
        "up" => "ArrowUp".to_owned(),
        "down" => "ArrowDown".to_owned(),
        "left" => "ArrowLeft".to_owned(),
        "right" => "ArrowRight".to_owned(),
        "pageup" => "PageUp".to_owned(),
        "pagedown" => "PageDown".to_owned(),
        "home" => "Home".to_owned(),
        "end" => "End".to_owned(),
        f if f.len() >= 2 && f.starts_with('f') && f[1..].chars().all(|c| c.is_ascii_digit()) => f.to_uppercase(),
        single if single.chars().count() == 1 => single.to_uppercase(),
        _ => return None,
    };
    Some(Chord::Keys { ctrl, shift, alt, meta, key: name })
}

/// What a pressed chord does once the user's `keybindings.json` is applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyResolution {
    /// The file says nothing about this chord: the keymap's own binding stands.
    Default,
    /// The file removed the keymap's binding (`-command`) and gave the chord nothing else.
    Unbound,
    Action(Action),
    /// A command Ember has no action for: it is sent to the extension host.
    Command(String),
}

/// Resolve a pressed chord. Entries are read in file order and a later one wins, as in Code-OSS.
/// An entry with a `when` clause is left out: Ember does not evaluate context keys yet, and a
/// binding meant for one context must not fire in every other.
pub fn resolve_key(rules: &[KeybindingRule], chord: &Chord, default: Option<Action>) -> KeyResolution {
    let mut resolution = KeyResolution::Default;
    for rule in rules {
        if rule.when.is_some() || chord_of_binding(&rule.key).as_ref() != Some(chord) {
            continue;
        }
        match rule.command.strip_prefix('-') {
            Some(removed) => {
                let removes_default = default.is_some() && action_of(removed) == default;
                let removes_current = match &resolution {
                    KeyResolution::Action(a) => action_of(removed) == Some(*a),
                    KeyResolution::Command(c) => c == removed,
                    _ => false,
                };
                if removes_current || (removes_default && resolution == KeyResolution::Default) {
                    resolution = KeyResolution::Unbound;
                }
            }
            None => {
                resolution = match action_of(&rule.command) {
                    Some(action) => KeyResolution::Action(action),
                    None => KeyResolution::Command(rule.command.clone()),
                };
            }
        }
    }
    resolution
}

// ---- the view -----------------------------------------------------------------------------------

pub mod view {
    //! The settings view: a search field, the scope edits go to, and one row per setting with its
    //! control. It is a tab of the editor area, as Code-OSS's settings editor is. Its own state is
    //! module state, so the shared window state does not grow for one view.

    use std::cell::Cell;
    use std::collections::HashMap;

    use dioxus_compose::html::prelude::dioxus_core::ScopeId;
    use dioxus_compose::html::prelude::dioxus_elements::Key;
    use dioxus_compose::html::prelude::dioxus_signals::{ReadableExt, Signal, WritableExt};
    use dioxus_compose::html::prelude::*;
    use ember_editor::settings::{search_settings, SettingKind, SettingSchema, SettingsScope};
    use serde_json::Value;

    use super::{file_tab_path, parse_input, row_value, scope_label, value_text, SettingsFile, SETTINGS_PAGE, SETTINGS_TAB};
    use crate::editor_host::HostCommand;
    use crate::ui::editor_workbench::page::icon_px;
    use crate::ui::editor_workbench::Shared;

    #[derive(Clone, Copy)]
    struct Ui {
        query: Signal<String>,
        /// Where edits are written: the user's file or the workspace's.
        target: Signal<SettingsScope>,
        /// Texts typed into fields and not committed yet, by key.
        drafts: Signal<HashMap<String, String>>,
        limit: Signal<usize>,
        focus_seq: Signal<u64>,
    }

    thread_local! {
        static UI: Cell<Option<Ui>> = const { Cell::new(None) };
    }

    fn ui() -> Ui {
        UI.with(|cell| {
            if let Some(ui) = cell.get() {
                return ui;
            }
            let root = ScopeId::ROOT;
            // Debug builds only: `EMBER_EDITOR_SETTINGS_QUERY` fills the search field and
            // `EMBER_EDITOR_SETTINGS=workspace` starts on the workspace scope, for window captures.
            let hook = |name: &str| if cfg!(debug_assertions) { std::env::var(name).ok() } else { None };
            let target = if hook("EMBER_EDITOR_SETTINGS").as_deref() == Some("workspace") { SettingsScope::Workspace } else { SettingsScope::User };
            let ui = Ui {
                query: Signal::new_in_scope(hook("EMBER_EDITOR_SETTINGS_QUERY").unwrap_or_default(), root),
                target: Signal::new_in_scope(target, root),
                drafts: Signal::new_in_scope(HashMap::new(), root),
                limit: Signal::new_in_scope(SETTINGS_PAGE, root),
                focus_seq: Signal::new_in_scope(0, root),
            };
            cell.set(Some(ui));
            ui
        })
    }

    /// Debug builds only: `EMBER_EDITOR_SETTINGS` (any value) opens the settings view when the
    /// window starts, so a capture needs no key press. Called by the page on every render; it
    /// acts once.
    pub fn apply_capture_hook(s: &Shared) {
        thread_local! { static DONE: Cell<bool> = const { Cell::new(false) }; }
        if !cfg!(debug_assertions) || DONE.with(|d| d.replace(true)) {
            return;
        }
        if std::env::var("EMBER_EDITOR_SETTINGS").is_ok_and(|v| !v.is_empty() && v != "0") {
            open(s);
        }
    }

    fn notice(text: impl Into<String>) {
        crate::ui::cr::notice::post(text);
    }

    /// Show the settings view and put the cursor in its search field.
    pub fn open(s: &Shared) {
        let mut tabs = s.tabs;
        tabs.write().open(SETTINGS_TAB);
        let mut focus = ui().focus_seq;
        let next = *focus.read() + 1;
        focus.set(next);
    }

    /// Open a settings file as a document in a tab. The host makes the file first when it is not there.
    pub fn open_file(s: &Shared, file: SettingsFile) {
        let path = file_tab_path(&s.host.state.lock().unwrap().settings, file);
        let Some(path) = path else {
            notice("이 기기에는 사용자 설정을 둘 폴더가 없습니다");
            return;
        };
        let mut tabs = s.tabs;
        tabs.write().open(&path);
        s.open_errors.borrow_mut().remove(&path);
        s.host.send(HostCommand::OpenSettingsFile(file));
    }

    /// Whether the file edits go to is open with edits that are not saved: writing a setting would
    /// reload it and lose them.
    fn target_file_dirty(s: &Shared, target: SettingsScope) -> bool {
        let file = if target == SettingsScope::Workspace { SettingsFile::Workspace } else { SettingsFile::User };
        let Some(path) = file_tab_path(&s.host.state.lock().unwrap().settings, file) else { return false };
        let uris = s.uris.borrow();
        let bridge = s.bridge.borrow();
        uris.get(&path).and_then(|uri| bridge.document(uri)).is_some_and(|doc| doc.dirty)
    }

    /// Write `value` for `key` into the target scope's file (`None` removes the key).
    fn commit(s: &Shared, key: &str, value: Option<Value>) {
        let target = *ui().target.read();
        if target_file_dirty(s, target) {
            notice("settings.json에 저장하지 않은 변경이 있습니다. 먼저 저장하세요.");
            return;
        }
        let mut drafts = ui().drafts;
        drafts.write().remove(key);
        s.host.send(HostCommand::WriteSetting { scope: target, key: key.to_owned(), value });
    }

    const FIELD: &str = "flex:1; min-width:0; border:0; background:none; outline:none; color:var(--text); font:inherit;";
    const CHIP: &str = "height:26px; padding:0 var(--sp3); border:1px solid var(--line); font-size:var(--fs-xs);";
    const CHIP_ON: &str = "height:26px; padding:0 var(--sp3); border:1px solid var(--line); font-size:var(--fs-xs); background:var(--sel); color:var(--text);";

    /// The control of one setting.
    fn control(s: &Shared, schema: &SettingSchema, value: &Value) -> Element {
        let key = schema.key.clone();
        match &schema.kind {
            SettingKind::Boolean => {
                let on = value.as_bool().unwrap_or(false);
                let s = s.clone();
                let label = format!("{} 켜기", schema.key);
                rsx! {
                    button { class: if on { "ed-pj on" } else { "ed-pj" }, style: if on { CHIP_ON } else { CHIP }, role: "switch", "aria-checked": "{on}", "aria-label": "{label}",
                        onclick: move |_| commit(&s, &key, Some(Value::Bool(!on))),
                        {icon_px(if on { "check" } else { "close" }, 12)}
                        if on { "켜짐" } else { "꺼짐" }
                    }
                }
            }
            SettingKind::Enum(options) => {
                let current = value.as_str().unwrap_or_default().to_owned();
                let options = options.clone();
                rsx! {
                    div { role: "radiogroup", "aria-label": "{schema.key}", style: "display:flex; flex-wrap:wrap; gap:var(--sp1);",
                        for option in options {
                            {
                                let on = option == current;
                                let s = s.clone();
                                let key = key.clone();
                                let chosen = option.clone();
                                rsx! {
                                    button { key: "{option}", class: if on { "ed-pj on" } else { "ed-pj" }, style: if on { CHIP_ON } else { CHIP }, role: "radio", "aria-checked": "{on}",
                                        onclick: move |_| commit(&s, &key, Some(Value::String(chosen.clone()))),
                                        "{option}"
                                    }
                                }
                            }
                        }
                    }
                }
            }
            SettingKind::Integer | SettingKind::Number | SettingKind::String => {
                let kind = schema.kind.clone();
                let mut drafts = ui().drafts;
                let draft = drafts.read().get(&schema.key).cloned();
                let shown = draft.clone().unwrap_or_else(|| value_text(value));
                let edited = draft.is_some();
                let (input_key, enter_key, cancel_key) = (key.clone(), key.clone(), key.clone());
                let enter_s = s.clone();
                rsx! {
                    div { class: "ed-find", style: "width:260px; max-width:100%; height:30px; cursor:text;",
                        input { style: "{FIELD}", "aria-label": "{schema.key}", value: "{shown}",
                            oninput: move |event| { drafts.write().insert(input_key.clone(), event.value()); },
                            onkeydown: move |event| {
                                match event.key() {
                                    Key::Enter => {
                                        event.stop_propagation();
                                        let typed = drafts.read().get(&enter_key).cloned();
                                        if let Some(typed) = typed {
                                            match parse_input(&kind, &typed) {
                                                Ok(v) => commit(&enter_s, &enter_key, Some(v)),
                                                Err(reason) => notice(reason),
                                            }
                                        }
                                    }
                                    Key::Escape => {
                                        event.stop_propagation();
                                        drafts.write().remove(&cancel_key);
                                    }
                                    _ => {}
                                }
                            },
                        }
                        if edited { span { class: "ed-sc", "aria-hidden": "true", "Enter" } }
                    }
                }
            }
            SettingKind::Other => {
                let s = s.clone();
                rsx! {
                    button { class: "ed-pj", style: "{CHIP}",
                        onclick: move |_| {
                            let target = *ui().target.read();
                            open_file(&s, if target == SettingsScope::Workspace { SettingsFile::Workspace } else { SettingsFile::User });
                        },
                        "settings.json에서 편집"
                    }
                }
            }
        }
    }

    /// The settings view, shown in the editor area while its tab is the current one.
    pub fn pane(s: &Shared) -> Element {
        let ui = ui();
        let (mut query, mut target_sig, mut limit) = (ui.query, ui.target, ui.limit);
        let focus_seq = *ui.focus_seq.read();
        let settings = s.host.state.lock().unwrap().settings.clone();
        let target = *target_sig.read();
        let q = query.read().clone();
        let found = search_settings(&settings.catalogue, &q);
        let total = found.len();
        let shown_count = (*limit.read()).min(total);
        let rows: Vec<SettingSchema> = found.into_iter().take(shown_count).cloned().collect();
        let count_text = if q.trim().is_empty() { format!("설정 {total}개") } else { format!("{total}개 찾음") };
        let file_error = if target == SettingsScope::Workspace { settings.workspace_error.clone() } else { settings.user_error.clone() };
        let keybindings_error = settings.keybindings_error.clone();
        let no_user_file = target == SettingsScope::User && settings.user_path.is_none();
        let (json_s, keys_s) = (s.clone(), s.clone());
        rsx! {
            section { "aria-label": "설정", style: "flex:1; min-width:0; min-height:0; display:flex; flex-direction:column;",
                div { style: "display:flex; flex-wrap:wrap; align-items:center; gap:var(--sp2); padding:var(--sp3) var(--sp4); border-bottom:1px solid var(--line); flex:none;",
                    div { class: "ed-find", role: "search", style: "flex:1 1 240px; min-width:0; width:auto; height:30px; cursor:text;",
                        {icon_px("search", 14)}
                        input { key: "settings-query-{focus_seq}", style: "{FIELD}", "aria-label": "설정 검색", placeholder: "설정 검색", value: "{q}", autofocus: true,
                            oninput: move |event| { query.set(event.value()); limit.set(SETTINGS_PAGE); },
                            onkeydown: move |event| {
                                if matches!(event.key(), Key::Escape) && !query.read().is_empty() { event.stop_propagation(); query.set(String::new()); }
                            },
                        }
                    }
                    div { role: "tablist", "aria-label": "설정 범위", style: "display:flex; gap:var(--sp1);",
                        for scope in [SettingsScope::User, SettingsScope::Workspace] {
                            {
                                let on = scope == target;
                                let label = scope_label(scope);
                                rsx! {
                                    button { key: "{label}", class: if on { "ed-pj on" } else { "ed-pj" }, style: if on { CHIP_ON } else { CHIP }, role: "tab", "aria-selected": "{on}",
                                        onclick: move |_| target_sig.set(scope),
                                        "{label}"
                                    }
                                }
                            }
                        }
                    }
                    button { class: "ed-pj", style: "{CHIP}", title: "이 범위의 settings.json을 편집기로 엽니다",
                        onclick: move |_| open_file(&json_s, if target == SettingsScope::Workspace { SettingsFile::Workspace } else { SettingsFile::User }),
                        "settings.json 열기"
                    }
                    button { class: "ed-pj", style: "{CHIP}", title: "keybindings.json을 편집기로 엽니다",
                        onclick: move |_| open_file(&keys_s, SettingsFile::Keybindings),
                        "keybindings.json 열기"
                    }
                }
                if let Some(text) = file_error {
                    div { role: "alert", style: "padding:var(--sp2) var(--sp4); background:var(--c-orange-soft); color:var(--text); font-size:var(--fs-xs); white-space:normal; flex:none;", "{text}" }
                }
                if let Some(text) = keybindings_error {
                    div { role: "alert", style: "padding:var(--sp2) var(--sp4); background:var(--c-orange-soft); color:var(--text); font-size:var(--fs-xs); white-space:normal; flex:none;", "{text}" }
                }
                if no_user_file {
                    div { role: "status", style: "padding:var(--sp2) var(--sp4); color:var(--muted); font-size:var(--fs-xs); flex:none;", "이 기기에는 사용자 설정을 둘 폴더가 없습니다. 작업 영역 범위를 사용하세요." }
                }
                div { role: "status", style: "padding:var(--sp2) var(--sp4) 0; color:var(--muted); font-size:var(--fs-xs); flex:none;", "{count_text}" }
                div { role: "list", "aria-label": "설정 목록", style: "flex:1; min-height:0; overflow-y:auto; padding:var(--sp2) var(--sp4) var(--sp4); display:flex; flex-direction:column;",
                    if total == 0 {
                        div { class: "ed-empty", b { "찾는 설정이 없습니다" } p { class: "muted", "검색어를 바꿔 보세요." } }
                    }
                    for schema in rows {
                        {
                            let row = row_value(&settings.scopes, &schema, target);
                            let key = schema.key.clone();
                            let reset_key = schema.key.clone();
                            let reset_s = s.clone();
                            let editor = control(s, &schema, &row.value);
                            let note = row.overridden_by.map(|scope| format!("{} 범위의 값이 적용됩니다", scope_label(scope)));
                            rsx! {
                                div { key: "{key}", role: "listitem", style: "display:flex; flex-direction:column; gap:var(--sp1); padding:var(--sp3) 0; border-bottom:1px solid var(--line);",
                                    div { style: "display:flex; align-items:center; gap:var(--sp2); min-width:0;",
                                        span { style: "font:var(--fs-sm)/20px var(--mono); font-weight:var(--fw-medium); overflow:hidden; text-overflow:ellipsis;", "{schema.key}" }
                                        span { class: "ed-sc", "{schema.source}" }
                                        if row.set_in_target { span { class: "ed-sc", "{scope_label(target)}에서 수정됨" } }
                                        div { class: "spacer" }
                                        if row.set_in_target {
                                            button { class: "ed-pj", style: "{CHIP}", "aria-label": "{schema.key} 기본값으로 되돌리기",
                                                onclick: move |_| commit(&reset_s, &reset_key, None),
                                                "기본값으로"
                                            }
                                        }
                                    }
                                    if !schema.description.is_empty() {
                                        div { style: "color:var(--text-2); font-size:var(--fs-xs); white-space:normal;", "{schema.description}" }
                                    }
                                    {editor}
                                    if let Some(text) = note {
                                        div { role: "note", style: "color:var(--muted); font-size:var(--fs-xs);", "{text}" }
                                    }
                                }
                            }
                        }
                    }
                    if shown_count < total {
                        button { class: "ed-pj", style: "align-self:flex-start; margin-top:var(--sp3); border:1px solid var(--line);",
                            onclick: move |_| { let next = *limit.read() + SETTINGS_PAGE; limit.set(next); },
                            "더 보기 ({total - shown_count}개 남음)"
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

    #[test]
    fn fr_l31_17_user_files_sit_in_the_user_folder() {
        let files = UserFiles::in_dir(Some(PathBuf::from("/data/ember/editor/User")));
        assert_eq!(files.settings.as_deref(), Some(std::path::Path::new("/data/ember/editor/User/settings.json")));
        assert_eq!(files.keybindings.as_deref(), Some(std::path::Path::new("/data/ember/editor/User/keybindings.json")));
        assert_eq!(files.paths().len(), 2);
        assert_eq!(UserFiles::tab_path(&files.settings).as_deref(), Some("/data/ember/editor/User/settings.json"));
        let none = UserFiles::in_dir(None);
        assert!(none.paths().is_empty());
        assert_eq!(read_user_layer(&none), (SettingsLayer::default(), None));
        assert_eq!(read_keybindings(&none), (Vec::new(), None));
    }

    #[test]
    fn fr_l31_17_the_settings_tab_is_not_a_file() {
        assert!(is_settings_tab(SETTINGS_TAB));
        assert!(!is_settings_tab("/w/settings.json"));
        assert_eq!(crate::ui::editor::base_name(SETTINGS_TAB), "설정");
    }
}
