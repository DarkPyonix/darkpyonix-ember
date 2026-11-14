//! Messages for the user, from any thread.
//!
//! compose-rust's `Message::show` queues on the calling thread, and only the UI thread's queue
//! is drained, while the results of main-server calls arrive on the runtime's workers. Workers
//! post here instead; [`NoticeHost`] (composed once, in the shell) shows each on the UI thread.

#![allow(non_snake_case)]

use std::sync::atomic::{AtomicU64, Ordering};

use compose_rust::runtime::launched_effect;
use compose_rust::{Message, MessageDuration};

use crate::ui::coexist::shared_ui;

static NEXT: AtomicU64 = AtomicU64::new(1);

/// Posts `text` to be shown as a long message. Safe from any thread.
pub fn post(text: impl Into<String>) {
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let text = owner_or_scope_text(&text.into());
    shared_ui().notice.set(Some((id, text)));
}

/// A call that needed the owner key (403 `owner_required`, FR-S12, #842) opens the claim sheet
/// again and claims anew when no claim is in progress, and reads as one fixed sentence. A call
/// the device's scope refused reads as one fixed sentence too (FR-N8). Anything else unchanged.
fn owner_or_scope_text(text: &str) -> String {
    use crate::owner_claim as claim;
    if claim::text_is_owner_required(text) {
        if claim::request_claim() {
            shared_ui().owner_claim_dismissed.set(None);
            return claim::OWNER_NEEDED.to_string();
        }
        return claim::OWNER_NEEDED_ELSEWHERE.to_string();
    }
    crate::ui::device_scope::failure_text(text)
}

/// Shows what [`post`] queued. Compose it once at the root.
pub fn NoticeHost() {
    let latest = shared_ui().notice.get();
    let key = latest.as_ref().map(|(id, _)| *id);
    launched_effect(key, move || async move {
        if let Some((_, text)) = latest {
            // TODO(compose-rust#446): Message has no Modifier for the mockup toast shadow.
            // TODO(compose-rust#577): Message has no placement, so the toast shows where the
            // toolkit puts it (top end) and not at the mockup's bottom centre (`.toast`).
            Message::new(text).with_duration(MessageDuration::Long).show();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use compose_rust::protocol::decode_batch;
    use compose_rust::{Host, Recomposer};

    /// A 403 `owner_required` from any owner action reads as one fixed sentence, never the
    /// server's English; without a running claim (the tests, a server elsewhere) it points at
    /// `ember-server owner` (FR-S12, #842).
    #[test]
    fn fr_s12_an_owner_required_failure_reads_as_one_sentence() {
        let _guard = crate::services::test_guard();
        assert!(crate::owner_claim::handle().is_none());
        post("응답하지 못했습니다: server answered 403: this route needs owner presence: the Ember app");
        let shown = shared_ui().notice.get_untracked().map(|(_, t)| t);
        assert_eq!(shown.as_deref(), Some(crate::owner_claim::OWNER_NEEDED_ELSEWHERE));
        post("모드를 바꾸지 못했습니다: server answered 500: boom");
        let shown = shared_ui().notice.get_untracked().map(|(_, t)| t);
        assert_eq!(shown.as_deref(), Some("모드를 바꾸지 못했습니다: server answered 500: boom"));
    }

    #[test]
    fn fr_l9_a_notice_posted_from_a_worker_thread_is_shown_on_the_ui_thread() {
        let _guard = crate::services::test_guard();
        let mut host = Host::new(|| Box::new(Recomposer::new(NoticeHost)));
        decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        std::thread::spawn(|| post("Export failed: disk full")).join().expect("worker");
        let mut shown = String::new();
        for _ in 0..4 {
            let batch = decode_batch(host.render_frame(16_000_000).expect("render")).expect("decode");
            shown.extend(batch.iter().map(|m| format!("{m:?}")));
        }
        assert!(shown.contains("Export failed: disk full"), "{shown}");
    }
}
