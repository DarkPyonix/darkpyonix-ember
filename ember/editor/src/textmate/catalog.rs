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
    /// Builds the catalog from `scanExtensions` results (raw `IExtensionDescription` JSON). The result
    /// does not depend on the order of `extensions` (a directory listing has none): entries are ordered
    /// by extension id, then scope, then path, and when two contribute the same scope or language the
    /// last in that order wins, so the same installation always gives the same answer.
    pub fn from_extensions(extensions: &[Value]) -> Self {
        let mut catalog = Self::default();
        let mut found: Vec<GrammarEntry> = Vec::new();
        for desc in extensions {
            let Some(grammars) = desc.get("contributes").and_then(|c| c.get("grammars")).and_then(Value::as_array) else {
                continue;
            };
            let dir = desc.get("extensionLocation").and_then(|l| l.get("path")).and_then(Value::as_str).unwrap_or("");
            let extension = shown(&extension_name(desc));
            for g in grammars {
                let Some(scope) = g.get("scopeName").and_then(Value::as_str).filter(|s| !s.is_empty()) else {
                    catalog.skipped.push(format!("{extension}: a grammar without scopeName"));
                    continue;
                };
                let Some(rel) = g.get("path").and_then(Value::as_str) else {
                    catalog.skipped.push(format!("{extension}: {} has no path", shown(scope)));
                    continue;
                };
                let Some(path) = resolve_inside(dir, rel) else {
                    catalog.skipped.push(format!("{extension}: {} points outside the extension folder ({})", shown(scope), shown(rel)));
                    continue;
                };
                found.push(GrammarEntry {
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
                });
            }
        }
        found.sort_by(|a, b| (&a.extension, &a.scope_name, &a.path).cmp(&(&b.extension, &b.scope_name, &b.path)));
        // The last entry of a scope wins; a language belongs to the last surviving entry that names it, so
        // a scope redefined for another language does not leave its old language pointing at it.
        for entry in found {
            if let Some(old) = catalog.by_scope.get(&entry.scope_name).copied() {
                catalog.entries[old].scope_name.clear();
                catalog.by_scope.remove(&entry.scope_name);
            }
            let index = catalog.entries.len();
            catalog.by_scope.insert(entry.scope_name.clone(), index);
            catalog.entries.push(entry);
        }
        for (i, entry) in catalog.entries.iter().enumerate() {
            if entry.scope_name.is_empty() {
                continue;
            }
            if let Some(language) = &entry.language {
                catalog.by_language.insert(language.clone(), i);
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

/// Untrusted text as shown in a message: control characters dropped, at most 120 characters.
fn shown(text: &str) -> String {
    let clean: String = text.chars().filter(|c| !c.is_control()).collect();
    if clean.chars().count() > 120 {
        format!("{}...", clean.chars().take(117).collect::<String>())
    } else {
        clean
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
    // A Windows server reports folders as `/c:/Users/...`; a relative path never carries a drive (`C:`),
    // a backslash, a NUL or a control character.
    if dir.is_empty() || rel.starts_with('/') || rel.chars().any(|c| c == '\\' || c.is_control()) {
        return None;
    }
    let mut parts: Vec<&str> = Vec::new();
    for part in rel.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            p if p.contains(':') => return None,
            p => parts.push(p),
        }
    }
    if parts.is_empty() {
        return None;
    }
    Some(format!("{}/{}", dir.trim_end_matches('/'), parts.join("/")))
}
