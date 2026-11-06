//! TextMate grammars for syntax highlighting (FR-E6): the loader half.
//!
//! The editor tokenizes with grammars that installed extensions contribute (`contributes.grammars`);
//! none is bundled. This module reads them: [`catalog`] lists which extension offers which grammar
//! (scope name, language id, file, injection targets) from the `scanExtensions` descriptions, and
//! [`grammar`] parses one grammar file into the raw rule model. The interpreter that runs the rules
//! builds on this model and comes in a later change.

pub mod catalog;
pub mod grammar;

pub use catalog::{GrammarCatalog, GrammarEntry};
pub use grammar::{parse_grammar, GrammarError, RawGrammar, RawRule};
