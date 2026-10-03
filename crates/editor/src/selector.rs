//! Document selector matching: a port of `score()` in `src/vs/editor/common/languageSelector.ts`
//! (Code-OSS at `ember_editor_conn::PINNED_COMMIT`).
//!
//! Selectors arrive as `IDocumentFilterDto[]`. The remote extension host has already rewritten
//! `scheme: "file"` to `"vscode-remote"` and relative-pattern base URIs with its URI transformer
//! (`extHostTypeConverters.DocumentSelector.from`), so the filters can be compared with the
//! `vscode-remote://` URIs Ember uses directly. A string selector (`"json"`) is serialized as
//! `{ language: "json" }`, so only filters reach us.
//!
//! Every document Ember opens is "synchronized" with the extension host, so the
//! `candidateIsSynchronized` / `hasAccessToAllModels` branches of upstream are always true here.
//! Notebook filters (`notebookType`) never match a text document.

use ember_editor_conn::exthost::DocumentFilter;
use ember_editor_conn::uri::UriComponents;
use serde_json::Value;

/// Highest score over the filters (upstream returns early at 10). 0 = no match.
pub fn score(selector: &[DocumentFilter], uri: &UriComponents, language: &str) -> u32 {
    let mut best = 0;
    for f in selector {
        let s = score_filter(f, uri, language);
        if s == 10 {
            return 10;
        }
        best = best.max(s);
    }
    best
}

fn score_filter(f: &DocumentFilter, uri: &UriComponents, language: &str) -> u32 {
    let mut ret = 0;
    if let Some(scheme) = f.scheme.as_deref() {
        if scheme == uri.scheme {
            ret = 10;
        } else if scheme == "*" {
            ret = 5;
        } else {
            return 0;
        }
    }
    if let Some(lang) = f.language.as_deref() {
        if lang == language {
            ret = 10;
        } else if lang == "*" {
            ret = ret.max(5);
        } else {
            return 0;
        }
    }
    if f.notebook_type.is_some() {
        // Text documents have no notebook type: upstream returns 0 for anything but a match.
        return 0;
    }
    if let Some(pattern) = &f.pattern {
        // `Uri.fsPath` of a `vscode-remote` URI on a POSIX server is its path.
        let path = uri.path.as_str();
        let matched = match pattern {
            Value::String(p) => p == path || glob_match(p, path),
            Value::Object(o) => {
                let base = o
                    .get("baseUri")
                    .and_then(|b| b.get("path"))
                    .and_then(Value::as_str)
                    .or_else(|| o.get("base").and_then(Value::as_str))
                    .unwrap_or("");
                let pat = o.get("pattern").and_then(Value::as_str).unwrap_or("");
                relative_match(base, pat, path)
            }
            _ => false,
        };
        if matched {
            ret = 10;
        } else {
            return 0;
        }
    }
    ret
}

/// `IRelativePattern` matching: `path` must be under `base`, and the rest must match `pattern`.
fn relative_match(base: &str, pattern: &str, path: &str) -> bool {
    let base = base.trim_end_matches('/');
    match path.strip_prefix(base) {
        Some(rest) if rest.starts_with('/') => glob_match(pattern, &rest[1..]),
        _ => false,
    }
}

/// Glob matching with the syntax VS Code document selectors use: `*` (no `/`), `**` (any
/// number of path segments), `?`, `{a,b}` alternatives (not nested), `[abc]` / `[a-z]` / `[!a]`
/// classes. Paths use `/`.
pub fn glob_match(pattern: &str, path: &str) -> bool {
    expand_braces(pattern).iter().any(|p| {
        let pc: Vec<char> = p.chars().collect();
        let sc: Vec<char> = path.chars().collect();
        match_from(&pc, &sc)
    })
}

fn expand_braces(p: &str) -> Vec<String> {
    let Some(open) = p.find('{') else { return vec![p.to_owned()] };
    let Some(close_rel) = p[open..].find('}') else { return vec![p.to_owned()] };
    let close = open + close_rel;
    let (head, body, tail) = (&p[..open], &p[open + 1..close], &p[close + 1..]);
    let mut out = Vec::new();
    for alt in body.split(',') {
        for rest in expand_braces(tail) {
            out.push(format!("{head}{alt}{rest}"));
        }
    }
    out
}

fn match_from(p: &[char], s: &[char]) -> bool {
    if p.is_empty() {
        return s.is_empty();
    }
    match p[0] {
        '*' if p.get(1) == Some(&'*') => {
            // `**` — also swallow a following `/` so `**/x` matches `x` at the top level.
            let rest = if p.get(2) == Some(&'/') { &p[3..] } else { &p[2..] };
            (0..=s.len()).any(|i| match_from(rest, &s[i..]))
        }
        '*' => {
            let rest = &p[1..];
            let mut i = 0;
            loop {
                if match_from(rest, &s[i..]) {
                    return true;
                }
                if i == s.len() || s[i] == '/' {
                    return false;
                }
                i += 1;
            }
        }
        '?' => !s.is_empty() && s[0] != '/' && match_from(&p[1..], &s[1..]),
        '[' => {
            let Some(close) = p.iter().position(|&c| c == ']') else {
                return !s.is_empty() && s[0] == '[' && match_from(&p[1..], &s[1..]);
            };
            if s.is_empty() || s[0] == '/' {
                return false;
            }
            let class = &p[1..close];
            let (negate, class) = match class.first() {
                Some('!') | Some('^') => (true, &class[1..]),
                _ => (false, class),
            };
            let mut hit = false;
            let mut i = 0;
            while i < class.len() {
                if i + 2 < class.len() && class[i + 1] == '-' {
                    if class[i] <= s[0] && s[0] <= class[i + 2] {
                        hit = true;
                    }
                    i += 3;
                } else {
                    if class[i] == s[0] {
                        hit = true;
                    }
                    i += 1;
                }
            }
            hit != negate && match_from(&p[close + 1..], &s[1..])
        }
        c => !s.is_empty() && s[0] == c && match_from(&p[1..], &s[1..]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn filter(v: Value) -> DocumentFilter {
        serde_json::from_value(v).unwrap()
    }

    fn uri(path: &str) -> UriComponents {
        UriComponents::remote("h:1", path)
    }

    #[test]
    fn language_and_scheme_scores_follow_upstream() {
        let u = uri("/w/a.json");
        assert_eq!(score(&[filter(json!({"$serialized": true, "language": "json"}))], &u, "json"), 10);
        assert_eq!(score(&[filter(json!({"language": "jsonc"}))], &u, "json"), 0);
        assert_eq!(score(&[filter(json!({"language": "*"}))], &u, "json"), 5);
        assert_eq!(score(&[filter(json!({"language": "json", "scheme": "vscode-remote"}))], &u, "json"), 10);
        assert_eq!(score(&[filter(json!({"language": "json", "scheme": "untitled"}))], &u, "json"), 0);
        assert_eq!(score(&[filter(json!({"scheme": "*"}))], &u, "json"), 5);
        // Max over filters.
        assert_eq!(
            score(&[filter(json!({"language": "*"})), filter(json!({"language": "json"}))], &u, "json"),
            10
        );
        // Notebook filters never match text documents; an empty filter scores 0.
        assert_eq!(score(&[filter(json!({"notebookType": "jupyter-notebook"}))], &u, "json"), 0);
        assert_eq!(score(&[filter(json!({}))], &u, "json"), 0);
        assert_eq!(score(&[], &u, "json"), 0);
    }

    #[test]
    fn patterns_string_and_relative() {
        let u = uri("/w/sub/package.json");
        assert_eq!(score(&[filter(json!({"language": "json", "pattern": "**/package.json"}))], &u, "json"), 10);
        assert_eq!(score(&[filter(json!({"language": "json", "pattern": "**/tsconfig.json"}))], &u, "json"), 0);
        let rel = json!({"pattern": {"baseUri": {"$mid": 1, "scheme": "vscode-remote", "authority": "h:1", "path": "/w"}, "pattern": "sub/*.json"}});
        assert_eq!(score(&[filter(rel)], &u, "json"), 10);
        let rel_other = json!({"pattern": {"baseUri": {"scheme": "vscode-remote", "path": "/x"}, "pattern": "**"}});
        assert_eq!(score(&[filter(rel_other)], &u, "json"), 0);
    }

    #[test]
    fn glob_syntax() {
        assert!(glob_match("**/*.rs", "/w/src/main.rs"));
        assert!(glob_match("**/*.rs", "main.rs"));
        assert!(!glob_match("*.rs", "/w/main.rs"));
        assert!(glob_match("/w/*.rs", "/w/main.rs"));
        assert!(!glob_match("/w/*.rs", "/w/src/main.rs"));
        assert!(glob_match("**/*.{json,jsonc}", "/a/b.jsonc"));
        assert!(glob_match("**/[Mm]akefile", "/a/Makefile"));
        assert!(!glob_match("**/[!M]akefile", "/a/Makefile"));
        assert!(glob_match("/a/?.txt", "/a/x.txt"));
        assert!(!glob_match("/a/?.txt", "/a/xy.txt"));
        assert!(glob_match("**", "/anything/at/all"));
    }
}
