//! SPEC FR-L31.17: what the Ember 에디터 window adds to the settings model. The user's files on this
//! machine, one row of the settings view, typed values, `files.exclude` in the explorer, the
//! user's `keybindings.json` against the keymap, and the shortcut of the settings view. No renderer.

use ember_app::config::AppConfig;
use ember_app::editor_host::HostState;
use ember_app::ui::editor::{action_for_key, chord_from_key, is_implemented, tree_rows, TreeRow};
use ember_app::ui::editor_settings::{
    action_of, chord_of_binding, command_of, file_tab_path, is_settings_tab, parse_input, read_keybindings, read_user_layer,
    resolve_key, row_value, saved_settings_file, scope_label, value_text, visible_tree_rows, KeyResolution, SettingsFile,
    SettingsState, UserFiles, KEYBINDINGS_TEMPLATE, SETTINGS_TAB, SETTINGS_TEMPLATE,
};
use ember_app::ui::keymap::{Action, Chord, Keymap, Os};
use ember_app::ui::palette::{action_items, Payload};
use ember_editor::settings::{
    ember_settings, parse_keybindings, parse_layer, user_data_uri, ExcludeMatcher, SettingKind, SettingsLayer, SettingsScope,
    SettingsScopes,
};
use ember_editor::SessionUpdate;
use ember_editor_conn::remote_fs::file_type::{DIRECTORY, FILE};
use ember_editor_conn::uri::UriComponents;
use serde_json::json;

fn scopes(user: &str, workspace: &str) -> SettingsScopes {
    SettingsScopes {
        defaults: SettingsLayer::from_values(ember_settings().into_iter().map(|s| (s.key, s.default)).collect()),
        user: parse_layer(user).unwrap(),
        remote: SettingsLayer::default(),
        workspace: parse_layer(workspace).unwrap(),
    }
}

fn schema(key: &str) -> ember_editor::settings::SettingSchema {
    ember_settings().into_iter().find(|s| s.key == key).unwrap_or_else(|| panic!("{key} is one of Ember's settings"))
}

// ---- the user's files -----------------------------------------------------------------------------

#[test]
fn fr_l31_17_user_settings_live_in_the_data_folder_of_this_machine() {
    let config = AppConfig::from_values(None, Some("/data/ember".into()), None);
    let files = UserFiles::in_dir(config.editor_user_dir());
    assert_eq!(files.settings.as_deref(), Some(std::path::Path::new("/data/ember/editor/User/settings.json")));
    assert_eq!(files.keybindings.as_deref(), Some(std::path::Path::new("/data/ember/editor/User/keybindings.json")));
    // Without a data folder there are no user files, and nothing is read.
    let none = UserFiles::in_dir(AppConfig::from_values(None, None, None).editor_user_dir());
    assert_eq!(none, UserFiles::default());
}

#[test]
fn fr_l31_17_user_files_are_read_from_disk_and_a_broken_one_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let files = UserFiles::in_dir(Some(dir.path().join("editor").join("User")));
    // Nothing written yet: empty, and no error.
    assert_eq!(read_user_layer(&files), (SettingsLayer::default(), None));
    assert_eq!(read_keybindings(&files), (Vec::new(), None));

    std::fs::create_dir_all(dir.path().join("editor").join("User")).unwrap();
    std::fs::write(files.settings.as_ref().unwrap(), "{\n  // mine\n  \"editor.tabSize\": 2,\n}\n").unwrap();
    std::fs::write(files.keybindings.as_ref().unwrap(), "[ { \"key\": \"ctrl+,\", \"command\": \"workbench.action.files.save\" } ]").unwrap();
    let (layer, error) = read_user_layer(&files);
    assert_eq!(layer.values, vec![("editor.tabSize".to_owned(), json!(2))]);
    assert_eq!(error, None);
    let (rules, error) = read_keybindings(&files);
    assert_eq!(rules.len(), 1);
    assert_eq!(error, None);

    std::fs::write(files.settings.as_ref().unwrap(), "{ \"editor.tabSize\": }").unwrap();
    let (layer, error) = read_user_layer(&files);
    assert_eq!(layer, SettingsLayer::default());
    assert!(error.unwrap().starts_with("settings.json을(를) 읽을 수 없습니다"));
    std::fs::write(files.keybindings.as_ref().unwrap(), "{}").unwrap();
    let (rules, error) = read_keybindings(&files);
    assert!(rules.is_empty());
    assert!(error.unwrap().starts_with("keybindings.json을(를) 읽을 수 없습니다"));
}

#[test]
fn fr_l31_17_new_settings_files_start_from_templates_that_parse() {
    assert_eq!(parse_layer(SETTINGS_TEMPLATE).unwrap(), SettingsLayer::default());
    assert!(parse_keybindings(KEYBINDINGS_TEMPLATE).unwrap().is_empty());
}

// ---- which file a save belongs to ---------------------------------------------------------------

fn state() -> SettingsState {
    let files = UserFiles::in_dir(Some("/data/ember/editor/User".into()));
    SettingsState {
        user_path: UserFiles::tab_path(&files.settings),
        keybindings_path: UserFiles::tab_path(&files.keybindings),
        workspace_path: Some("/w/p/.vscode/settings.json".to_owned()),
        ..SettingsState::default()
    }
}

#[test]
fn fr_l31_17_a_saved_settings_file_is_recognised_by_path_and_scheme() {
    let st = state();
    let saved = |uri: UriComponents| saved_settings_file(&st, &SessionUpdate::Saved { uri });
    assert_eq!(saved(user_data_uri(std::path::Path::new("/data/ember/editor/User/settings.json"))), Some(SettingsFile::User));
    assert_eq!(saved(user_data_uri(std::path::Path::new("/data/ember/editor/User/keybindings.json"))), Some(SettingsFile::Keybindings));
    assert_eq!(saved(UriComponents::remote("h", "/w/p/.vscode/settings.json")), Some(SettingsFile::Workspace));
    assert_eq!(saved(UriComponents::remote("h", "/w/p/src/settings.json")), None);
    // A file on the server with the path of the user's file is not the user's file.
    assert_eq!(saved(UriComponents::remote("h", "/data/ember/editor/User/settings.json")), None);
    // Another update is not a save.
    assert_eq!(saved_settings_file(&st, &SessionUpdate::Closed { uri: UriComponents::remote("h", "/w/p/.vscode/settings.json") }), None);

    assert_eq!(file_tab_path(&st, SettingsFile::User).as_deref(), Some("/data/ember/editor/User/settings.json"));
    assert_eq!(file_tab_path(&st, SettingsFile::Workspace).as_deref(), Some("/w/p/.vscode/settings.json"));
    assert_eq!(file_tab_path(&SettingsState::default(), SettingsFile::Keybindings), None);
}

#[test]
fn fr_l31_17_the_host_starts_with_no_settings_and_the_settings_tab_is_not_a_file() {
    let host = HostState::default();
    assert!(host.settings.catalogue.is_empty());
    assert!(host.settings.keybindings.is_empty());
    assert!(host.settings.exclude_matcher().is_empty());
    assert_eq!(host.settings.editor(None).tab_size, 4);
    assert!(is_settings_tab(SETTINGS_TAB));
    assert!(!is_settings_tab("/w/p/.vscode/settings.json"));
}

// ---- one row of the view --------------------------------------------------------------------------

#[test]
fn fr_l31_17_a_row_shows_the_value_in_effect_and_where_it_is_set() {
    let s = scopes(r#"{ "editor.tabSize": 2 }"#, r#"{ "editor.tabSize": 8, "editor.wordWrap": "on" }"#);
    let tab = schema("editor.tabSize");
    // Editing the user's file: the value in effect is the workspace's, and the row says so.
    let user = row_value(&s, &tab, SettingsScope::User);
    assert_eq!(user.value, json!(8));
    assert!(user.set_in_target);
    assert_eq!(user.overridden_by, Some(SettingsScope::Workspace));
    let workspace = row_value(&s, &tab, SettingsScope::Workspace);
    assert_eq!(workspace.value, json!(8));
    assert!(workspace.set_in_target);
    assert_eq!(workspace.overridden_by, None);
    // Set only in the workspace: the user's row can not reset it.
    let wrap = row_value(&s, &schema("editor.wordWrap"), SettingsScope::User);
    assert_eq!(wrap.value, json!("on"));
    assert!(!wrap.set_in_target);
    assert_eq!(wrap.overridden_by, Some(SettingsScope::Workspace));
    // Set nowhere: the default, nothing to reset, nothing stronger.
    let save = row_value(&s, &schema("editor.formatOnSave"), SettingsScope::User);
    assert_eq!(save.value, json!(false));
    assert!(!save.set_in_target);
    assert_eq!(save.overridden_by, None);
    // A setting no scope knows shows its registered default.
    let unknown = ember_editor::settings::SettingSchema { key: "x.y".into(), default: json!("d"), ..schema("editor.wordWrap") };
    assert_eq!(row_value(&SettingsScopes::default(), &unknown, SettingsScope::User).value, json!("d"));
}

#[test]
fn fr_l31_17_typed_values_are_checked_against_the_kind() {
    assert_eq!(parse_input(&SettingKind::Integer, " 2 "), Ok(json!(2)));
    assert_eq!(parse_input(&SettingKind::Integer, "-1"), Ok(json!(-1)));
    assert!(parse_input(&SettingKind::Integer, "2.5").is_err());
    assert!(parse_input(&SettingKind::Integer, "two").is_err());
    assert_eq!(parse_input(&SettingKind::Number, "1.5"), Ok(json!(1.5)));
    assert_eq!(parse_input(&SettingKind::Number, "3"), Ok(json!(3)));
    assert!(parse_input(&SettingKind::Number, "NaN").is_err());
    assert_eq!(parse_input(&SettingKind::String, " /usr/bin/git "), Ok(json!("/usr/bin/git")));
    assert_eq!(parse_input(&SettingKind::String, ""), Ok(json!("")));
    let kind = SettingKind::Enum(vec!["off".into(), "on".into()]);
    assert_eq!(parse_input(&kind, "on"), Ok(json!("on")));
    assert!(parse_input(&kind, "maybe").is_err());
    assert_eq!(parse_input(&SettingKind::Boolean, "true"), Ok(json!(true)));
    assert!(parse_input(&SettingKind::Other, "{}").is_err());

    assert_eq!(value_text(&json!("off")), "off");
    assert_eq!(value_text(&json!(4)), "4");
    assert_eq!(value_text(&json!(null)), "");
    assert_eq!(value_text(&json!({ "a": true })), "{\"a\":true}");
    assert_eq!(scope_label(SettingsScope::User), "사용자");
    assert_eq!(scope_label(SettingsScope::Workspace), "작업 영역");
}

// ---- files.exclude in the explorer ----------------------------------------------------------------

#[test]
fn fr_l31_17_files_exclude_hides_explorer_rows() {
    let mut listings = std::collections::HashMap::new();
    listings.insert(
        "/w/p".to_owned(),
        vec![(".git".to_owned(), DIRECTORY), ("src".to_owned(), DIRECTORY), ("target".to_owned(), DIRECTORY), (".DS_Store".to_owned(), FILE), ("a.rs".to_owned(), FILE)],
    );
    listings.insert("/w/p/src".to_owned(), vec![("main.rs".to_owned(), FILE), ("gen.pyc".to_owned(), FILE)]);
    listings.insert("/w/p/target".to_owned(), vec![("debug".to_owned(), DIRECTORY)]);
    let expanded: std::collections::HashSet<String> = ["/w/p/src".to_owned(), "/w/p/target".to_owned()].into_iter().collect();
    let rows = tree_rows("/w/p", &listings, &expanded);
    let names = |rows: &[TreeRow]| -> Vec<String> { rows.iter().map(|r| r.path.trim_start_matches("/w/p/").to_owned()).collect() };
    assert_eq!(names(&rows), vec![".git", "src", "src/gen.pyc", "src/main.rs", "target", "target/debug", ".DS_Store", "a.rs"]);

    // Code-OSS's defaults hide the version control folder and the Finder file.
    let defaults = SettingsState { scopes: scopes("", ""), ..SettingsState::default() };
    assert_eq!(names(&visible_tree_rows(rows.clone(), "/w/p", &defaults.exclude_matcher())), vec!["src", "src/gen.pyc", "src/main.rs", "target", "target/debug", "a.rs"]);

    // The workspace adds patterns and switches a default off; a hidden folder takes its rows with it.
    let custom = SettingsState {
        scopes: scopes("", r#"{ "files.exclude": { "**/.git": false, "target": true, "**/*.pyc": true } }"#),
        ..SettingsState::default()
    };
    assert_eq!(names(&visible_tree_rows(rows.clone(), "/w/p/", &custom.exclude_matcher())), vec![".git", "src", "src/main.rs", "a.rs"]);

    // No pattern: every row stays.
    assert_eq!(visible_tree_rows(rows.clone(), "/w/p", &ExcludeMatcher::default()), rows);
}

// ---- keybindings.json -----------------------------------------------------------------------------

fn keys(key: &str, ctrl: bool, shift: bool, alt: bool, meta: bool) -> Chord {
    Chord::Keys { ctrl, shift, alt, meta, key: key.to_owned() }
}

#[test]
fn fr_l31_17_binding_keys_read_as_the_chords_the_window_sees() {
    assert_eq!(chord_of_binding("cmd+shift+p"), Some(keys("P", false, true, false, true)));
    assert_eq!(chord_of_binding("Ctrl+,"), Some(keys(",", true, false, false, false)));
    assert_eq!(chord_of_binding("alt+f12"), Some(keys("F12", false, false, true, false)));
    assert_eq!(chord_of_binding("escape"), Some(keys("Esc", false, false, false, false)));
    assert_eq!(chord_of_binding("ctrl++"), Some(keys("+", true, false, false, false)));
    assert_eq!(chord_of_binding("shift+up"), Some(keys("ArrowUp", false, true, false, false)));
    assert_eq!(chord_of_binding("win+s"), Some(keys("S", false, false, false, true)));
    // The chord is the one a key press makes, so the two compare equal.
    assert_eq!(chord_of_binding("ctrl+shift+p"), Some(chord_from_key("p", true, true, false, false)));
    // Two strokes, an unknown modifier and an unknown key are not taken.
    assert_eq!(chord_of_binding("ctrl+k ctrl+s"), None);
    assert_eq!(chord_of_binding("hyper+s"), None);
    assert_eq!(chord_of_binding("ctrl+numpad_add"), None);
    assert_eq!(chord_of_binding(""), None);
}

#[test]
fn fr_l31_17_commands_and_actions_map_both_ways() {
    for action in Action::ALL {
        if let Some(command) = command_of(action) {
            assert_eq!(action_of(command), Some(action), "{command}");
        }
    }
    assert_eq!(action_of("workbench.action.files.save"), Some(Action::Save));
    assert_eq!(action_of("workbench.action.openSettings"), Some(Action::Settings));
    assert_eq!(action_of("workbench.action.openSettingsJson"), Some(Action::SettingsJson));
    assert_eq!(action_of("workbench.action.openGlobalKeybindingsFile"), Some(Action::KeybindingsJson));
    assert_eq!(action_of("rust-analyzer.run"), None);
}

#[test]
fn fr_l31_17_user_keybindings_are_read_on_top_of_the_keymap() {
    let rules = parse_keybindings(
        r#"[
            { "key": "ctrl+alt+s", "command": "workbench.action.files.save" },
            { "key": "ctrl+r", "command": "rust-analyzer.run" },
            { "key": "ctrl+s", "command": "-workbench.action.files.save" },
            { "key": "ctrl+w", "command": "workbench.action.files.save", "when": "editorTextFocus" },
            { "key": "ctrl+k ctrl+s", "command": "workbench.action.openSettings" },
            { "key": "f6", "command": "workbench.action.gotoLine" },
            { "key": "f6", "command": "-workbench.action.gotoLine" },
            { "key": "f7", "command": "workbench.action.gotoLine" },
            { "key": "f7", "command": "workbench.action.showCommands" }
        ]"#,
    )
    .unwrap();
    let os = Os::Other;
    let press = |key: &str, ctrl: bool, alt: bool| {
        let chord = chord_from_key(key, ctrl, false, alt, false);
        let default = action_for_key(Keymap::VsCode, os, key, ctrl, false, alt, false);
        resolve_key(&rules, &chord, default)
    };
    // A new key for an action, and a command Ember has no action for.
    assert_eq!(press("s", true, true), KeyResolution::Action(Action::Save));
    assert_eq!(press("r", true, false), KeyResolution::Command("rust-analyzer.run".to_owned()));
    // The default key was removed.
    assert_eq!(press("s", true, false), KeyResolution::Unbound);
    // An entry with a `when` clause is not applied: Ctrl+W still closes the tab.
    assert_eq!(press("w", true, false), KeyResolution::Default);
    assert_eq!(action_for_key(Keymap::VsCode, os, "w", true, false, false, false), Some(Action::Close));
    // A key the file does not name is the keymap's.
    assert_eq!(press("p", true, false), KeyResolution::Default);
    // Later entries win: bound then removed, and bound twice.
    assert_eq!(press("F6", false, false), KeyResolution::Unbound);
    assert_eq!(press("F7", false, false), KeyResolution::Action(Action::Palette));
    // Removing a command the chord does not run changes nothing.
    let other = parse_keybindings(r#"[ { "key": "ctrl+s", "command": "-workbench.action.gotoLine" } ]"#).unwrap();
    assert_eq!(resolve_key(&other, &chord_from_key("s", true, false, false, false), Some(Action::Save)), KeyResolution::Default);
    assert_eq!(resolve_key(&[], &chord_from_key("s", true, false, false, false), Some(Action::Save)), KeyResolution::Default);
}

// ---- the shortcut and the palette -----------------------------------------------------------------

#[test]
fn fr_l31_17_the_settings_view_has_its_code_oss_shortcut_and_palette_rows() {
    assert_eq!(Keymap::VsCode.hint(Action::Settings, Os::Mac), "\u{2318},");
    assert_eq!(Keymap::VsCode.hint(Action::Settings, Os::Other), "Ctrl+,");
    assert_eq!(Keymap::JetBrains.hint(Action::Settings, Os::Mac), "\u{2318},");
    assert_eq!(Keymap::JetBrains.hint(Action::Settings, Os::Other), "Ctrl+Alt+S");
    assert_eq!(action_for_key(Keymap::VsCode, Os::Mac, ",", false, false, false, true), Some(Action::Settings));
    assert_eq!(action_for_key(Keymap::VsCode, Os::Other, ",", true, false, false, false), Some(Action::Settings));
    assert_eq!(action_for_key(Keymap::JetBrains, Os::Other, "s", true, false, true, false), Some(Action::Settings));
    // The two files have no default shortcut and are reached from the palette and the view.
    for action in [Action::Settings, Action::SettingsJson, Action::KeybindingsJson] {
        assert!(is_implemented(action), "{action:?}");
        let items = action_items(Keymap::VsCode, Os::Mac);
        let item = items.iter().find(|i| i.payload == Payload::Action(action)).unwrap_or_else(|| panic!("{action:?} is a palette row"));
        assert!(item.enabled);
        assert!(!item.title.is_empty());
    }
    assert_eq!(Keymap::VsCode.hint(Action::SettingsJson, Os::Mac), "");
    assert_eq!(Keymap::VsCode.hint(Action::KeybindingsJson, Os::Other), "");
}
