//! ember-editor: the session layer of Ember's editor core (M8, SPEC FR-E1..E4,
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
//! | [`extensions`] | FR-L31.14: the list rows of a scan, and the scan without the disabled ids |
//! | [`languages`] | file → language id from `contributes.languages`, and the names for the language picker |
//! | [`text_format`] | FR-L31.7: line ending and encoding detection, decode / encode, the status bar's document format |
//! | [`textmate`] | FR-E6: the grammars extensions contribute (`contributes.grammars`), read into the raw rule model |
//! | [`ids`] | stable, non-zero, session-unique decoration ids |
//! | [`coords`] | widget (0-based) ↔ extension host (1-based) positions, UTF-16 helpers, ranges through edits |
//! | [`widget`] | the FR-38 `CodeEditor` wire contract (44-byte decoration records, events, commands) |
//! | [`bridge`] | [`WidgetBridge`]: `SessionUpdate` → per-document widget state and encoded `Decorations` bytes |
//! | [`fallback`] | connect-time version check outcome: when to offer "Open in VS Code" instead |
//! | [`replies`] | answers for the extension host's other requests |
//! | [`settings`] | FR-L31.17: JSONC, settings scopes and their merge, the settings Ember honours, editing one key, `keybindings.json` |
//! | [`search`] | FR-E7: search across files through the extension host, unsaved buffers, replace with re-check |
//! | [`trust`] | FR-L31.18: workspace trust answers, waiting requests, the grant |
//!
//! **Status: written without compiling** (no cargo was run for this change). See
//! `docs/design/EDITOR-SESSION.md` §7 for the APIs that are uncertain.

pub mod bridge;
pub mod codelens;
pub mod commands;
pub mod coords;
pub mod engine;
pub mod extensions;
pub mod fallback;
pub mod folding;
pub mod diagnostics;
pub mod paths;
pub mod output;
pub mod document;
pub mod highlight;
pub mod hover;
pub mod ids;
pub mod inline;
pub mod languages;
pub mod registry;
pub mod replies;
pub mod search;
pub mod selector;
pub mod semantic;
pub mod textmate;
pub mod text_format;
pub mod trust;
pub mod notifications;
pub mod session;
pub mod settings;
pub mod widget;

pub use crate::bridge::{apply_text_change, BridgeAction, DocumentWidgetState, SyntaxRuns, WidgetBridge};
pub use crate::folding::{compute_indent_folds, merge_folds, parse_provider_folds, FoldRange, FoldRegion};
pub use crate::semantic::{decode_semantic_tokens, encode_semantic_tokens_dto, overlay_semantic_runs, role_for_semantic_token};
pub use crate::commands::{parse_contributed_commands, ContributedCommand};
pub use crate::fallback::{fallback_for, vscode_fallback_url, ConnectOutcome, Fallback, FallbackTarget};
pub use crate::extensions::InstalledExtension;
pub use crate::engine::{CoreOptions, Effect, ExternalChange, Input, SessionCore, SessionUpdate};
pub use crate::languages::LanguageInfo;
pub use crate::text_format::{DocumentFormat, Encoding, EncodingMode, Eol};
pub use crate::coords::{WidgetPos, WidgetRange};
pub use crate::session::{DirLister, EditorSession, ServerSettingsFile, SessionConfig};
pub use crate::settings::{EditorSettings, SettingsLayer, SettingsScope, SettingsScopes};
pub use crate::output::{ExtensionMessage, MessageAction, MessageSeverity, OutputChannel, MAX_OUTPUT_CHANNEL_BYTES};
pub use crate::widget::{
    Decoration, DecorationId, DecorationKind, HoverPhase, Severity, WidgetCommand, WidgetEvent,
    DECORATION_RECORD_LEN,
};

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
    /// An explorer edit named a path or a name that is not allowed.
    #[error("{0}")]
    Path(crate::paths::PathError),
}
