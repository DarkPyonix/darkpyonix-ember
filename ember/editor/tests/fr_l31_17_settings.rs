//! SPEC FR-L31.17: settings of the Ember 에디터 window. Reading JSONC, merging the scopes, the
//! configuration the extension host receives, the settings Ember honours, editing one key of a
//! settings file, the settings list and `keybindings.json`.

use std::path::Path;
use std::time::Instant;

use ember_editor::engine::ReadPurpose;
use ember_editor::languages::LanguageRegistry;
use ember_editor::settings::{
    contributed_settings, ember_settings, override_identifiers, parse_jsonc, parse_keybindings, parse_layer, search_settings,
    set_setting, setting_catalogue, strip_jsonc, user_data_path, user_data_uri, AutoSave, EditorSettings, ExcludeMatcher,
    SettingKind, SettingsLayer, SettingsScope, SettingsScopes, WordWrap, EMBER_SOURCE, SCHEME_USER_DATA,
};
use ember_editor::{CoreOptions, Effect, Input, SessionCore, SessionUpdate};
use ember_editor_conn::exthost::accept_configuration_changed;
use ember_editor_conn::rpc::Arg;
use ember_editor_conn::uri::UriComponents;
use serde_json::{json, Value};

fn layer(text: &str) -> SettingsLayer {
    parse_layer(text).expect("the settings parse")
}

fn scopes(defaults: &str, user: &str, remote: &str, workspace: &str) -> SettingsScopes {
    SettingsScopes { defaults: layer(defaults), user: layer(user), remote: layer(remote), workspace: layer(workspace) }
}

// ---- JSONC ------------------------------------------------------------------------------------

#[test]
fn fr_l31_17_jsonc_reads_comments_and_trailing_commas() {
    let text = r#"// the user's settings
{
    "editor.tabSize": 2, // two columns
    /* a block
       comment */
    "files.exclude": {
        "**/target": true,
    },
    "list": [1, 2, ],
}
"#;
    let value = parse_jsonc(text).unwrap();
    assert_eq!(value, json!({ "editor.tabSize": 2, "files.exclude": { "**/target": true }, "list": [1, 2] }));
}

#[test]
fn fr_l31_17_jsonc_keeps_comment_marks_and_commas_inside_strings() {
    let text = r#"{ "a": "http://x/y", "b": "/* not a comment */", "c": "x,}", "d": "quote \" // still text" }"#;
    let value = parse_jsonc(text).unwrap();
    assert_eq!(value["a"], "http://x/y");
    assert_eq!(value["b"], "/* not a comment */");
    assert_eq!(value["c"], "x,}");
    assert_eq!(value["d"], "quote \" // still text");
}

#[test]
fn fr_l31_17_jsonc_empty_file_is_an_empty_object() {
    assert_eq!(parse_jsonc("").unwrap(), json!({}));
    assert_eq!(parse_jsonc("  \n// only a comment\n").unwrap(), json!({}));
    assert_eq!(parse_jsonc("\u{feff}{}").unwrap(), json!({}));
}

#[test]
fn fr_l31_17_jsonc_error_points_into_the_original_text() {
    // The comment is blanked, not removed, so the mistake stays on line 3.
    let text = "{\n  // note\n  \"a\": oops\n}";
    let error = parse_jsonc(text).unwrap_err();
    assert_eq!(error.line, 3);
    assert_eq!(strip_jsonc(text).lines().count(), text.lines().count());
}

#[test]
fn fr_l31_17_language_blocks_are_read_as_overrides() {
    assert_eq!(override_identifiers("[rust]"), Some(vec!["rust".to_owned()]));
    assert_eq!(override_identifiers("[javascript][typescript]"), Some(vec!["javascript".to_owned(), "typescript".to_owned()]));
    assert_eq!(override_identifiers("editor.tabSize"), None);
    assert_eq!(override_identifiers("[]"), None);
    assert_eq!(override_identifiers("[rust"), None);

    let l = layer(r#"{ "editor.tabSize": 4, "[rust][toml]": { "editor.tabSize": 2 }, "[go]": 3 }"#);
    assert_eq!(l.values, vec![("editor.tabSize".to_owned(), json!(4))]);
    // A language key whose value is not an object says nothing.
    assert_eq!(l.overrides.len(), 1);
    assert_eq!(l.overrides[0].identifiers, vec!["rust".to_owned(), "toml".to_owned()]);
    assert_eq!(l.overrides[0].values, vec![("editor.tabSize".to_owned(), json!(2))]);
}

#[test]
fn fr_l31_17_a_settings_file_must_be_an_object() {
    assert!(parse_layer("[1, 2]").is_err());
    assert!(parse_layer("{ \"a\": }").is_err());
}

// ---- merging scopes -----------------------------------------------------------------------------

#[test]
fn fr_l31_17_scopes_merge_default_user_remote_workspace() {
    let s = scopes(
        r#"{ "a": "default", "b": "default", "c": "default", "d": "default" }"#,
        r#"{ "b": "user", "c": "user", "d": "user" }"#,
        r#"{ "c": "remote", "d": "remote" }"#,
        r#"{ "d": "workspace" }"#,
    );
    assert_eq!(s.value("a", None), Some(json!("default")));
    assert_eq!(s.value("b", None), Some(json!("user")));
    assert_eq!(s.value("c", None), Some(json!("remote")));
    assert_eq!(s.value("d", None), Some(json!("workspace")));
    assert_eq!(s.value("missing", None), None);
    assert_eq!(s.winning_scope("a"), Some(SettingsScope::Default));
    assert_eq!(s.winning_scope("c"), Some(SettingsScope::Remote));
    assert_eq!(s.winning_scope("d"), Some(SettingsScope::Workspace));
    assert_eq!(s.winning_scope("missing"), None);
}

#[test]
fn fr_l31_17_a_language_block_wins_over_every_plain_key() {
    let s = scopes(
        r#"{ "editor.tabSize": 4 }"#,
        r#"{ "[rust]": { "editor.tabSize": 2 } }"#,
        "",
        r#"{ "editor.tabSize": 8 }"#,
    );
    // The user's Rust block beats the workspace's plain key, as in Code-OSS.
    assert_eq!(s.value("editor.tabSize", Some("rust")), Some(json!(2)));
    assert_eq!(s.value("editor.tabSize", Some("python")), Some(json!(8)));
    assert_eq!(s.value("editor.tabSize", None), Some(json!(8)));

    // Among language blocks the stronger scope wins.
    let s = scopes("", r#"{ "[rust]": { "editor.tabSize": 2 } }"#, "", r#"{ "[rust]": { "editor.tabSize": 3 } }"#);
    assert_eq!(s.value("editor.tabSize", Some("rust")), Some(json!(3)));
}

#[test]
fn fr_l31_17_object_values_merge_key_by_key() {
    let s = scopes(
        r#"{ "files.exclude": { "**/.git": true, "**/.DS_Store": true } }"#,
        r#"{ "files.exclude": { "**/target": true } }"#,
        "",
        r#"{ "files.exclude": { "**/.git": false, "**/node_modules": true } }"#,
    );
    assert_eq!(
        s.value("files.exclude", None),
        Some(json!({ "**/.git": false, "**/.DS_Store": true, "**/target": true, "**/node_modules": true }))
    );
    // A value that is not an object replaces.
    let s = scopes(r#"{ "x": { "a": 1 } }"#, r#"{ "x": 5 }"#, "", "");
    assert_eq!(s.value("x", None), Some(json!(5)));
}

#[test]
fn fr_l31_17_change_lists_the_keys_and_language_keys_that_differ() {
    let before = scopes(r#"{ "a": 1 }"#, r#"{ "b": 1, "c": 1, "[rust]": { "k": 1, "same": 1 } }"#, "", r#"{ "w": 1 }"#);
    let after = scopes(r#"{ "a": 1 }"#, r#"{ "b": 2, "[rust]": { "k": 2, "same": 1 }, "[go]": { "g": 1 } }"#, "", r#"{ "w": 1, "n": 1 }"#);
    let change = after.change_from(&before);
    assert_eq!(change.keys, vec!["[go]", "[rust]", "b", "c", "n"]);
    assert_eq!(
        change.overrides,
        vec![("go".to_owned(), vec!["g".to_owned()]), ("rust".to_owned(), vec!["k".to_owned()])]
    );
    assert!(after.change_from(&after).is_empty());
}

// ---- the configuration payload --------------------------------------------------------------------

#[test]
fn fr_l31_17_configuration_payload_places_each_scope_in_its_model() {
    let s = scopes(
        r#"{ "editor.tabSize": 4 }"#,
        r#"{ "editor.tabSize": 2, "[rust]": { "editor.tabSize": 8 } }"#,
        r#"{ "git.path": "/usr/bin/git" }"#,
        r#"{ "editor.insertSpaces": false }"#,
    );
    let data = s.configuration_data(&[("editor.tabSize".to_owned(), Some(6)), ("git.path".to_owned(), Some(2))]);
    assert_eq!(data["defaults"]["contents"], json!({ "editor": { "tabSize": 4 } }));
    assert_eq!(data["userLocal"]["contents"]["editor"]["tabSize"], 2);
    assert_eq!(data["userLocal"]["overrides"][0]["identifiers"], json!(["rust"]));
    assert_eq!(data["userLocal"]["overrides"][0]["contents"], json!({ "editor": { "tabSize": 8 } }));
    assert_eq!(data["userRemote"]["contents"], json!({ "git": { "path": "/usr/bin/git" } }));
    assert_eq!(data["workspace"]["contents"], json!({ "editor": { "insertSpaces": false } }));
    assert_eq!(data["workspace"]["keys"], json!(["editor.insertSpaces"]));
    assert_eq!(data["configurationScopes"], json!([["editor.tabSize", 6], ["git.path", 2]]));
    for unused in ["policy", "application"] {
        assert_eq!(data[unused], json!({ "contents": {}, "keys": [], "overrides": [] }));
    }
    assert_eq!(data["folders"], json!([]));
}

#[test]
fn fr_l31_17_a_changed_scope_becomes_one_configuration_change_call() {
    let before = scopes(r#"{ "editor.tabSize": 4 }"#, "", "", "");
    let mut after = before.clone();
    *after.layer_mut(SettingsScope::Workspace) = layer(r#"{ "editor.tabSize": 2, "[rust]": { "editor.formatOnSave": true } }"#);
    let change = after.change_from(&before);
    let data = after.configuration_data(&[("editor.tabSize".to_owned(), Some(6))]);
    let call = accept_configuration_changed(&data, &change.keys, &change.overrides);
    assert_eq!(call.method, "$acceptConfigurationChanged");
    let sent: Vec<Value> = call
        .args
        .iter()
        .map(|a| match a {
            Arg::Json(v) => v.clone(),
            _ => panic!("the arguments are JSON"),
        })
        .collect();
    assert_eq!(sent[0]["workspace"]["contents"]["editor"]["tabSize"], 2);
    assert_eq!(sent[1], json!({ "keys": ["[rust]", "editor.tabSize"], "overrides": [["rust", ["editor.formatOnSave"]]] }));
}

// ---- the settings Ember honours -----------------------------------------------------------------

#[test]
fn fr_l31_17_editor_settings_resolve_with_code_oss_defaults() {
    let defaults = SettingsScopes {
        defaults: SettingsLayer::from_values(ember_settings().into_iter().map(|s| (s.key, s.default)).collect()),
        ..SettingsScopes::default()
    };
    let resolved = EditorSettings::resolve(&defaults, None);
    assert_eq!(resolved.tab_size, 4);
    assert!(resolved.insert_spaces);
    assert_eq!(resolved.word_wrap, WordWrap::Off);
    assert_eq!(resolved.auto_save, AutoSave::Off);
    assert_eq!(resolved.auto_save_delay_ms, 1000);
    assert!(!resolved.format_on_save);
    assert_eq!(resolved.files_exclude, vec!["**/.DS_Store", "**/.git", "**/.hg", "**/.svn", "**/Thumbs.db"]);
    // No settings at all gives the same editor options and hides nothing.
    let bare = EditorSettings::resolve(&SettingsScopes::default(), None);
    assert_eq!(bare, EditorSettings { files_exclude: Vec::new(), ..resolved });
}

#[test]
fn fr_l31_17_editor_settings_follow_the_scopes_and_the_language() {
    let s = scopes(
        r#"{ "files.exclude": { "**/.git": true } }"#,
        r#"{
            "editor.tabSize": 2,
            "editor.insertSpaces": false,
            "editor.wordWrap": "on",
            "files.autoSave": "afterDelay",
            "files.autoSaveDelay": 250,
            "editor.formatOnSave": true,
            "[go]": { "editor.tabSize": 8, "editor.formatOnSave": false }
        }"#,
        "",
        r#"{ "files.exclude": { "**/.git": false, "**/target": true, "**/*.js": { "when": "$(basename).ts" } } }"#,
    );
    let plain = EditorSettings::resolve(&s, Some("rust"));
    assert_eq!(plain.tab_size, 2);
    assert!(!plain.insert_spaces);
    assert_eq!(plain.word_wrap, WordWrap::On);
    assert_eq!(plain.auto_save, AutoSave::AfterDelay);
    assert_eq!(plain.auto_save_delay_ms, 250);
    assert!(plain.format_on_save);
    // `false` switches the default's pattern off; a `when` object hides nothing.
    assert_eq!(plain.files_exclude, vec!["**/target"]);
    let go = EditorSettings::resolve(&s, Some("go"));
    assert_eq!(go.tab_size, 8);
    assert!(!go.format_on_save);
}

#[test]
fn fr_l31_17_editor_settings_ignore_values_of_the_wrong_type() {
    let s = scopes(
        "",
        r#"{ "editor.tabSize": "wide", "editor.insertSpaces": 1, "editor.wordWrap": true, "files.autoSave": "sometimes", "files.exclude": ["x"] }"#,
        "",
        "",
    );
    assert_eq!(EditorSettings::resolve(&s, None), EditorSettings::default());
    let s = scopes("", r#"{ "editor.tabSize": 0 }"#, "", "");
    assert_eq!(EditorSettings::resolve(&s, None).tab_size, 1);
    let s = scopes("", r#"{ "editor.tabSize": 4000 }"#, "", "");
    assert_eq!(EditorSettings::resolve(&s, None).tab_size, 100);
}

#[test]
fn fr_l31_17_documents_open_with_the_tab_size_of_their_language() {
    let t0 = Instant::now();
    let languages = LanguageRegistry::from_extensions(&[json!({ "contributes": { "languages": [
        { "id": "json", "extensions": [".json"] },
        { "id": "go", "extensions": [".go"] }
    ] } })]);
    let mut core = SessionCore::new(languages, CoreOptions::default(), t0);
    core.set_settings(scopes("", r#"{ "editor.tabSize": 2, "[go]": { "editor.tabSize": 8 } }"#, "", ""));
    let mut tab_width_of = |path: &str| {
        let uri = UriComponents::remote("127.0.0.1:9", path);
        core.handle(t0, Input::Open { uri: uri.clone() });
        let effects = core.handle(t0, Input::FileRead { uri, purpose: ReadPurpose::Open, result: Ok(b"x\n".to_vec()) });
        effects
            .iter()
            .find_map(|e| match e {
                Effect::Ui(SessionUpdate::Opened { tab_width, .. }) => Some(*tab_width),
                _ => None,
            })
            .expect("Opened")
    };
    assert_eq!(tab_width_of("/w/a.json"), 2);
    assert_eq!(tab_width_of("/w/main.go"), 8);
}

// ---- files.exclude ------------------------------------------------------------------------------

#[test]
fn fr_l31_17_files_exclude_globs_match_as_in_code_oss() {
    let m = ExcludeMatcher::new(&["**/.git", "**/*.{pyc,pyo}", "build", "docs/**/draft-?.md", "**/[Tt]humbs.db", "*.log"]);
    assert!(m.matches(".git"));
    assert!(m.matches("vendor/lib/.git"));
    assert!(!m.matches(".gitignore"));
    assert!(m.matches("a/b/mod.pyc"));
    assert!(m.matches("mod.pyo"));
    assert!(!m.matches("mod.py"));
    // A pattern without `**` is anchored at the workspace folder.
    assert!(m.matches("build"));
    assert!(!m.matches("sub/build"));
    assert!(m.matches("top.log"));
    assert!(!m.matches("sub/top.log"));
    assert!(m.matches("docs/draft-1.md"));
    assert!(m.matches("docs/a/b/draft-x.md"));
    assert!(!m.matches("docs/draft-10.md"));
    assert!(m.matches("x/thumbs.db"));
    assert!(m.matches("Thumbs.db"));
    assert!(!m.matches(""));
}

#[test]
fn fr_l31_17_files_exclude_hides_everything_below_a_hidden_folder() {
    let m = ExcludeMatcher::new(&["**/node_modules"]);
    assert!(m.hides("node_modules"));
    assert!(m.hides("web/node_modules/react/index.js"));
    assert!(!m.matches("web/node_modules/react/index.js"));
    assert!(!m.hides("web/src/index.js"));
    let none = ExcludeMatcher::new::<&str>(&[]);
    assert!(none.is_empty());
    assert!(!none.hides("anything"));
}

// ---- editing one key ----------------------------------------------------------------------------

#[test]
fn fr_l31_17_set_setting_keeps_comments_and_layout() {
    let text = "{\n  // indent\n  \"editor.tabSize\": 4, // four\n  \"files.autoSave\": \"off\"\n}\n";
    let out = set_setting(text, "editor.tabSize", Some(&json!(2))).unwrap();
    assert_eq!(out, "{\n  // indent\n  \"editor.tabSize\": 2, // four\n  \"files.autoSave\": \"off\"\n}\n");
    assert_eq!(parse_jsonc(&out).unwrap()["editor.tabSize"], 2);
}

#[test]
fn fr_l31_17_set_setting_adds_a_key_with_the_files_indent() {
    let text = "{\n  \"a\": 1 // last\n}\n";
    let out = set_setting(text, "b", Some(&json!(true))).unwrap();
    assert_eq!(out, "{\n  \"a\": 1, // last\n  \"b\": true\n}\n");
    // A trailing comma that is already there is not doubled.
    let out = set_setting("{\n\t\"a\": 1,\n}\n", "b", Some(&json!("x"))).unwrap();
    assert_eq!(out, "{\n\t\"a\": 1,\n\t\"b\": \"x\"\n}\n");
    // An object value is laid out over lines, one indent deeper.
    let out = set_setting("{\n  \"a\": 1\n}\n", "files.exclude", Some(&json!({ "**/target": true }))).unwrap();
    assert_eq!(out, "{\n  \"a\": 1,\n  \"files.exclude\": {\n    \"**/target\": true\n  }\n}\n");
}

#[test]
fn fr_l31_17_set_setting_starts_an_empty_file() {
    assert_eq!(set_setting("", "editor.tabSize", Some(&json!(2))).unwrap(), "{\n    \"editor.tabSize\": 2\n}\n");
    assert_eq!(set_setting("{}", "a", Some(&json!(1))).unwrap(), "{\n    \"a\": 1\n}");
    assert_eq!(set_setting("", "a", None).unwrap(), "");
    let out = set_setting("// mine\n{\n}\n", "a", Some(&json!(1))).unwrap();
    assert_eq!(out, "// mine\n{\n    \"a\": 1\n}\n");
}

#[test]
fn fr_l31_17_set_setting_removes_a_key_and_its_comma() {
    let text = "{\n  \"a\": 1,\n  \"b\": 2,\n  \"c\": 3\n}\n";
    assert_eq!(set_setting(text, "a", None).unwrap(), "{\n  \"b\": 2,\n  \"c\": 3\n}\n");
    assert_eq!(set_setting(text, "b", None).unwrap(), "{\n  \"a\": 1,\n  \"c\": 3\n}\n");
    // The last key has no comma of its own: the one before it goes.
    assert_eq!(set_setting(text, "c", None).unwrap(), "{\n  \"a\": 1,\n  \"b\": 2\n}\n");
    assert_eq!(set_setting(text, "missing", None).unwrap(), text);
    assert_eq!(set_setting("{\n  \"a\": 1\n}\n", "a", None).unwrap(), "{\n}\n");
    for key in ["a", "b", "c"] {
        let out = set_setting(text, key, None).unwrap();
        assert!(parse_jsonc(&out).unwrap().get(key).is_none());
    }
}

#[test]
fn fr_l31_17_set_setting_keeps_windows_line_endings() {
    let out = set_setting("{\r\n  \"a\": 1\r\n}\r\n", "b", Some(&json!(2))).unwrap();
    assert_eq!(out, "{\r\n  \"a\": 1,\r\n  \"b\": 2\r\n}\r\n");
    assert_eq!(set_setting(&out, "b", None).unwrap(), "{\r\n  \"a\": 1\r\n}\r\n");
}

#[test]
fn fr_l31_17_set_setting_refuses_a_broken_file() {
    // A file the person broke by hand is never overwritten.
    assert!(set_setting("{ \"a\": 1, \"b\": }", "a", Some(&json!(2))).is_err());
    assert!(set_setting("[1]", "a", Some(&json!(2))).is_err());
    assert!(set_setting("{ \"a\": 1", "a", Some(&json!(2))).is_err());
}

// ---- the settings list --------------------------------------------------------------------------

fn extensions() -> Vec<Value> {
    vec![
        json!({
            "name": "json-language-features",
            "displayName": "JSON Language Features",
            "contributes": { "configuration": {
                "title": "JSON",
                "properties": {
                    "json.validate.enable": { "type": "boolean", "default": true, "scope": "resource", "description": "Enable JSON validation." },
                    "json.maxItemsComputed": { "type": "integer", "default": 5000, "markdownDescription": "Outline symbol limit." },
                    "json.trace.server": { "type": "string", "enum": ["off", "messages", "verbose"], "default": "off", "scope": "window" },
                    "json.schemas": { "type": "array", "default": [] }
                }
            } }
        }),
        json!({
            "identifier": { "value": "ms.go" },
            "contributes": { "configuration": [
                { "properties": { "go.gopath": { "type": ["string", "null"], "default": null, "scope": "machine-overridable" } } },
                { "properties": { "go.lintFlags": { "type": ["string", "array"] }, "editor.tabSize": { "type": "number", "default": 9 } } }
            ] }
        }),
        json!({ "name": "no-settings" }),
    ]
}

#[test]
fn fr_l31_17_contributed_settings_are_read_from_the_manifests() {
    let all = contributed_settings(&extensions());
    let find = |key: &str| all.iter().find(|s| s.key == key).unwrap_or_else(|| panic!("{key} is listed"));
    assert_eq!(all.len(), 7);
    let validate = find("json.validate.enable");
    assert_eq!(validate.kind, SettingKind::Boolean);
    assert_eq!(validate.default, json!(true));
    assert_eq!(validate.scope, Some(5));
    assert_eq!(validate.description, "Enable JSON validation.");
    assert_eq!(validate.source, "JSON Language Features");
    assert_eq!(find("json.maxItemsComputed").kind, SettingKind::Integer);
    assert_eq!(find("json.maxItemsComputed").description, "Outline symbol limit.");
    assert_eq!(find("json.maxItemsComputed").scope, None);
    assert_eq!(
        find("json.trace.server").kind,
        SettingKind::Enum(vec!["off".to_owned(), "messages".to_owned(), "verbose".to_owned()])
    );
    assert_eq!(find("json.trace.server").scope, Some(4));
    assert_eq!(find("json.schemas").kind, SettingKind::Other);
    // A nullable string is a string; two real types are edited in settings.json.
    assert_eq!(find("go.gopath").kind, SettingKind::String);
    assert_eq!(find("go.gopath").scope, Some(7));
    assert_eq!(find("go.gopath").source, "ms.go");
    assert_eq!(find("go.lintFlags").kind, SettingKind::Other);
    assert_eq!(find("go.lintFlags").default, Value::Null);
}

#[test]
fn fr_l31_17_the_catalogue_lists_embers_settings_first_and_each_key_once() {
    let catalogue = setting_catalogue(&extensions());
    let own = ember_settings();
    assert_eq!(catalogue.len(), own.len() + 6);
    assert!(catalogue[..own.len()].iter().all(|s| s.source == EMBER_SOURCE));
    // The extension that registers `editor.tabSize` again does not replace Ember's row.
    let tab: Vec<_> = catalogue.iter().filter(|s| s.key == "editor.tabSize").collect();
    assert_eq!(tab.len(), 1);
    assert_eq!(tab[0].default, json!(4));
    for key in ["editor.tabSize", "editor.insertSpaces", "editor.wordWrap", "editor.formatOnSave", "files.autoSave", "files.exclude"] {
        assert!(own.iter().any(|s| s.key == key), "{key} is one of Ember's settings");
    }
}

#[test]
fn fr_l31_17_search_matches_every_word_and_puts_key_matches_first() {
    let catalogue = setting_catalogue(&extensions());
    let keys = |query: &str| -> Vec<&str> { search_settings(&catalogue, query).into_iter().map(|s| s.key.as_str()).collect() };
    assert_eq!(keys("").len(), catalogue.len());
    assert_eq!(keys("TABSIZE"), vec!["editor.tabSize"]);
    assert_eq!(keys("json trace"), vec!["json.trace.server"]);
    // "validation" is only in a description; "json" also names the extension of every JSON setting.
    assert_eq!(keys("validation"), vec!["json.validate.enable"]);
    let by_source = keys("language features");
    assert_eq!(by_source.len(), 4);
    assert!(by_source.iter().all(|k| k.starts_with("json.")));
    assert!(keys("no such setting").is_empty());
    // A key match comes before a description match, whatever the catalogue's order.
    let row = |key: &str, description: &str| ember_editor::settings::SettingSchema {
        key: key.to_owned(),
        kind: SettingKind::Boolean,
        default: json!(false),
        description: description.to_owned(),
        scope: None,
        source: "x".to_owned(),
    };
    let two = vec![row("a.first", "mentions wrap in its text"), row("b.wrap", "")];
    let found: Vec<&str> = search_settings(&two, "wrap").into_iter().map(|s| s.key.as_str()).collect();
    assert_eq!(found, vec!["b.wrap", "a.first"]);
}

// ---- keybindings.json ---------------------------------------------------------------------------

#[test]
fn fr_l31_17_keybindings_json_is_read_in_file_order() {
    let text = r#"// Place your key bindings in this file to override the defaults
[
    { "key": "cmd+k cmd+s", "command": "workbench.action.files.save" },
    { "key": "ctrl+shift+p", "command": "-workbench.action.showCommands", "when": "editorTextFocus" },
    { "key": "f5", "command": "x.run", "args": { "fast": true } },
    { "key": "f6" },
    { "command": "no.key" },
    "junk",
]
"#;
    let rules = parse_keybindings(text).unwrap();
    assert_eq!(rules.len(), 3);
    assert_eq!(rules[0].key, "cmd+k cmd+s");
    assert_eq!(rules[0].command, "workbench.action.files.save");
    assert_eq!(rules[0].when, None);
    assert_eq!(rules[1].command, "-workbench.action.showCommands");
    assert_eq!(rules[1].when.as_deref(), Some("editorTextFocus"));
    assert_eq!(rules[2].args, Some(json!({ "fast": true })));
    assert!(parse_keybindings("").unwrap().is_empty());
    assert!(parse_keybindings("// nothing yet\n").unwrap().is_empty());
    assert!(parse_keybindings("{}").is_err());
    assert!(parse_keybindings("[ { ").is_err());
}

// ---- settings files on the client machine -------------------------------------------------------

#[test]
fn fr_l31_17_user_files_get_the_user_data_scheme() {
    let uri = user_data_uri(Path::new("/Users/me/Library/Application Support/ember/editor/User/settings.json"));
    assert_eq!(uri.scheme, SCHEME_USER_DATA);
    assert_eq!(uri.authority, "");
    assert_eq!(uri.path, "/Users/me/Library/Application Support/ember/editor/User/settings.json");
    assert_eq!(user_data_path(&uri).unwrap(), Path::new("/Users/me/Library/Application Support/ember/editor/User/settings.json"));
    // A Windows path gains the URI's leading slash and loses it again.
    let windows = UriComponents::new(SCHEME_USER_DATA, "", "/C:/Users/me/AppData/Roaming/ember/editor/User/settings.json");
    assert_eq!(user_data_path(&windows).unwrap(), Path::new("C:/Users/me/AppData/Roaming/ember/editor/User/settings.json"));
    // A file on the server is not a local file.
    assert_eq!(user_data_path(&UriComponents::remote("h", "/etc/passwd")), None);
}

#[test]
fn fr_l31_17_server_settings_file_that_does_not_parse_reports_why() {
    // #790: a broken .vscode/settings.json read at connect gave an empty scope and no reason.
    let (_, error) = ember_editor::session::server_layer_from_bytes(b"{ \"editor.tabSize\": 2 }");
    assert!(error.is_none(), "a file that parses has no error");
    let (_, error) = ember_editor::session::server_layer_from_bytes(b"{ \"editor.tabSize\": ");
    assert!(error.is_some(), "a parse error must come back");
}
