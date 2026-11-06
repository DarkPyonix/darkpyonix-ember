//! Which grammars the installed extensions offer: `contributes.grammars` of the `scanExtensions`
//! descriptions.
//!
//! A grammar entry names a scope (`source.ts`), optionally the language id it highlights, the file
//! (relative to the extension folder), the scopes it is injected into, and the languages embedded
//! scopes stand for. Entries whose file would leave the extension folder are skipped and listed in
//! [`GrammarCatalog::skipped`]: an extension's `path` is data from outside the editor.

use std::collections::{BTreeMap, HashMap};

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrammarEntry {
    pub scope_name: String,
    /// The language id this grammar highlights (absent for injection-only grammars such as a code block).
    pub language: Option<String>,
    /// The grammar file, an absolute path on the machine the extension is installed on.
    pub path: String,
    /// Scopes this grammar is injected into.
    pub inject_to: Vec<String>,
    /// Scope prefix to language id, for embedded code (`meta.embedded.block.rust` to `rust`).
    pub embedded_languages: BTreeMap<String, String>,
    /// The contributing extension, `publisher.name`, for messages.
    pub extension: String,
}

#[derive(Debug, Clone, Default)]
pub struct GrammarCatalog {
    entries: Vec<GrammarEntry>,
    by_scope: HashMap<String, usize>,
    by_language: HashMap<String, usize>,
    skipped: Vec<String>,
}

impl GrammarCatalog {
    /// Builds the catalog from `scanExtensions` results (raw `IExtensionDescription` JSON). When two
    /// extensions contribute the same scope or language, the later one wins, as in VS Code.
    pub fn from_extensions(extensions: &[Value]) -> Self {
        let mut catalog = Self::default();
        for desc in extensions {
            let Some(grammars) = desc.get("contributes").and_then(|c| c.get("grammars")).and_then(Value::as_array) else {
                continue;
            };
            let dir = desc.get("extensionLocation").and_then(|l| l.get("path")).and_then(Value::as_str).unwrap_or("");
            let extension = extension_name(desc);
            for g in grammars {
                let Some(scope) = g.get("scopeName").and_then(Value::as_str).filter(|s| !s.is_empty()) else {
                    catalog.skipped.push(format!("{extension}: a grammar without scopeName"));
                    continue;
                };
                let Some(rel) = g.get("path").and_then(Value::as_str) else {
                    catalog.skipped.push(format!("{extension}: {scope} has no path"));
                    continue;
                };
                let Some(path) = resolve_inside(dir, rel) else {
                    catalog.skipped.push(format!("{extension}: {scope} points outside the extension folder ({rel})"));
                    continue;
                };
                let entry = GrammarEntry {
                    scope_name: scope.to_owned(),
                    language: g.get("language").and_then(Value::as_str).filter(|l| !l.is_empty()).map(str::to_owned),
                    path,
                    inject_to: g
                        .get("injectTo")
                        .and_then(Value::as_array)
                        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect())
                        .unwrap_or_default(),
                    embedded_languages: g
                        .get("embeddedLanguages")
                        .and_then(Value::as_object)
                        .map(|m| m.iter().filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned()))).collect())
                        .unwrap_or_default(),
                    extension: extension.clone(),
                };
                let index = catalog.entries.len();
                catalog.by_scope.insert(entry.scope_name.clone(), index);
                if let Some(language) = &entry.language {
                    catalog.by_language.insert(language.clone(), index);
                }
                catalog.entries.push(entry);
            }
        }
        catalog
    }

    pub fn by_scope(&self, scope: &str) -> Option<&GrammarEntry> {
        self.by_scope.get(scope).map(|i| &self.entries[*i])
    }

    /// The grammar that highlights a language id.
    pub fn for_language(&self, language: &str) -> Option<&GrammarEntry> {
        self.by_language.get(language).map(|i| &self.entries[*i])
    }

    /// The grammars that are injected into `scope`.
    pub fn injections_into(&self, scope: &str) -> Vec<&GrammarEntry> {
        let mut v: Vec<&GrammarEntry> = self
            .by_scope
            .values()
            .map(|i| &self.entries[*i])
            .filter(|e| e.inject_to.iter().any(|t| t == scope))
            .collect();
        v.sort_by(|a, b| a.scope_name.cmp(&b.scope_name));
        v
    }

    /// Entries that were refused, each with the reason in a sentence.
    pub fn skipped(&self) -> &[String] {
        &self.skipped
    }

    pub fn len(&self) -> usize {
        self.by_scope.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_scope.is_empty()
    }
}

fn extension_name(desc: &Value) -> String {
    let id = desc.get("identifier").and_then(|i| i.get("value")).and_then(Value::as_str);
    match id {
        Some(id) => id.to_owned(),
        None => {
            let publisher = desc.get("publisher").and_then(Value::as_str).unwrap_or("?");
            let name = desc.get("name").and_then(Value::as_str).unwrap_or("?");
            format!("{publisher}.{name}")
        }
    }
}

/// `dir` joined with `rel`, with `.` and `..` resolved; `None` when the result is not inside `dir`.
fn resolve_inside(dir: &str, rel: &str) -> Option<String> {
    if dir.is_empty() || rel.starts_with('/') || rel.contains('\\') || rel.contains('\0') {
        return None;
    }
    let mut parts: Vec<&str> = Vec::new();
    for part in rel.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            p => parts.push(p),
        }
    }
    if parts.is_empty() {
        return None;
    }
    Some(format!("{}/{}", dir.trim_end_matches('/'), parts.join("/")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ext(id: &str, dir: &str, grammars: Value) -> Value {
        json!({"identifier": {"value": id}, "extensionLocation": {"path": dir}, "contributes": {"grammars": grammars}})
    }

    #[test]
    fn a_grammar_is_found_by_scope_and_by_language_with_its_absolute_path() {
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
    fn injection_grammars_and_embedded_languages_are_kept() {
        let c = GrammarCatalog::from_extensions(&[ext(
            "vscode.markdown",
            "/e/md",
            json!([
                {"language": "markdown", "scopeName": "text.html.markdown", "path": "./md.json",
                 "embeddedLanguages": {"meta.embedded.block.rust": "rust", "bad": 3}},
                {"scopeName": "markdown.rust.codeblock", "path": "./rust-block.json",
                 "injectTo": ["text.html.markdown"]}
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
    fn a_path_that_leaves_the_extension_folder_is_skipped_and_reported() {
        let c = GrammarCatalog::from_extensions(&[ext(
            "evil.ext",
            "/e/evil",
            json!([
                {"scopeName": "a", "path": "../other/a.json"},
                {"scopeName": "b", "path": "/etc/passwd"},
                {"scopeName": "c", "path": "syntaxes/../../x.json"},
                {"scopeName": "d", "path": "./ok/../d.json"},
                {"scopeName": "e"},
                {"path": "./f.json"}
            ]),
        )]);
        assert!(c.by_scope("a").is_none() && c.by_scope("b").is_none() && c.by_scope("c").is_none());
        assert_eq!(c.by_scope("d").unwrap().path, "/e/evil/d.json", ".. inside the folder is fine");
        assert!(c.by_scope("e").is_none());
        assert_eq!(c.skipped().len(), 5);
        assert!(c.skipped()[0].contains("evil.ext") && c.skipped()[0].contains("outside"));
    }

    #[test]
    fn the_later_extension_wins_a_scope_or_language_both_contribute() {
        let c = GrammarCatalog::from_extensions(&[
            ext("a.one", "/e/1", json!([{"language": "x", "scopeName": "source.x", "path": "./1.json"}])),
            ext("b.two", "/e/2", json!([{"language": "x", "scopeName": "source.x", "path": "./2.json"}])),
            json!({"contributes": {}}),
            json!({"contributes": {"grammars": "not a list"}}),
        ]);
        assert_eq!(c.by_scope("source.x").unwrap().path, "/e/2/2.json");
        assert_eq!(c.for_language("x").unwrap().extension, "b.two");
    }

    #[test]
    fn a_description_without_a_location_contributes_nothing_usable() {
        let c = GrammarCatalog::from_extensions(&[json!({"contributes": {"grammars": [{"scopeName": "s", "path": "./a.json"}]}})]);
        assert!(c.is_empty());
        assert_eq!(c.skipped().len(), 1);
    }
}
