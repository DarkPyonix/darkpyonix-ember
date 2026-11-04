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
    shared_ui().notice.set(Some((id, text.into())));
}

/// Shows what [`post`] queued. Compose it once at the root.
pub fn NoticeHost() {
    let latest = shared_ui().notice.get();
    let key = latest.as_ref().map(|(id, _)| *id);
    launched_effect(key, move || async move {
        if let Some((_, text)) = latest {
            Message::new(text).with_duration(MessageDuration::Long).show();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use compose_rust::protocol::decode_batch;
    use compose_rust::{Host, Recomposer};

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
