//! Commands contributed by extensions through `contributes.commands` in `package.json`.

use serde_json::Value;

/// An extension command described by `contributes.commands`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContributedCommand {
    pub id: String,
    pub title: String,
    pub category: Option<String>,
}

impl ContributedCommand {
    pub fn new(id: impl Into<String>, title: impl Into<String>, category: Option<String>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            category,
        }
    }

    /// The title displayed in the palette: `"Category: Title"` if category is present, else `"Title"`.
    pub fn display_title(&self) -> String {
        match &self.category {
            Some(cat) if !cat.trim().is_empty() => format!("{cat}: {}", self.title),
            _ => self.title.clone(),
        }
    }
}

fn text_val(v: Option<&Value>) -> Option<String> {
    let v = v?;
    if let Some(s) = v.as_str() {
        return Some(s.to_string());
    }
    if let Some(s) = v.get("value").and_then(Value::as_str) {
        return Some(s.to_string());
    }
    None
}

/// Collect `ContributedCommand`s from extensions' `contributes.commands`.
pub fn parse_contributed_commands(extensions: &[Value]) -> Vec<ContributedCommand> {
    let mut commands = Vec::new();
    for desc in extensions {
        let Some(cmds) = desc.get("contributes").and_then(|c| c.get("commands")).and_then(Value::as_array) else {
            continue;
        };
        for cmd in cmds {
            let Some(id) = cmd.get("command").and_then(Value::as_str) else { continue };
            let title = text_val(cmd.get("title")).unwrap_or_else(|| id.to_string());
            let category = text_val(cmd.get("category"));
            commands.push(ContributedCommand {
                id: id.to_string(),
                title,
                category,
            });
        }
    }
    commands
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_commands_from_extension_json() {
        let exts = vec![
            json!({
                "contributes": {
                    "commands": [
                        { "command": "git.stage", "title": "Stage", "category": "Git" },
                        { "command": "editor.format", "title": { "value": "Format Document" } }
                    ]
                }
            }),
            json!({ "contributes": {} }),
        ];
        let cmds = parse_contributed_commands(&exts);
        assert_eq!(cmds.len(), 2);
        assert_eq!(cmds[0].id, "git.stage");
        assert_eq!(cmds[0].display_title(), "Git: Stage");
        assert_eq!(cmds[1].id, "editor.format");
        assert_eq!(cmds[1].display_title(), "Format Document");
    }
}
