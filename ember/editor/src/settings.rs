//! Settings of the Ember 에디터 window (SPEC FR-L31.17, `docs/INTENT.md` D46).
//!
//! Code-OSS keeps settings in JSON files with comments (JSONC) and merges them by scope. This
//! module is the pure part of that: reading JSONC, the scopes and their merge, the settings Ember
//! honours itself, editing one key of a settings file without losing its comments, the list of
//! settings the settings view shows, and `keybindings.json`.
//!
//! | scope | file | where |
//! | ----- | ---- | ----- |
//! | default | none | `contributes.configuration` of every extension, plus [`ember_settings`] |
//! | user | `settings.json` | the client machine, in Ember's data folder |
//! | remote | `Machine/settings.json` | the OSE server's data folder |
//! | workspace | `.vscode/settings.json` | the project folder on the server |
//!
//! A later row wins over an earlier one. A language block (`"[rust]": { ... }`) of any scope wins
//! over every plain key, and among language blocks the later scope wins, as in Code-OSS
//! (`Configuration.getConsolidatedConfigurationModel` then `override`). Object values are merged
//! key by key across scopes, so `files.exclude` of the workspace adds to the default's.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use ember_editor_conn::exthost::{self, ConfigModel, ConfigOverride};
use ember_editor_conn::uri::UriComponents;
use serde::Serialize;
use serde_json::{json, Map, Value};

/// The settings of one scope: plain dotted keys and language override blocks.
pub type SettingsLayer = ConfigModel;

/// The URI scheme of a settings file on the client machine (`Schemas.vscodeUserData`).
pub const SCHEME_USER_DATA: &str = "vscode-userdata";

/// The workspace settings file, relative to the workspace folder.
pub const WORKSPACE_SETTINGS_FILE: &str = ".vscode/settings.json";

/// Where a JSONC text stopped being valid. Lines and columns start at 1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsoncError {
    pub line: u32,
    pub column: u32,
    pub message: String,
}

impl std::fmt::Display for JsoncError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}: {}", self.line, self.column, self.message)
    }
}

impl JsoncError {
    fn at(text: &str, offset: usize, message: &str) -> Self {
        let before = &text.as_bytes()[..offset.min(text.len())];
        let line = before.iter().filter(|b| **b == b'\n').count() as u32 + 1;
        let column = before.iter().rev().take_while(|b| **b != b'\n').count() as u32 + 1;
        Self { line, column, message: message.to_owned() }
    }
}

// ---- JSONC ------------------------------------------------------------------------------------

/// The text with comments blanked out and trailing commas removed, so a strict JSON parser reads
/// it. Every replaced character becomes a space and line breaks stay, so positions in an error
/// still point into the original text.
pub fn strip_jsonc(text: &str) -> String {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    // Pass 1: comments to spaces.
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                out.push(c);
                while let Some(s) = chars.next() {
                    out.push(s);
                    match s {
                        '\\' => {
                            if let Some(escaped) = chars.next() {
                                out.push(escaped);
                            }
                        }
                        '"' => break,
                        _ => {}
                    }
                }
            }
            '/' if chars.peek() == Some(&'/') => {
                out.push(' ');
                while let Some(n) = chars.peek().copied() {
                    if n == '\n' {
                        break;
                    }
                    chars.next();
                    out.push(if n == '\r' { '\r' } else { ' ' });
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                out.push_str("  ");
                let mut star = false;
                for n in chars.by_ref() {
                    if star && n == '/' {
                        out.push(' ');
                        break;
                    }
                    star = n == '*';
                    out.push(if n == '\n' || n == '\r' { n } else { ' ' });
                }
            }
            _ => out.push(c),
        }
    }
    // Pass 2: a comma followed only by white space and a closing bracket is dropped.
    let mut bytes = out.into_bytes();
    let mut i = 0;
    let mut in_string = false;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if in_string => i += 1,
            b'"' => in_string = !in_string,
            b',' if !in_string => {
                let mut j = i + 1;
                while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                    j += 1;
                }
                if j < bytes.len() && (bytes[j] == b'}' || bytes[j] == b']') {
                    bytes[i] = b' ';
                }
            }
            _ => {}
        }
        i += 1;
    }
    // Only ASCII bytes were replaced by ASCII bytes, so the text is still UTF-8.
    String::from_utf8(bytes).unwrap_or_default()
}

/// Reads JSON with `//` and `/* */` comments and trailing commas. An empty file (or one holding
/// only comments) reads as an empty object, as Code-OSS reads an empty settings file.
pub fn parse_jsonc(text: &str) -> Result<Value, JsoncError> {
    let strict = strip_jsonc(text);
    if strict.trim().is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    serde_json::from_str(&strict).map_err(|e| JsoncError {
        line: e.line() as u32,
        column: e.column() as u32,
        message: e.to_string(),
    })
}

/// The language identifiers of an override key: `[rust]` gives `rust`, `[javascript][typescript]`
/// gives both. `None` for any other key.
pub fn override_identifiers(key: &str) -> Option<Vec<String>> {
    let mut rest = key.trim();
    if !rest.starts_with('[') {
        return None;
    }
    let mut ids = Vec::new();
    while !rest.is_empty() {
        let inner = rest.strip_prefix('[')?;
        let end = inner.find(']')?;
        let id = inner[..end].trim();
        if id.is_empty() {
            return None;
        }
        ids.push(id.to_owned());
        rest = inner[end + 1..].trim_start();
    }
    Some(ids)
}

/// One scope from the parsed root of a settings file. The root must be an object.
pub fn layer_from_value(root: &Value) -> Result<SettingsLayer, JsoncError> {
    let Some(object) = root.as_object() else {
        return Err(JsoncError { line: 1, column: 1, message: "the settings file must hold one JSON object".into() });
    };
    let mut layer = SettingsLayer::default();
    for (key, value) in object {
        match (override_identifiers(key), value.as_object()) {
            (Some(identifiers), Some(block)) => layer.overrides.push(ConfigOverride {
                identifiers,
                values: block.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            }),
            // A language key whose value is not an object says nothing.
            (Some(_), None) => {}
            (None, _) => layer.values.push((key.clone(), value.clone())),
        }
    }
    Ok(layer)
}

/// One scope from the text of a settings file.
pub fn parse_layer(text: &str) -> Result<SettingsLayer, JsoncError> {
    layer_from_value(&parse_jsonc(text)?)
}

// ---- scopes -----------------------------------------------------------------------------------

/// Where a setting is written. Ordered from the weakest to the strongest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SettingsScope {
    Default,
    User,
    Remote,
    Workspace,
}

impl SettingsScope {
    pub const ALL: [SettingsScope; 4] = [SettingsScope::Default, SettingsScope::User, SettingsScope::Remote, SettingsScope::Workspace];
}

/// What changed between two sets of scopes, as `IConfigurationChange` names it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SettingsChange {
    /// Plain keys whose value changed in some scope, and the `[language]` key of every language
    /// whose block changed. Sorted.
    pub keys: Vec<String>,
    /// Per language identifier, the keys that changed inside its blocks. Sorted.
    pub overrides: Vec<(String, Vec<String>)>,
}

impl SettingsChange {
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty() && self.overrides.is_empty()
    }
}

/// Every scope of the window's settings.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SettingsScopes {
    pub defaults: SettingsLayer,
    pub user: SettingsLayer,
    pub remote: SettingsLayer,
    pub workspace: SettingsLayer,
}

fn layer_get<'a>(values: &'a [(String, Value)], key: &str) -> Option<&'a Value> {
    // A key written twice keeps its last value, as a JSON object does.
    values.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v)
}

/// `over` on top of `base`: two objects merge key by key, anything else is replaced.
fn merge_value(base: Option<Value>, over: &Value) -> Value {
    match (base, over) {
        (Some(Value::Object(mut b)), Value::Object(o)) => {
            for (k, v) in o {
                let merged = merge_value(b.remove(k), v);
                b.insert(k.clone(), merged);
            }
            Value::Object(b)
        }
        (_, over) => over.clone(),
    }
}

/// Per language identifier, the keys of a layer's blocks with their values (a later block wins).
fn override_table(layer: &SettingsLayer) -> BTreeMap<String, BTreeMap<String, Value>> {
    let mut table: BTreeMap<String, BTreeMap<String, Value>> = BTreeMap::new();
    for block in &layer.overrides {
        for id in &block.identifiers {
            let entry = table.entry(id.clone()).or_default();
            for (k, v) in &block.values {
                entry.insert(k.clone(), v.clone());
            }
        }
    }
    table
}

impl SettingsScopes {
    pub fn layer(&self, scope: SettingsScope) -> &SettingsLayer {
        match scope {
            SettingsScope::Default => &self.defaults,
            SettingsScope::User => &self.user,
            SettingsScope::Remote => &self.remote,
            SettingsScope::Workspace => &self.workspace,
        }
    }

    pub fn layer_mut(&mut self, scope: SettingsScope) -> &mut SettingsLayer {
        match scope {
            SettingsScope::Default => &mut self.defaults,
            SettingsScope::User => &mut self.user,
            SettingsScope::Remote => &mut self.remote,
            SettingsScope::Workspace => &mut self.workspace,
        }
    }

    /// The value of `key` for a document of `language` (or for no document): the scopes merged
    /// from the weakest to the strongest, then the language blocks in the same order.
    pub fn value(&self, key: &str, language: Option<&str>) -> Option<Value> {
        let mut merged: Option<Value> = None;
        for scope in SettingsScope::ALL {
            if let Some(v) = layer_get(&self.layer(scope).values, key) {
                merged = Some(merge_value(merged, v));
            }
        }
        if let Some(language) = language {
            for scope in SettingsScope::ALL {
                for block in &self.layer(scope).overrides {
                    if !block.identifiers.iter().any(|id| id == language) {
                        continue;
                    }
                    if let Some(v) = layer_get(&block.values, key) {
                        merged = Some(merge_value(merged, v));
                    }
                }
            }
        }
        merged
    }

    /// The strongest scope that writes `key` as a plain key.
    pub fn winning_scope(&self, key: &str) -> Option<SettingsScope> {
        SettingsScope::ALL.into_iter().rev().find(|s| layer_get(&self.layer(*s).values, key).is_some())
    }

    /// What differs from `previous`, for `$acceptConfigurationChanged`.
    pub fn change_from(&self, previous: &SettingsScopes) -> SettingsChange {
        let mut keys: BTreeSet<String> = BTreeSet::new();
        let mut overrides: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for scope in SettingsScope::ALL {
            let (new, old) = (self.layer(scope), previous.layer(scope));
            let names: BTreeSet<&String> = new.values.iter().chain(old.values.iter()).map(|(k, _)| k).collect();
            for name in names {
                if layer_get(&new.values, name) != layer_get(&old.values, name) {
                    keys.insert(name.clone());
                }
            }
            let (new_table, old_table) = (override_table(new), override_table(old));
            let ids: BTreeSet<&String> = new_table.keys().chain(old_table.keys()).collect();
            let none = BTreeMap::new();
            for id in ids {
                let (n, o) = (new_table.get(id).unwrap_or(&none), old_table.get(id).unwrap_or(&none));
                let names: BTreeSet<&String> = n.keys().chain(o.keys()).collect();
                for name in names {
                    if n.get(name) != o.get(name) {
                        overrides.entry(id.clone()).or_default().insert(name.clone());
                    }
                }
            }
        }
        for id in overrides.keys() {
            keys.insert(format!("[{id}]"));
        }
        SettingsChange {
            keys: keys.into_iter().collect(),
            overrides: overrides.into_iter().map(|(id, names)| (id, names.into_iter().collect())).collect(),
        }
    }

    /// `IConfigurationInitData` for the extension host. `scope_table` is the `ConfigurationScope`
    /// of every registered key (`configurationScopes`).
    pub fn configuration_data(&self, scope_table: &[(String, Option<u8>)]) -> Value {
        exthost::configuration_data(&self.defaults, scope_table, &self.user, &self.remote, &self.workspace)
    }
}

// ---- the settings Ember honours ---------------------------------------------------------------

/// `editor.wordWrap`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WordWrap {
    #[default]
    Off,
    On,
    WordWrapColumn,
    Bounded,
}

/// `files.autoSave`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AutoSave {
    #[default]
    Off,
    AfterDelay,
    OnFocusChange,
    OnWindowChange,
}

/// The largest tab size Ember takes. Code-OSS has no useful upper limit; a line indented by more
/// columns than this is a typing mistake, and the widget would draw nothing readable.
pub const MAX_TAB_SIZE: u32 = 100;

/// The settings the editor itself acts on, resolved for one document's language.
#[derive(Debug, Clone, PartialEq)]
pub struct EditorSettings {
    /// `editor.tabSize`, 1 to [`MAX_TAB_SIZE`].
    pub tab_size: u32,
    /// `editor.insertSpaces`.
    pub insert_spaces: bool,
    pub word_wrap: WordWrap,
    pub auto_save: AutoSave,
    /// `files.autoSaveDelay` in milliseconds.
    pub auto_save_delay_ms: u64,
    /// `editor.formatOnSave`.
    pub format_on_save: bool,
    /// The glob patterns of `files.exclude` that are switched on, sorted.
    pub files_exclude: Vec<String>,
}

impl Default for EditorSettings {
    fn default() -> Self {
        Self {
            tab_size: 4,
            insert_spaces: true,
            word_wrap: WordWrap::Off,
            auto_save: AutoSave::Off,
            auto_save_delay_ms: 1000,
            format_on_save: false,
            files_exclude: Vec::new(),
        }
    }
}

impl EditorSettings {
    /// Resolve for a document of `language`. A value of the wrong type falls back to the default,
    /// as Code-OSS validates an editor option.
    pub fn resolve(scopes: &SettingsScopes, language: Option<&str>) -> Self {
        let base = Self::default();
        let get = |key: &str| scopes.value(key, language);
        let tab_size = get("editor.tabSize")
            .and_then(|v| v.as_u64())
            .map(|n| n.clamp(1, MAX_TAB_SIZE as u64) as u32)
            .unwrap_or(base.tab_size);
        let insert_spaces = get("editor.insertSpaces").and_then(|v| v.as_bool()).unwrap_or(base.insert_spaces);
        let word_wrap = match get("editor.wordWrap").as_ref().and_then(Value::as_str) {
            Some("on") => WordWrap::On,
            Some("wordWrapColumn") => WordWrap::WordWrapColumn,
            Some("bounded") => WordWrap::Bounded,
            _ => WordWrap::Off,
        };
        let auto_save = match get("files.autoSave").as_ref().and_then(Value::as_str) {
            Some("afterDelay") => AutoSave::AfterDelay,
            Some("onFocusChange") => AutoSave::OnFocusChange,
            Some("onWindowChange") => AutoSave::OnWindowChange,
            _ => AutoSave::Off,
        };
        let auto_save_delay_ms = get("files.autoSaveDelay").and_then(|v| v.as_u64()).unwrap_or(base.auto_save_delay_ms);
        let format_on_save = get("editor.formatOnSave").and_then(|v| v.as_bool()).unwrap_or(base.format_on_save);
        let mut files_exclude: Vec<String> = match get("files.exclude") {
            // Only `true` hides. `false` switches a weaker scope's pattern off, and an object (a
            // `when` clause on sibling files) is not read: such a pattern hides nothing.
            Some(Value::Object(map)) => map.into_iter().filter(|(_, on)| *on == Value::Bool(true)).map(|(glob, _)| glob).collect(),
            _ => Vec::new(),
        };
        files_exclude.sort();
        Self { tab_size, insert_spaces, word_wrap, auto_save, auto_save_delay_ms, format_on_save, files_exclude }
    }
}

// ---- files.exclude ----------------------------------------------------------------------------

/// Matches paths against `files.exclude` patterns. Paths are relative to the workspace folder
/// and use `/`. The glob syntax is Code-OSS's: `*` and `?` inside one path segment, `**` for any
/// number of segments, `{a,b}` alternatives and `[a-z]` character ranges.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExcludeMatcher {
    /// Each pattern split into segments, alternatives already expanded.
    patterns: Vec<Vec<String>>,
}

fn expand_braces(pattern: &str) -> Vec<String> {
    let Some(open) = pattern.find('{') else { return vec![pattern.to_owned()] };
    let mut depth = 0usize;
    let mut close = None;
    for (i, c) in pattern[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(open + i);
                    break;
                }
            }
            _ => {}
        }
    }
    let Some(close) = close else { return vec![pattern.to_owned()] };
    let (head, body, tail) = (&pattern[..open], &pattern[open + 1..close], &pattern[close + 1..]);
    // Split the body on the commas of its own level.
    let mut parts = Vec::new();
    let (mut level, mut start) = (0usize, 0usize);
    for (i, c) in body.char_indices() {
        match c {
            '{' => level += 1,
            '}' => level = level.saturating_sub(1),
            ',' if level == 0 => {
                parts.push(&body[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&body[start..]);
    parts.into_iter().flat_map(|part| expand_braces(&format!("{head}{part}{tail}"))).collect()
}

fn match_class(class: &[char], c: char) -> bool {
    let (negated, class) = match class.first() {
        Some('!') | Some('^') => (true, &class[1..]),
        _ => (false, class),
    };
    let mut hit = false;
    let mut i = 0;
    while i < class.len() {
        if i + 2 < class.len() && class[i + 1] == '-' {
            hit |= class[i] <= c && c <= class[i + 2];
            i += 3;
        } else {
            hit |= class[i] == c;
            i += 1;
        }
    }
    hit != negated
}

fn match_segment(pattern: &[char], name: &[char]) -> bool {
    match pattern.first() {
        None => name.is_empty(),
        Some('*') => (0..=name.len()).any(|skip| match_segment(&pattern[1..], &name[skip..])),
        Some('?') => !name.is_empty() && match_segment(&pattern[1..], &name[1..]),
        Some('[') => match pattern.iter().skip(2).position(|c| *c == ']') {
            // `skip(2)`: a `]` right after `[` is a member of the class, not its end.
            Some(offset) => {
                let end = offset + 2;
                !name.is_empty() && match_class(&pattern[1..end], name[0]) && match_segment(&pattern[end + 1..], &name[1..])
            }
            None => !name.is_empty() && name[0] == '[' && match_segment(&pattern[1..], &name[1..]),
        },
        Some(c) => !name.is_empty() && name[0] == *c && match_segment(&pattern[1..], &name[1..]),
    }
}

fn match_segments(pattern: &[String], path: &[&str]) -> bool {
    match pattern.first() {
        None => path.is_empty(),
        Some(seg) if seg == "**" => (0..=path.len()).any(|skip| match_segments(&pattern[1..], &path[skip..])),
        Some(seg) => {
            let Some(name) = path.first() else { return false };
            let (p, n): (Vec<char>, Vec<char>) = (seg.chars().collect(), name.chars().collect());
            match_segment(&p, &n) && match_segments(&pattern[1..], &path[1..])
        }
    }
}

impl ExcludeMatcher {
    pub fn new<S: AsRef<str>>(globs: &[S]) -> Self {
        let mut patterns = Vec::new();
        for glob in globs {
            let glob = glob.as_ref().trim().trim_start_matches("./").trim_matches('/');
            if glob.is_empty() {
                continue;
            }
            for expanded in expand_braces(glob) {
                patterns.push(expanded.split('/').filter(|s| !s.is_empty()).map(str::to_owned).collect());
            }
        }
        Self { patterns }
    }

    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }

    /// Whether a pattern matches exactly this path.
    pub fn matches(&self, relative: &str) -> bool {
        let path: Vec<&str> = relative.split('/').filter(|s| !s.is_empty()).collect();
        !path.is_empty() && self.patterns.iter().any(|p| match_segments(p, &path))
    }

    /// Whether the explorer leaves this path out: a pattern matches it or a folder above it.
    pub fn hides(&self, relative: &str) -> bool {
        if self.patterns.is_empty() {
            return false;
        }
        let path: Vec<&str> = relative.split('/').filter(|s| !s.is_empty()).collect();
        (1..=path.len()).any(|depth| self.patterns.iter().any(|p| match_segments(p, &path[..depth])))
    }
}

// ---- editing one key of a settings file ---------------------------------------------------------

struct Property {
    key: String,
    key_start: usize,
    value_start: usize,
    value_end: usize,
}

struct RootObject {
    close: usize,
    properties: Vec<Property>,
}

fn skip_trivia(b: &[u8], mut i: usize) -> usize {
    loop {
        match b.get(i) {
            Some(c) if c.is_ascii_whitespace() => i += 1,
            Some(b'/') if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            Some(b'/') if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i < b.len() && !(b[i] == b'*' && b.get(i + 1) == Some(&b'/')) {
                    i += 1;
                }
                i = (i + 2).min(b.len());
            }
            _ => return i,
        }
    }
}

/// The index after the string that starts at `i`.
fn scan_string(b: &[u8], mut i: usize) -> Option<usize> {
    i += 1;
    loop {
        match b.get(i)? {
            b'\\' => i += 2,
            b'"' => return Some(i + 1),
            _ => i += 1,
        }
    }
}

/// The index after the JSON value that starts at `i`.
fn scan_value(b: &[u8], i: usize) -> Option<usize> {
    match b.get(i)? {
        b'"' => scan_string(b, i),
        b'{' | b'[' => {
            let (mut j, mut depth) = (i, 0usize);
            loop {
                match b.get(j)? {
                    b'"' => {
                        j = scan_string(b, j)?;
                        continue;
                    }
                    b'/' if matches!(b.get(j + 1), Some(b'/') | Some(b'*')) => {
                        j = skip_trivia(b, j);
                        continue;
                    }
                    b'{' | b'[' => depth += 1,
                    b'}' | b']' => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(j + 1);
                        }
                    }
                    _ => {}
                }
                j += 1;
            }
        }
        _ => {
            let mut j = i;
            while j < b.len()
                && !b[j].is_ascii_whitespace()
                && !matches!(b[j], b',' | b'}' | b']')
                && !(b[j] == b'/' && matches!(b.get(j + 1), Some(b'/') | Some(b'*')))
            {
                j += 1;
            }
            (j > i).then_some(j)
        }
    }
}

/// The root object's properties with their positions. `None` for a file with nothing in it.
fn scan_root(text: &str) -> Result<Option<RootObject>, JsoncError> {
    let b = text.as_bytes();
    let start = if text.starts_with('\u{feff}') { 3 } else { 0 };
    let mut i = skip_trivia(b, start);
    if i >= b.len() {
        return Ok(None);
    }
    if b[i] != b'{' {
        return Err(JsoncError::at(text, i, "the settings file must hold one JSON object"));
    }
    i += 1;
    let mut properties = Vec::new();
    loop {
        i = skip_trivia(b, i);
        match b.get(i) {
            None => return Err(JsoncError::at(text, i, "the object is not closed")),
            Some(b'}') => return Ok(Some(RootObject { close: i, properties })),
            Some(b',') => i += 1,
            Some(b'"') => {
                let key_start = i;
                let key_end = scan_string(b, i).ok_or_else(|| JsoncError::at(text, i, "the key is not closed"))?;
                let key: String = serde_json::from_str(&text[key_start..key_end])
                    .map_err(|_| JsoncError::at(text, key_start, "the key is not a JSON string"))?;
                i = skip_trivia(b, key_end);
                if b.get(i) != Some(&b':') {
                    return Err(JsoncError::at(text, i, "a colon is missing after the key"));
                }
                let value_start = skip_trivia(b, i + 1);
                let value_end =
                    scan_value(b, value_start).ok_or_else(|| JsoncError::at(text, value_start, "the value is not complete"))?;
                properties.push(Property { key, key_start, value_start, value_end });
                i = value_end;
            }
            Some(_) => return Err(JsoncError::at(text, i, "a key in double quotes is expected")),
        }
    }
}

fn render_value(value: &Value, indent: &str, eol: &str) -> String {
    let mut buffer = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(indent.as_bytes());
    let mut serializer = serde_json::Serializer::with_formatter(&mut buffer, formatter);
    if value.serialize(&mut serializer).is_err() {
        return "null".to_owned();
    }
    // The property sits one level deep, so every further line of its value is one indent in.
    String::from_utf8(buffer).unwrap_or_default().replace('\n', &format!("{eol}{indent}"))
}

/// The start of the line holding `offset` when only blanks precede it there.
fn blank_line_start(b: &[u8], offset: usize) -> Option<usize> {
    let mut i = offset;
    while i > 0 && (b[i - 1] == b' ' || b[i - 1] == b'\t') {
        i -= 1;
    }
    (i == 0 || b[i - 1] == b'\n').then_some(i)
}

/// The text of a settings file with `key` set to `value`, or removed when `value` is `None`.
/// Everything else stays as written: comments, order, other keys and their layout. The file's
/// own indentation and line ending are used for a new line. A text that is not a JSON object is
/// an error and nothing is changed, so a file the person broke by hand is never overwritten.
pub fn set_setting(text: &str, key: &str, value: Option<&Value>) -> Result<String, JsoncError> {
    let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let quoted = serde_json::to_string(key).unwrap_or_else(|_| format!("\"{key}\""));
    let Some(root) = scan_root(text)? else {
        return Ok(match value {
            None => text.to_owned(),
            Some(v) => format!("{{{eol}    {quoted}: {}{eol}}}{eol}", render_value(v, "    ", eol)),
        });
    };
    // The rest of the file must be sound too, or an edit could hide a mistake further down.
    parse_jsonc(text)?;
    let b = text.as_bytes();
    let indent = root
        .properties
        .first()
        .and_then(|p| blank_line_start(b, p.key_start).map(|start| &text[start..p.key_start]))
        .filter(|s| !s.is_empty())
        .unwrap_or("    ")
        .to_owned();
    let matching: Vec<usize> = root.properties.iter().enumerate().filter(|(_, p)| p.key == key).map(|(i, _)| i).collect();

    match (value, matching.last().copied()) {
        (Some(v), Some(index)) => {
            let p = &root.properties[index];
            Ok(format!("{}{}{}", &text[..p.value_start], render_value(v, &indent, eol), &text[p.value_end..]))
        }
        (Some(v), None) => {
            let entry = format!("{quoted}: {}", render_value(v, &indent, eol));
            let head_end = text[..root.close].trim_end().len();
            let mut out = String::with_capacity(text.len() + entry.len() + 8);
            match root.properties.last() {
                Some(last) => {
                    let has_comma = b.get(skip_trivia(b, last.value_end)) == Some(&b',');
                    out.push_str(&text[..last.value_end]);
                    if !has_comma {
                        out.push(',');
                    }
                    out.push_str(&text[last.value_end..head_end]);
                }
                None => out.push_str(&text[..head_end]),
            }
            out.push_str(eol);
            out.push_str(&indent);
            out.push_str(&entry);
            out.push_str(eol);
            out.push_str(&text[root.close..]);
            Ok(out)
        }
        (None, None) => Ok(text.to_owned()),
        (None, Some(_)) => {
            // Cut from the back so earlier positions stay valid.
            let mut out = text.to_owned();
            for index in matching.into_iter().rev() {
                let p = &root.properties[index];
                let b = out.as_bytes();
                let mut end = p.value_end;
                while end < b.len() && (b[end] == b' ' || b[end] == b'\t') {
                    end += 1;
                }
                let own_comma = b.get(end) == Some(&b',');
                if own_comma {
                    end += 1;
                    while end < b.len() && (b[end] == b' ' || b[end] == b'\t') {
                        end += 1;
                    }
                }
                let mut start = p.key_start;
                if let Some(line_start) = blank_line_start(b, p.key_start) {
                    // The property has its line to itself: the line goes with it.
                    if b.get(end) == Some(&b'\r') && b.get(end + 1) == Some(&b'\n') {
                        start = line_start;
                        end += 2;
                    } else if b.get(end) == Some(&b'\n') {
                        start = line_start;
                        end += 1;
                    }
                }
                // The last property has no comma of its own: the one before it goes instead,
                // unless more properties follow (a trailing comma was written).
                let mut earlier_comma = None;
                if !own_comma && index > 0 {
                    let previous_end = root.properties[index - 1].value_end;
                    let at = skip_trivia(b, previous_end);
                    if b.get(at) == Some(&b',') && at < start {
                        earlier_comma = Some(at);
                    }
                }
                out.replace_range(start..end, "");
                if let Some(at) = earlier_comma {
                    out.replace_range(at..at + 1, "");
                }
            }
            Ok(out)
        }
    }
}

// ---- the list of settings -----------------------------------------------------------------------

/// How the settings view edits a setting.
#[derive(Debug, Clone, PartialEq)]
pub enum SettingKind {
    Boolean,
    Integer,
    Number,
    String,
    /// One of these strings.
    Enum(Vec<String>),
    /// An object, an array or a mixed type: edited in settings.json.
    Other,
}

/// One setting the settings view lists.
#[derive(Debug, Clone, PartialEq)]
pub struct SettingSchema {
    pub key: String,
    pub kind: SettingKind,
    pub default: Value,
    pub description: String,
    /// `ConfigurationScope`: 1 application, 2 machine, 3 application-machine, 4 window,
    /// 5 resource, 6 language-overridable, 7 machine-overridable. `None`: window.
    pub scope: Option<u8>,
    /// Who registered it: the extension's display name, or `Ember`.
    pub source: String,
}

/// Who registers Ember's own settings.
pub const EMBER_SOURCE: &str = "Ember";

fn scope_code(name: &str) -> u8 {
    match name {
        "application" => 1,
        "machine" => 2,
        "application-machine" => 3,
        "resource" => 5,
        "language-overridable" => 6,
        "machine-overridable" => 7,
        _ => 4,
    }
}

fn kind_of(schema: &Value) -> SettingKind {
    if let Some(options) = schema.get("enum").and_then(Value::as_array) {
        let names: Vec<String> = options.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect();
        if !names.is_empty() && names.len() == options.len() {
            return SettingKind::Enum(names);
        }
        return SettingKind::Other;
    }
    let name = match schema.get("type") {
        Some(Value::String(s)) => Some(s.as_str()),
        // `["string", "null"]`: the one real type decides.
        Some(Value::Array(types)) => {
            let real: Vec<&str> = types.iter().filter_map(Value::as_str).filter(|t| *t != "null").collect();
            if real.len() == 1 { Some(real[0]) } else { None }
        }
        _ => None,
    };
    match name {
        Some("boolean") => SettingKind::Boolean,
        Some("integer") => SettingKind::Integer,
        Some("number") => SettingKind::Number,
        Some("string") => SettingKind::String,
        _ => SettingKind::Other,
    }
}

/// The settings extensions register in `contributes.configuration` (an object or a list of
/// sections), in the order the extensions were scanned and sorted by key inside each.
pub fn contributed_settings(extensions: &[Value]) -> Vec<SettingSchema> {
    let mut out = Vec::new();
    for description in extensions {
        let Some(configuration) = description.get("contributes").and_then(|c| c.get("configuration")) else { continue };
        let source = description
            .get("displayName")
            .and_then(Value::as_str)
            .or_else(|| description.get("name").and_then(Value::as_str))
            .or_else(|| description.get("identifier").and_then(|i| i.get("value")).and_then(Value::as_str))
            .unwrap_or("확장")
            .to_owned();
        let sections: Vec<&Value> = match configuration {
            Value::Array(list) => list.iter().collect(),
            other => vec![other],
        };
        for section in sections {
            let Some(properties) = section.get("properties").and_then(Value::as_object) else { continue };
            for (key, schema) in properties {
                let description = schema
                    .get("description")
                    .or_else(|| schema.get("markdownDescription"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                out.push(SettingSchema {
                    key: key.clone(),
                    kind: kind_of(schema),
                    default: schema.get("default").cloned().unwrap_or(Value::Null),
                    description,
                    scope: schema.get("scope").and_then(Value::as_str).map(scope_code),
                    source: source.clone(),
                });
            }
        }
    }
    out
}

/// The settings the workbench registers itself in Code-OSS and Ember therefore registers: the
/// ones the editor acts on ([`EditorSettings`]) and the two the session already told the
/// extension host about. Defaults are Code-OSS's.
pub fn ember_settings() -> Vec<SettingSchema> {
    let enumeration = |names: &[&str]| SettingKind::Enum(names.iter().map(|s| (*s).to_owned()).collect());
    let row = |key: &str, kind: SettingKind, default: Value, scope: u8, description: &str| SettingSchema {
        key: key.to_owned(),
        kind,
        default,
        description: description.to_owned(),
        scope: Some(scope),
        source: EMBER_SOURCE.to_owned(),
    };
    vec![
        row("editor.tabSize", SettingKind::Integer, json!(4), 6, "탭 하나의 너비(칸 수)입니다."),
        row("editor.insertSpaces", SettingKind::Boolean, json!(true), 6, "Tab 키를 누르면 공백을 넣습니다."),
        row(
            "editor.wordWrap",
            enumeration(&["off", "on", "wordWrapColumn", "bounded"]),
            json!("off"),
            6,
            "긴 줄을 화면 너비에서 줄바꿈합니다.",
        ),
        row("editor.formatOnSave", SettingKind::Boolean, json!(false), 6, "저장할 때 문서를 정리합니다."),
        row("editor.codeLens", SettingKind::Boolean, json!(true), 6, "코드 위에 CodeLens를 표시합니다."),
        row("editor.inlineSuggest.enabled", SettingKind::Boolean, json!(true), 6, "입력하는 동안 인라인 제안을 표시합니다."),
        row(
            "files.autoSave",
            enumeration(&["off", "afterDelay", "onFocusChange", "onWindowChange"]),
            json!("off"),
            6,
            "변경한 파일을 자동으로 저장합니다.",
        ),
        row("files.autoSaveDelay", SettingKind::Integer, json!(1000), 6, "자동 저장까지 기다리는 시간(밀리초)입니다."),
        row(
            "files.exclude",
            SettingKind::Other,
            json!({ "**/.git": true, "**/.svn": true, "**/.hg": true, "**/.DS_Store": true, "**/Thumbs.db": true }),
            5,
            "탐색기에서 숨길 파일과 폴더의 glob 패턴입니다.",
        ),
    ]
}

/// Ember's settings first, then the extensions'. A key registered twice keeps its first row.
pub fn setting_catalogue(extensions: &[Value]) -> Vec<SettingSchema> {
    let mut seen = BTreeSet::new();
    ember_settings().into_iter().chain(contributed_settings(extensions)).filter(|s| seen.insert(s.key.clone())).collect()
}

/// The settings matching `query`: every word must appear in the key, the description or the
/// source, in any letter case. Settings whose key matches every word come first; the order is
/// otherwise the catalogue's. An empty query lists everything.
pub fn search_settings<'a>(catalogue: &'a [SettingSchema], query: &str) -> Vec<&'a SettingSchema> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    if words.is_empty() {
        return catalogue.iter().collect();
    }
    let mut by_key = Vec::new();
    let mut by_text = Vec::new();
    for schema in catalogue {
        let key = schema.key.to_lowercase();
        if words.iter().all(|w| key.contains(w.as_str())) {
            by_key.push(schema);
            continue;
        }
        let text = format!("{key}\n{}\n{}", schema.description.to_lowercase(), schema.source.to_lowercase());
        if words.iter().all(|w| text.contains(w.as_str())) {
            by_text.push(schema);
        }
    }
    by_key.extend(by_text);
    by_key
}

// ---- keybindings.json -------------------------------------------------------------------------

/// One entry of `keybindings.json`. A command that starts with `-` removes a default binding.
#[derive(Debug, Clone, PartialEq)]
pub struct KeybindingRule {
    /// The key as written: `cmd+shift+p`, `ctrl+k ctrl+s`.
    pub key: String,
    pub command: String,
    pub when: Option<String>,
    pub args: Option<Value>,
}

/// The entries of a `keybindings.json` text, in file order. The root must be an array; an entry
/// without a `key` or a `command` string is left out. An empty file has no entries.
pub fn parse_keybindings(text: &str) -> Result<Vec<KeybindingRule>, JsoncError> {
    if strip_jsonc(text).trim().is_empty() {
        return Ok(Vec::new());
    }
    let root = parse_jsonc(text)?;
    let Some(list) = root.as_array() else {
        return Err(JsoncError { line: 1, column: 1, message: "keybindings.json must hold one JSON array".into() });
    };
    Ok(list
        .iter()
        .filter_map(|entry| {
            let key = entry.get("key")?.as_str()?.trim();
            let command = entry.get("command")?.as_str()?.trim();
            if key.is_empty() || command.is_empty() {
                return None;
            }
            Some(KeybindingRule {
                key: key.to_owned(),
                command: command.to_owned(),
                when: entry.get("when").and_then(Value::as_str).map(str::to_owned),
                args: entry.get("args").cloned(),
            })
        })
        .collect())
}

// ---- settings files on the client machine -------------------------------------------------------

/// The `vscode-userdata:` URI of a file on the client machine. The path uses `/` and starts with
/// one (`/C:/Users/...` on Windows), as a URI path does.
pub fn user_data_uri(path: &Path) -> UriComponents {
    let mut text = path.to_string_lossy().replace('\\', "/");
    if !text.starts_with('/') {
        text.insert(0, '/');
    }
    UriComponents::new(SCHEME_USER_DATA, "", text)
}

/// The file a `vscode-userdata:` URI names; `None` for any other scheme.
pub fn user_data_path(uri: &UriComponents) -> Option<PathBuf> {
    if uri.scheme != SCHEME_USER_DATA {
        return None;
    }
    let b = uri.path.as_bytes();
    // `/C:/...` is a Windows drive path with the URI's leading slash.
    let drive = b.len() >= 3 && b[0] == b'/' && b[1].is_ascii_alphabetic() && b[2] == b':';
    Some(PathBuf::from(if drive { &uri.path[1..] } else { &uri.path[..] }))
}
