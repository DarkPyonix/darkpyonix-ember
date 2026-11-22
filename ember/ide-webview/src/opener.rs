//! Bridge host wiring, detached-window queue, sibling registry, and JS shims.

use std::collections::BTreeMap;

use ember_bridge::{
    notify_sibling_closed, BridgeError, OpenWindow, ScreenPoint, WebviewBridge, WindowOpener,
    HANDLER_NAME,
};

use crate::cli::resolve_detached_url;

/// A validated request to create a sibling native window in the event loop.
#[derive(Debug, Clone, PartialEq)]
pub struct OpenWindowCommand {
    pub window_id: String,
    pub url: String,
    pub title: String,
    pub screen: Option<ScreenPoint>,
}

/// [`WindowOpener`] that resolves each [`OpenWindow`] against the session origin
/// and queues commands for the native event loop to realize.
pub struct QueuedWindowOpener {
    origin: String,
    default_title: String,
    pending: Vec<OpenWindowCommand>,
}

impl QueuedWindowOpener {
    pub fn new(origin: impl Into<String>, default_title: impl Into<String>) -> Self {
        Self {
            origin: origin.into(),
            default_title: default_title.into(),
            pending: Vec::new(),
        }
    }

    pub fn take_pending(&mut self) -> Vec<OpenWindowCommand> {
        std::mem::take(&mut self.pending)
    }
}

impl WindowOpener for QueuedWindowOpener {
    fn open_window(&mut self, request: &OpenWindow) -> Result<(), BridgeError> {
        let Some(url) = resolve_detached_url(&self.origin, request) else {
            return Err(BridgeError::Platform(
                "detached window request has no workspace folder".to_owned(),
            ));
        };
        self.pending.push(OpenWindowCommand {
            window_id: request.window_id.clone(),
            url,
            title: self.default_title.clone(),
            screen: request.screen,
        });
        Ok(())
    }
}

/// Result of closing a window and broadcasting `sibling_window_closed` to remaining siblings.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct CloseReport {
    pub removed: bool,
    pub remaining: usize,
    pub notified: Vec<String>,
    pub errors: Vec<(String, String)>,
}

/// Tracks open windows by logical window id and broadcasts close notifications.
pub struct WindowRegistry<B> {
    windows: BTreeMap<String, B>,
}

impl<B> Default for WindowRegistry<B> {
    fn default() -> Self {
        Self {
            windows: BTreeMap::new(),
        }
    }
}

impl<B: WebviewBridge> WindowRegistry<B> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, window_id: impl Into<String>, bridge: B) {
        self.windows.insert(window_id.into(), bridge);
    }

    pub fn is_empty(&self) -> bool {
        self.windows.is_empty()
    }

    pub fn close(&mut self, closed_window_id: &str) -> CloseReport {
        let removed = self.windows.remove(closed_window_id).is_some();
        if !removed {
            return CloseReport {
                removed: false,
                remaining: self.windows.len(),
                notified: Vec::new(),
                errors: Vec::new(),
            };
        }

        let mut notified = Vec::new();
        let mut errors = Vec::new();
        for (sibling_id, bridge) in &self.windows {
            match notify_sibling_closed(bridge, closed_window_id) {
                Ok(()) => notified.push(sibling_id.clone()),
                Err(err) => errors.push((sibling_id.clone(), err.to_string())),
            }
        }

        CloseReport {
            removed: true,
            remaining: self.windows.len(),
            notified,
            errors,
        }
    }
}

/// Initialization script injected before page scripts run.
///
/// Sets the logical window id in `sessionStorage` so `detach.js` reports the
/// host-assigned id, and exposes `window.webkit.messageHandlers.emberBridge`
/// forwarding to `window.ipc.postMessage` so every platform webview routes
/// `detach.js` messages into the native IPC handler.
pub fn bridge_init_script(window_id: &str) -> String {
    let id_json = serde_json::to_string(window_id).expect("window id serializes as JSON");
    let handler_json = serde_json::to_string(HANDLER_NAME).expect("handler name serializes as JSON");
    format!(
        "(function () {{\n\
           if (typeof window === 'undefined') return;\n\
           var winId = {id_json};\n\
           var handlerName = {handler_json};\n\
           window.__emberWindowId = winId;\n\
           try {{\n\
             if (window.sessionStorage && !window.sessionStorage.getItem('ember_window_id')) {{\n\
               window.sessionStorage.setItem('ember_window_id', winId);\n\
             }}\n\
           }} catch (e) {{}}\n\
           if (window.ipc && typeof window.ipc.postMessage === 'function') {{\n\
             if (!window.webkit) window.webkit = {{}};\n\
             if (!window.webkit.messageHandlers) window.webkit.messageHandlers = {{}};\n\
             if (!window.webkit.messageHandlers[handlerName]) {{\n\
               window.webkit.messageHandlers[handlerName] = {{\n\
                 postMessage: function (payload) {{\n\
                   var text = (typeof payload === 'string') ? payload : JSON.stringify(payload);\n\
                   window.ipc.postMessage(text);\n\
                 }}\n\
               }};\n\
             }}\n\
           }}\n\
         }})();"
    )
}

/// JavaScript snippet that delivers a native-to-webview JSON payload to `detach.js`.
pub fn receive_eval_script(payload: &str) -> String {
    let arg_json = serde_json::to_string(payload).expect("payload serializes as JSON");
    format!(
        "if (window.__emberBridge && typeof window.__emberBridge.receive === 'function') {{ window.__emberBridge.receive({arg_json}); }}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ember_bridge::{
        decode, encode, BridgeHost, BridgeMessage, HostOutcome, Inbound, LoopbackBridge, Position,
        ScreenPoint, SiblingWindowClosed, TabDetach, Workspace,
    };

    fn sample_detach(file_uri: &str, folder: Option<&str>) -> String {
        let msg = BridgeMessage::TabDetach(TabDetach {
            source_window_id: "win-1".into(),
            workspace: folder.map(|f| Workspace {
                folder: Some(f.to_owned()),
            }),
            file_uri: file_uri.to_owned(),
            cursor: Some(Position { line: 12, column: 4 }),
            scroll: None,
            selection: None,
            screen: Some(ScreenPoint { x: 420.0, y: 180.0 }),
            label: Some("main.rs".into()),
            editor: Some("vscode-web".into()),
            state_source: Some("dataTransfer".into()),
            sent_at_ms: Some(1000.0),
        });
        encode(&msg)
    }

    #[test]
    fn fr_b3_tab_detach_queues_sibling_window_on_same_origin() {
        let opener = QueuedWindowOpener::new("http://127.0.0.1:8890", "Ember IDE");
        let mut host = BridgeHost::new(opener, "ide");

        let outcome = host.handle_from_webview(&sample_detach(
            "vscode-remote://h/work/src/main.rs",
            Some("/work"),
        ));
        let HostOutcome::Opened(req) = outcome else {
            panic!("expected HostOutcome::Opened, got {outcome:?}");
        };
        assert_eq!(req.window_id, "ide-1");

        let pending = host.opener_mut().take_pending();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].window_id, "ide-1");
        assert_eq!(pending[0].title, "Ember IDE");
        assert_eq!(pending[0].screen, Some(ScreenPoint { x: 420.0, y: 180.0 }));
        assert!(pending[0]
            .url
            .starts_with("http://127.0.0.1:8890/?folder=%2Fwork&payload="));
    }

    #[test]
    fn fr_b3_tab_detach_without_workspace_folder_fails_loudly() {
        let opener = QueuedWindowOpener::new("http://127.0.0.1:8890", "Ember IDE");
        let mut host = BridgeHost::new(opener, "ide");

        let outcome = host.handle_from_webview(&sample_detach(
            "vscode-remote://h/work/src/main.rs",
            None,
        ));
        assert!(matches!(outcome, HostOutcome::OpenFailed { .. }));
        assert!(host.opener_mut().take_pending().is_empty());
    }

    #[test]
    fn fr_b2_unknown_kind_and_malformed_json_do_not_open_windows() {
        let opener = QueuedWindowOpener::new("http://127.0.0.1:8890", "Ember IDE");
        let mut host = BridgeHost::new(opener, "ide");

        assert!(matches!(
            host.handle_from_webview(r#"{"kind":"future_event","data":1}"#),
            HostOutcome::Ignored { .. }
        ));
        assert!(matches!(
            host.handle_from_webview("not-json"),
            HostOutcome::Rejected(_)
        ));
        assert!(host.opener_mut().take_pending().is_empty());
    }

    #[test]
    fn fr_b4_closing_window_broadcasts_sibling_window_closed_to_survivors_only() {
        let mut reg = WindowRegistry::new();
        reg.insert("win-1", LoopbackBridge::new());
        reg.insert("win-2", LoopbackBridge::new());
        reg.insert("win-3", LoopbackBridge::new());

        let report = reg.close("win-2");
        assert!(report.removed);
        assert_eq!(report.remaining, 2);
        assert_eq!(report.notified, vec!["win-1".to_owned(), "win-3".to_owned()]);
        assert!(report.errors.is_empty());

        for id in ["win-1", "win-3"] {
            let sent = reg.windows.get(id).unwrap().take_to_webview();
            assert_eq!(sent.len(), 1);
            assert_eq!(
                decode(&sent[0]).unwrap(),
                Inbound::Message(BridgeMessage::SiblingWindowClosed(SiblingWindowClosed {
                    window_id: "win-2".into(),
                }))
            );
        }

        // Closing an already-closed window is a no-op.
        let again = reg.close("win-2");
        assert!(!again.removed);
        assert_eq!(again.remaining, 2);
    }

    #[test]
    fn fr_l7_scripts_embed_handler_name_and_escape_payload() {
        let init = bridge_init_script("win-42");
        assert!(init.contains(HANDLER_NAME));
        assert!(init.contains("win-42"));
        assert!(init.contains("window.ipc.postMessage"));

        let eval = receive_eval_script(r#"{"kind":"sibling_window_closed","windowId":"win-2"}"#);
        assert!(eval.contains("window.__emberBridge.receive("));
        assert!(eval.contains("sibling_window_closed"));
    }
}
