//! FR-E2, FR-L31.16, #448: Output channels and extension message notifications.
//!
//! Output channels are registered by extensions with `MainThreadOutputService.$register`
//! or created by the Ember workbench. Text appended to a channel is capped in memory
//! at [`MAX_OUTPUT_CHANNEL_BYTES`] (64 KiB per channel).

use serde::{Deserialize, Serialize};

/// Maximum bytes kept in memory for one output channel (old text is trimmed from the start).
pub const MAX_OUTPUT_CHANNEL_BYTES: usize = 64 * 1024;

/// One output channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputChannel {
    pub id: String,
    pub label: String,
    pub text: String,
}

impl OutputChannel {
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            text: String::new(),
        }
    }

    /// Append text to the channel buffer, capping the total length to `MAX_OUTPUT_CHANNEL_BYTES`.
    pub fn append(&mut self, chunk: &str) {
        self.text.push_str(chunk);
        if self.text.len() > MAX_OUTPUT_CHANNEL_BYTES {
            let overflow = self.text.len() - MAX_OUTPUT_CHANNEL_BYTES;
            let mut cut = overflow;
            while cut < self.text.len() && !self.text.is_char_boundary(cut) {
                cut += 1;
            }
            self.text.drain(..cut);
        }
    }

    /// Clear the buffer.
    pub fn clear(&mut self) {
        self.text.clear();
    }
}

/// Severity of an extension message / notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageSeverity {
    Info,
    Warning,
    Error,
}

impl MessageSeverity {
    pub fn from_u8(v: u8) -> Self {
        match v {
            3 => Self::Error,
            2 => Self::Warning,
            _ => Self::Info,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Info => "정보",
            Self::Warning => "경고",
            Self::Error => "오류",
        }
    }
}

/// A button of an extension message: one `{ title, isCloseAffordance, handle }` of
/// `MainThreadMessageService.$showMessage`. Choosing it answers the call with `handle`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageAction {
    pub title: String,
    pub handle: i64,
    /// For a modal message: the button that cancels (Code-OSS's dialog cancel button).
    pub is_close_affordance: bool,
}

/// An extension notification surfaced to the workbench.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionMessage {
    /// Ember's id of this message; the answer to the extension names it.
    pub id: u64,
    pub severity: MessageSeverity,
    pub message: String,
    /// The extension's display name (`options.source.label`).
    pub source: Option<String>,
    /// The buttons, in the extension's order.
    pub actions: Vec<MessageAction>,
    /// `options.modal`: Code-OSS shows a dialog instead of a toast.
    pub modal: bool,
    /// `options.detail`: the dialog's second line (modal only in Code-OSS).
    pub detail: Option<String>,
}

impl ExtensionMessage {
    /// The message of a `$showMessage(severity, message, options, commands)` call.
    pub fn from_show_message(id: u64, severity: u8, message: String, options: &serde_json::Value, commands: &[serde_json::Value]) -> Self {
        // `options.source` is `{ identifier, label }` in Code-OSS 1.139; a bare string is taken too.
        let source = match options.get("source") {
            Some(serde_json::Value::String(s)) => Some(s.clone()),
            Some(v) => v
                .get("label")
                .and_then(|l| l.as_str())
                .map(str::to_owned)
                .or_else(|| v.get("identifier").and_then(|i| i.get("value").or(Some(i))).and_then(|i| i.as_str()).map(str::to_owned)),
            None => None,
        };
        let actions = commands
            .iter()
            .filter_map(|c| {
                Some(MessageAction {
                    title: c.get("title")?.as_str()?.to_owned(),
                    handle: c.get("handle")?.as_i64()?,
                    is_close_affordance: c.get("isCloseAffordance").and_then(|v| v.as_bool()).unwrap_or(false),
                })
            })
            .collect();
        Self {
            id,
            severity: MessageSeverity::from_u8(severity),
            message,
            source,
            actions,
            modal: options.get("modal").and_then(|v| v.as_bool()).unwrap_or(false),
            detail: options.get("detail").and_then(|v| v.as_str()).map(str::to_owned),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_channel_append_and_clear() {
        let mut ch = OutputChannel::new("c1", "Test");
        assert_eq!(ch.text, "");
        ch.append("hello\n");
        ch.append("world\n");
        assert_eq!(ch.text, "hello\nworld\n");
        ch.clear();
        assert_eq!(ch.text, "");
    }

    #[test]
    fn output_channel_caps_at_max_bytes() {
        let mut ch = OutputChannel::new("c1", "Test");
        let chunk = "A".repeat(40 * 1024);
        ch.append(&chunk);
        ch.append(&chunk);
        assert!(ch.text.len() <= MAX_OUTPUT_CHANNEL_BYTES);
        assert_eq!(ch.text.len(), MAX_OUTPUT_CHANNEL_BYTES);
    }

    #[test]
    fn message_severity_mapping() {
        assert_eq!(MessageSeverity::from_u8(1), MessageSeverity::Info);
        assert_eq!(MessageSeverity::from_u8(2), MessageSeverity::Warning);
        assert_eq!(MessageSeverity::from_u8(3), MessageSeverity::Error);
        assert_eq!(MessageSeverity::from_u8(0), MessageSeverity::Info);
        assert_eq!(MessageSeverity::Error.label(), "오류");
    }

    #[test]
    fn fr_l31_16_show_message_reads_source_label_actions_modal_and_detail() {
        let options = serde_json::json!({
            "source": {"identifier": {"value": "rust-lang.rust-analyzer"}, "label": "rust-analyzer"},
            "modal": true,
            "detail": "Restart the server?",
        });
        let commands = vec![
            serde_json::json!({"title": "Restart", "isCloseAffordance": false, "handle": 0}),
            serde_json::json!({"title": "Cancel", "isCloseAffordance": true, "handle": 1}),
        ];
        let m = ExtensionMessage::from_show_message(7, 2, "Server crashed".into(), &options, &commands);
        assert_eq!(m.id, 7);
        assert_eq!(m.severity, MessageSeverity::Warning);
        assert_eq!(m.source.as_deref(), Some("rust-analyzer"));
        assert!(m.modal);
        assert_eq!(m.detail.as_deref(), Some("Restart the server?"));
        assert_eq!(
            m.actions,
            vec![
                MessageAction { title: "Restart".into(), handle: 0, is_close_affordance: false },
                MessageAction { title: "Cancel".into(), handle: 1, is_close_affordance: true },
            ]
        );
    }

    #[test]
    fn fr_l31_16_show_message_without_options_is_a_plain_toast() {
        let m = ExtensionMessage::from_show_message(1, 1, "Hi".into(), &serde_json::json!({}), &[]);
        assert_eq!(m.source, None);
        assert!(m.actions.is_empty());
        assert!(!m.modal);
        assert_eq!(m.detail, None);
        let s = ExtensionMessage::from_show_message(2, 1, "Hi".into(), &serde_json::json!({"source": "Git"}), &[]);
        assert_eq!(s.source.as_deref(), Some("Git"));
    }
}
