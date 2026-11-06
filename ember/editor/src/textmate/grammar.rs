//! One TextMate grammar file, parsed into the raw rule model.
//!
//! Only the JSON form (`*.tmLanguage.json`, `*.json`) is read. VS Code also accepts plist and YAML
//! grammar files; a path with another extension is refused with [`GrammarError::UnsupportedFormat`]
//! so the caller can say so rather than tokenize nothing silently.
//!
//! The model keeps every field of the format the interpreter needs (`match`, `begin`/`end`/`while`,
//! the capture maps, `patterns`, `include`, `repository`, `injections`, `applyEndPatternLast`) and
//! drops the editor-only ones (`foldingStartMarker`, `fileTypes`, ...). Regular expressions stay as
//! text: compiling them is the interpreter's job, because an `end` pattern may refer to captures of
//! its `begin` and is only known while a line is being tokenized.

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer};

#[derive(Debug, thiserror::Error)]
pub enum GrammarError {
    #[error("grammar is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("grammar has no scopeName")]
    MissingScopeName,
    #[error("grammar file {0} is not JSON (only .json grammars are read)")]
    UnsupportedFormat(String),
}

/// A grammar file: the scope it defines, its top-level rules, and the repository `include`s resolve in.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RawGrammar {
    pub name: Option<String>,
    pub scope_name: String,
    pub patterns: Vec<RawRule>,
    pub repository: BTreeMap<String, RawRule>,
    /// Rules injected into other scopes: the key is a scope selector, the value a rule holding patterns.
    pub injections: BTreeMap<String, RawRule>,
    /// A selector naming scopes this grammar is injected into (grammar-level form of `injections`).
    pub injection_selector: Option<String>,
}

/// One rule. Which fields are set decides its kind: `match` (a single pattern), `begin` with `end` or
/// `while` (a region), `include` (a reference), or only `patterns` (a group).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RawRule {
    pub name: Option<String>,
    pub content_name: Option<String>,
    #[serde(rename = "match")]
    pub match_: Option<String>,
    pub begin: Option<String>,
    pub end: Option<String>,
    #[serde(rename = "while")]
    pub while_: Option<String>,
    #[serde(deserialize_with = "capture_map")]
    pub captures: BTreeMap<String, RawRule>,
    #[serde(deserialize_with = "capture_map")]
    pub begin_captures: BTreeMap<String, RawRule>,
    #[serde(deserialize_with = "capture_map")]
    pub end_captures: BTreeMap<String, RawRule>,
    #[serde(deserialize_with = "capture_map")]
    pub while_captures: BTreeMap<String, RawRule>,
    pub patterns: Vec<RawRule>,
    pub include: Option<String>,
    pub repository: BTreeMap<String, RawRule>,
    #[serde(deserialize_with = "flag")]
    pub apply_end_pattern_last: bool,
    #[serde(deserialize_with = "flag")]
    pub disabled: bool,
}

/// A capture map. Real grammars contain stray non-rule entries (the XML grammar has `"end": "--%>"` inside a
/// `captures` object); vscode-textmate ignores what is not a rule, so they are dropped here instead of
/// failing the whole grammar.
fn capture_map<'de, D: Deserializer<'de>>(d: D) -> Result<BTreeMap<String, RawRule>, D::Error> {
    let raw = Option::<BTreeMap<String, serde_json::Value>>::deserialize(d)?.unwrap_or_default();
    let mut out = BTreeMap::new();
    for (key, value) in raw {
        if value.is_object() {
            out.insert(key, serde_json::from_value(value).map_err(serde::de::Error::custom)?);
        }
    }
    Ok(out)
}

/// Grammars write flags as `1`, `0`, `true` or `false`.
fn flag<'de, D: Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Flag {
        Bool(bool),
        Int(i64),
    }
    Ok(match Option::<Flag>::deserialize(d)? {
        Some(Flag::Bool(b)) => b,
        Some(Flag::Int(n)) => n != 0,
        None => false,
    })
}

/// Parses the text of a grammar file. `path` is only used to refuse formats that are not JSON.
pub fn parse_grammar(path: &str, text: &str) -> Result<RawGrammar, GrammarError> {
    let lower = path.to_lowercase();
    if !lower.ends_with(".json") {
        return Err(GrammarError::UnsupportedFormat(path.to_owned()));
    }
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let grammar: RawGrammar = serde_json::from_str(text)?;
    if grammar.scope_name.is_empty() {
        return Err(GrammarError::MissingScopeName);
    }
    Ok(grammar)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_grammar_keeps_its_scope_rules_and_repository() {
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
    fn a_region_keeps_begin_end_while_captures_and_content_name() {
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
        assert_eq!(s.patterns[0].name.as_deref(), Some("constant.character.escape"));
        let q = &g.patterns[1];
        assert_eq!(q.while_.as_deref(), Some("^>"));
        assert!(q.apply_end_pattern_last, "1 means true");
    }

    #[test]
    fn flags_accept_numbers_and_booleans_and_default_to_false() {
        let g = parse_grammar(
            "a.json",
            r#"{"scopeName":"s","patterns":[{"match":"a","disabled":1},{"match":"b","disabled":false},{"match":"c","applyEndPatternLast":true},{"match":"d"}]}"#,
        )
        .unwrap();
        let flags: Vec<(bool, bool)> = g.patterns.iter().map(|r| (r.disabled, r.apply_end_pattern_last)).collect();
        assert_eq!(flags, vec![(true, false), (false, false), (false, true), (false, false)]);
    }

    #[test]
    fn injections_and_a_byte_order_mark_are_read() {
        let g = parse_grammar(
            "a.json",
            "\u{feff}{\"scopeName\":\"text.md\",\"injectionSelector\":\"L:source.demo\",\"injections\":{\"L:text.md -comment\":{\"patterns\":[{\"match\":\"x\"}]}}}",
        )
        .unwrap();
        assert_eq!(g.injection_selector.as_deref(), Some("L:source.demo"));
        assert_eq!(g.injections["L:text.md -comment"].patterns.len(), 1);
    }

    #[test]
    fn a_stray_non_rule_entry_in_a_capture_map_is_ignored() {
        let g = parse_grammar(
            "xml.json",
            r#"{"scopeName":"text.xml","patterns":[{"begin":"<%--","captures":{"0":{"name":"punctuation"},"end":"--%>","name":"comment.block.xml"}}]}"#,
        )
        .unwrap();
        let c = &g.patterns[0].captures;
        assert_eq!(c.len(), 1);
        assert_eq!(c["0"].name.as_deref(), Some("punctuation"));
    }

    #[test]
    fn bad_input_is_an_error_not_an_empty_grammar() {
        assert!(matches!(parse_grammar("a.json", "{nope"), Err(GrammarError::Json(_))));
        assert!(matches!(parse_grammar("a.json", "{\"patterns\":[]}"), Err(GrammarError::MissingScopeName)));
        assert!(matches!(parse_grammar("a.tmLanguage", "<plist/>"), Err(GrammarError::UnsupportedFormat(_))));
        assert!(matches!(parse_grammar("a.YAML-tmLanguage", "scopeName: x"), Err(GrammarError::UnsupportedFormat(_))));
    }
}
