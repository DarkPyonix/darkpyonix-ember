//! `.vscode/launch.json`: the configurations and compounds it lists, and the substitution of the
//! variables a configuration may use.
//!
//! The file is JSONC (comments and trailing commas), read with [`crate::settings::parse_jsonc`].
//! Only the variables Ember can answer are replaced; `${env:X}`, `${config:x}`, `${command:x}`
//! and `${input:x}` stay as written, because the server's environment and the workbench's
//! settings and pickers are not known here.

use serde_json::{Map, Value};

use crate::settings::parse_jsonc;

/// The workspace-relative path of the file.
pub const LAUNCH_JSON: &str = ".vscode/launch.json";

#[derive(Debug, Clone, PartialEq)]
pub struct Compound {
    pub name: String,
    /// The names of the configurations that start together.
    pub configurations: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct LaunchFile {
    pub configurations: Vec<Value>,
    pub compounds: Vec<Compound>,
}

/// The name of a configuration object, `""` when it has none.
pub fn config_name(config: &Value) -> &str {
    config.get("name").and_then(Value::as_str).unwrap_or("")
}

impl LaunchFile {
    pub fn find(&self, name: &str) -> Option<&Value> {
        self.configurations.iter().find(|c| config_name(c) == name)
    }

    pub fn compound(&self, name: &str) -> Option<&Compound> {
        self.compounds.iter().find(|c| c.name == name)
    }
}

/// Read the text of a `launch.json`. An empty file has no configurations. Entries that are not
/// objects are skipped, as the workbench skips them.
pub fn parse_launch_json(text: &str) -> Result<LaunchFile, String> {
    let root = parse_jsonc(text).map_err(|e| format!("cannot read launch.json at line {}, column {}: {}", e.line, e.column, e.message))?;
    let configurations = root
        .get("configurations")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter(|c| c.is_object()).cloned().collect())
        .unwrap_or_default();
    let compounds = root
        .get("compounds")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|c| {
                    let name = c.get("name")?.as_str()?.to_owned();
                    let configurations = c
                        .get("configurations")?
                        .as_array()?
                        .iter()
                        .filter_map(|e| match e {
                            Value::String(s) => Some(s.clone()),
                            Value::Object(o) => o.get("name").and_then(Value::as_str).map(str::to_owned),
                            _ => None,
                        })
                        .collect();
                    Some(Compound { name, configurations })
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(LaunchFile { configurations, compounds })
}

/// What the substitution may answer with.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VarContext {
    /// Path of the workspace folder on the server.
    pub workspace_folder: Option<String>,
    /// Path of the file in the active editor.
    pub file: Option<String>,
}

fn basename(path: &str) -> &str {
    path.trim_end_matches('/').rsplit('/').next().unwrap_or(path)
}

fn dirname(path: &str) -> &str {
    match path.trim_end_matches('/').rsplit_once('/') {
        Some(("", _)) => "/",
        Some((dir, _)) => dir,
        None => ".",
    }
}

/// `path.extname`: the text from the last dot of the base name, `""` for no dot or a leading one.
fn extname(base: &str) -> &str {
    match base.rfind('.') {
        Some(i) if i > 0 => &base[i..],
        _ => "",
    }
}

fn relative<'a>(path: &'a str, folder: &str) -> Option<&'a str> {
    let folder = folder.trim_end_matches('/');
    path.strip_prefix(folder)?.strip_prefix('/')
}

impl VarContext {
    fn lookup(&self, name: &str) -> Option<String> {
        let folder = self.workspace_folder.as_deref();
        let file = self.file.as_deref();
        match name {
            "workspaceFolder" => folder.map(str::to_owned),
            "workspaceFolderBasename" => folder.map(|f| basename(f).to_owned()),
            "pathSeparator" | "/" => Some("/".to_owned()),
            "file" => file.map(str::to_owned),
            "fileBasename" => file.map(|f| basename(f).to_owned()),
            "fileBasenameNoExtension" => file.map(|f| {
                let base = basename(f);
                base[..base.len() - extname(base).len()].to_owned()
            }),
            "fileDirname" => file.map(|f| dirname(f).to_owned()),
            "fileExtname" => file.map(|f| extname(basename(f)).to_owned()),
            "relativeFile" => file.map(|f| folder.and_then(|d| relative(f, d)).unwrap_or(f).to_owned()),
            "relativeFileDirname" => file.map(|f| {
                let rel = folder.and_then(|d| relative(f, d)).unwrap_or(f);
                match rel.rsplit_once('/') {
                    Some((dir, _)) => dir.to_owned(),
                    None => ".".to_owned(),
                }
            }),
            _ => None,
        }
    }

    /// `text` with every variable this context answers replaced.
    pub fn substitute_str(&self, text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(start) = rest.find("${") {
            out.push_str(&rest[..start]);
            let after = &rest[start + 2..];
            match after.find('}') {
                Some(end) => {
                    let name = &after[..end];
                    match self.lookup(name) {
                        Some(value) => out.push_str(&value),
                        None => {
                            out.push_str("${");
                            out.push_str(name);
                            out.push('}');
                        }
                    }
                    rest = &after[end + 1..];
                }
                None => {
                    out.push_str(&rest[start..]);
                    rest = "";
                }
            }
        }
        out.push_str(rest);
        out
    }

    /// `value` with the variables replaced in every string, at any depth.
    pub fn substitute(&self, value: &Value) -> Value {
        match value {
            Value::String(s) => Value::String(self.substitute_str(s)),
            Value::Array(a) => Value::Array(a.iter().map(|v| self.substitute(v)).collect()),
            Value::Object(o) => {
                let mut out = Map::new();
                for (k, v) in o {
                    out.insert(k.clone(), self.substitute(v));
                }
                Value::Object(out)
            }
            other => other.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn fr_l31_42_launch_json_reads_comments_trailing_commas_and_compounds() {
        let text = r#"{
            // the usual
            "version": "0.2.0",
            "configurations": [
                { "name": "Run", "type": "debugpy", "request": "launch", "program": "${file}", },
                /* not an object */ 7,
                { "name": "Attach", "type": "debugpy", "request": "attach" },
            ],
            "compounds": [ { "name": "Both", "configurations": ["Run", { "name": "Attach", "folder": "x" }] } ],
        }"#;
        let file = parse_launch_json(text).unwrap();
        assert_eq!(file.configurations.len(), 2);
        assert_eq!(config_name(&file.configurations[1]), "Attach");
        assert_eq!(file.find("Run").unwrap()["program"], "${file}");
        assert_eq!(file.compound("Both").unwrap().configurations, vec!["Run".to_owned(), "Attach".to_owned()]);
        assert_eq!(parse_launch_json("").unwrap(), LaunchFile::default());
        assert_eq!(parse_launch_json("// only a comment\n").unwrap(), LaunchFile::default());
        let err = parse_launch_json("{ \"configurations\": [").unwrap_err();
        assert!(err.contains("launch.json"), "{err}");
    }

    #[test]
    fn fr_l31_42_variables_are_replaced_and_unknown_ones_are_left() {
        let ctx = VarContext { workspace_folder: Some("/w/proj".into()), file: Some("/w/proj/src/app.py".into()) };
        let sub = |s: &str| ctx.substitute_str(s);
        assert_eq!(sub("${workspaceFolder}/x"), "/w/proj/x");
        assert_eq!(sub("${workspaceFolderBasename}"), "proj");
        assert_eq!(sub("${file}"), "/w/proj/src/app.py");
        assert_eq!(sub("${fileBasename}"), "app.py");
        assert_eq!(sub("${fileBasenameNoExtension}"), "app");
        assert_eq!(sub("${fileDirname}"), "/w/proj/src");
        assert_eq!(sub("${fileExtname}"), ".py");
        assert_eq!(sub("${relativeFile}"), "src/app.py");
        assert_eq!(sub("${relativeFileDirname}"), "src");
        assert_eq!(sub("a${/}b"), "a/b");
        assert_eq!(sub("${env:HOME} ${config:a.b} ${command:x} ${input:y}"), "${env:HOME} ${config:a.b} ${command:x} ${input:y}");
        assert_eq!(sub("open ${ brace"), "open ${ brace");
        assert_eq!(sub("no variables"), "no variables");
        let none = VarContext::default();
        assert_eq!(none.substitute_str("${file} ${workspaceFolder}"), "${file} ${workspaceFolder}");
    }

    #[test]
    fn fr_l31_42_substitution_reaches_nested_strings_and_keeps_other_values() {
        let ctx = VarContext { workspace_folder: Some("/w".into()), file: Some("/w/m.py".into()) };
        let config = json!({"program": "${file}", "args": ["${workspaceFolder}", 3, true], "env": {"P": "${fileBasename}"}, "n": null});
        assert_eq!(
            ctx.substitute(&config),
            json!({"program": "/w/m.py", "args": ["/w", 3, true], "env": {"P": "m.py"}, "n": null})
        );
    }
}
