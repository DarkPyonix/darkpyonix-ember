//! FR-E6 (syntax highlighting), the loader: which grammars the extensions contribute and how a grammar
//! file is read. A grammar file and an extension's `path` are input from outside the editor, so most of
//! this is about what is refused, what is forgiven, and what is capped.

use std::path::{Path, PathBuf};

use ember_editor::textmate::{
    parse_grammar, read_grammar, GrammarCatalog, GrammarError, MAX_DEPTH, MAX_GRAMMAR_BYTES, MAX_RULES,
};
use serde_json::{json, Value};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/textmate/demo-lang")
}

/// A scratch folder inside the build directory (never the system temp folder).
fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("fr_e6").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn ext(id: &str, dir: &str, grammars: Value) -> Value {
    json!({"identifier": {"value": id}, "extensionLocation": {"path": dir}, "contributes": {"grammars": grammars}})
}

// ---- catalog ----------------------------------------------------------------------------------------

#[test]
fn fr_e6_a_grammar_is_found_by_scope_and_by_language_with_its_absolute_path() {
    let c = GrammarCatalog::from_extensions(&[ext(
        "vscode.rust",
        "/ose/extensions/rust",
        json!([{"language": "rust", "scopeName": "source.rust", "path": "./syntaxes/rust.tmLanguage.json"}]),
    )]);
    let e = c.by_scope("source.rust").unwrap();
    assert_eq!(e.path, "/ose/extensions/rust/syntaxes/rust.tmLanguage.json");
    assert_eq!(e.language.as_deref(), Some("rust"));
    assert_eq!(e.extension, "vscode.rust");
    assert_eq!(c.for_language("rust").unwrap().scope_name, "source.rust");
    assert!(c.for_language("go").is_none());
    assert_eq!(c.len(), 1);
}

#[test]
fn fr_e6_injection_grammars_and_embedded_languages_are_kept() {
    let c = GrammarCatalog::from_extensions(&[ext(
        "vscode.markdown",
        "/e/md",
        json!([
            {"language": "markdown", "scopeName": "text.html.markdown", "path": "./md.json",
             "embeddedLanguages": {"meta.embedded.block.rust": "rust", "bad": 3}},
            {"scopeName": "markdown.rust.codeblock", "path": "./rust-block.json", "injectTo": ["text.html.markdown"]}
        ]),
    )]);
    let md = c.by_scope("text.html.markdown").unwrap();
    assert_eq!(md.embedded_languages.get("meta.embedded.block.rust").map(String::as_str), Some("rust"));
    assert_eq!(md.embedded_languages.len(), 1, "a non-string value is dropped");
    assert_eq!(c.by_scope("markdown.rust.codeblock").unwrap().language, None);
    let inj: Vec<&str> = c.injections_into("text.html.markdown").iter().map(|e| e.scope_name.as_str()).collect();
    assert_eq!(inj, vec!["markdown.rust.codeblock"]);
    assert!(c.injections_into("source.rust").is_empty());
}

#[test]
fn fr_e6_a_path_that_leaves_the_extension_folder_is_skipped_and_reported() {
    let c = GrammarCatalog::from_extensions(&[ext(
        "evil.ext",
        "/e/evil",
        json!([
            {"scopeName": "a", "path": "../other/a.json"},
            {"scopeName": "b", "path": "/etc/passwd"},
            {"scopeName": "c", "path": "syntaxes/../../x.json"},
            {"scopeName": "d", "path": "./ok/../d.json"},
            {"scopeName": "e"},
            {"path": "./f.json"},
            {"scopeName": "g", "path": "C:/Windows/x.json"},
            {"scopeName": "h", "path": "syntaxes\\..\\..\\x.json"},
            {"scopeName": "i", "path": "a/C:b.json"},
            {"scopeName": "j", "path": "a\nb.json"}
        ]),
    )]);
    for gone in ["a", "b", "c", "e", "g", "h", "i", "j"] {
        assert!(c.by_scope(gone).is_none(), "{gone} must be refused");
    }
    assert_eq!(c.by_scope("d").unwrap().path, "/e/evil/d.json", ".. that stays inside the folder is fine");
    assert_eq!(c.skipped().len(), 9);
    assert!(c.skipped()[0].contains("evil.ext") && c.skipped()[0].contains("outside"));
}

#[test]
fn fr_e6_a_windows_extension_folder_is_joined_like_any_other() {
    let c = GrammarCatalog::from_extensions(&[ext(
        "vscode.json",
        "/c:/Users/me/.ember/extensions/json",
        json!([{"scopeName": "source.json", "path": "./syntaxes/JSON.tmLanguage.json"}]),
    )]);
    assert_eq!(c.by_scope("source.json").unwrap().path, "/c:/Users/me/.ember/extensions/json/syntaxes/JSON.tmLanguage.json");
}

#[test]
fn fr_e6_skipped_messages_do_not_repeat_untrusted_text_unbounded() {
    let long = "x".repeat(5000);
    let c = GrammarCatalog::from_extensions(&[ext(
        "evil.ext",
        "/e/evil",
        json!([{"scopeName": "s", "path": format!("../{long}\u{1b}[31m\nred")}]),
    )]);
    let msg = &c.skipped()[0];
    assert!(msg.chars().count() < 400, "{} characters", msg.chars().count());
    assert!(!msg.chars().any(char::is_control), "no control character reaches a message");
}

#[test]
fn fr_e6_the_catalog_does_not_depend_on_the_order_extensions_are_listed_in() {
    let one = ext("a.one", "/e/1", json!([{"language": "x", "scopeName": "source.x", "path": "./1.json"}]));
    let two = ext("b.two", "/e/2", json!([{"language": "x", "scopeName": "source.x", "path": "./2.json"}]));
    let forward = GrammarCatalog::from_extensions(&[one.clone(), two.clone()]);
    let backward = GrammarCatalog::from_extensions(&[two, one]);
    for c in [&forward, &backward] {
        assert_eq!(c.by_scope("source.x").unwrap().path, "/e/2/2.json", "the later extension id wins");
        assert_eq!(c.for_language("x").unwrap().extension, "b.two");
    }
}

#[test]
fn fr_e6_a_scope_redefined_for_another_language_leaves_no_stale_language_entry() {
    let c = GrammarCatalog::from_extensions(&[
        ext("a.one", "/e/1", json!([{"language": "old", "scopeName": "source.x", "path": "./1.json"}])),
        ext("b.two", "/e/2", json!([{"language": "new", "scopeName": "source.x", "path": "./2.json"}])),
    ]);
    assert_eq!(c.for_language("new").unwrap().path, "/e/2/2.json");
    assert!(c.for_language("old").is_none(), "the grammar that language pointed at was replaced");
}

#[test]
fn fr_e6_descriptions_without_a_usable_location_or_list_contribute_nothing() {
    let c = GrammarCatalog::from_extensions(&[
        json!({"contributes": {"grammars": [{"scopeName": "s", "path": "./a.json"}]}}),
        json!({"contributes": {}}),
        json!({"contributes": {"grammars": "not a list"}}),
    ]);
    assert!(c.is_empty());
    assert_eq!(c.skipped().len(), 1);
}

// ---- parsing ----------------------------------------------------------------------------------------

#[test]
fn fr_e6_a_grammar_keeps_its_scope_rules_and_repository() {
    let g = parse_grammar(
        "x.tmLanguage.json",
        r##"{"name":"Demo","scopeName":"source.demo","patterns":[{"include":"#kw"},{"match":"\\d+","name":"constant.numeric.demo"}],
            "repository":{"kw":{"match":"\\b(if|else)\\b","captures":{"1":{"name":"keyword.control.demo"}}}}}"##,
    )
    .unwrap();
    assert_eq!(g.scope_name, "source.demo");
    assert_eq!(g.patterns.len(), 2);
    assert_eq!(g.patterns[0].include.as_deref(), Some("#kw"));
    assert_eq!(g.patterns[1].match_.as_deref(), Some("\\d+"));
    assert_eq!(g.repository["kw"].captures["1"].name.as_deref(), Some("keyword.control.demo"));
}

#[test]
fn fr_e6_a_region_keeps_begin_end_while_captures_and_content_name() {
    let g = parse_grammar(
        "a.json",
        r#"{"scopeName":"source.r","patterns":[
            {"begin":"\"","end":"\"","name":"string.quoted","contentName":"meta.string-contents",
             "beginCaptures":{"0":{"name":"punctuation.open"}},"endCaptures":{"0":{"name":"punctuation.close"}},
             "patterns":[{"match":"\\\\.","name":"constant.character.escape"}]},
            {"begin":"^>","while":"^>","applyEndPatternLast":1}]}"#,
    )
    .unwrap();
    let s = &g.patterns[0];
    assert_eq!((s.begin.as_deref(), s.end.as_deref()), (Some("\""), Some("\"")));
    assert_eq!(s.content_name.as_deref(), Some("meta.string-contents"));
    assert_eq!(s.begin_captures["0"].name.as_deref(), Some("punctuation.open"));
    assert_eq!(s.end_captures["0"].name.as_deref(), Some("punctuation.close"));
    let q = &g.patterns[1];
    assert_eq!(q.while_.as_deref(), Some("^>"));
    assert!(q.apply_end_pattern_last, "1 means true");
}

#[test]
fn fr_e6_flags_accept_numbers_and_booleans_and_anything_else_is_false() {
    let g = parse_grammar(
        "a.json",
        r#"{"scopeName":"s","patterns":[{"match":"a","disabled":1},{"match":"b","disabled":false},{"match":"c","applyEndPatternLast":true},{"match":"d"},{"match":"e","disabled":"x"}]}"#,
    )
    .unwrap();
    let flags: Vec<(bool, bool)> = g.patterns.iter().map(|r| (r.disabled, r.apply_end_pattern_last)).collect();
    assert_eq!(flags, vec![(true, false), (false, false), (false, true), (false, false), (false, false)]);
}

#[test]
fn fr_e6_a_value_of_the_wrong_type_is_dropped_and_the_rest_of_the_grammar_is_kept() {
    let g = parse_grammar(
        "a.json",
        r#"{"scopeName":"source.lenient","name":7,
            "patterns":[{"match":"a","name":5},"not a rule",null,{"match":"b","patterns":null,"repository":null,"captures":"no"},{"match":"c"}],
            "repository":{"ok":{"match":"x"},"bad":"text","worse":[1,2],"nul":null},
            "injections":{"L:text":"nope"}}"#,
    )
    .unwrap();
    assert_eq!(g.name, None, "a number is not a name");
    assert_eq!(g.patterns.len(), 3, "the two non-objects are gone, the three rules stay");
    assert_eq!(g.patterns[0].name, None);
    assert_eq!(g.patterns[0].match_.as_deref(), Some("a"));
    assert!(g.patterns[1].patterns.is_empty() && g.patterns[1].repository.is_empty() && g.patterns[1].captures.is_empty());
    assert_eq!(g.repository.keys().collect::<Vec<_>>(), vec!["ok"]);
    assert!(g.injections.is_empty());
}

#[test]
fn fr_e6_patterns_null_and_repository_null_at_the_top_level_still_load() {
    let g = parse_grammar("a.json", r#"{"scopeName":"s","patterns":null,"repository":null,"injections":null}"#).unwrap();
    assert!(g.patterns.is_empty() && g.repository.is_empty() && g.injections.is_empty());
}

#[test]
fn fr_e6_a_stray_non_rule_entry_in_a_capture_map_is_ignored() {
    let text = std::fs::read_to_string(fixtures().join("syntaxes/typo.tmLanguage.json")).unwrap();
    let g = parse_grammar("typo.tmLanguage.json", &text).unwrap();
    let c = &g.patterns[0].captures;
    assert_eq!(c.len(), 1);
    assert_eq!(c["0"].name.as_deref(), Some("punctuation.definition.comment.typo"));
}

#[test]
fn fr_e6_injections_and_a_byte_order_mark_are_read() {
    let g = parse_grammar(
        "a.json",
        "\u{feff}{\"scopeName\":\"text.md\",\"injectionSelector\":\"L:source.demo\",\"injections\":{\"L:text.md -comment\":{\"patterns\":[{\"match\":\"x\"}]}}}",
    )
    .unwrap();
    assert_eq!(g.injection_selector.as_deref(), Some("L:source.demo"));
    assert_eq!(g.injections["L:text.md -comment"].patterns.len(), 1);
}

#[test]
fn fr_e6_bad_input_is_an_error_not_an_empty_grammar() {
    assert!(matches!(parse_grammar("a.json", "{nope"), Err(GrammarError::Json { .. })));
    assert!(matches!(parse_grammar("a.json", "{\"patterns\":[]}"), Err(GrammarError::MissingScopeName)));
    assert!(matches!(parse_grammar("a.json", "[1,2]"), Err(GrammarError::MissingScopeName)));
    assert!(matches!(parse_grammar("a.json", "{\"scopeName\":5}"), Err(GrammarError::MissingScopeName)));
    assert!(matches!(parse_grammar("a.tmLanguage", "<plist/>"), Err(GrammarError::UnsupportedFormat(_))));
    assert!(matches!(parse_grammar("a.YAML-tmLanguage", "scopeName: x"), Err(GrammarError::UnsupportedFormat(_))));
}

#[test]
fn fr_e6_an_error_never_repeats_the_text_of_the_file() {
    let secret = "TOP-SECRET-TOKEN-12345";
    for content in [format!("{{\"scopeName\": \"{secret}\" nope"), format!("{{\"scopeName\":5,\"x\":\"{secret}\"}}")] {
        let message = parse_grammar("a.json", &content).unwrap_err().to_string();
        assert!(!message.contains(secret), "{message}");
    }
}

// ---- limits -----------------------------------------------------------------------------------------

fn nested(depth: usize) -> String {
    let mut s = String::from(r#"{"scopeName":"deep","patterns":["#);
    for _ in 0..depth {
        s.push_str(r#"{"patterns":["#);
    }
    s.push_str(r#"{"match":"x"}"#);
    for _ in 0..depth {
        s.push_str("]}");
    }
    s.push_str("]}");
    s
}

#[test]
fn fr_e6_nesting_is_capped_and_deeper_input_is_an_error_not_a_crash() {
    assert!(parse_grammar("a.json", &nested(60)).is_ok(), "a sane nesting loads");
    for depth in [MAX_DEPTH + 5, 400, 5000] {
        assert!(parse_grammar("a.json", &nested(depth)).is_err(), "depth {depth} must be refused");
    }
}

#[test]
fn fr_e6_the_number_of_rules_is_capped() {
    let rules = vec![r#"{"match":"a"}"#; MAX_RULES + 1].join(",");
    let text = format!(r#"{{"scopeName":"wide","patterns":[{rules}]}}"#);
    assert!(matches!(parse_grammar("a.json", &text), Err(GrammarError::TooManyRules)));
    let ok = vec![r#"{"match":"a"}"#; 1000].join(",");
    assert!(parse_grammar("a.json", &format!(r#"{{"scopeName":"wide","patterns":[{ok}]}}"#)).is_ok());
}

#[test]
fn fr_e6_text_larger_than_the_cap_is_refused_before_it_is_parsed() {
    let big = format!(r#"{{"scopeName":"big","pad":"{}"}}"#, "x".repeat(MAX_GRAMMAR_BYTES as usize));
    assert!(matches!(parse_grammar("a.json", &big), Err(GrammarError::TooLarge)));
}

// ---- reading from disk ------------------------------------------------------------------------------

#[test]
fn fr_e6_a_grammar_on_disk_is_read_through_the_catalog_entry() {
    let dir = fixtures();
    let c = GrammarCatalog::from_extensions(&[ext(
        "demo.lang",
        &dir.to_string_lossy(),
        json!([{"language": "demo", "scopeName": "source.demo", "path": "./syntaxes/demo.tmLanguage.json"}]),
    )]);
    let entry = c.for_language("demo").expect("demo grammar");
    let g = read_grammar(&dir, Path::new(&entry.path)).unwrap();
    assert_eq!(g.scope_name, "source.demo");
    assert_eq!(g.patterns.len(), 4);
    assert_eq!(g.repository["comment"].begin_captures["0"].name.as_deref(), Some("punctuation.definition.comment.begin.demo"));
    assert_eq!(g.repository["string"].patterns[0].name.as_deref(), Some("constant.character.escape.demo"));
}

#[cfg(unix)]
#[test]
fn fr_e6_a_symbolic_link_that_leaves_the_extension_folder_is_refused() {
    let root = scratch("symlink");
    let ext_dir = root.join("ext");
    std::fs::create_dir_all(ext_dir.join("syntaxes")).unwrap();
    std::fs::write(root.join("outside.json"), r#"{"scopeName":"source.outside"}"#).unwrap();
    std::fs::write(ext_dir.join("syntaxes/inside.json"), r#"{"scopeName":"source.inside"}"#).unwrap();
    std::os::unix::fs::symlink("../../outside.json", ext_dir.join("syntaxes/link.json")).unwrap();
    std::os::unix::fs::symlink(&root, ext_dir.join("syntaxes/dir-link")).unwrap();

    assert_eq!(read_grammar(&ext_dir, &ext_dir.join("syntaxes/inside.json")).unwrap().scope_name, "source.inside");
    assert!(matches!(read_grammar(&ext_dir, &ext_dir.join("syntaxes/link.json")), Err(GrammarError::OutsideExtension)));
    assert!(matches!(read_grammar(&ext_dir, &ext_dir.join("syntaxes/dir-link/outside.json")), Err(GrammarError::OutsideExtension)));
    assert!(matches!(read_grammar(&ext_dir, &ext_dir.join("syntaxes/../../outside.json")), Err(GrammarError::OutsideExtension)));
}

#[test]
fn fr_e6_a_file_over_the_size_cap_is_refused_and_a_folder_is_not_a_grammar() {
    let root = scratch("size");
    std::fs::create_dir_all(root.join("syntaxes")).unwrap();
    let big = root.join("syntaxes/big.json");
    let mut content = String::from(r#"{"scopeName":"big","pad":""#);
    content.push_str(&"x".repeat(MAX_GRAMMAR_BYTES as usize));
    content.push_str(r#""}"#);
    std::fs::write(&big, content).unwrap();
    assert!(matches!(read_grammar(&root, &big), Err(GrammarError::TooLarge)));
    assert!(matches!(read_grammar(&root, &root.join("syntaxes")), Err(GrammarError::Io(_))));
    assert!(matches!(read_grammar(&root, &root.join("syntaxes/missing.json")), Err(GrammarError::Io(_))));
}
