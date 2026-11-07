//! Text search through the extension host's own search provider (SPEC FR-E7).
//!
//! The Node extension host registers a ripgrep text search provider for the `file` scheme when its
//! init data says `remote.isRemote` with an authority (`NativeExtHostSearch` constructor,
//! `_registerEHSearchProviders`); the URI transformer turns the scheme into `vscode-remote` on the way
//! out, so Ember sees `MainThreadSearch.$registerTextSearchProvider(handle, "vscode-remote")`.
//!
//! A search is `ExtHostSearch.$provideTextSearchResults(handle, session, query, token)` with a
//! Code-OSS `IRawTextQuery` ([`TextQuery::to_raw`]); the matches arrive as
//! `MainThreadSearch.$handleTextMatch(handle, session, IRawFileMatch2[])` ([`parse_file_matches`]) and
//! the call's reply is `ISearchCompleteStats` ([`SearchComplete::from_json`]). The shapes are those of
//! `TextSearchManager` and its result collector in the pinned extension host bundle: ranges are 0-based
//! lines and 0-based UTF-16 columns, `previewText` is the matched line (at most `charsPerLine` long),
//! and context lines (`{ text, lineNumber }`) are not asked for.

use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::exthost::{proxy, Call};
use crate::rpc::Arg;
use crate::uri::UriComponents;
use crate::Result;

/// `QueryType.Text`.
pub const QUERY_TYPE_TEXT: u8 = 2;

/// What the workbench's search view asks for a preview (`searchView` / `queryBuilder`): one line of at
/// most this many UTF-16 units around each match.
pub const PREVIEW_CHARS_PER_LINE: u32 = 1000;

/// Code-OSS's default `files.exclude` and `search.exclude` patterns, applied while "use exclude
/// settings and ignore files" is on.
pub const DEFAULT_EXCLUDES: [&str; 8] = [
    "**/.git",
    "**/.svn",
    "**/.hg",
    "**/.DS_Store",
    "**/Thumbs.db",
    "**/node_modules",
    "**/bower_components",
    "**/*.code-search",
];

/// One text search over one workspace folder.
#[derive(Debug, Clone, PartialEq)]
pub struct TextQuery {
    pub pattern: String,
    pub is_regexp: bool,
    pub is_case_sensitive: bool,
    pub is_word_match: bool,
    pub folder: UriComponents,
    /// Glob patterns after [`expand_patterns`] (relative to the folder).
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    /// Honour `.gitignore` files and add [`DEFAULT_EXCLUDES`].
    pub use_ignore_files: bool,
    pub max_results: u32,
}

impl TextQuery {
    /// Whether ripgrep must run in multiline mode: a literal with a line break, or a regex that names one.
    pub fn is_multiline(&self) -> bool {
        if self.is_regexp {
            self.pattern.contains("\\n") || self.pattern.contains('\n') || self.pattern.contains("\\r")
        } else {
            self.pattern.contains('\n')
        }
    }

    /// The patterns that exclude, including the defaults when ignore files are used.
    pub fn effective_excludes(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        if self.use_ignore_files {
            out.extend(DEFAULT_EXCLUDES.iter().map(|s| s.to_string()));
        }
        for p in &self.exclude {
            if !out.contains(p) {
                out.push(p.clone());
            }
        }
        out
    }

    /// The `IRawTextQuery` JSON.
    pub fn to_raw(&self) -> Value {
        let disregard = !self.use_ignore_files;
        let mut q = json!({
            "type": QUERY_TYPE_TEXT,
            "contentPattern": {
                "pattern": self.pattern,
                "isRegExp": self.is_regexp,
                "isCaseSensitive": self.is_case_sensitive,
                "isWordMatch": self.is_word_match,
                "isMultiline": self.is_multiline(),
            },
            "folderQueries": [{
                "folder": self.folder,
                "disregardIgnoreFiles": disregard,
                "disregardParentIgnoreFiles": disregard,
                "disregardGlobalIgnoreFiles": disregard,
            }],
            "maxResults": self.max_results,
            "previewOptions": { "matchLines": 1, "charsPerLine": PREVIEW_CHARS_PER_LINE },
            "surroundingContext": 0,
        });
        if !self.include.is_empty() {
            q["includePattern"] = glob_expression(&self.include);
        }
        let excludes = self.effective_excludes();
        if !excludes.is_empty() {
            q["excludePattern"] = glob_expression(&excludes);
        }
        q
    }
}

/// `glob.IExpression`: `{ "<pattern>": true }`.
pub fn glob_expression(patterns: &[String]) -> Value {
    let mut m = Map::new();
    for p in patterns {
        m.insert(p.clone(), Value::Bool(true));
    }
    Value::Object(m)
}

/// Split what the user typed into patterns: commas separate, except inside `{...}`; blanks are dropped.
pub fn split_patterns(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut depth = 0u32;
    for c in input.chars() {
        match c {
            '{' => {
                depth += 1;
                cur.push(c);
            }
            '}' => {
                depth = depth.saturating_sub(1);
                cur.push(c);
            }
            ',' if depth == 0 => {
                let t = cur.trim();
                if !t.is_empty() {
                    out.push(t.to_string());
                }
                cur.clear();
            }
            _ => cur.push(c),
        }
    }
    let t = cur.trim();
    if !t.is_empty() {
        out.push(t.to_string());
    }
    out
}

fn collapse_globstars(p: &str) -> String {
    let mut s = p.to_string();
    while s.contains("**/**") {
        s = s.replace("**/**", "**");
    }
    s
}

/// Expand one typed pattern as Code-OSS's `QueryBuilder` does: `./p` is relative to the folder (`p` and
/// `p/**`); anything else matches at any depth (`**/p` and `**/p/**`). Backslashes become slashes and a
/// trailing slash is dropped.
pub fn expand_pattern(pattern: &str) -> Vec<String> {
    let p = pattern.trim().replace('\\', "/");
    let p = p.trim_end_matches('/');
    if p.is_empty() {
        return Vec::new();
    }
    if let Some(rel) = p.strip_prefix("./") {
        let rel = rel.trim_start_matches('/');
        if rel.is_empty() {
            return vec!["**".to_string()];
        }
        return vec![collapse_globstars(rel), collapse_globstars(&format!("{rel}/**"))];
    }
    let p = p.trim_start_matches('/');
    vec![collapse_globstars(&format!("**/{p}")), collapse_globstars(&format!("**/{p}/**"))]
}

/// [`split_patterns`] then [`expand_pattern`] for each, without duplicates.
pub fn expand_patterns(input: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for p in split_patterns(input) {
        for e in expand_pattern(&p) {
            if !out.contains(&e) {
                out.push(e);
            }
        }
    }
    out
}

/// `ExtHostSearch.$provideTextSearchResults(handle, session, query, token)`.
pub fn provide_text_search_results(handle: i64, session: i64, query: &TextQuery) -> Call {
    Call {
        proxy: proxy::EXT_HOST_SEARCH,
        method: "$provideTextSearchResults",
        args: vec![Arg::Json(handle.into()), Arg::Json(session.into()), Arg::Json(query.to_raw())],
        cancellable: true,
    }
}

// ---- results --------------------------------------------------------------------------------

/// A range in search results: 0-based lines, 0-based UTF-16 columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchRange {
    pub start_line_number: u32,
    pub start_column: u32,
    pub end_line_number: u32,
    pub end_column: u32,
}

/// Where a match is in the file (`source`) and in the preview text (`preview`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct RangeLocation {
    pub source: SearchRange,
    pub preview: SearchRange,
}

/// One `ITextSearchMatch`: the preview line and the matches on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextMatch {
    pub preview_text: String,
    pub ranges: Vec<RangeLocation>,
}

/// One `IRawFileMatch2`: a file and its matches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileMatch {
    pub resource: UriComponents,
    pub matches: Vec<TextMatch>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawResult {
    preview_text: Option<String>,
    range_locations: Option<Vec<RangeLocation>>,
}

#[derive(Deserialize)]
struct RawFileMatch {
    resource: UriComponents,
    #[serde(default)]
    results: Vec<RawResult>,
}

/// Decode the `IRawFileMatch2[]` argument of `$handleTextMatch`. Context lines are dropped.
pub fn parse_file_matches(v: &Value) -> Result<Vec<FileMatch>> {
    let raw: Vec<RawFileMatch> = serde_json::from_value(v.clone())?;
    Ok(raw
        .into_iter()
        .map(|f| FileMatch {
            resource: f.resource,
            matches: f
                .results
                .into_iter()
                .filter_map(|r| match (r.preview_text, r.range_locations) {
                    (Some(preview_text), Some(ranges)) => Some(TextMatch { preview_text, ranges }),
                    _ => None,
                })
                .collect(),
        })
        .collect())
}

/// `ISearchCompleteStats`: whether the provider stopped at `maxResults`, and its messages.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchComplete {
    pub limit_hit: bool,
    pub messages: Vec<String>,
}

impl SearchComplete {
    pub fn from_json(v: &Value) -> Self {
        let limit_hit = v.get("limitHit").and_then(Value::as_bool).unwrap_or(false);
        let messages = v
            .get("messages")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|m| m.get("text").and_then(Value::as_str).or_else(|| m.as_str()))
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        Self { limit_hit, messages }
    }
}

/// The scheme Ember's files have, as the extension host registers it after the URI transformer.
pub fn is_workspace_scheme(scheme: &str) -> bool {
    scheme == crate::uri::SCHEME_REMOTE || scheme == "file"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query() -> TextQuery {
        TextQuery {
            pattern: "foo".into(),
            is_regexp: false,
            is_case_sensitive: true,
            is_word_match: false,
            folder: UriComponents::remote("h", "/w"),
            include: expand_patterns("*.rs"),
            exclude: Vec::new(),
            use_ignore_files: true,
            max_results: 2000,
        }
    }

    #[test]
    fn fr_e7_query_is_a_code_oss_raw_text_query() {
        let q = query().to_raw();
        assert_eq!(q["type"], 2);
        assert_eq!(q["contentPattern"], json!({"pattern": "foo", "isRegExp": false, "isCaseSensitive": true, "isWordMatch": false, "isMultiline": false}));
        assert_eq!(q["folderQueries"][0]["folder"]["path"], "/w");
        assert_eq!(q["folderQueries"][0]["folder"]["$mid"], 1);
        assert_eq!(q["folderQueries"][0]["disregardIgnoreFiles"], false);
        assert_eq!(q["includePattern"], json!({"**/*.rs": true, "**/*.rs/**": true}));
        assert_eq!(q["excludePattern"]["**/node_modules"], true);
        assert_eq!(q["excludePattern"]["**/.git"], true);
        assert_eq!(q["maxResults"], 2000);
        assert_eq!(q["previewOptions"], json!({"matchLines": 1, "charsPerLine": 1000}));
    }

    #[test]
    fn fr_e7_without_ignore_files_nothing_is_excluded_by_default() {
        let mut q = query();
        q.use_ignore_files = false;
        q.include.clear();
        let raw = q.to_raw();
        assert!(raw.get("excludePattern").is_none());
        assert!(raw.get("includePattern").is_none());
        assert_eq!(raw["folderQueries"][0]["disregardIgnoreFiles"], true);
    }

    #[test]
    fn fr_e7_multiline_follows_the_pattern() {
        let mut q = query();
        assert!(!q.is_multiline());
        q.is_regexp = true;
        q.pattern = "a\\nb".into();
        assert!(q.is_multiline());
        q.is_regexp = false;
        assert!(!q.is_multiline());
    }

    #[test]
    fn fr_e7_patterns_expand_like_the_code_oss_query_builder() {
        assert_eq!(expand_pattern("*.rs"), vec!["**/*.rs", "**/*.rs/**"]);
        assert_eq!(expand_pattern("src"), vec!["**/src", "**/src/**"]);
        assert_eq!(expand_pattern("./src/"), vec!["src", "src/**"]);
        assert_eq!(expand_pattern("**/target"), vec!["**/target", "**/target/**"]);
        assert_eq!(expand_pattern("a\\b"), vec!["**/a/b", "**/a/b/**"]);
        assert!(expand_pattern("  ").is_empty());
        assert_eq!(split_patterns("*.rs, {a,b}.md ,, x"), vec!["*.rs", "{a,b}.md", "x"]);
        assert_eq!(expand_patterns("src, src").len(), 2);
    }

    #[test]
    fn fr_e7_text_search_call_is_cancellable_on_ext_host_search() {
        let c = provide_text_search_results(1, 7, &query());
        assert_eq!((c.proxy, c.method, c.cancellable), ("ExtHostSearch", "$provideTextSearchResults", true));
        assert_eq!(c.args.len(), 3);
        assert_eq!(c.args[1], Arg::Json(7.into()));
        assert!(crate::rpc_ids::id_of(c.proxy).is_some());
    }

    #[test]
    fn fr_e7_handle_text_match_decodes_matches_and_drops_context_lines() {
        let v = json!([{
            "resource": {"$mid": 1, "scheme": "vscode-remote", "authority": "h", "path": "/w/a.rs"},
            "results": [
                {"previewText": "let 가foo = 1;\n", "rangeLocations": [{
                    "source": {"startLineNumber": 4, "startColumn": 5, "endLineNumber": 4, "endColumn": 8},
                    "preview": {"startLineNumber": 0, "startColumn": 5, "endLineNumber": 0, "endColumn": 8}
                }]},
                {"text": "context", "lineNumber": 3}
            ]
        }]);
        let files = parse_file_matches(&v).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].resource, UriComponents::remote("h", "/w/a.rs"));
        assert_eq!(files[0].matches.len(), 1);
        assert_eq!(files[0].matches[0].ranges[0].source.start_line_number, 4);
        assert_eq!(files[0].matches[0].ranges[0].preview.end_column, 8);
    }

    #[test]
    fn fr_e7_complete_stats_say_whether_the_limit_was_hit() {
        let c = SearchComplete::from_json(&json!({"limitHit": true, "messages": [{"text": "slow", "type": 1}], "stats": {"type": "searchProcess"}}));
        assert!(c.limit_hit);
        assert_eq!(c.messages, vec!["slow"]);
        assert_eq!(SearchComplete::from_json(&Value::Null), SearchComplete::default());
    }
}
