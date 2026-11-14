//! One TextMate grammar file, parsed into the raw rule model.
//!
//! Two forms are read: JSON (`*.tmLanguage.json`, `*.json`) and XML property lists (`*.tmLanguage`, `*.plist`,
//! see [`super::plist`]). VS Code also accepts YAML grammar files; a path with another extension is refused with
//! [`GrammarError::UnsupportedFormat`] so the caller can say so rather than tokenize nothing silently.
//!
//! A grammar file comes from an extension, so it is input from outside the editor. The reader is as
//! forgiving as vscode-textmate: a value of the wrong type (`"patterns": null`, `"name": 5`, a string
//! where a rule belongs, `"disabled": "x"`) is dropped and the rest of the grammar is kept. It is
//! strict about size: the file, the number of rules and the nesting depth are capped, and an error
//! never repeats the file's text.
//!
//! The model keeps every field the interpreter needs (`match`, `begin`/`end`/`while`, the capture
//! maps, `patterns`, `include`, `repository`, `injections`, `applyEndPatternLast`) and drops the
//! editor-only ones. Regular expressions stay as text: compiling them is the interpreter's job,
//! because an `end` pattern may refer to captures of its `begin` and is only known while a line is
//! being tokenized.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;

use serde_json::{Map, Value};

/// The largest grammar file read. The biggest in a Code-OSS build is under 0.5 MiB.
pub const MAX_GRAMMAR_BYTES: u64 = 4 * 1024 * 1024;
/// The most rules (every nested rule and capture counts) one grammar may hold.
pub const MAX_RULES: usize = 100_000;

#[derive(Debug, thiserror::Error)]
pub enum GrammarError {
    /// `at` is the line and column; the text of the file is never part of the message.
    /// Also what a document nested more than 128 levels deep (about 64 rules) is refused with: the JSON
    /// reader's own recursion limit, which comes first.
    #[error("grammar is not valid JSON or is nested too deeply (line {line}, column {column})")]
    Json { line: usize, column: usize },
    #[error("grammar has no scopeName")]
    MissingScopeName,
    #[error("grammar file {0} is neither JSON nor an XML property list (only .json, .tmLanguage and .plist are read)")]
    UnsupportedFormat(String),
    #[error("{0}")]
    Plist(super::plist::PlistError),
    #[error("grammar file is larger than {MAX_GRAMMAR_BYTES} bytes")]
    TooLarge,
    #[error("grammar has more than {MAX_RULES} rules")]
    TooManyRules,
    #[error("grammar file is outside the extension folder")]
    OutsideExtension,
    #[error("grammar file could not be read: {0}")]
    Io(String),
}

/// A grammar file: the scope it defines, its top-level rules, and the repository `include`s resolve in.
#[derive(Debug, Clone, Default)]
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
#[derive(Debug, Clone, Default)]
pub struct RawRule {
    pub name: Option<String>,
    pub content_name: Option<String>,
    pub match_: Option<String>,
    pub begin: Option<String>,
    pub end: Option<String>,
    pub while_: Option<String>,
    pub captures: BTreeMap<String, RawRule>,
    pub begin_captures: BTreeMap<String, RawRule>,
    pub end_captures: BTreeMap<String, RawRule>,
    pub while_captures: BTreeMap<String, RawRule>,
    pub patterns: Vec<RawRule>,
    pub include: Option<String>,
    pub repository: BTreeMap<String, RawRule>,
    pub apply_end_pattern_last: bool,
    pub disabled: bool,
}

/// Counts what has been built, against [`MAX_RULES`].
struct Budget {
    rules: usize,
}

fn text(obj: &Map<String, Value>, key: &str) -> Option<String> {
    obj.get(key).and_then(Value::as_str).map(str::to_owned)
}

/// Grammars write flags as `1`, `0`, `true` or `false`; anything else is false.
fn flag(obj: &Map<String, Value>, key: &str) -> bool {
    match obj.get(key) {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_i64().is_some_and(|n| n != 0),
        _ => false,
    }
}

/// An object of rules (`repository`, a capture map): entries that are not objects are dropped.
fn rule_map(obj: &Map<String, Value>, key: &str, budget: &mut Budget) -> Result<BTreeMap<String, RawRule>, GrammarError> {
    let mut out = BTreeMap::new();
    if let Some(Value::Object(map)) = obj.get(key) {
        for (name, value) in map {
            if let Value::Object(rule) = value {
                out.insert(name.clone(), rule_from(rule, budget)?);
            }
        }
    }
    Ok(out)
}

/// A list of rules (`patterns`): entries that are not objects are dropped.
fn rule_list(obj: &Map<String, Value>, key: &str, budget: &mut Budget) -> Result<Vec<RawRule>, GrammarError> {
    let mut out = Vec::new();
    if let Some(Value::Array(items)) = obj.get(key) {
        for value in items {
            if let Value::Object(rule) = value {
                out.push(rule_from(rule, budget)?);
            }
        }
    }
    Ok(out)
}

fn rule_from(obj: &Map<String, Value>, budget: &mut Budget) -> Result<RawRule, GrammarError> {
    budget.rules += 1;
    if budget.rules > MAX_RULES {
        return Err(GrammarError::TooManyRules);
    }
    Ok(RawRule {
        name: text(obj, "name"),
        content_name: text(obj, "contentName"),
        match_: text(obj, "match"),
        begin: text(obj, "begin"),
        end: text(obj, "end"),
        while_: text(obj, "while"),
        captures: rule_map(obj, "captures", budget)?,
        begin_captures: rule_map(obj, "beginCaptures", budget)?,
        end_captures: rule_map(obj, "endCaptures", budget)?,
        while_captures: rule_map(obj, "whileCaptures", budget)?,
        patterns: rule_list(obj, "patterns", budget)?,
        include: text(obj, "include"),
        repository: rule_map(obj, "repository", budget)?,
        apply_end_pattern_last: flag(obj, "applyEndPatternLast"),
        disabled: flag(obj, "disabled"),
    })
}

impl RawGrammar {
    /// The scope names of the other grammars this one includes (`source.js.regexp`, or the part before the `#`
    /// of `source.css#rule-list`), in order of first use, without `$self`, `$base` and local `#name` includes.
    pub fn referenced_scopes(&self) -> Vec<String> {
        fn walk(rule: &RawRule, own: &str, out: &mut Vec<String>, seen: &mut std::collections::HashSet<String>) {
            if let Some(include) = &rule.include {
                let scope = include.split('#').next().unwrap_or("");
                if !scope.is_empty() && !scope.starts_with('$') && scope != own && seen.insert(scope.to_owned()) {
                    out.push(scope.to_owned());
                }
            }
            let maps = [&rule.captures, &rule.begin_captures, &rule.end_captures, &rule.while_captures, &rule.repository];
            for map in maps {
                for r in map.values() {
                    walk(r, own, out, seen);
                }
            }
            for r in &rule.patterns {
                walk(r, own, out, seen);
            }
        }
        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for r in self.patterns.iter().chain(self.repository.values()).chain(self.injections.values()) {
            walk(r, &self.scope_name, &mut out, &mut seen);
        }
        out
    }
}

/// Parses the text of a grammar file. `path` decides the format: `.json` is JSON, `.tmLanguage` and `.plist` are an XML
/// property list, anything else is refused.
pub fn parse_grammar(path: &str, content: &str) -> Result<RawGrammar, GrammarError> {
    let lower = path.to_lowercase();
    let is_plist = lower.ends_with(".tmlanguage") || lower.ends_with(".plist");
    if !lower.ends_with(".json") && !is_plist {
        return Err(GrammarError::UnsupportedFormat(path.chars().filter(|c| !c.is_control()).take(120).collect()));
    }
    if content.len() as u64 > MAX_GRAMMAR_BYTES {
        return Err(GrammarError::TooLarge);
    }
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    let value: Value = if is_plist {
        super::plist::parse_plist(content).map_err(GrammarError::Plist)?
    } else {
        serde_json::from_str(content).map_err(|e| GrammarError::Json { line: e.line(), column: e.column() })?
    };
    grammar_from_value(value)
}

/// The rule model of a grammar read into a value, whatever its format.
fn grammar_from_value(value: Value) -> Result<RawGrammar, GrammarError> {
    let Value::Object(root) = value else {
        return Err(GrammarError::MissingScopeName);
    };
    let scope_name = text(&root, "scopeName").filter(|s| !s.is_empty()).ok_or(GrammarError::MissingScopeName)?;
    let mut budget = Budget { rules: 0 };
    Ok(RawGrammar {
        name: text(&root, "name"),
        scope_name,
        patterns: rule_list(&root, "patterns", &mut budget)?,
        repository: rule_map(&root, "repository", &mut budget)?,
        injections: rule_map(&root, "injections", &mut budget)?,
        injection_selector: text(&root, "injectionSelector"),
    })
}

/// Reads a grammar file from the local disk, on behalf of one extension. The file must lie inside the
/// extension folder once symbolic links are resolved (`path` may be relative or contain `..`; a link in
/// the folder that leads out of it is refused), and its size is checked before it is read.
///
/// This reads the machine the editor runs on. A grammar on a server must be read through a reader that
/// makes the same checks on the server's side. A path component swapped for a link between the check
/// and the open is not guarded against: the extension folder belongs to the user running the editor.
pub fn read_grammar(extension_dir: &Path, path: &Path) -> Result<RawGrammar, GrammarError> {
    let dir = std::fs::canonicalize(extension_dir).map_err(|e| GrammarError::Io(e.kind().to_string()))?;
    let file = std::fs::canonicalize(path).map_err(|e| GrammarError::Io(e.kind().to_string()))?;
    if !file.starts_with(&dir) {
        return Err(GrammarError::OutsideExtension);
    }
    // A regular file only, checked before opening: opening a named pipe would wait for a writer.
    let meta = std::fs::metadata(&file).map_err(|e| GrammarError::Io(e.kind().to_string()))?;
    if !meta.is_file() {
        return Err(GrammarError::Io("not a regular file".to_owned()));
    }
    let f = std::fs::File::open(&file).map_err(|e| GrammarError::Io(e.kind().to_string()))?;
    if meta.len() > MAX_GRAMMAR_BYTES {
        return Err(GrammarError::TooLarge);
    }
    // The size can change between the check and the read: read at most one byte over the cap.
    let mut bytes = Vec::new();
    f.take(MAX_GRAMMAR_BYTES + 1).read_to_end(&mut bytes).map_err(|e| GrammarError::Io(e.kind().to_string()))?;
    if bytes.len() as u64 > MAX_GRAMMAR_BYTES {
        return Err(GrammarError::TooLarge);
    }
    let content = String::from_utf8(bytes).map_err(|_| GrammarError::Io("not UTF-8".to_owned()))?;
    parse_grammar(&file.to_string_lossy(), &content)
}
