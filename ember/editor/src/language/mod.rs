//! FR-L31.13: language features beyond diagnostics, hover data, CodeLens and inline completions.
//!
//! The extension host registers a provider per feature and document selector
//! ([`ember_editor_conn::language`], kept in [`crate::registry`]). The window asks for a feature
//! with a [`LanguageRequest`]; the session core asks the matching providers and answers with
//! [`LanguageUpdate`]s. Edits (an accepted completion, formatting, a rename, a code action) reach an
//! open document as `EditCode` commands for its widget, so they join the reader's undo history and
//! come back as ordinary changes; a file that is not open is read, edited and written.
//!
//! | module | does |
//! | ------ | ---- |
//! | [`completion`] | `ISuggestResultDto` → items, the list's filter, snippet text |
//! | [`edits`] | `TextEdit[]` and `IWorkspaceEditDto` → edits per file, applied to a text |
//! | [`navigation`] | definition and reference locations, document symbols and the path at a position |
//! | [`assist`] | code actions, signature help, where a rename starts |
//!
//! What differs from Code-OSS is listed in `docs/design/EDITOR-PARITY.md` (row "Language features").

pub mod assist;
pub mod completion;
pub mod edits;
pub mod navigation;

use ember_editor_conn::uri::UriComponents;

use crate::coords::{WidgetPos, WidgetRange};

pub use assist::{CodeAction, Parameter, RenameLocation, Signature, SignatureHelp};
pub use completion::{CompletionItem, CompletionList};
pub use edits::{EditSummary, TextEdit, WorkspaceEdit};
pub use navigation::{DocumentSymbol, Location};

/// The features of this module, for saying which one an update is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Feature {
    Completion,
    Definition,
    References,
    Format,
    Rename,
    CodeActions,
    SignatureHelp,
    DocumentSymbols,
}

/// What the window asks of the session. A position left out is the document's caret as the session
/// last heard of it.
#[derive(Debug, Clone, PartialEq)]
pub enum LanguageRequest {
    /// Ask the completion providers. `trigger` is the character that was typed, when one started it.
    Complete { uri: UriComponents, pos: Option<WidgetPos>, trigger: Option<String> },
    /// Insert item `index` of the last [`LanguageUpdate::Completions`].
    AcceptCompletion { uri: UriComponents, index: usize },
    /// The list was closed without choosing.
    CancelCompletion { uri: UriComponents },
    Definition { uri: UriComponents, pos: Option<WidgetPos> },
    References { uri: UriComponents, pos: Option<WidgetPos> },
    /// Format the document, or `range` of it.
    Format { uri: UriComponents, range: Option<WidgetRange> },
    /// Ask where a rename at the position starts; answered with `RenameReady` or `RenameRejected`.
    PrepareRename { uri: UriComponents, pos: Option<WidgetPos> },
    Rename { uri: UriComponents, pos: WidgetPos, new_name: String },
    /// Ask for the code actions of `range` (the caret when left out).
    CodeActions { uri: UriComponents, range: Option<WidgetRange> },
    /// Run action `index` of the last [`LanguageUpdate::CodeActions`].
    ApplyCodeAction { uri: UriComponents, index: usize },
    DismissCodeActions { uri: UriComponents },
    SignatureHelp { uri: UriComponents, pos: Option<WidgetPos>, trigger: Option<String> },
    DismissSignatureHelp { uri: UriComponents },
    DocumentSymbols { uri: UriComponents },
}

impl LanguageRequest {
    pub fn uri(&self) -> &UriComponents {
        match self {
            Self::Complete { uri, .. }
            | Self::AcceptCompletion { uri, .. }
            | Self::CancelCompletion { uri }
            | Self::Definition { uri, .. }
            | Self::References { uri, .. }
            | Self::Format { uri, .. }
            | Self::PrepareRename { uri, .. }
            | Self::Rename { uri, .. }
            | Self::CodeActions { uri, .. }
            | Self::ApplyCodeAction { uri, .. }
            | Self::DismissCodeActions { uri }
            | Self::SignatureHelp { uri, .. }
            | Self::DismissSignatureHelp { uri }
            | Self::DocumentSymbols { uri } => uri,
        }
    }
}

/// What the session answers.
#[derive(Debug, Clone, PartialEq)]
pub enum LanguageUpdate {
    /// No provider of `feature` is registered for the document.
    Unavailable { uri: UriComponents, feature: Feature },
    /// The provider failed.
    Failed { uri: UriComponents, feature: Feature, error: String },
    /// The completion list so far, written against document `version` with the caret at `pos`.
    /// `complete` is false while a provider has not answered yet; each update replaces the last.
    Completions { uri: UriComponents, version: u32, pos: WidgetPos, items: Vec<CompletionItem>, incomplete: bool, complete: bool },
    /// The accepted item's edits were sent to the widget.
    CompletionApplied { uri: UriComponents },
    /// Definitions or references of the symbol at `origin`. An empty list means none was found.
    Locations { uri: UriComponents, feature: Feature, origin: WidgetPos, locations: Vec<Location> },
    /// Formatting sent `edits` edits to the widget (0: the document was already formatted).
    Formatted { uri: UriComponents, edits: usize },
    /// A rename can start: the name's range and the text to offer.
    RenameReady { uri: UriComponents, pos: WidgetPos, range: WidgetRange, placeholder: String },
    /// The provider refuses to rename here (its reason when it gave one).
    RenameRejected { uri: UriComponents, reason: Option<String> },
    /// A workspace edit (a rename, a code action, an extension's `workspace.applyEdit`) was applied.
    WorkspaceEditApplied { feature: Feature, summary: EditSummary },
    /// The code actions so far; `complete` as for completions.
    CodeActions { uri: UriComponents, version: u32, actions: Vec<CodeAction>, complete: bool },
    /// The parameter hint to show, or `None` to hide it.
    SignatureHelp { uri: UriComponents, help: Option<SignatureHelp> },
    /// The symbols of the document as it stood at `version`.
    DocumentSymbols { uri: UriComponents, version: u32, symbols: Vec<DocumentSymbol> },
}
