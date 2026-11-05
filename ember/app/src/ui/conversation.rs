//! Transcript item key helper (FR-L5–FR-L7).

use ember_client::transcript::TranscriptItem;

/// A transcript row's stable key: its event sequence number.
pub(crate) fn item_key(item: &TranscriptItem) -> String {
    let seq = match item {
        TranscriptItem::User { seq, .. }
        | TranscriptItem::Assistant { seq, .. }
        | TranscriptItem::ToolCall { seq, .. }
        | TranscriptItem::Approval { seq, .. }
        | TranscriptItem::TurnEnded { seq, .. }
        | TranscriptItem::Error { seq, .. }
        | TranscriptItem::Notice { seq, .. } => *seq,
    };
    seq.to_string()
}
