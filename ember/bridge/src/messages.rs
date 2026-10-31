//! Wire types for the IDE window bridge (ARCHITECTURE.md §3).
//!
//! Every message is one JSON object with a string `kind`, the rest of its fields flattened next
//! to it (camelCase). The webview sends it as a JSON
//! *string* through the platform message API, so every host decodes the same bytes.
//!
//! Rules (SPEC FR-B2, FR-B4):
//! - There is no version (INTENT D15). Adding an optional field is always compatible; unknown
//!   fields are ignored on decode, and an unknown `kind` is logged and kept as
//!   [`Inbound::UnknownKind`], never silently dropped.
//! - Positions are **0-based** `(line, column)`, like LSP; `detach.js` converts Monaco's
//!   1-based values.
//!
//! Keep in sync with `ember/proxy/static/detach.js` (`buildDetachMessage`,
//! `buildWorkspaceQuery`). `ember/vectors/bridge/detach_vectors.json` is checked by both.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::url::encode_uri_component;

/// The name the native shell registers its message handler under
/// (`window.webkit.messageHandlers.emberBridge`).
pub const HANDLER_NAME: &str = "emberBridge";

pub const KIND_TAB_DETACH: &str = "tab_detach";
pub const KIND_SIBLING_WINDOW_CLOSED: &str = "sibling_window_closed";
pub const KIND_OPEN_WINDOW: &str = "open_window";

/// 0-based line and column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Position {
    pub line: u32,
    pub column: u32,
}

/// A normalised range (`start <= end`). The cursor is carried separately, so a backwards
/// selection keeps its active end in [`TabDetach::cursor`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

/// Editor scroll offset in CSS pixels.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Scroll {
    pub top: f64,
    #[serde(default)]
    pub left: f64,
}

/// A point in screen coordinates (CSS pixels), e.g. where a tab was dropped.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ScreenPoint {
    pub x: f64,
    pub y: f64,
}

/// The project a window shows. `folder` is the value VS Code Web's `?folder=` takes
/// (an absolute path on the computer serving it).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workspace {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
}

/// webview → native: a tab was dragged out of the tab strip (FR-B1, FR-B2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TabDetach {
    pub source_window_id: String,
    #[serde(default)]
    pub workspace: Option<Workspace>,
    /// The editor's resource, e.g. `vscode-remote://host:8888/abs/path/file.rs`.
    pub file_uri: String,
    #[serde(default)]
    pub cursor: Option<Position>,
    #[serde(default)]
    pub scroll: Option<Scroll>,
    #[serde(default)]
    pub selection: Option<Range>,
    // ---- additive fields (FR-B4) ----
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screen: Option<ScreenPoint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Which editor produced it: `"vscode-web"` today; the Ember editor core later.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub editor: Option<String>,
    /// `"dataTransfer"` (VS Code's own drag data) or `"dom"` (DOM fallback).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_source: Option<String>,
    /// Epoch milliseconds at send time, for the NFR-B2 encode-to-receipt measurement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sent_at_ms: Option<f64>,
}

impl TabDetach {
    /// Encode-to-receipt latency (NFR-B2), given the receiver's clock in epoch ms.
    pub fn latency_ms(&self, now_ms: f64) -> Option<f64> {
        self.sent_at_ms.map(|sent| now_ms - sent)
    }
}

/// native → webview: another IDE window was closed (FR-B4 cross-window sync push).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SiblingWindowClosed {
    pub window_id: String,
}

/// A request to open a new IDE window at a project with editor state (FR-B3).
///
/// The native host produces it from a [`TabDetach`] and gives it to its window opener;
/// it may also arrive from a webview (an "open in new window" action without a drag).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenWindow {
    /// The new window's id. May be empty when a webview asks; the host assigns one.
    #[serde(default)]
    pub window_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_window_id: Option<String>,
    #[serde(default)]
    pub workspace: Option<Workspace>,
    #[serde(default)]
    pub file_uri: Option<String>,
    #[serde(default)]
    pub cursor: Option<Position>,
    #[serde(default)]
    pub selection: Option<Range>,
    #[serde(default)]
    pub scroll: Option<Scroll>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screen: Option<ScreenPoint>,
}

impl OpenWindow {
    pub fn from_detach(d: &TabDetach, window_id: impl Into<String>) -> Self {
        OpenWindow {
            window_id: window_id.into(),
            source_window_id: Some(d.source_window_id.clone()),
            workspace: d.workspace.clone(),
            file_uri: Some(d.file_uri.clone()),
            cursor: d.cursor,
            selection: d.selection,
            scroll: d.scroll,
            screen: d.screen,
        }
    }

    /// Path and query that make VS Code Web open the workspace with the file at the cursor,
    /// relative to the IDE window's origin (`/?folder=…&payload=…`). `None` without a folder.
    ///
    /// VS Code Web (`src/vs/code/browser/workbench/workbench.ts`, `WorkspaceProvider`) reads
    /// `folder` and a JSON `payload` of `[key, value]` pairs; `environmentService.ts` turns
    /// `openFile` into the file to open and, with `gotoLineMode` present, splits a 1-based
    /// `:line:column` suffix off the URI path. Only the cursor survives this way; selection
    /// range and scroll do not (the editor reveals the cursor instead).
    pub fn vscode_web_path(&self) -> Option<String> {
        let folder = self.workspace.as_ref()?.folder.as_deref()?;
        let mut q = format!("/?folder={}", encode_uri_component(folder));
        if let Some(uri) = &self.file_uri {
            let has_query_or_fragment = uri.contains(['?', '#']);
            let payload: Vec<[String; 2]> = match self.cursor {
                Some(c) if !has_query_or_fragment => vec![
                    [
                        "openFile".to_owned(),
                        format!("{uri}:{}:{}", c.line.saturating_add(1), c.column.saturating_add(1)),
                    ],
                    ["gotoLineMode".to_owned(), "true".to_owned()],
                ],
                _ => vec![["openFile".to_owned(), uri.clone()]],
            };
            let json = serde_json::to_string(&payload).expect("string pairs always serialize");
            q.push_str("&payload=");
            q.push_str(&encode_uri_component(&json));
        }
        Some(q)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum BridgeMessage {
    TabDetach(TabDetach),
    SiblingWindowClosed(SiblingWindowClosed),
    OpenWindow(OpenWindow),
}

impl BridgeMessage {
    pub fn kind(&self) -> &'static str {
        match self {
            BridgeMessage::TabDetach(_) => KIND_TAB_DETACH,
            BridgeMessage::SiblingWindowClosed(_) => KIND_SIBLING_WINDOW_CLOSED,
            BridgeMessage::OpenWindow(_) => KIND_OPEN_WINDOW,
        }
    }

    fn body(&self) -> Value {
        let v = match self {
            BridgeMessage::TabDetach(m) => serde_json::to_value(m),
            BridgeMessage::SiblingWindowClosed(m) => serde_json::to_value(m),
            BridgeMessage::OpenWindow(m) => serde_json::to_value(m),
        };
        v.expect("bridge message structs always serialize")
    }
}

/// The JSON string that crosses the bridge.
pub fn encode(msg: &BridgeMessage) -> String {
    let mut obj = Map::new();
    obj.insert("kind".to_owned(), Value::from(msg.kind()));
    if let Value::Object(body) = msg.body() {
        for (k, v) in body {
            obj.insert(k, v);
        }
    }
    Value::Object(obj).to_string()
}

/// What a received string turned out to be.
#[derive(Debug, Clone, PartialEq)]
pub enum Inbound {
    /// A known kind, decoded.
    Message(BridgeMessage),
    /// A kind this build does not know (e.g. from a newer peer). Already logged.
    UnknownKind { kind: String, raw: Value },
}

impl Inbound {
    /// The usable message, if any.
    pub fn message(&self) -> Option<&BridgeMessage> {
        match self {
            Inbound::Message(m) => Some(m),
            Inbound::UnknownKind { .. } => None,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("bridge message is not JSON: {0}")]
    NotJson(#[source] serde_json::Error),
    #[error("bridge message is not a JSON object with a string \"kind\"")]
    NoKind,
    #[error("bridge message {kind} has invalid fields: {source}")]
    Invalid {
        kind: String,
        #[source]
        source: serde_json::Error,
    },
}

fn decode_body(kind: &str, raw: &Value) -> Option<Result<BridgeMessage, serde_json::Error>> {
    let v = raw.clone();
    Some(match kind {
        KIND_TAB_DETACH => serde_json::from_value(v).map(BridgeMessage::TabDetach),
        KIND_SIBLING_WINDOW_CLOSED => serde_json::from_value(v).map(BridgeMessage::SiblingWindowClosed),
        KIND_OPEN_WINDOW => serde_json::from_value(v).map(BridgeMessage::OpenWindow),
        _ => return None,
    })
}

/// Decodes one received string. Unknown kinds are logged and kept, not dropped (FR-B2).
pub fn decode(text: &str) -> Result<Inbound, DecodeError> {
    let raw: Value = serde_json::from_str(text).map_err(DecodeError::NotJson)?;
    let kind = raw
        .get("kind")
        .and_then(Value::as_str)
        .ok_or(DecodeError::NoKind)?
        .to_owned();

    match decode_body(&kind, &raw) {
        Some(Ok(m)) => Ok(Inbound::Message(m)),
        Some(Err(source)) => {
            tracing::warn!(kind = %kind, error = %source, "ember-bridge: invalid message fields");
            Err(DecodeError::Invalid { kind, source })
        }
        None => {
            tracing::warn!(kind = %kind, "ember-bridge: unknown message kind; kept, not decoded");
            Ok(Inbound::UnknownKind { kind, raw })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detach() -> TabDetach {
        TabDetach {
            source_window_id: "win-a".into(),
            workspace: Some(Workspace { folder: Some("/Users/me/proj".into()) }),
            file_uri: "vscode-remote://localhost:8888/Users/me/proj/src/main.rs".into(),
            cursor: Some(Position { line: 41, column: 6 }),
            scroll: Some(Scroll { top: 1180.0, left: 0.0 }),
            selection: Some(Range {
                start: Position { line: 39, column: 0 },
                end: Position { line: 41, column: 6 },
            }),
            screen: None,
            label: Some("main.rs".into()),
            editor: Some("vscode-web".into()),
            state_source: Some("dataTransfer".into()),
            sent_at_ms: Some(1000.0),
        }
    }

    #[test]
    fn round_trips_every_kind() {
        let msgs = [
            BridgeMessage::TabDetach(detach()),
            BridgeMessage::SiblingWindowClosed(SiblingWindowClosed { window_id: "w2".into() }),
            BridgeMessage::OpenWindow(OpenWindow::from_detach(&detach(), "w3")),
        ];
        for m in msgs {
            let text = encode(&m);
            assert_eq!(decode(&text).unwrap(), Inbound::Message(m));
        }
    }

    #[test]
    fn encodes_the_architecture_shape() {
        let v: Value = serde_json::from_str(&encode(&BridgeMessage::TabDetach(detach()))).unwrap();
        assert_eq!(v["kind"], "tab_detach");
        assert_eq!(v["sourceWindowId"], "win-a");
        assert_eq!(v["fileUri"], "vscode-remote://localhost:8888/Users/me/proj/src/main.rs");
        assert_eq!(v["cursor"]["line"], 41);
        assert_eq!(v["cursor"]["column"], 6);
        assert_eq!(v["scroll"]["top"], 1180.0);
        assert_eq!(v["selection"]["start"]["line"], 39);
        // Unset additive fields are omitted, not null.
        assert!(v.get("screen").is_none());
    }

    #[test]
    fn decodes_what_detach_js_sends() {
        // Shape produced by detach.js buildDetachMessage (nulls included).
        let text = r#"{"kind":"tab_detach","sourceWindowId":"w","workspace":null,
            "fileUri":"vscode-remote://h/a.rs","cursor":null,"scroll":null,"selection":null,
            "screen":null,"label":null,"editor":"vscode-web","stateSource":"dom","sentAtMs":5.5}"#;
        match decode(text).unwrap() {
            Inbound::Message(BridgeMessage::TabDetach(d)) => {
                assert_eq!(d.file_uri, "vscode-remote://h/a.rs");
                assert_eq!(d.cursor, None);
                assert_eq!(d.latency_ms(8.0), Some(2.5));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn unknown_fields_are_ignored_including_a_stale_version() {
        let text = r#"{"kind":"sibling_window_closed","version":1,"windowId":"w","reason":"user"}"#;
        assert_eq!(
            decode(text).unwrap(),
            Inbound::Message(BridgeMessage::SiblingWindowClosed(SiblingWindowClosed {
                window_id: "w".into()
            }))
        );
    }

    #[test]
    fn unknown_kind_is_kept() {
        let text = r#"{"kind":"from_the_future","x":1}"#;
        match decode(text).unwrap() {
            Inbound::UnknownKind { kind, raw } => {
                assert_eq!(kind, "from_the_future");
                assert_eq!(raw["x"], 1);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn malformed_input_is_an_error() {
        assert!(matches!(decode("not json"), Err(DecodeError::NotJson(_))));
        assert!(matches!(decode("[1,2]"), Err(DecodeError::NoKind)));
        assert!(matches!(decode(r#"{"kind":5}"#), Err(DecodeError::NoKind)));
        assert!(matches!(
            decode(r#"{"kind":"tab_detach","sourceWindowId":"w"}"#),
            Err(DecodeError::Invalid { .. })
        ));
    }

    #[derive(Deserialize)]
    struct Vectors {
        cases: Vec<VectorCase>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct VectorCase {
        name: String,
        detach: Value,
        expected_path: Option<String>,
    }

    /// The same vectors ember/proxy/tests/js/detach.test.js checks buildWorkspaceQuery against.
    #[test]
    fn browser_url_matches_shared_vectors() {
        let vectors: Vectors =
            serde_json::from_str(include_str!("../../vectors/bridge/detach_vectors.json")).unwrap();
        assert!(!vectors.cases.is_empty());
        for case in vectors.cases {
            let inbound = decode(&case.detach.to_string()).unwrap();
            let Some(BridgeMessage::TabDetach(d)) = inbound.message().cloned() else {
                panic!("{}: not a tab_detach", case.name);
            };
            let req = OpenWindow::from_detach(&d, "new");
            assert_eq!(req.vscode_web_path(), case.expected_path, "{}", case.name);
        }
    }
}
