//! FR-E6 (syntax highlighting), the loader: which grammars the extensions contribute and how a grammar
//! file is read. A grammar file and an extension's `path` are input from outside the editor, so most of
//! this is about what is refused, what is forgiven, and what is capped.

use std::path::{Path, PathBuf};

use ember_editor::textmate::{
    parse_grammar, parse_plist, read_grammar, GrammarCatalog, GrammarError, PlistError,
    MAX_GRAMMAR_BYTES, MAX_PLIST_BYTES, MAX_PLIST_DEPTH, MAX_PLIST_NODES, MAX_RULES,
};
use serde_json::{json, Value};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/textmate/demo-lang")
}

fn textmate_fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/textmate")
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
    assert!(matches!(parse_grammar("a.tmLanguage", "<plist/>"), Err(GrammarError::Plist(_))), "a property list that is not a grammar");
    assert!(matches!(parse_grammar("a.txt", "x"), Err(GrammarError::UnsupportedFormat(_))));
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
    for depth in [70, 400, 5000] {
        assert!(parse_grammar("a.json", &nested(depth)).is_err(), "depth {depth} must be refused");
    }
}

#[cfg(unix)]
#[test]
fn fr_e6_a_named_pipe_in_the_extension_folder_is_refused_without_waiting_for_a_writer() {
    let root = scratch("fifo");
    let fifo = root.join("pipe.json");
    let status = std::process::Command::new("mkfifo").arg(&fifo).status().expect("mkfifo");
    assert!(status.success());
    assert!(matches!(read_grammar(&root, &fifo), Err(GrammarError::Io(_))));
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

// ---- property lists (plist / .tmLanguage) -----------------------------------------------------------

#[test]
fn fr_e6_plist_js_regexp_matches_expected_json_and_loads_as_grammar() {
    let tmlanguage_path = textmate_fixtures().join("grammars/Regular Expressions (JavaScript).tmLanguage");
    let expected_path = textmate_fixtures().join("plist-js-regexp.expected.json");
    let tmlanguage_text = std::fs::read_to_string(&tmlanguage_path).expect("read tmLanguage fixture");
    let expected_text = std::fs::read_to_string(&expected_path).expect("read expected json fixture");

    let parsed_plist = parse_plist(&tmlanguage_text).expect("parse plist");
    let expected_json: Value = serde_json::from_str(&expected_text).expect("parse expected json");
    assert_eq!(parsed_plist, expected_json, "parsed plist must equal expected JSON structure");

    for ext_path in [
        "syntaxes/Regular Expressions (JavaScript).tmLanguage",
        "syntaxes/Regular Expressions (JavaScript).plist",
    ] {
        let g = parse_grammar(ext_path, &tmlanguage_text).expect("parse grammar from plist");
        assert_eq!(g.scope_name, "source.js.regexp");
        assert_eq!(g.name.as_deref(), Some("Regular Expressions (JavaScript)"));
        assert_eq!(g.patterns.len(), 1);
        assert_eq!(g.patterns[0].include.as_deref(), Some("#regexp"));
        assert!(g.repository.contains_key("regexp"));
        assert!(g.repository.contains_key("regex-character-class"));
    }

    let ext_dir = textmate_fixtures().join("grammars");
    let g_disk = read_grammar(&ext_dir, &tmlanguage_path).expect("read grammar from disk");
    assert_eq!(g_disk.scope_name, "source.js.regexp");
}

#[test]
fn fr_e6_plist_parses_all_value_types_and_xml_constructs() {
    let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<!-- Leading comment before root -->
<plist version="1.0">
<dict>
    <!-- Basic data types -->
    <key>string_val</key>
    <string>hello world</string>
    <key>string_empty</key>
    <string/>
    <key>string_open_close</key>
    <string></string>
    <key>int_pos</key>
    <integer>42</integer>
    <key>int_neg</key>
    <integer>-17</integer>
    <key>real_pos</key>
    <real>3.1415</real>
    <key>real_neg</key>
    <real>-0.5</real>
    <key>bool_true_self</key>
    <true/>
    <key>bool_true_open</key>
    <true></true>
    <key>bool_false_self</key>
    <false/>
    <key>bool_false_open</key>
    <false></false>
    <key>date_val</key>
    <date>2026-11-07T00:00:00Z</date>
    <key>date_empty</key>
    <date/>
    <key>data_val</key>
    <data>SGVsbG8gV29ybGQ=</data>
    <key>data_empty</key>
    <data/>
    <!-- Containers -->
    <key>empty_dict</key>
    <dict/>
    <key>empty_array</key>
    <array/>
    <key>nested_array</key>
    <array>
        <string>one</string>
        <integer>2</integer>
        <true/>
    </array>
    <!-- CDATA and entities -->
    <key>cdata_val</key>
    <string><![CDATA[escaped <tag> & "quotes"]]></string>
    <key>cdata_mixed</key>
    <string>prefix <![CDATA[<inner>]]> suffix</string>
    <key>predefined_entities</key>
    <string>&amp; &lt; &gt; &quot; &apos;</string>
    <key>numeric_entities</key>
    <string>&#38; &#x26; &#X26; &#65; &#x41;</string>
</dict>
</plist>
<!-- Trailing comment -->
"#;
    let v = parse_plist(xml).expect("parse plist with all types");
    let obj = v.as_object().expect("root must be object");

    assert_eq!(obj.get("string_val").and_then(Value::as_str), Some("hello world"));
    assert_eq!(obj.get("string_empty").and_then(Value::as_str), Some(""));
    assert_eq!(obj.get("string_open_close").and_then(Value::as_str), Some(""));
    assert_eq!(obj.get("int_pos").and_then(Value::as_i64), Some(42));
    assert_eq!(obj.get("int_neg").and_then(Value::as_i64), Some(-17));
    assert_eq!(obj.get("real_pos").and_then(Value::as_f64), Some(3.1415));
    assert_eq!(obj.get("real_neg").and_then(Value::as_f64), Some(-0.5));
    assert_eq!(obj.get("bool_true_self"), Some(&Value::Bool(true)));
    assert_eq!(obj.get("bool_true_open"), Some(&Value::Bool(true)));
    assert_eq!(obj.get("bool_false_self"), Some(&Value::Bool(false)));
    assert_eq!(obj.get("bool_false_open"), Some(&Value::Bool(false)));
    assert_eq!(obj.get("date_val").and_then(Value::as_str), Some("2026-11-07T00:00:00Z"));
    assert_eq!(obj.get("date_empty").and_then(Value::as_str), Some(""));
    assert_eq!(obj.get("data_val").and_then(Value::as_str), Some("SGVsbG8gV29ybGQ="));
    assert_eq!(obj.get("data_empty").and_then(Value::as_str), Some(""));
    assert_eq!(obj.get("empty_dict"), Some(&json!({})));
    assert_eq!(obj.get("empty_array"), Some(&json!([])));
    assert_eq!(obj.get("nested_array"), Some(&json!(["one", 2, true])));
    assert_eq!(obj.get("cdata_val").and_then(Value::as_str), Some("escaped <tag> & \"quotes\""));
    assert_eq!(obj.get("cdata_mixed").and_then(Value::as_str), Some("prefix <inner> suffix"));
    assert_eq!(obj.get("predefined_entities").and_then(Value::as_str), Some("& < > \" '"));
    assert_eq!(obj.get("numeric_entities").and_then(Value::as_str), Some("& & & A A"));
}

#[test]
fn fr_e6_plist_security_dtd_internal_subsets_and_doctype_refusals() {
    let subset = r#"<!DOCTYPE plist [ <!ENTITY xxe SYSTEM "file:///etc/passwd"> ]><plist><dict/></plist>"#;
    assert_eq!(parse_plist(subset), Err(PlistError::Dtd));

    let inline_entity = r#"<!DOCTYPE plist [ <!ENTITY foo "bar"> ]><plist><string>&foo;</string></plist>"#;
    assert_eq!(parse_plist(inline_entity), Err(PlistError::Dtd));

    let wrong_doctype = r#"<!DOCTYPE catalog SYSTEM "catalog.dtd"><plist><dict/></plist>"#;
    assert_eq!(parse_plist(wrong_doctype), Err(PlistError::Dtd));

    let stray_entity = r#"<!ENTITY test "val"><plist><dict/></plist>"#;
    assert_eq!(parse_plist(stray_entity), Err(PlistError::Dtd));

    let doctype_inside = r#"<plist><!DOCTYPE plist><dict/></plist>"#;
    assert_eq!(parse_plist(doctype_inside), Err(PlistError::Dtd));

    assert!(matches!(parse_grammar("test.tmLanguage", subset), Err(GrammarError::Plist(PlistError::Dtd))));
}

#[test]
fn fr_e6_plist_security_non_standard_and_invalid_entities_are_refused() {
    let xxe = "<plist><string>&xxe;</string></plist>";
    assert_eq!(parse_plist(xxe), Err(PlistError::Entity(1)));

    let custom = "<plist>\n\n<string>&customEntity;</string></plist>";
    assert_eq!(parse_plist(custom), Err(PlistError::Entity(3)), "line number matches entity location");

    let nul_dec = "<plist><string>&#0;</string></plist>";
    assert_eq!(parse_plist(nul_dec), Err(PlistError::Entity(1)));

    let nul_hex = "<plist><string>&#x0;</string></plist>";
    assert_eq!(parse_plist(nul_hex), Err(PlistError::Entity(1)));

    let surrogate = "<plist><string>&#xD800;</string></plist>";
    assert_eq!(parse_plist(surrogate), Err(PlistError::Entity(1)));

    let out_of_range = "<plist><string>&#x110000;</string></plist>";
    assert_eq!(parse_plist(out_of_range), Err(PlistError::Entity(1)));

    let unclosed = "<plist><string>&amp without semicolon</string></plist>";
    assert_eq!(parse_plist(unclosed), Err(PlistError::Entity(1)));
}

fn nested_plist(depth: usize) -> String {
    let mut s = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\">\n");
    for _ in 0..depth {
        s.push_str("<dict><key>k</key>\n");
    }
    s.push_str("<string>leaf</string>\n");
    for _ in 0..depth {
        s.push_str("</dict>\n");
    }
    s.push_str("</plist>");
    s
}

#[test]
fn fr_e6_plist_security_nesting_is_capped_at_max_plist_depth() {
    assert!(parse_plist(&nested_plist(MAX_PLIST_DEPTH)).is_ok());

    assert_eq!(parse_plist(&nested_plist(MAX_PLIST_DEPTH + 1)), Err(PlistError::TooDeep));

    assert!(matches!(
        parse_grammar("deep.tmLanguage", &nested_plist(MAX_PLIST_DEPTH + 1)),
        Err(GrammarError::Plist(PlistError::TooDeep))
    ));
}

#[test]
fn fr_e6_plist_security_node_count_is_capped_at_max_plist_nodes() {
    let mut text = String::with_capacity(2_000_000);
    text.push_str("<plist version=\"1.0\"><array>");
    for _ in 0..MAX_PLIST_NODES {
        text.push_str("<string/>");
    }
    text.push_str("</array></plist>");
    assert_eq!(parse_plist(&text), Err(PlistError::TooManyNodes));

    let mut ok = String::from("<plist version=\"1.0\"><array>");
    for _ in 0..500 {
        ok.push_str("<string/>");
    }
    ok.push_str("</array></plist>");
    assert!(parse_plist(&ok).is_ok());
}

#[test]
fn fr_e6_plist_security_byte_size_is_capped_at_max_plist_bytes() {
    let big = format!("<plist version=\"1.0\"><string>{}</string></plist>", "x".repeat(MAX_PLIST_BYTES));
    assert_eq!(parse_plist(&big), Err(PlistError::TooLarge));

    assert!(matches!(
        parse_grammar("big.tmLanguage", &big),
        Err(GrammarError::TooLarge | GrammarError::Plist(PlistError::TooLarge))
    ));
}

#[test]
fn fr_e6_plist_syntax_refusals_on_malformed_xml_and_invalid_schema() {
    assert!(matches!(parse_plist("<plist><dict>"), Err(PlistError::Syntax(_))));
    assert!(matches!(parse_plist("<plist><string>unclosed"), Err(PlistError::Syntax(_))));
    assert!(matches!(parse_plist("<plist><dict></array></plist>"), Err(PlistError::Syntax(_))));
    assert!(matches!(parse_plist("<plist><dict></dict/></plist>"), Err(PlistError::Syntax(_))));
    assert!(matches!(parse_plist("<plist><dict><string>val</string></dict></plist>"), Err(PlistError::Syntax(_))));
    assert!(matches!(parse_plist("<plist><dict><key>k</key></dict></plist>"), Err(PlistError::Syntax(_))));
    assert!(matches!(parse_plist("<plist><dict><key>k1</key><key>k2</key><string>v</string></dict></plist>"), Err(PlistError::Syntax(_))));
    assert!(matches!(parse_plist("<plist><array><key>k</key></array></plist>"), Err(PlistError::Syntax(_))));
    assert!(matches!(parse_plist("<plist><integer>abc</integer></plist>"), Err(PlistError::Syntax(_))));
    assert!(matches!(parse_plist("<plist><real>abc</real></plist>"), Err(PlistError::Syntax(_))));
    assert!(matches!(parse_plist("<plist><true>invalid</true></plist>"), Err(PlistError::Syntax(_))));
    assert!(matches!(parse_plist("<plist><dict/><dict/></plist>"), Err(PlistError::Syntax(_))));
    assert!(matches!(parse_plist("<plist/>"), Err(PlistError::Syntax(_))));
    assert!(matches!(parse_plist("<dict/>"), Err(PlistError::Syntax(_))));
    assert!(matches!(parse_plist("garbage <plist><dict/></plist>"), Err(PlistError::Syntax(_))));
    assert!(matches!(parse_plist("<plist><unknown/></plist>"), Err(PlistError::Syntax(_))));
}

#[test]
fn fr_e6_plist_error_messages_never_repeat_untrusted_file_text() {
    let secret = "SECRET_TOKEN_XYZ_12345";
    let cases = [
        format!("<!DOCTYPE plist [ <!ENTITY {secret} \"val\"> ]><plist><dict/></plist>"),
        format!("<plist><dict><key>k</key><string>&{secret};</string></dict></plist>"),
        format!("<plist><dict><key>{secret}</key><string>v</string>broken"),
        format!("<plist><dict><key>k</key><integer>{secret}</integer></dict></plist>"),
        format!("<plist><dict><key>k</key><real>{secret}</real></dict></plist>"),
        format!("<plist><dict><key>k</key><true>{secret}</true></dict></plist>"),
        format!("<plist><{secret}/></plist>"),
    ];
    for input in cases {
        let err = parse_plist(&input).unwrap_err().to_string();
        assert!(!err.contains(secret), "error '{err}' must not contain secret '{secret}'");
        let gerr = parse_grammar("grammar.tmLanguage", &input).unwrap_err().to_string();
        assert!(!gerr.contains(secret), "grammar error '{gerr}' must not contain secret '{secret}'");
    }
}
