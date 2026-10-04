//! The IDE window bridge (SPEC §B, ARCHITECTURE.md §3, INTENT.md D11).
//!
//! The webview side is `ember/proxy/static/detach.js`, injected into VS Code Web. It posts
//! JSON messages through the platform's own webview message API
//! (`WKScriptMessageHandler` named [`HANDLER_NAME`] on macOS/iOS, WebView2 `postMessage` on
//! Windows); the native shell hands the received string to this crate.
//!
//! - [`messages`]: the wire types and `encode`/`decode`; unknown kinds are logged and
//!   kept, not dropped (FR-B2).
//! - [`bridge`]: the [`WebviewBridge`] trait every platform implements, plus a
//!   [`LoopbackBridge`] for tests. No platform webview implementation lives here yet.
//! - [`host`]: what the native host does with a message: it turns a `tab_detach` into an
//!   `open_window` request for a [`WindowOpener`] (FR-B3) and pushes
//!   `sibling_window_closed` back into webviews (FR-B4).
//!
//! The same message schema is meant to be reused by the Ember editor core later; only the
//! `editor` field (`"vscode-web"` today) changes.

pub mod bridge;
pub mod host;
pub mod messages;
mod url;

pub use bridge::{BridgeError, LoopbackBridge, WebviewBridge};
pub use host::{notify_sibling_closed, BridgeHost, HostOutcome, WindowOpener};
pub use messages::{
    decode, encode, BridgeMessage, DecodeError, Inbound, OpenWindow, Position,
    Range, ScreenPoint, Scroll, SiblingWindowClosed, TabDetach, Workspace, HANDLER_NAME,
};
