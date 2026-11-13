//! Messages for the user, from any thread.
//!
//! compose-rust's `Message::show` queues on the calling thread, and only the UI thread's queue
//! is drained, while the results of main-server calls arrive on the runtime's workers. Workers
//! post here instead; [`NoticeHost`] (composed once, in the shell) shows each on the UI thread.

#![allow(non_snake_case)]

use std::sync::atomic::{AtomicU64, Ordering};

use compose_rust::runtime::{current_window_size, launched_effect};
use compose_rust::{Message, MessageDuration, MessageHorizontal, MessagePlacement, MessageVertical};

use crate::ui::coexist::shared_ui;

static NEXT: AtomicU64 = AtomicU64::new(1);

/// Posts `text` to be shown as a long message. Safe from any thread.
pub fn post(text: impl Into<String>) {
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    // A call the device's scope refused reads as one fixed sentence wherever it fails (FR-N8).
    let text = crate::ui::device_scope::failure_text(&text.into());
    shared_ui().notice.set(Some((id, text)));
}

/// Where the toast sits: the bottom centre (`.toast` `left:50%`, compose-rust#577, v0.0.9).
pub const TOAST_PLACEMENT: MessagePlacement =
    MessagePlacement { vertical: MessageVertical::Bottom, horizontal: MessageHorizontal::Center };

/// How far the toast is above the bottom of the safe area: `.toast` `bottom:90px`, and at 560 dp
/// and below 12 dp above the floating tab bar (`calc(var(--tabbar-clear) + 12px)`, the 64 dp bar,
/// its 10 dp gap and 10 dp more). The tab bar is Ember's own, not a toolkit bar, so the toolkit
/// does not know to keep clear of it and the offset counts it. An unreported width (0) is the
/// desktop's.
pub fn toast_offset(window_dp: f32) -> f32 {
    if window_dp > 0.0 && window_dp <= 560.0 { 64.0 + 10.0 + 10.0 + 12.0 } else { 90.0 }
}

/// Shows what [`post`] queued. Compose it once at the root.
///
/// Its shadow and pill corner are the app theme's overlay tokens (compose-rust#446,
/// `fonts::APP_OVERLAY`), because a message takes no Modifier.
pub fn NoticeHost() {
    let latest = shared_ui().notice.get();
    let key = latest.as_ref().map(|(id, _)| *id);
    let offset = toast_offset(current_window_size().width_dp);
    launched_effect(key, move || async move {
        if let Some((_, text)) = latest {
            Message::new(text)
                .with_duration(MessageDuration::Long)
                .with_placement(TOAST_PLACEMENT)
                .with_offset(offset)
                .show();
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

    #[test]
    fn fr_l9_the_toast_sits_at_the_bottom_centre_clear_of_the_tab_bar() {
        // compose-rust#577: the toast takes the mockup's place instead of the toolkit's top end.
        assert_eq!(TOAST_PLACEMENT.vertical, MessageVertical::Bottom);
        assert_eq!(TOAST_PLACEMENT.horizontal, MessageHorizontal::Center);
        assert_eq!(toast_offset(390.0), 96.0);
        assert_eq!(toast_offset(560.0), 96.0);
        assert_eq!(toast_offset(561.0), 90.0);
        assert_eq!(toast_offset(0.0), 90.0);
        let production = include_str!("notice.rs").split("#[cfg(test)]").next().unwrap();
        assert!(production.contains(".with_placement(TOAST_PLACEMENT)"));
        assert!(production.contains(".with_offset(offset)"));
        assert!(!production.contains(concat!("TODO(compose-rust#", "446)")));
        assert!(!production.contains(concat!("TODO(compose-rust#", "577)")));
    }
}
