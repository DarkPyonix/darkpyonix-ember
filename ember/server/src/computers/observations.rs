//! File observations and their per-file comparison after a computer switch (SPEC FR-S7,
//! `INTENT.md` Q4).
//!
//! The files an agent read or edited are found in its stored tool calls. At a switch each is
//! hashed on the computer being left and on the one being entered; only files whose hash differs
//! (or that are missing) are flagged for a re-read.

use std::collections::BTreeMap;

use crate::events::AgentEvent;
use crate::store::StoredEvent;

use super::NodeApi;

/// Most files compared at one switch (the latest ones win), so a long session cannot make a
/// switch slow.
pub const MAX_FILES: usize = 100;

/// Tools whose input names a file the agent has seen or changed.
const FILE_TOOLS: &[&str] = &["Read", "Edit", "Write", "MultiEdit", "NotebookEdit"];

/// What a computer holds at a path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileState {
    /// SHA-256 of the whole file (hex).
    Hash(String),
    Missing,
}

/// Absolute paths of the files in the agent's tool calls, oldest first, each once, at most
/// [`MAX_FILES`] (the most recent).
pub fn observed_paths(events: &[StoredEvent]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for e in events {
        let AgentEvent::ToolCall { name, input, .. } = &e.event else { continue };
        if !FILE_TOOLS.contains(&name.as_str()) {
            continue;
        }
        let Some(p) = input.get("file_path").or_else(|| input.get("notebook_path")).and_then(|v| v.as_str())
        else {
            continue;
        };
        if !p.starts_with('/') {
            continue;
        }
        out.retain(|x| x != p);
        out.push(p.to_string());
    }
    if out.len() > MAX_FILES {
        out.drain(..out.len() - MAX_FILES);
    }
    out
}

/// Outcome of comparing the two computers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Comparison {
    pub same: Vec<String>,
    /// Different content, missing on one side, or not hashable on one side.
    pub changed: Vec<String>,
}

/// Compare per path. A path that could not be hashed on either side (`None`) counts as changed.
pub fn compare(
    paths: &[String],
    before: &BTreeMap<String, Option<FileState>>,
    after: &BTreeMap<String, Option<FileState>>,
) -> Comparison {
    let mut c = Comparison::default();
    for p in paths {
        match (before.get(p), after.get(p)) {
            (Some(Some(a)), Some(Some(b))) if a == b => c.same.push(p.clone()),
            _ => c.changed.push(p.clone()),
        }
    }
    c
}

/// Hash every path through `node`; a failure for one path is `None`, not an error.
pub async fn hash_all(node: &dyn NodeApi, paths: &[String]) -> BTreeMap<String, Option<FileState>> {
    let mut m = BTreeMap::new();
    for p in paths {
        m.insert(p.clone(), node.file_state(p).await.ok());
    }
    m
}

/// The one-time notice after a switch, with the per-file result (FR-S7). With nothing observed
/// the v0 wording applies.
pub fn switch_notice_with(from: &str, to: &str, cmp: &Comparison) -> String {
    let mut s = format!(
        "[Ember system notice] This session's computer changed from \"{from}\" to \"{to}\". \
         Directory listings, processes and command output you observed earlier came from \
         \"{from}\" and may differ here: re-run commands whose results you rely on. Use the \
         current computer description, not the earlier one."
    );
    if cmp.changed.is_empty() {
        s.push_str(&format!(
            " The {} file(s) you read or edited have identical content on \"{to}\"; they need no re-read.",
            cmp.same.len()
        ));
    } else {
        s.push_str(" Re-read each of these files before editing it, because it differs or is missing on the new computer: ");
        s.push_str(&cmp.changed.join(", "));
        s.push('.');
        if !cmp.same.is_empty() {
            s.push_str(&format!(" The other {} file(s) you read or edited are identical.", cmp.same.len()));
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call(name: &str, input: serde_json::Value) -> StoredEvent {
        StoredEvent {
            session_id: "s".into(),
            seq: 1,
            at: 0,
            event: AgentEvent::ToolCall { call_id: "c".into(), name: name.into(), input },
        }
    }

    #[test]
    fn paths_come_from_file_tools_once_latest_last() {
        let ev = vec![
            call("Read", json!({"file_path": "/a"})),
            call("Bash", json!({"command": "ls"})),
            call("Edit", json!({"file_path": "/b"})),
            call("Read", json!({"file_path": "/a"})),
            call("Read", json!({"file_path": "rel"})),
        ];
        assert_eq!(observed_paths(&ev), vec!["/b".to_string(), "/a".to_string()]);
    }

    #[test]
    fn paths_are_capped_to_the_latest() {
        let ev: Vec<_> = (0..MAX_FILES + 5).map(|i| call("Read", json!({"file_path": format!("/f{i}")}))).collect();
        let p = observed_paths(&ev);
        assert_eq!(p.len(), MAX_FILES);
        assert_eq!(p[0], "/f5");
    }

    #[test]
    fn only_differing_files_are_flagged() {
        let h = |s: &str| Some(FileState::Hash(s.into()));
        let paths: Vec<String> = ["/same", "/diff", "/gone", "/unknown"].iter().map(|s| s.to_string()).collect();
        let before: BTreeMap<_, _> = [
            ("/same".to_string(), h("1")),
            ("/diff".to_string(), h("1")),
            ("/gone".to_string(), h("1")),
            ("/unknown".to_string(), h("1")),
        ]
        .into();
        let after: BTreeMap<_, _> = [
            ("/same".to_string(), h("1")),
            ("/diff".to_string(), h("2")),
            ("/gone".to_string(), Some(FileState::Missing)),
            ("/unknown".to_string(), None),
        ]
        .into();
        let c = compare(&paths, &before, &after);
        assert_eq!(c.same, vec!["/same".to_string()]);
        assert_eq!(c.changed, vec!["/diff".to_string(), "/gone".to_string(), "/unknown".to_string()]);
    }

    #[test]
    fn missing_on_both_sides_counts_as_same() {
        let m = |s| Some(s);
        let before: BTreeMap<_, _> = [("/x".to_string(), m(FileState::Missing))].into();
        let c = compare(&["/x".to_string()], &before, &before.clone());
        assert_eq!(c.same.len(), 1);
    }

    #[test]
    fn notice_lists_changed_files_and_counts_identical() {
        let c = Comparison { same: vec!["/a".into()], changed: vec!["/b".into()] };
        let n = switch_notice_with("mac", "pi", &c);
        assert!(n.contains("from \"mac\" to \"pi\""));
        assert!(n.contains("/b"));
        assert!(!n.contains("/a"));
        assert!(n.contains("other 1 file(s)"));
        let none = switch_notice_with("mac", "pi", &Comparison { same: vec!["/a".into()], changed: vec![] });
        assert!(none.contains("need no re-read"));
    }
}
