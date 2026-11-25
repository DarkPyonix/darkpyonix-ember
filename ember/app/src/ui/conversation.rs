//! Transcript item key and shell-call grouping helpers (FR-L5–FR-L7, #1075).

use ember_client::transcript::TranscriptItem;

/// A transcript row's stable key: its event sequence number.
pub(crate) fn item_key(item: &TranscriptItem) -> String {
    let seq = match item {
        TranscriptItem::User { seq, .. }
        | TranscriptItem::Assistant { seq, .. }
        | TranscriptItem::ToolCall { seq, .. }
        | TranscriptItem::Approval { seq, .. }
        | TranscriptItem::Question { seq, .. }
        | TranscriptItem::TurnEnded { seq, .. }
        | TranscriptItem::Error { seq, .. }
        | TranscriptItem::Notice { seq, .. } => *seq,
    };
    seq.to_string()
}

/// A display item in the conversation transcript (FR-L5, #1075).
///
/// Two or more consecutive shell tool calls fold into a single `ShellGroup` row;
/// a single shell call and all non-shell items remain `Single`.
#[derive(Debug, Clone, PartialEq)]
pub enum ConversationDisplayItem {
    Single(TranscriptItem),
    ShellGroup(Vec<TranscriptItem>),
}

/// Whether `item` is a shell command tool call that can be folded into a shell group (FR-L5).
///
/// A shell call whose output parses as a unified file diff renders as a file-change card
/// instead of a plain shell call, so it is excluded from shell grouping.
pub fn is_shell_tool_call(item: &TranscriptItem) -> bool {
    let TranscriptItem::ToolCall {
        name,
        input,
        result,
        ..
    } = item
    else {
        return false;
    };
    let out = result.as_ref().map(|r| r.output.as_str()).unwrap_or("");
    if crate::ui::tool_card::file_diff(name, input, out).is_some() {
        return false;
    }
    crate::ui::tool_card::is_shell_tool(name, input)
}

/// Fold 2 or more consecutive shell tool calls into a single [`ConversationDisplayItem::ShellGroup`],
/// keeping single shell calls and all non-shell items untouched as [`ConversationDisplayItem::Single`]
/// (FR-L5, #1075).
pub fn group_shell_calls(items: &[TranscriptItem]) -> Vec<ConversationDisplayItem> {
    let mut out = Vec::with_capacity(items.len());
    let mut run: Vec<TranscriptItem> = Vec::new();
    let flush = |run: &mut Vec<TranscriptItem>, out: &mut Vec<ConversationDisplayItem>| {
        if run.len() >= 2 {
            out.push(ConversationDisplayItem::ShellGroup(std::mem::take(run)));
        } else if let Some(single) = run.pop() {
            out.push(ConversationDisplayItem::Single(single));
        }
    };
    for item in items {
        if is_shell_tool_call(item) {
            run.push(item.clone());
        } else {
            flush(&mut run, &mut out);
            out.push(ConversationDisplayItem::Single(item.clone()));
        }
    }
    flush(&mut run, &mut out);
    out
}

/// Stable key for a [`ConversationDisplayItem::ShellGroup`] in the expanded state set.
///
/// Derived from the first call's sequence number so appending another consecutive call to a live
/// run preserves the user's expand/collapse toggle.
pub fn shell_group_key(items: &[TranscriptItem]) -> String {
    let first = items.first().map(item_key).unwrap_or_default();
    format!("shell-group:{first}")
}

/// Summary label for a folded shell group row (FR-L5, #1075).
pub fn shell_group_label(count: usize) -> String {
    format!("명령 {count}개 실행")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ember_client::transcript::ToolOutput;
    use serde_json::json;

    fn shell(seq: i64, id: &str, name: &str, cmd: &str) -> TranscriptItem {
        TranscriptItem::ToolCall {
            seq,
            call_id: id.into(),
            name: name.into(),
            input: json!({ "command": cmd }),
            result: Some(ToolOutput {
                output: String::new(),
                is_error: false,
            }),
        }
    }

    fn read_tool(seq: i64, id: &str, path: &str) -> TranscriptItem {
        TranscriptItem::ToolCall {
            seq,
            call_id: id.into(),
            name: "Read".into(),
            input: json!({ "file_path": path }),
            result: Some(ToolOutput {
                output: "fn main() {}".into(),
                is_error: false,
            }),
        }
    }

    fn assistant(seq: i64, text: &str) -> TranscriptItem {
        TranscriptItem::Assistant {
            seq,
            text: text.into(),
            streaming: false,
        }
    }

    #[test]
    fn fr_l5_three_consecutive_shell_calls_fold_into_one_group() {
        let items = vec![
            shell(1, "c1", "Bash", "cargo fmt --check"),
            shell(2, "c2", "shell", "cargo clippy"),
            shell(3, "c3", "exec_command", "cargo test --locked"),
        ];
        let grouped = group_shell_calls(&items);
        assert_eq!(grouped.len(), 1);
        match &grouped[0] {
            ConversationDisplayItem::ShellGroup(calls) => {
                assert_eq!(calls.len(), 3);
                assert_eq!(shell_group_label(calls.len()), "명령 3개 실행");
                assert_eq!(shell_group_key(calls), "shell-group:1");
            }
            other => panic!("expected ShellGroup, got {other:?}"),
        }
    }

    #[test]
    fn fr_l5_single_shell_call_stays_ungrouped() {
        let items = vec![
            assistant(1, "확인하겠습니다."),
            shell(2, "c1", "Bash", "ls -la"),
            assistant(3, "완료했습니다."),
        ];
        let grouped = group_shell_calls(&items);
        assert_eq!(grouped.len(), 3);
        assert!(matches!(grouped[0], ConversationDisplayItem::Single(_)));
        assert!(matches!(grouped[1], ConversationDisplayItem::Single(_)));
        assert!(matches!(grouped[2], ConversationDisplayItem::Single(_)));
    }

    #[test]
    fn fr_l5_non_shell_items_break_runs_into_separate_groups() {
        let items = vec![
            shell(1, "c1", "Bash", "ls"),
            shell(2, "c2", "Bash", "pwd"),
            read_tool(3, "r1", "src/main.rs"),
            shell(4, "c3", "Bash", "cargo check"),
            shell(5, "c4", "Bash", "cargo test"),
            assistant(6, "끝났습니다."),
            shell(7, "c5", "Bash", "git status"),
        ];
        let grouped = group_shell_calls(&items);
        assert_eq!(grouped.len(), 5);
        assert!(matches!(&grouped[0], ConversationDisplayItem::ShellGroup(v) if v.len() == 2));
        assert!(matches!(&grouped[1], ConversationDisplayItem::Single(TranscriptItem::ToolCall { name, .. }) if name == "Read"));
        assert!(matches!(&grouped[2], ConversationDisplayItem::ShellGroup(v) if v.len() == 2));
        assert!(matches!(&grouped[3], ConversationDisplayItem::Single(TranscriptItem::Assistant { .. })));
        assert!(matches!(&grouped[4], ConversationDisplayItem::Single(TranscriptItem::ToolCall { call_id, .. }) if call_id == "c5"));
    }

    #[test]
    fn fr_l5_shell_call_with_unified_diff_is_not_folded() {
        let diff_call = TranscriptItem::ToolCall {
            seq: 2,
            call_id: "c2".into(),
            name: "Bash".into(),
            input: json!({ "command": "git diff" }),
            result: Some(ToolOutput {
                output: "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1,2 @@\n a\n+b\n".into(),
                is_error: false,
            }),
        };
        assert!(!is_shell_tool_call(&diff_call));
        let items = vec![
            shell(1, "c1", "Bash", "git status"),
            diff_call,
            shell(3, "c3", "Bash", "cargo test"),
        ];
        let grouped = group_shell_calls(&items);
        assert_eq!(grouped.len(), 3);
        assert!(grouped.iter().all(|i| matches!(i, ConversationDisplayItem::Single(_))));
    }
}
