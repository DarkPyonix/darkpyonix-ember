//! The completion list: `$provideCompletionItems` replies into items, the filter the list applies as
//! the reader types, and what accepting an item inserts.
//!
//! `ISuggestResultDto` (extHost.protocol.ts L2716) is `{ a: { insert, replace }, b: ISuggestDataDto[],
//! c?: true, d: number, x?: number }`: the default ranges, the items, "incomplete", the provider's
//! duration and the cache id to release. `ISuggestDataDto` (L2687) names its fields with single
//! letters (`ISuggestDataDtoField`, L2668). The defaults for a missing field are the ones
//! `MainThreadLanguageFeatures._inflateSuggestDto` fills in: the kind is `Property`, the insert text
//! is the label, the sort and filter texts are the label, the range is the list's default.

use serde_json::Value;

use crate::codelens::CommandDto;
use crate::coords::{utf16_len, WidgetRange};
use crate::hover::MarkdownString;
use crate::language::edits::{parse_range, parse_text_edits, TextEdit};

/// `CompletionItemKind.Property` (`languages.ts`), the kind of an item that names none.
pub const KIND_PROPERTY: u32 = 9;
/// `CompletionItemInsertTextRule.InsertAsSnippet`.
const INSERT_AS_SNIPPET: u64 = 4;
/// `CompletionItemTag.Deprecated`.
const TAG_DEPRECATED: u64 = 1;

/// One entry of the list.
#[derive(Debug, Clone, PartialEq)]
pub struct CompletionItem {
    pub label: String,
    /// `CompletionItemLabel.detail`: drawn right after the label (a signature, for instance).
    pub label_detail: Option<String>,
    /// `CompletionItemLabel.description`: drawn at the end of the row (a module, for instance).
    pub description: Option<String>,
    /// `CompletionItemKind` as the protocol numbers it (see [`kind_name`]).
    pub kind: u32,
    pub detail: Option<String>,
    pub documentation: Option<MarkdownString>,
    pub sort_text: String,
    pub filter_text: String,
    pub preselect: bool,
    pub deprecated: bool,
    pub insert_text: String,
    /// The insert text is a snippet (`$1`, `${1:name}`, `$0`).
    pub is_snippet: bool,
    /// What accepting replaces when the text after the caret is kept (Code-OSS's default).
    pub insert_range: WidgetRange,
    /// What accepting replaces when the word under the caret is overwritten.
    pub replace_range: WidgetRange,
    pub commit_characters: String,
    pub additional_edits: Vec<TextEdit>,
    pub command: Option<CommandDto>,
    /// The provider the item came from.
    pub handle: i64,
    /// `x`: what `$resolveCompletionItem` takes.
    pub cache_id: Option<(i64, i64)>,
}

/// One provider's answer.
#[derive(Debug, Clone, PartialEq)]
pub struct CompletionList {
    pub handle: i64,
    /// `x`: what `$releaseCompletionItems` takes.
    pub cache_id: Option<i64>,
    pub incomplete: bool,
    pub items: Vec<CompletionItem>,
}

fn text(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn ranges(v: Option<&Value>) -> Option<(WidgetRange, WidgetRange)> {
    let v = v?;
    match (v.get("insert"), v.get("replace")) {
        (Some(insert), Some(replace)) => Some((parse_range(insert)?, parse_range(replace)?)),
        _ => parse_range(v).map(|r| (r, r)),
    }
}

/// One `ISuggestDataDto`. `default` is the list's `(insert, replace)` range for an item without one.
pub fn inflate_item(handle: i64, data: &Value, default: (WidgetRange, WidgetRange)) -> Option<CompletionItem> {
    let (label, label_detail, description) = match data.get("a")? {
        Value::String(s) => (s.clone(), None, None),
        obj => (obj.get("label")?.as_str()?.to_owned(), text(obj, "detail"), text(obj, "description")),
    };
    let (insert_range, replace_range) = ranges(data.get("j")).unwrap_or(default);
    let cache_id = data.get("x").and_then(Value::as_array).and_then(|a| Some((a.first()?.as_i64()?, a.get(1)?.as_i64()?)));
    let command = text(data, "o").map(|id| CommandDto {
        id,
        title: String::new(),
        tooltip: None,
        arguments: data.get("p").and_then(Value::as_array).cloned().unwrap_or_default(),
    });
    Some(CompletionItem {
        kind: data.get("b").and_then(Value::as_u64).map_or(KIND_PROPERTY, |k| k as u32),
        detail: text(data, "c"),
        documentation: data.get("d").and_then(MarkdownString::from_json),
        sort_text: text(data, "e").unwrap_or_else(|| label.clone()),
        filter_text: text(data, "f").unwrap_or_else(|| label.clone()),
        preselect: data.get("g").and_then(Value::as_bool).unwrap_or(false),
        insert_text: text(data, "h").unwrap_or_else(|| label.clone()),
        is_snippet: data.get("i").and_then(Value::as_u64).is_some_and(|rules| rules & INSERT_AS_SNIPPET != 0),
        insert_range,
        replace_range,
        commit_characters: text(data, "k").unwrap_or_default(),
        additional_edits: data.get("l").map(parse_text_edits).unwrap_or_default(),
        deprecated: data.get("m").and_then(Value::as_array).is_some_and(|tags| tags.iter().any(|t| t.as_u64() == Some(TAG_DEPRECATED))),
        command,
        handle,
        cache_id,
        label,
        label_detail,
        description,
    })
}

/// An `ISuggestResultDto`. `fallback` is the range for a reply without default ranges. `None` for
/// `undefined` (the provider has nothing to say).
pub fn parse_suggest_result(handle: i64, v: &Value, fallback: (WidgetRange, WidgetRange)) -> Option<CompletionList> {
    let entries = v.get("b")?.as_array()?;
    let default = ranges(v.get("a")).unwrap_or(fallback);
    Some(CompletionList {
        handle,
        cache_id: v.get("x").and_then(Value::as_i64),
        incomplete: v.get("c").and_then(Value::as_bool).unwrap_or(false),
        items: entries.iter().filter_map(|data| inflate_item(handle, data, default)).collect(),
    })
}

/// The Korean name of a `CompletionItemKind`, for the list's row.
pub fn kind_name(kind: u32) -> &'static str {
    match kind {
        0 => "메서드",
        1 => "함수",
        2 => "생성자",
        3 => "필드",
        4 => "변수",
        5 => "클래스",
        6 => "구조체",
        7 => "인터페이스",
        8 => "모듈",
        9 => "속성",
        10 => "이벤트",
        11 => "연산자",
        12 => "단위",
        13 => "값",
        14 => "상수",
        15 => "열거형",
        16 => "열거형 멤버",
        17 => "키워드",
        18 => "텍스트",
        19 => "색",
        20 => "파일",
        21 => "참조",
        23 => "폴더",
        24 => "타입 매개변수",
        27 => "스니펫",
        _ => "항목",
    }
}

/// How well `query` matches `candidate`, smaller is better; `None` when it does not match.
/// A prefix beats a substring, which beats letters in order with gaps; case matters only to break
/// a tie between two prefixes.
fn match_rank(candidate: &str, query: &str) -> Option<u8> {
    if query.is_empty() {
        return Some(0);
    }
    if candidate.starts_with(query) {
        return Some(0);
    }
    let (c, q) = (candidate.to_lowercase(), query.to_lowercase());
    if c.starts_with(&q) {
        return Some(1);
    }
    if c.contains(&q) {
        return Some(2);
    }
    let mut rest = c.chars();
    q.chars().all(|want| rest.any(|have| have == want)).then_some(3)
}

/// The indices of the items `query` matches, best first: by how well the filter text matches, then
/// by sort text, then by label. An empty query keeps every item.
pub fn filter(items: &[CompletionItem], query: &str) -> Vec<usize> {
    let mut ranked: Vec<(u8, usize)> =
        items.iter().enumerate().filter_map(|(i, item)| match_rank(&item.filter_text, query).map(|rank| (rank, i))).collect();
    ranked.sort_by(|(ra, a), (rb, b)| {
        ra.cmp(rb).then_with(|| items[*a].sort_text.cmp(&items[*b].sort_text)).then_with(|| items[*a].label.cmp(&items[*b].label))
    });
    ranked.into_iter().map(|(_, i)| i).collect()
}

/// A snippet turned into the text it inserts, and the UTF-16 offset inside that text where the
/// caret belongs: the first tab stop (`$1`), or the final one (`$0`), or `None` for the end.
///
/// A tab stop with a placeholder (`${1:name}`) inserts the placeholder, a choice (`${1|a,b|}`)
/// its first option, and a variable (`$TM_FILENAME`, `${VAR:default}`) its default or nothing.
/// Ember has no snippet session: the stops are not linked and Tab does not move between them.
// TODO(compose-rust#556): hand the snippet to the widget's snippet session and place the caret.
pub fn expand_snippet(snippet: &str) -> (String, Option<u32>) {
    let chars: Vec<char> = snippet.chars().collect();
    let mut out = String::new();
    let mut stops: Vec<(u32, u32)> = Vec::new();
    let mut at = 0usize;
    expand_into(&chars, &mut at, false, &mut out, &mut stops);
    let first = stops.iter().filter(|(n, _)| *n > 0).min_by_key(|(n, _)| *n);
    let caret = first.or_else(|| stops.iter().find(|(n, _)| *n == 0)).map(|(_, offset)| *offset);
    (out, caret)
}

fn expand_into(chars: &[char], at: &mut usize, nested: bool, out: &mut String, stops: &mut Vec<(u32, u32)>) {
    while *at < chars.len() {
        let c = chars[*at];
        match c {
            '\\' if matches!(chars.get(*at + 1).copied(), Some('$' | '}' | '\\')) => {
                out.push(chars[*at + 1]);
                *at += 2;
            }
            '}' if nested => return,
            '$' => {
                *at += 1;
                expand_dollar(chars, at, out, stops);
            }
            _ => {
                out.push(c);
                *at += 1;
            }
        }
    }
}

fn take_while(chars: &[char], at: &mut usize, keep: impl Fn(char) -> bool) -> String {
    let start = *at;
    while *at < chars.len() && keep(chars[*at]) {
        *at += 1;
    }
    chars[start..*at].iter().collect()
}

/// What follows a `$`: `1`, `name`, `{1}`, `{1:placeholder}`, `{1|a,b|}`, `{name}`, `{name:default}`.
fn expand_dollar(chars: &[char], at: &mut usize, out: &mut String, stops: &mut Vec<(u32, u32)>) {
    let braced = chars.get(*at) == Some(&'{');
    if braced {
        *at += 1;
    }
    let digits = take_while(chars, at, |c| c.is_ascii_digit());
    let name = if digits.is_empty() { take_while(chars, at, |c| c.is_ascii_alphanumeric() || c == '_') } else { String::new() };
    if digits.is_empty() && name.is_empty() {
        // A lone `$` (or `${` followed by neither): literal text.
        out.push('$');
        if braced {
            out.push('{');
        }
        return;
    }
    if let Ok(n) = digits.parse::<u32>() {
        stops.push((n, utf16_len(out)));
    }
    if !braced {
        return;
    }
    match chars.get(*at).copied() {
        Some(':') => {
            *at += 1;
            expand_into(chars, at, true, out, stops);
        }
        Some('|') => {
            *at += 1;
            let options = take_while(chars, at, |c| c != '|');
            out.push_str(options.split(',').next().unwrap_or_default());
            *at += 1; // the closing `|`
        }
        Some('/') => {
            // A transform (`${1/regex/format/options}`): nothing is inserted for it.
            let _ = take_while(chars, at, |c| c != '}');
        }
        _ => {}
    }
    if chars.get(*at) == Some(&'}') {
        *at += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coords::WidgetPos;
    use serde_json::json;

    fn range(sl: u32, sc: u32, el: u32, ec: u32) -> WidgetRange {
        WidgetRange::new(WidgetPos::new(sl, sc), WidgetPos::new(el, ec))
    }

    fn irange(sl: u32, sc: u32, el: u32, ec: u32) -> Value {
        json!({"startLineNumber": sl, "startColumn": sc, "endLineNumber": el, "endColumn": ec})
    }

    const FALLBACK: (WidgetRange, WidgetRange) = (WidgetRange::empty(WidgetPos::new(9, 9)), WidgetRange::empty(WidgetPos::new(9, 9)));

    #[test]
    fn fr_l31_13_suggest_result_fills_the_defaults_upstream_fills() {
        let v = json!({
            "a": {"insert": irange(2, 5, 2, 7), "replace": irange(2, 5, 2, 9)},
            "b": [{"a": "push", "x": [4, 0]}],
            "d": 3,
            "x": 4
        });
        let list = parse_suggest_result(7, &v, FALLBACK).unwrap();
        assert_eq!((list.handle, list.cache_id, list.incomplete), (7, Some(4), false));
        let item = &list.items[0];
        assert_eq!(item.label, "push");
        assert_eq!(item.kind, KIND_PROPERTY);
        assert_eq!((item.insert_text.as_str(), item.sort_text.as_str(), item.filter_text.as_str()), ("push", "push", "push"));
        assert_eq!(item.insert_range, range(1, 4, 1, 6));
        assert_eq!(item.replace_range, range(1, 4, 1, 8));
        assert_eq!((item.handle, item.cache_id), (7, Some((4, 0))));
        assert!(!item.is_snippet && !item.preselect && !item.deprecated && item.command.is_none());
    }

    #[test]
    fn fr_l31_13_suggest_item_decodes_every_field() {
        let data = json!({
            "a": {"label": "len", "detail": "()", "description": "Vec<T>"},
            "b": 0,
            "c": "fn len(&self) -> usize",
            "d": {"value": "Returns the number of elements."},
            "e": "0001",
            "f": "len",
            "g": true,
            "h": "len()$0",
            "i": 4,
            "j": irange(1, 3, 1, 5),
            "k": ".;",
            "l": [{"range": irange(1, 1, 1, 1), "text": "use std::vec::Vec;\n"}],
            "m": [1],
            "n": "ident",
            "o": "editor.action.triggerParameterHints",
            "p": [1, "two"],
            "x": [8, 3]
        });
        let item = inflate_item(2, &data, FALLBACK).unwrap();
        assert_eq!(item.label, "len");
        assert_eq!(item.label_detail.as_deref(), Some("()"));
        assert_eq!(item.description.as_deref(), Some("Vec<T>"));
        assert_eq!(item.kind, 0);
        assert_eq!(item.detail.as_deref(), Some("fn len(&self) -> usize"));
        assert_eq!(item.documentation.as_ref().unwrap().value, "Returns the number of elements.");
        assert_eq!(item.sort_text, "0001");
        assert!(item.preselect && item.is_snippet && item.deprecated);
        assert_eq!(item.insert_text, "len()$0");
        assert_eq!((item.insert_range, item.replace_range), (range(0, 2, 0, 4), range(0, 2, 0, 4)));
        assert_eq!(item.commit_characters, ".;");
        assert_eq!(item.additional_edits, vec![TextEdit { range: range(0, 0, 0, 0), text: "use std::vec::Vec;\n".into() }]);
        let command = item.command.unwrap();
        assert_eq!(command.id, "editor.action.triggerParameterHints");
        assert_eq!(command.arguments, vec![json!(1), json!("two")]);
        assert_eq!(item.cache_id, Some((8, 3)));
    }

    #[test]
    fn fr_l31_13_suggest_result_without_ranges_uses_the_fallback_and_undefined_is_none() {
        let list = parse_suggest_result(1, &json!({"b": [{"a": "x"}, {"no": "label"}], "c": true}), FALLBACK).unwrap();
        assert!(list.incomplete);
        assert_eq!(list.cache_id, None);
        assert_eq!(list.items.len(), 1);
        assert_eq!(list.items[0].insert_range, FALLBACK.0);
        assert!(parse_suggest_result(1, &Value::Null, FALLBACK).is_none());
    }

    fn named(label: &str, sort: &str) -> CompletionItem {
        inflate_item(1, &json!({"a": label, "e": sort}), FALLBACK).unwrap()
    }

    #[test]
    fn fr_l31_13_filter_ranks_prefix_then_substring_then_letters_in_order() {
        let items = vec![named("append", "b"), named("push", "a"), named("pop", "c"), named("spush", "a"), named("Pin", "a"), named("up_sh", "a")];
        let labels = |q: &str| filter(&items, q).into_iter().map(|i| items[i].label.as_str()).collect::<Vec<_>>();
        // No query: sort text, then label.
        assert_eq!(labels(""), ["Pin", "push", "spush", "up_sh", "append", "pop"]);
        // Case-exact prefixes, then the prefix in another case, then substrings, then scattered letters.
        assert_eq!(labels("p"), ["push", "pop", "Pin", "spush", "up_sh", "append"]);
        assert_eq!(labels("pu"), ["push", "spush"]);
        assert_eq!(labels("psh"), ["push", "spush", "up_sh"]);
        assert!(labels("zz").is_empty());
    }

    #[test]
    fn fr_l31_13_snippets_expand_to_plain_text_with_a_caret() {
        assert_eq!(expand_snippet("len()"), ("len()".to_string(), None));
        assert_eq!(expand_snippet("push($1)$0"), ("push()".to_string(), Some(5)));
        assert_eq!(expand_snippet("println!(\"$0\")"), ("println!(\"\")".to_string(), Some(10)));
        assert_eq!(expand_snippet("for ${1:item} in ${2:iter} {\n\t$0\n}"), ("for item in iter {\n\t\n}".to_string(), Some(4)));
        assert_eq!(expand_snippet("${1|one,two|} ${TM_FILENAME:main.rs} $UNKNOWN!"), ("one main.rs !".to_string(), Some(0)));
        assert_eq!(expand_snippet("cost: \\$5 \\} \\\\ $"), ("cost: $5 } \\ $".to_string(), None));
        assert_eq!(expand_snippet("${1:a${2:b}}"), ("ab".to_string(), Some(0)));
    }
}
