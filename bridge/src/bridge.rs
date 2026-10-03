//! The one trait every platform bridge implements (ARCHITECTURE.md §3, INTENT.md D11).
//!
//! Payloads are the JSON strings produced by [`crate::encode`]. Platform mapping:
//!
//! | Direction            | macOS / iOS (WKWebView)                               | Windows (WebView2)                    |
//! |----------------------|-------------------------------------------------------|---------------------------------------|
//! | webview → native     | `WKScriptMessageHandler` named [`crate::HANDLER_NAME`] | `WebMessageReceived` (`TryGetWebMessageAsString`) |
//! | native → webview     | `evaluateJavaScript("window.__emberBridge.receive(<json>)")` | `PostWebMessageAsString` (detach.js listens on `chrome.webview`) |
//!
//! `send_to_native` exists on the trait (not only in JS) so a non-webview editor surface —
//! the Ember editor core — can speak the same protocol, and so FR-B1–B4 are tested once
//! against the trait with [`LoopbackBridge`].

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use crate::messages::{encode, BridgeMessage};

#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    #[error("the webview or window is gone")]
    Closed,
    #[error("platform bridge error: {0}")]
    Platform(String),
}

pub trait WebviewBridge {
    /// Deliver an encoded message to the native host.
    fn send_to_native(&self, payload: &str) -> Result<(), BridgeError>;
    /// Deliver an encoded message into the webview (FR-B4).
    fn send_to_webview(&self, payload: &str) -> Result<(), BridgeError>;

    fn post_to_native(&self, msg: &BridgeMessage) -> Result<(), BridgeError> {
        self.send_to_native(&encode(msg))
    }
    fn post_to_webview(&self, msg: &BridgeMessage) -> Result<(), BridgeError> {
        self.send_to_webview(&encode(msg))
    }
}

/// An in-memory bridge that records both directions. For tests and for running the host
/// logic without a platform webview.
#[derive(Debug, Default)]
pub struct LoopbackBridge {
    to_native: Mutex<Vec<String>>,
    to_webview: Mutex<Vec<String>>,
    closed: AtomicBool,
}

impl LoopbackBridge {
    pub fn new() -> Self {
        Self::default()
    }

    /// Simulates the webview going away: every later send fails with [`BridgeError::Closed`].
    pub fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
    }

    pub fn take_to_native(&self) -> Vec<String> {
        std::mem::take(&mut *self.to_native.lock().expect("loopback lock"))
    }

    pub fn take_to_webview(&self) -> Vec<String> {
        std::mem::take(&mut *self.to_webview.lock().expect("loopback lock"))
    }

    fn push(&self, queue: &Mutex<Vec<String>>, payload: &str) -> Result<(), BridgeError> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(BridgeError::Closed);
        }
        queue.lock().expect("loopback lock").push(payload.to_owned());
        Ok(())
    }
}

impl WebviewBridge for LoopbackBridge {
    fn send_to_native(&self, payload: &str) -> Result<(), BridgeError> {
        self.push(&self.to_native, payload)
    }
    fn send_to_webview(&self, payload: &str) -> Result<(), BridgeError> {
        self.push(&self.to_webview, payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::{decode, Inbound, SiblingWindowClosed};

    #[test]
    fn loopback_records_each_direction_separately() {
        let b = LoopbackBridge::new();
        let msg = BridgeMessage::SiblingWindowClosed(SiblingWindowClosed { window_id: "w".into() });
        b.post_to_webview(&msg).unwrap();
        b.send_to_native("raw").unwrap();
        let to_webview = b.take_to_webview();
        assert_eq!(to_webview.len(), 1);
        assert_eq!(decode(&to_webview[0]).unwrap(), Inbound::Message(msg));
        assert_eq!(b.take_to_native(), vec!["raw".to_owned()]);
        assert!(b.take_to_webview().is_empty());
    }

    #[test]
    fn closed_bridge_fails_loudly() {
        let b = LoopbackBridge::new();
        b.close();
        assert!(matches!(b.send_to_webview("x"), Err(BridgeError::Closed)));
        assert!(matches!(b.send_to_native("x"), Err(BridgeError::Closed)));
    }

    #[test]
    fn trait_is_object_safe() {
        let b: Box<dyn WebviewBridge> = Box::new(LoopbackBridge::new());
        b.send_to_webview("{}").unwrap();
    }
}
