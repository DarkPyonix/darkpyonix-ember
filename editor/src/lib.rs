//! ember-editor — the session layer of Ember's editor core (M8, SPEC FR-E1..E4,
//! `docs/design/EDITOR-SESSION.md`).
//!
//! It sits between [`ember_editor_conn`] (the Code-OSS server's management and extension-host
//! connections) and the dioxus-compose `CodeEditor` widget (compose-rust SPEC FR-38). It does not
//! depend on dioxus-compose: the widget's events and props are modelled as plain data in
//! [`widget`], and the UI layer translates between those and the real widget.
//!
//! | module | does |
//! | ------ | ---- |
//! | [`session`] | [`EditorSession`]: connect (version gate, management, extension host bootstrap), then one task that drives the core |
//! | [`engine`] | [`SessionCore`]: pure state machine, `Input` → `Effect`s (RPC, file I/O, UI updates) |
//! | [`document`] | one open file: widget versions ↔ extension-host versions, reset / reopen, save, BOM / EOL |
//! | [`diagnostics`] | FR-E2: `$changeMany` / `$clear` → underline decorations |
//! | [`codelens`] | FR-E3: `$provideCodeLenses` / `$resolveCodeLens` → line-above inlays; click → command |
//! | [`hover`] | FR-E3: `CodeHovered` → `$provideHover` → Markdown popup model |
//! | [`inline`] | FR-E4: `$provideInlineCompletions` with debounce + cancellation → ghost text; accept |
//! | [`registry`] | provider registrations (`$register…`) and extension-host commands |
//! | [`selector`] | `languages.score` port: document selector matching |
//! | [`languages`] | file → language id from `contributes.languages` |
//! | [`ids`] | stable, non-zero, session-unique decoration ids |
//! | [`coords`] | widget (0-based) ↔ extension host (1-based) positions, UTF-16 helpers, ranges through edits |
//! | [`replies`] | answers for the extension host's other requests |
//!
//! **Status: written without compiling** (no cargo was run for this change). See
//! `docs/design/EDITOR-SESSION.md` §7 for the APIs that are uncertain.

pub mod codelens;
pub mod coords;
pub mod engine;
pub mod diagnostics;
pub mod document;
pub mod hover;
pub mod ids;
pub mod inline;
pub mod languages;
pub mod registry;
pub mod replies;
pub mod selector;
pub mod session;
pub mod widget;

pub use crate::engine::{CoreOptions, Effect, ExternalChange, Input, SessionCore, SessionUpdate};
pub use crate::coords::{WidgetPos, WidgetRange};
pub use crate::session::{EditorSession, SessionConfig};
pub use crate::widget::{Decoration, DecorationId, DecorationKind, HoverPhase, Severity, WidgetCommand, WidgetEvent};

/// Errors from connecting or talking to a session.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Conn(#[from] ember_editor_conn::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    /// The management handshake answered something other than `{"type":"ok"}`.
    #[error("server refused the management connection: {0}")]
    Refused(String),
    /// The session task has ended.
    #[error("session closed")]
    Closed,
}
