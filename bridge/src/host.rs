//! What the native host does with bridge messages (FR-B3, FR-B4).
//!
//! The shell gives every string its webview posts to [`BridgeHost::handle_from_webview`].
//! A `tab_detach` becomes an [`OpenWindow`] request for the shell's [`WindowOpener`], which
//! creates the new native window and loads [`OpenWindow::vscode_web_path`] on the same
//! origin (same project, same computer). The source webview closes its own tab once the
//! post succeeds (detach.js); dirty tabs are never detached.

use crate::bridge::{BridgeError, WebviewBridge};
use crate::messages::{decode, BridgeMessage, Inbound, OpenWindow, SiblingWindowClosed};

/// Opens a new IDE window. Implemented by the platform shell.
pub trait WindowOpener {
    fn open_window(&mut self, request: &OpenWindow) -> Result<(), BridgeError>;
}

#[derive(Debug, Clone, PartialEq)]
pub enum HostOutcome {
    /// A window was requested from the opener.
    Opened(OpenWindow),
    /// The opener refused or failed.
    OpenFailed { request: OpenWindow, error: String },
    /// Understood but nothing to do (already logged where relevant).
    Ignored { kind: String, reason: &'static str },
    /// Not a bridge message at all.
    Rejected(String),
}

pub struct BridgeHost<O> {
    opener: O,
    id_prefix: String,
    next_id: u64,
}

impl<O: WindowOpener> BridgeHost<O> {
    /// `id_prefix` namespaces the window ids this host hands out (e.g. a process id).
    pub fn new(opener: O, id_prefix: impl Into<String>) -> Self {
        BridgeHost { opener, id_prefix: id_prefix.into(), next_id: 1 }
    }

    pub fn opener(&self) -> &O {
        &self.opener
    }

    pub fn opener_mut(&mut self) -> &mut O {
        &mut self.opener
    }

    fn next_window_id(&mut self) -> String {
        let id = format!("{}-{}", self.id_prefix, self.next_id);
        self.next_id += 1;
        id
    }

    fn open(&mut self, request: OpenWindow) -> HostOutcome {
        match self.opener.open_window(&request) {
            Ok(()) => HostOutcome::Opened(request),
            Err(e) => {
                tracing::warn!(error = %e, "ember-bridge: opening the detached window failed");
                HostOutcome::OpenFailed { request, error: e.to_string() }
            }
        }
    }

    /// Handles one string posted by a webview.
    ///
    /// Version skew does not drop the message: `decode` logs it, and a best-effort decode
    /// is acted on (FR-B2).
    pub fn handle_from_webview(&mut self, payload: &str) -> HostOutcome {
        let inbound = match decode(payload) {
            Ok(i) => i,
            Err(e) => return HostOutcome::Rejected(e.to_string()),
        };
        let msg = match inbound {
            Inbound::Message(m) => m,
            Inbound::VersionMismatch { decoded: Some(m), .. } => m,
            Inbound::VersionMismatch { kind, decoded: None, .. } => {
                return HostOutcome::Ignored { kind, reason: "version mismatch, fields unreadable (logged)" }
            }
            Inbound::UnknownKind { kind, .. } => {
                return HostOutcome::Ignored { kind, reason: "unknown kind (logged)" }
            }
        };
        match msg {
            BridgeMessage::TabDetach(d) => {
                let id = self.next_window_id();
                self.open(OpenWindow::from_detach(&d, id))
            }
            BridgeMessage::OpenWindow(mut req) => {
                if req.window_id.is_empty() {
                    req.window_id = self.next_window_id();
                }
                self.open(req)
            }
            BridgeMessage::SiblingWindowClosed(_) => HostOutcome::Ignored {
                kind: "sibling_window_closed".to_owned(),
                reason: "native-to-webview message received from a webview",
            },
        }
    }
}

/// Tells one webview that another IDE window closed (FR-B4).
pub fn notify_sibling_closed<B: WebviewBridge + ?Sized>(
    bridge: &B,
    closed_window_id: &str,
) -> Result<(), BridgeError> {
    bridge.post_to_webview(&BridgeMessage::SiblingWindowClosed(SiblingWindowClosed {
        window_id: closed_window_id.to_owned(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::LoopbackBridge;
    use crate::messages::{encode, Position, TabDetach, Workspace};

    #[derive(Default)]
    struct RecordingOpener {
        opened: Vec<OpenWindow>,
        fail: bool,
    }

    impl WindowOpener for RecordingOpener {
        fn open_window(&mut self, request: &OpenWindow) -> Result<(), BridgeError> {
            if self.fail {
                return Err(BridgeError::Platform("no display".into()));
            }
            self.opened.push(request.clone());
            Ok(())
        }
    }

    fn detach_json(version: u32) -> String {
        format!(
            r#"{{"kind":"tab_detach","version":{version},"sourceWindowId":"src",
                "workspace":{{"folder":"/p"}},"fileUri":"vscode-remote://h/p/a.rs",
                "cursor":{{"line":4,"column":2}},"scroll":{{"top":80}},"selection":null}}"#
        )
    }

    #[test]
    fn detach_opens_a_window_at_the_same_project_with_the_state() {
        let mut host = BridgeHost::new(RecordingOpener::default(), "t");
        let out = host.handle_from_webview(&detach_json(1));
        let HostOutcome::Opened(req) = out else { panic!("expected HostOutcome::Opened") };
        assert_eq!(req.window_id, "t-1");
        assert_eq!(req.source_window_id.as_deref(), Some("src"));
        assert_eq!(req.workspace, Some(Workspace { folder: Some("/p".into()) }));
        assert_eq!(req.cursor, Some(Position { line: 4, column: 2 }));
        assert_eq!(req.scroll.map(|s| s.top), Some(80.0));
        assert_eq!(host.opener().opened, vec![req.clone()]);
        assert!(req.vscode_web_path().unwrap().starts_with("/?folder=%2Fp&payload="));
    }

    #[test]
    fn a_newer_detach_version_is_still_acted_on() {
        let mut host = BridgeHost::new(RecordingOpener::default(), "t");
        assert!(matches!(host.handle_from_webview(&detach_json(2)), HostOutcome::Opened(_)));
        assert_eq!(host.opener().opened.len(), 1);
    }

    #[test]
    fn window_ids_are_unique() {
        let mut host = BridgeHost::new(RecordingOpener::default(), "t");
        host.handle_from_webview(&detach_json(1));
        host.handle_from_webview(&detach_json(1));
        let ids: Vec<_> = host.opener().opened.iter().map(|r| r.window_id.clone()).collect();
        assert_eq!(ids, vec!["t-1", "t-2"]);
    }

    #[test]
    fn open_window_from_a_webview_gets_an_id() {
        let mut host = BridgeHost::new(RecordingOpener::default(), "t");
        let req = OpenWindow {
            window_id: String::new(),
            source_window_id: None,
            workspace: Some(Workspace { folder: Some("/p".into()) }),
            file_uri: None,
            cursor: None,
            selection: None,
            scroll: None,
            screen: None,
        };
        let out = host.handle_from_webview(&encode(&BridgeMessage::OpenWindow(req)));
        let HostOutcome::Opened(r) = out else { panic!("expected HostOutcome::Opened") };
        assert_eq!(r.window_id, "t-1");
        assert_eq!(r.vscode_web_path().as_deref(), Some("/?folder=%2Fp"));
    }

    #[test]
    fn opener_failure_is_reported() {
        let mut host = BridgeHost::new(RecordingOpener { fail: true, ..Default::default() }, "t");
        assert!(matches!(host.handle_from_webview(&detach_json(1)), HostOutcome::OpenFailed { .. }));
    }

    #[test]
    fn junk_unknown_and_wrong_direction_are_not_acted_on() {
        let mut host = BridgeHost::new(RecordingOpener::default(), "t");
        assert!(matches!(host.handle_from_webview("{"), HostOutcome::Rejected(_)));
        assert!(matches!(
            host.handle_from_webview(r#"{"kind":"nope","version":1}"#),
            HostOutcome::Ignored { .. }
        ));
        assert!(matches!(
            host.handle_from_webview(r#"{"kind":"sibling_window_closed","version":1,"windowId":"w"}"#),
            HostOutcome::Ignored { .. }
        ));
        assert!(host.opener().opened.is_empty());
    }

    #[test]
    fn sibling_closed_is_pushed_into_the_webview() {
        let b = LoopbackBridge::new();
        notify_sibling_closed(&b, "w9").unwrap();
        let sent = b.take_to_webview();
        assert_eq!(sent.len(), 1);
        let v: serde_json::Value = serde_json::from_str(&sent[0]).unwrap();
        assert_eq!(v["kind"], "sibling_window_closed");
        assert_eq!(v["version"], 1);
        assert_eq!(v["windowId"], "w9");
    }

    #[test]
    fn detach_round_trip_through_the_trait() {
        // webview side posts through the trait, host side drains and handles.
        let b = LoopbackBridge::new();
        let d = TabDetach {
            source_window_id: "src".into(),
            workspace: Some(Workspace { folder: Some("/p".into()) }),
            file_uri: "vscode-remote://h/p/a.rs".into(),
            cursor: None,
            scroll: None,
            selection: None,
            screen: None,
            label: None,
            editor: Some("vscode-web".into()),
            state_source: None,
            sent_at_ms: None,
        };
        b.post_to_native(&BridgeMessage::TabDetach(d)).unwrap();
        let mut host = BridgeHost::new(RecordingOpener::default(), "t");
        for payload in b.take_to_native() {
            assert!(matches!(host.handle_from_webview(&payload), HostOutcome::Opened(_)));
        }
        assert_eq!(host.opener().opened.len(), 1);
    }
}
