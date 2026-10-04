//! Language ids for files, from the extensions' `contributes.languages`.
//!
//! Upstream the renderer's `ILanguageService` owns this: every extension contributes language ids
//! with file extensions, file names and patterns, and the model gets the id when it is created
//! (`languagesAssociations.ts`). The extension host never decides a document's language; it is
//! told in `IModelAddedData.languageId`, and `onLanguage:<id>` activation depends on it. So Ember
//! has to reproduce the association, from the same `scanExtensions` output it hands the extension
//! host.
//!
//! Precedence (as upstream): exact file name, then file-name glob pattern, then the longest
//! matching extension. First-line matching (`firstLine` regexes) is not implemented.

use std::collections::{BTreeSet, HashMap};

use serde_json::Value;

/// Language id for files nothing claims.
pub const PLAINTEXT: &str = "plaintext";

#[derive(Debug, Clone, Default)]
pub struct LanguageRegistry {
    ids: BTreeSet<String>,
    /// lowercased file name → id
    by_filename: HashMap<String, String>,
    /// lowercased extension including the dot (`.d.ts`) → id
    by_extension: HashMap<String, String>,
    /// (glob over the file name or path, id)
    by_pattern: Vec<(String, String)>,
}

impl LanguageRegistry {
    /// Build from `scanExtensions` results (raw `IExtensionDescription` JSON).
    pub fn from_extensions(extensions: &[Value]) -> Self {
        let mut reg = Self::default();
        reg.ids.insert(PLAINTEXT.to_owned());
        for desc in extensions {
            let Some(langs) = desc.get("contributes").and_then(|c| c.get("languages")).and_then(Value::as_array) else {
                continue;
            };
            for lang in langs {
                let Some(id) = lang.get("id").and_then(Value::as_str) else { continue };
                reg.ids.insert(id.to_owned());
                for e in strings(lang.get("extensions")) {
                    reg.by_extension.entry(e.to_lowercase()).or_insert_with(|| id.to_owned());
                }
                for f in strings(lang.get("filenames")) {
                    reg.by_filename.entry(f.to_lowercase()).or_insert_with(|| id.to_owned());
                }
                for p in strings(lang.get("filenamePatterns")) {
                    reg.by_pattern.push((p.to_owned(), id.to_owned()));
                }
            }
        }
        reg
    }

    /// Every known language id (the reply to `MainThreadLanguages.$getLanguages`).
    pub fn ids(&self) -> Vec<String> {
        self.ids.iter().cloned().collect()
    }

    /// The language of a file by its path.
    pub fn language_for(&self, path: &str) -> String {
        let name = path.rsplit('/').next().unwrap_or(path);
        let lower = name.to_lowercase();
        if let Some(id) = self.by_filename.get(&lower) {
            return id.clone();
        }
        for (pat, id) in &self.by_pattern {
            let target = if pat.contains('/') { path } else { name };
            if crate::selector::glob_match(pat, target) {
                return id.clone();
            }
        }
        // Longest extension first: `.d.ts` beats `.ts`.
        let mut best: Option<(&str, usize)> = None;
        for (ext, id) in &self.by_extension {
            let longer = match best {
                Some((_, len)) => ext.len() > len,
                None => true,
            };
            if lower.ends_with(ext.as_str()) && longer {
                best = Some((id.as_str(), ext.len()));
            }
        }
        best.map(|(id, _)| id.to_owned()).unwrap_or_else(|| PLAINTEXT.to_owned())
    }
}

fn strings(v: Option<&Value>) -> Vec<&str> {
    v.and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn reg() -> LanguageRegistry {
        LanguageRegistry::from_extensions(&[
            json!({"contributes": {"languages": [
                {"id": "json", "extensions": [".json", ".jsonl"], "filenames": [".babelrc"]},
                {"id": "jsonc", "extensions": [".jsonc"], "filenames": ["tsconfig.json"], "filenamePatterns": ["**/.vscode/*.json"]}
            ]}}),
            json!({"contributes": {"languages": [
                {"id": "typescript", "extensions": [".ts"]},
                {"id": "typescriptdecl", "extensions": [".d.ts"]}
            ]}}),
            json!({"contributes": {}}),
        ])
    }

    #[test]
    fn filename_beats_pattern_beats_extension() {
        let r = reg();
        assert_eq!(r.language_for("/w/a.json"), "json");
        assert_eq!(r.language_for("/w/tsconfig.json"), "jsonc");
        assert_eq!(r.language_for("/w/.vscode/settings.json"), "jsonc");
        assert_eq!(r.language_for("/w/.babelrc"), "json");
        assert_eq!(r.language_for("/w/A.JSON"), "json");
        assert_eq!(r.language_for("/w/x.d.ts"), "typescriptdecl");
        assert_eq!(r.language_for("/w/x.ts"), "typescript");
        assert_eq!(r.language_for("/w/README"), PLAINTEXT);
    }

    #[test]
    fn ids_include_plaintext() {
        let ids = reg().ids();
        assert!(ids.contains(&"plaintext".to_string()));
        assert!(ids.contains(&"jsonc".to_string()));
    }
}
