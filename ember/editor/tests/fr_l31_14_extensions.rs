//! FR-L31.14 (#446): what the extensions panel reads from a scan, and which descriptions the
//! extension host is given when some ids are disabled. The descriptions below have the shape of
//! `IExtensionDescription` (`identifier`, the manifest fields, `isBuiltin`).

use ember_editor::extensions::{enabled_only, group, id_key, parse_installed, InstalledExtension};
use serde_json::{json, Value};

fn scan() -> Vec<Value> {
    vec![
        json!({
            "identifier": { "value": "vscode.json-language-features", "_lower": "vscode.json-language-features" },
            "name": "json-language-features", "publisher": "vscode", "version": "1.0.0",
            "displayName": "JSON Language Features", "description": "Provides rich language support for JSON files.",
            "isBuiltin": true, "isUserBuiltin": false, "isUnderDevelopment": false,
            "contributes": { "commands": [{ "command": "json.sort", "title": "Sort Document" }] }
        }),
        json!({
            "identifier": { "value": "rust-lang.rust-analyzer" },
            "name": "rust-analyzer", "publisher": "rust-lang", "version": "0.4.3116",
            "displayName": "rust-analyzer", "description": "Rust language support for Visual Studio Code",
            "isBuiltin": false
        }),
        json!({
            "identifier": { "value": "RedHat.java" },
            "name": "java", "publisher": "redhat", "version": "1.40.0",
            "displayName": "%displayName%", "description": "%description%"
        }),
    ]
}

#[test]
fn fr_l31_14_a_scan_becomes_rows_with_name_publisher_version_and_description() {
    let rows = parse_installed(&scan());
    assert_eq!(rows.len(), 3);
    assert_eq!(
        rows[0],
        InstalledExtension {
            id: "vscode.json-language-features".into(),
            name: "json-language-features".into(),
            publisher: "vscode".into(),
            version: "1.0.0".into(),
            display_name: "JSON Language Features".into(),
            description: "Provides rich language support for JSON files.".into(),
            builtin: true,
        }
    );
    assert!(!rows[1].builtin);
    assert_eq!(rows[1].version, "0.4.3116");
}

#[test]
fn fr_l31_14_an_unresolved_nls_key_falls_back_to_the_manifest_name() {
    let rows = parse_installed(&scan());
    assert_eq!(rows[2].display_name, "java");
    assert_eq!(rows[2].description, "", "an untranslated %key% is not shown as a description");
    assert!(!rows[2].builtin, "isBuiltin absent reads as a user extension");
}

#[test]
fn fr_l31_14_the_id_falls_back_to_publisher_and_name_and_a_row_without_one_is_skipped() {
    let rows = parse_installed(&[
        json!({ "name": "x", "publisher": "p", "version": "1.0.0" }),
        json!({ "version": "1.0.0" }),
        json!("not an object"),
        json!({ "identifier": { "value": "  " }, "name": "y", "publisher": "q" }),
    ]);
    let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, ["p.x", "q.y"]);
    assert_eq!(rows[0].display_name, "x");
}

#[test]
fn fr_l31_14_groups_are_sorted_by_display_name_without_regard_to_case() {
    let mut list = parse_installed(&scan());
    list.push(InstalledExtension {
        id: "a.zeta".into(),
        name: "zeta".into(),
        publisher: "a".into(),
        version: "1.0.0".into(),
        display_name: "Alpha".into(),
        description: String::new(),
        builtin: false,
    });
    let user: Vec<String> = group(&list, false).into_iter().map(|e| e.display_name).collect();
    assert_eq!(user, ["Alpha", "java", "rust-analyzer"]);
    let builtin: Vec<String> = group(&list, true).into_iter().map(|e| e.id).collect();
    assert_eq!(builtin, ["vscode.json-language-features"]);
}

#[test]
fn fr_l31_14_disabled_ids_are_left_out_of_what_the_extension_host_gets() {
    let all = scan();
    assert_eq!(enabled_only(&all, &[]).len(), 3, "nothing disabled keeps the scan as it is");
    // Ids compare without regard to case, as in Code-OSS; a built-in one can be disabled too.
    let left = enabled_only(&all, &["redhat.JAVA".into(), " vscode.json-language-features ".into(), "not.installed".into()]);
    assert_eq!(left.len(), 1);
    assert_eq!(left[0]["identifier"]["value"], "rust-lang.rust-analyzer");
    assert_eq!(left[0], all[1], "a kept description is passed on untouched");
}

#[test]
fn fr_l31_14_a_disabled_extension_contributes_no_commands() {
    let all = scan();
    assert_eq!(ember_editor::parse_contributed_commands(&all).len(), 1);
    let left = enabled_only(&all, &["vscode.json-language-features".into()]);
    assert!(ember_editor::parse_contributed_commands(&left).is_empty());
}

#[test]
fn fr_l31_14_id_keys_ignore_case_and_outer_space() {
    assert_eq!(id_key(" RedHat.Java "), "redhat.java");
    assert_eq!(parse_installed(&scan())[2].key(), "redhat.java");
}
