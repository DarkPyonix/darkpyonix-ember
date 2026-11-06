//! Oniguruma is linked into the editor core and compiles the patterns TextMate grammars rely on.
//! The grammar interpreter builds on it; this test fails first if the C library does not build or link
//! on a platform, which is why the Oniguruma CI job runs it on Windows and macOS as well.

use onig::{Regex, RegexOptions, Syntax};

fn regex(pattern: &str) -> Regex {
    Regex::with_options(pattern, RegexOptions::REGEX_OPTION_CAPTURE_GROUP, Syntax::default())
        .unwrap_or_else(|e| panic!("Oniguruma rejected {pattern:?}: {e}"))
}

#[test]
fn oniguruma_compiles_the_patterns_textmate_grammars_use() {
    // A lookbehind whose alternatives differ in length (Oniguruma allows that at the top level of the
    // lookbehind; fancy-regex does not, which is why the grammars' Markdown and YAML rules need this engine).
    let behind = regex(r"(?<=fn |let )\w+");
    assert_eq!(behind.find("let value = 1").map(|(s, e)| &"let value = 1"[s..e]), Some("value"));
    // A named group and a subroutine call, as the Markdown grammar writes them.
    let call = regex(r"(?<pair>\[\g<pair>*\])");
    assert!(call.is_match("[[][]]"));
    // A back reference, as a grammar's `end` pattern uses after substituting a `begin` capture.
    let back = regex(r#"(['"]).*?\1"#);
    assert_eq!(back.find("say 'hi' now").map(|(s, e)| &"say 'hi' now"[s..e]), Some("'hi'"));
}

#[test]
fn oniguruma_counts_columns_in_bytes_of_utf8_text() {
    // The interpreter converts these byte offsets to the widget's UTF-16 columns.
    let r = regex("나");
    assert_eq!(r.find("가나다"), Some((3, 6)));
}
