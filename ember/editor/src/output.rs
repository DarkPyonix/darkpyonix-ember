//! FR-E2, #448: Output channels and extension message notifications.
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

/// An extension notification surfaced to the workbench.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionMessage {
    pub severity: MessageSeverity,
    pub message: String,
    pub source: Option<String>,
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
}
