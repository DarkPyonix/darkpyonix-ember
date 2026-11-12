//! FR-L31.16, #448: the notification toasts and notification center of the editor window.
//!
//! Code-OSS 1.139 (`notificationsToasts.ts`, `notificationsCenter.ts`, `mainThreadMessageService.ts`)
//! decides the behaviour: a message arrives as a toast at the bottom right, at most
//! [`MAX_TOASTS`] at once, newest on top; a toast hides after [`purge_timeout_ms`] unless it is
//! sticky (an error with buttons); a hidden toast stays in the center until it is closed. A button
//! closes the notification and answers the extension with its handle; closing it any other way
//! answers `undefined`. Opening the center hides every toast. A modal message is a dialog, not a
//! notification: it is kept apart and never enters the center.
//! The model is pure (time is passed in) so the UI only draws it.

use crate::output::{ExtensionMessage, MessageSeverity};

/// Toasts shown at once (Code-OSS `NotificationsToasts.MAX_NOTIFICATIONS`).
pub const MAX_TOASTS: usize = 3;

/// How long a toast stays before it hides into the center (Code-OSS `PURGE_TIMEOUT`).
pub fn purge_timeout_ms(severity: MessageSeverity) -> u64 {
    match severity {
        MessageSeverity::Info => 15_000,
        MessageSeverity::Warning => 18_000,
        MessageSeverity::Error => 20_000,
    }
}

/// Code-OSS keeps an error with buttons on screen until it is answered.
pub fn is_sticky(message: &ExtensionMessage) -> bool {
    message.severity == MessageSeverity::Error && !message.actions.is_empty()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub message: ExtensionMessage,
    pub shown_at_ms: u64,
    /// Still on screen as a toast.
    pub toast: bool,
}

/// The answer to send for a closed message: its id and the chosen handle (`None`: `undefined`).
pub type Answer = (u64, Option<i64>);

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Notifications {
    /// Non-modal notifications, oldest first.
    entries: Vec<Entry>,
    /// Modal dialogs waiting for an answer, oldest first.
    modals: Vec<ExtensionMessage>,
    center_open: bool,
}

impl Notifications {
    pub fn new() -> Self {
        Self::default()
    }

    /// A new message at `now_ms`. While the center is open it goes there without a toast.
    pub fn push(&mut self, message: ExtensionMessage, now_ms: u64) {
        if message.modal {
            self.modals.push(message);
            return;
        }
        let toast = !self.center_open;
        self.entries.push(Entry { message, shown_at_ms: now_ms, toast });
    }

    /// Hide the toasts whose time is up. Returns true when one was hidden.
    pub fn tick(&mut self, now_ms: u64) -> bool {
        let mut changed = false;
        for e in &mut self.entries {
            if e.toast && !is_sticky(&e.message) && now_ms.saturating_sub(e.shown_at_ms) >= purge_timeout_ms(e.message.severity) {
                e.toast = false;
                changed = true;
            }
        }
        changed
    }

    /// The milliseconds until the next toast hides, if any is waiting to.
    pub fn next_deadline_ms(&self, now_ms: u64) -> Option<u64> {
        self.entries
            .iter()
            .filter(|e| e.toast && !is_sticky(&e.message))
            .map(|e| (e.shown_at_ms + purge_timeout_ms(e.message.severity)).saturating_sub(now_ms))
            .min()
    }

    /// The toasts to draw, newest first, at most [`MAX_TOASTS`].
    pub fn toasts(&self) -> Vec<&Entry> {
        if self.center_open {
            return Vec::new();
        }
        self.entries.iter().rev().filter(|e| e.toast).take(MAX_TOASTS).collect()
    }

    /// The center's rows, newest first.
    pub fn center(&self) -> Vec<&Entry> {
        self.entries.iter().rev().collect()
    }

    /// The modal dialog to draw: the oldest waiting one.
    pub fn modal(&self) -> Option<&ExtensionMessage> {
        self.modals.first()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn center_open(&self) -> bool {
        self.center_open
    }

    /// Open or close the center. Opening hides every toast (Code-OSS hides toasts while the
    /// center is visible, and they do not come back when it closes).
    pub fn set_center_open(&mut self, open: bool) {
        self.center_open = open;
        if open {
            for e in &mut self.entries {
                e.toast = false;
            }
        }
    }

    /// A button of message `id` was chosen: the message closes and the extension gets `handle`.
    pub fn choose(&mut self, id: u64, handle: i64) -> Option<Answer> {
        self.remove(id).map(|_| (id, Some(handle)))
    }

    /// Message `id` was closed without a button (the toast's close, the center's clear, a
    /// dialog's cancel without a close-affordance button): the extension gets `undefined`.
    pub fn dismiss(&mut self, id: u64) -> Option<Answer> {
        self.remove(id).map(|_| (id, None))
    }

    /// The center's 모두 지우기: every non-modal notification closes.
    pub fn clear_all(&mut self) -> Vec<Answer> {
        self.entries.drain(..).map(|e| (e.message.id, None)).collect()
    }

    /// The dialog's cancel: the close-affordance button's handle when there is one, else `undefined`
    /// (Code-OSS adds its own 취소 button then).
    pub fn cancel_modal(&mut self, id: u64) -> Option<Answer> {
        let pos = self.modals.iter().position(|m| m.id == id)?;
        let m = self.modals.remove(pos);
        Some((id, m.actions.iter().find(|a| a.is_close_affordance).map(|a| a.handle)))
    }

    /// The buttons a modal dialog shows: the extension's non-cancel buttons, then the cancel
    /// button (the close-affordance one, else 취소 with no handle).
    pub fn modal_buttons(message: &ExtensionMessage) -> Vec<(String, Option<i64>)> {
        let mut out: Vec<(String, Option<i64>)> =
            message.actions.iter().filter(|a| !a.is_close_affordance).map(|a| (a.title.clone(), Some(a.handle))).collect();
        match message.actions.iter().find(|a| a.is_close_affordance) {
            Some(a) => out.push((a.title.clone(), Some(a.handle))),
            None => out.push(("취소".to_string(), None)),
        }
        out
    }

    fn remove(&mut self, id: u64) -> Option<ExtensionMessage> {
        if let Some(pos) = self.entries.iter().position(|e| e.message.id == id) {
            return Some(self.entries.remove(pos).message);
        }
        let pos = self.modals.iter().position(|m| m.id == id)?;
        Some(self.modals.remove(pos))
    }
}

/// The center's header: "알림" with the count, or "새 알림 없음" (Code-OSS "No New Notifications").
pub fn center_title(count: usize) -> String {
    if count == 0 { "새 알림 없음".to_string() } else { format!("알림 {count}개") }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::MessageAction;

    fn msg(id: u64, severity: MessageSeverity, actions: &[(&str, i64, bool)], modal: bool) -> ExtensionMessage {
        ExtensionMessage {
            id,
            severity,
            message: format!("m{id}"),
            source: None,
            actions: actions.iter().map(|(t, h, c)| MessageAction { title: (*t).into(), handle: *h, is_close_affordance: *c }).collect(),
            modal,
            detail: None,
        }
    }

    #[test]
    fn fr_l31_16_toasts_show_newest_first_and_at_most_three() {
        let mut n = Notifications::new();
        for id in 1..=5 {
            n.push(msg(id, MessageSeverity::Info, &[], false), 0);
        }
        let ids: Vec<u64> = n.toasts().iter().map(|e| e.message.id).collect();
        assert_eq!(ids, vec![5, 4, 3]);
        assert_eq!(n.center().len(), 5);
    }

    #[test]
    fn fr_l31_16_toasts_hide_after_the_code_oss_timeouts_but_stay_in_the_center() {
        let mut n = Notifications::new();
        n.push(msg(1, MessageSeverity::Info, &[], false), 0);
        n.push(msg(2, MessageSeverity::Warning, &[], false), 0);
        n.push(msg(3, MessageSeverity::Error, &[], false), 0);
        assert_eq!(n.next_deadline_ms(0), Some(15_000));
        assert!(!n.tick(14_999));
        assert!(n.tick(15_000));
        assert_eq!(n.toasts().iter().map(|e| e.message.id).collect::<Vec<_>>(), vec![3, 2]);
        assert!(n.tick(18_000));
        assert!(n.tick(20_000));
        assert!(n.toasts().is_empty());
        assert_eq!(n.len(), 3);
        assert_eq!(n.next_deadline_ms(20_000), None);
    }

    #[test]
    fn fr_l31_16_an_error_with_buttons_is_sticky() {
        let mut n = Notifications::new();
        n.push(msg(1, MessageSeverity::Error, &[("Retry", 0, false)], false), 0);
        n.push(msg(2, MessageSeverity::Warning, &[("OK", 0, false)], false), 0);
        n.tick(60_000);
        assert_eq!(n.toasts().iter().map(|e| e.message.id).collect::<Vec<_>>(), vec![1]);
    }

    #[test]
    fn fr_l31_16_a_button_answers_its_handle_and_closing_answers_undefined() {
        let mut n = Notifications::new();
        n.push(msg(1, MessageSeverity::Info, &[("Yes", 4, false), ("No", 5, false)], false), 0);
        n.push(msg(2, MessageSeverity::Info, &[], false), 0);
        assert_eq!(n.choose(1, 5), Some((1, Some(5))));
        assert_eq!(n.choose(1, 5), None);
        assert_eq!(n.dismiss(2), Some((2, None)));
        assert!(n.is_empty());
    }

    #[test]
    fn fr_l31_16_opening_the_center_hides_toasts_and_clear_all_answers_each() {
        let mut n = Notifications::new();
        n.push(msg(1, MessageSeverity::Info, &[], false), 0);
        n.set_center_open(true);
        assert!(n.toasts().is_empty());
        n.push(msg(2, MessageSeverity::Info, &[], false), 1);
        assert!(n.toasts().is_empty());
        n.set_center_open(false);
        assert!(n.toasts().is_empty(), "toasts do not come back after the center closes");
        assert_eq!(center_title(n.len()), "알림 2개");
        assert_eq!(n.clear_all(), vec![(1, None), (2, None)]);
        assert_eq!(center_title(0), "새 알림 없음");
    }

    #[test]
    fn fr_l31_16_modal_messages_are_dialogs_with_a_cancel_button() {
        let mut n = Notifications::new();
        n.push(msg(1, MessageSeverity::Warning, &[("Delete", 0, false)], true), 0);
        n.push(msg(2, MessageSeverity::Warning, &[("Save", 0, false), ("Keep", 1, true)], true), 0);
        assert!(n.is_empty(), "modal messages never enter the center");
        let first = n.modal().cloned().unwrap();
        assert_eq!(first.id, 1);
        assert_eq!(Notifications::modal_buttons(&first), vec![("Delete".to_string(), Some(0)), ("취소".to_string(), None)]);
        assert_eq!(n.cancel_modal(1), Some((1, None)));
        let second = n.modal().cloned().unwrap();
        assert_eq!(Notifications::modal_buttons(&second), vec![("Save".to_string(), Some(0)), ("Keep".to_string(), Some(1))]);
        assert_eq!(n.cancel_modal(2), Some((2, Some(1))));
        assert!(n.modal().is_none());
    }
}
