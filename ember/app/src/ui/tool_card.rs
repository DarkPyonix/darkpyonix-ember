//! Pure models for the transcript's tool cards (FR-L5): the file-change card (#535) and the
//! approval card text (#536). The compose-rust view in `ui/cr/conversation.rs` only draws them.
//!
//! Mockup `session.js`: an edit shows the file name, a green `+N` and a red `-N` pill and the
//! changed lines; an approval shows the command only, titled `<컴퓨터>에서 명령을 실행할까요?`.

use serde_json::Value;

/// One line of a file change, without its `+` / `-` / ` ` marker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffLine {
    Add(String),
    Del(String),
    Context(String),
}

impl DiffLine {
    /// The line as the mockup prints it: the marker, then the text.
    pub fn shown(&self) -> String {
        match self {
            DiffLine::Add(t) => format!("+{t}"),
            DiffLine::Del(t) => format!("-{t}"),
            DiffLine::Context(t) => format!(" {t}"),
        }
    }
}

/// A file change as the card shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    /// The file name (last path segment), as the card title.
    pub file: String,
    pub added: usize,
    pub removed: usize,
    pub lines: Vec<DiffLine>,
}

/// The most changed lines a card draws; the pills still count all of them.
pub const MAX_DIFF_LINES: usize = 40;

fn base_name(path: &str) -> String {
    let p = path.trim();
    p.rsplit(['/', '\\']).find(|s| !s.is_empty()).unwrap_or(p).to_string()
}

fn count(lines: &[DiffLine]) -> (usize, usize) {
    lines.iter().fold((0, 0), |(a, d), l| match l {
        DiffLine::Add(_) => (a + 1, d),
        DiffLine::Del(_) => (a, d + 1),
        DiffLine::Context(_) => (a, d),
    })
}

fn split_lines(s: &str) -> impl Iterator<Item = &str> {
    s.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l))
}

/// Parse the first file of a unified diff (`--- a/x`, `+++ b/x`, `@@` hunks). `None` when the
/// text holds no diff. Headers, `@@` lines and `\ No newline` notes are not shown.
pub fn parse_unified_diff(text: &str) -> Option<FileDiff> {
    let mut file: Option<String> = None;
    let mut old_file: Option<String> = None;
    let mut lines = Vec::new();
    let mut in_hunk = false;
    for raw in split_lines(text) {
        if !in_hunk || file.is_none() {
            if let Some(rest) = raw.strip_prefix("--- ") {
                old_file = Some(strip_diff_path(rest));
                continue;
            }
            if let Some(rest) = raw.strip_prefix("+++ ") {
                let p = strip_diff_path(rest);
                file = Some(if p == "/dev/null" { old_file.clone().unwrap_or(p) } else { p });
                continue;
            }
        }
        if raw.starts_with("diff --git ") {
            if file.is_some() && in_hunk {
                break;
            }
            continue;
        }
        if raw.starts_with("@@") {
            in_hunk = true;
            continue;
        }
        if !in_hunk {
            continue;
        }
        if let Some(t) = raw.strip_prefix('+') {
            lines.push(DiffLine::Add(t.to_string()));
        } else if let Some(t) = raw.strip_prefix('-') {
            lines.push(DiffLine::Del(t.to_string()));
        } else if let Some(t) = raw.strip_prefix(' ') {
            lines.push(DiffLine::Context(t.to_string()));
        } else if raw.starts_with('\\') || raw.is_empty() {
            continue;
        }
    }
    let file = file?;
    let (added, removed) = count(&lines);
    Some(FileDiff { file: base_name(&file), added, removed, lines })
}

fn strip_diff_path(rest: &str) -> String {
    let p = rest.split('\t').next().unwrap_or(rest).trim();
    p.strip_prefix("a/").or_else(|| p.strip_prefix("b/")).unwrap_or(p).to_string()
}

fn str_field<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|k| v.get(*k).and_then(Value::as_str))
}

/// The lines `old` -> `new` changes: lines both share at the start and the end are dropped, so
/// an edit that appends a block counts only the block, as its unified diff would.
fn replace_lines(lines: &mut Vec<DiffLine>, old: &str, new: &str) {
    let to_vec = |s: &str| -> Vec<String> {
        if s.is_empty() {
            return Vec::new();
        }
        split_lines(s.strip_suffix('\n').unwrap_or(s)).map(str::to_string).collect()
    };
    let (o, n) = (to_vec(old), to_vec(new));
    let pre = o.iter().zip(n.iter()).take_while(|(a, b)| a == b).count();
    let max_suf = o.len().min(n.len()) - pre;
    let suf = o.iter().rev().zip(n.iter().rev()).take(max_suf).take_while(|(a, b)| a == b).count();
    lines.extend(o[pre..o.len() - suf].iter().cloned().map(DiffLine::Del));
    lines.extend(n[pre..n.len() - suf].iter().cloned().map(DiffLine::Add));
}

/// The file change a tool call made, from its input (Claude Code `Edit`, `MultiEdit`, `Write`)
/// or, failing that, from a unified diff in its result (Codex `apply_patch`, a `git diff`).
/// `None` for a call that is not a file change, so the card keeps its plain layout.
pub fn file_diff(tool: &str, input: &Value, output: &str) -> Option<FileDiff> {
    let path = str_field(input, &["file_path", "path", "notebook_path"]);
    // A unified diff in the result is what really changed: it wins over the input.
    if let Some(mut d) = parse_unified_diff(output) {
        if let Some(p) = path {
            d.file = base_name(p);
        }
        return Some(d);
    }
    let mut lines = Vec::new();
    let from_input = match (tool, path) {
        ("Edit" | "edit" | "str_replace", Some(_)) => {
            let old = str_field(input, &["old_string", "old_str"]).unwrap_or("");
            let new = str_field(input, &["new_string", "new_str"]).unwrap_or("");
            replace_lines(&mut lines, old, new);
            true
        }
        ("MultiEdit", Some(_)) => {
            for e in input.get("edits").and_then(Value::as_array).into_iter().flatten() {
                let old = str_field(e, &["old_string"]).unwrap_or("");
                let new = str_field(e, &["new_string"]).unwrap_or("");
                replace_lines(&mut lines, old, new);
            }
            true
        }
        ("Write" | "write" | "create", Some(_)) => {
            let content = str_field(input, &["content", "file_text"]).unwrap_or("");
            replace_lines(&mut lines, "", content);
            true
        }
        _ => false,
    };
    if from_input {
        let (added, removed) = count(&lines);
        return Some(FileDiff { file: base_name(path.unwrap_or_default()), added, removed, lines });
    }
    // A patch handed in as input (Codex `apply_patch` carries it in `input`/`patch`).
    str_field(input, &["patch", "input", "diff"]).and_then(parse_unified_diff)
}

/// What an approval asks for: a shell command, a file change, or another tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalKind {
    Command,
    FileChange,
    Other,
}

/// The approval card's words (#536).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalText {
    pub kind: ApprovalKind,
    pub title: String,
    /// The one thing shown under the title: the command, the file path, or a summary.
    pub body: String,
}

pub const ALLOW_ONCE: &str = "한 번 허용";
/// Mockup `session.js`: the always button names its scope.
pub const ALLOW_ALWAYS: &str = "이 프로젝트에서 항상 허용";
pub const DENY: &str = "거부";
/// The computer word when the session's computer is not known yet.
pub const THIS_COMPUTER: &str = "이 컴퓨터";

fn command_of(input: &Value) -> Option<String> {
    match input.get("command").or_else(|| input.get("cmd")) {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Array(a)) => {
            let parts: Vec<&str> = a.iter().filter_map(Value::as_str).collect();
            // Codex wraps commands as `bash -lc "<cmd>"`; show the inner command.
            if parts.len() == 3 && parts[1] == "-lc" && parts[0].ends_with("sh") {
                Some(parts[2].to_string())
            } else {
                Some(parts.join(" "))
            }
        }
        _ => None,
    }
}

/// The approval card's title and body for `tool` with `input` on `computer`.
pub fn approval_text(computer: Option<&str>, tool: &str, input: &Value) -> ApprovalText {
    let pc = computer.map(str::trim).filter(|c| !c.is_empty()).unwrap_or(THIS_COMPUTER);
    let is_shell = matches!(tool, "Bash" | "bash" | "shell" | "exec_command" | "local_shell");
    if is_shell || (command_of(input).is_some() && str_field(input, &["file_path"]).is_none()) {
        if let Some(cmd) = command_of(input) {
            return ApprovalText {
                kind: ApprovalKind::Command,
                title: format!("{pc}에서 명령을 실행할까요?"),
                body: cmd,
            };
        }
    }
    let file_tool = matches!(tool, "Edit" | "MultiEdit" | "Write" | "NotebookEdit" | "apply_patch" | "edit" | "write");
    if file_tool {
        let body = str_field(input, &["file_path", "path", "notebook_path"])
            .map(str::to_string)
            .or_else(|| file_diff(tool, input, "").map(|d| d.file))
            .unwrap_or_default();
        return ApprovalText {
            kind: ApprovalKind::FileChange,
            title: format!("{pc}에서 파일을 수정할까요?"),
            body,
        };
    }
    let name = if tool.trim().is_empty() { "도구" } else { tool };
    ApprovalText {
        kind: ApprovalKind::Other,
        title: format!("{pc}에서 {name}을(를) 실행할까요?"),
        body: crate::model::json_summary(input, 200),
    }
}

/// The resolved approval's pill (mockup: 허용함 / 항상 허용 green, 거부함 red).
pub fn resolved_label(d: ember_client::wire::ApprovalDecision) -> (&'static str, bool) {
    use ember_client::wire::ApprovalDecision as D;
    match d {
        D::AllowOnce => ("허용함", true),
        D::AllowAlways => ("항상 허용", true),
        D::Deny => ("거부함", false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn fr_l5_edit_input_becomes_file_name_counts_and_typed_lines() {
        let input = json!({
            "file_path": "/work/ember/src/config.rs",
            "old_string": "port = 80\nhost = a",
            "new_string": "port = 8080\nhost = a\ntls = true",
        });
        let d = file_diff("Edit", &input, "The file has been updated.").unwrap();
        assert_eq!(d.file, "config.rs");
        // No shared suffix (host = a is followed by tls), shared prefix none: all lines count.
        assert_eq!((d.added, d.removed), (3, 2));
        assert_eq!(d.lines[0], DiffLine::Del("port = 80".into()));
        assert_eq!(d.lines[2], DiffLine::Add("port = 8080".into()));
        assert_eq!(d.lines[2].shown(), "+port = 8080");
        // An append keeps only the new block, as its unified diff would.
        let app = json!({"file_path": "c.rs", "old_string": "a\n}\n", "new_string": "a\n}\n\nimpl C {}\n"});
        let d = file_diff("Edit", &app, "").unwrap();
        assert_eq!(d.lines, vec![DiffLine::Add(String::new()), DiffLine::Add("impl C {}".into())]);
        // A diff in the result wins over the input.
        let out = "--- a/src/c.rs\n+++ b/src/c.rs\n@@ -1 +1,2 @@\n a\n+b\n";
        let d = file_diff("Edit", &app, out).unwrap();
        assert_eq!((d.file.as_str(), d.added, d.removed), ("c.rs", 1, 0));
        assert_eq!(d.lines[0].shown(), "-port = 80");
    }

    #[test]
    fn fr_l5_unified_diff_drops_headers_and_counts_lines() {
        let out = "diff --git a/src/main.rs b/src/main.rs\nindex 1..2 100644\n--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1,3 +1,3 @@\n fn main() {\n-    old();\n+    new();\n+    more();\n }\n\\ No newline at end of file\n";
        let d = parse_unified_diff(out).unwrap();
        assert_eq!(d.file, "main.rs");
        assert_eq!((d.added, d.removed), (2, 1));
        assert!(d.lines.iter().all(|l| !l.shown().contains("+++") && !l.shown().contains("---")));
        assert_eq!(
            d.lines,
            vec![
                DiffLine::Context("fn main() {".into()),
                DiffLine::Del("    old();".into()),
                DiffLine::Add("    new();".into()),
                DiffLine::Add("    more();".into()),
                DiffLine::Context("}".into()),
            ]
        );
    }

    #[test]
    fn fr_l5_multi_edit_write_and_non_file_tools() {
        let m = json!({"file_path": "a/b.txt", "edits": [
            {"old_string": "x", "new_string": "y"}, {"old_string": "", "new_string": "z"}]});
        let d = file_diff("MultiEdit", &m, "").unwrap();
        assert_eq!((d.file.as_str(), d.added, d.removed), ("b.txt", 2, 1));
        let w = json!({"file_path": "new.md", "content": "one\ntwo\n"});
        let d = file_diff("Write", &w, "").unwrap();
        assert_eq!((d.added, d.removed), (2, 0));
        assert_eq!(file_diff("Bash", &json!({"command": "ls"}), "a\nb"), None);
        assert_eq!(parse_unified_diff("just text\n+ not a diff"), None);
    }

    #[test]
    fn fr_l5_bash_approval_shows_the_command_on_the_computer() {
        let t = approval_text(Some("작업실 맥"), "Bash", &json!({"command": "cargo test", "description": "run"}));
        assert_eq!(t.kind, ApprovalKind::Command);
        assert_eq!(t.title, "작업실 맥에서 명령을 실행할까요?");
        assert_eq!(t.body, "cargo test");
        assert!(!t.body.contains('{'));
        let t = approval_text(None, "shell", &json!({"command": ["bash", "-lc", "ls -la"]}));
        assert_eq!(t.title, "이 컴퓨터에서 명령을 실행할까요?");
        assert_eq!(t.body, "ls -la");
        assert_eq!(ALLOW_ALWAYS, "이 프로젝트에서 항상 허용");
    }

    #[test]
    fn fr_l5_edit_and_other_approvals_have_their_own_text() {
        let t = approval_text(Some("pc"), "Edit", &json!({"file_path": "/x/y.rs", "old_string": "a", "new_string": "b"}));
        assert_eq!(t.kind, ApprovalKind::FileChange);
        assert_eq!(t.title, "pc에서 파일을 수정할까요?");
        assert_eq!(t.body, "/x/y.rs");
        let t = approval_text(Some("pc"), "WebFetch", &json!({"url": "https://e.x"}));
        assert_eq!(t.kind, ApprovalKind::Other);
        assert_eq!(t.title, "pc에서 WebFetch을(를) 실행할까요?");
        assert!(t.body.contains("https://e.x"));
        assert!(!t.body.contains('\n'));
    }
}
