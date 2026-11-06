//! TextMate grammars for syntax highlighting (FR-E6): the loader half.
//!
//! The editor tokenizes with grammars that installed extensions contribute (`contributes.grammars`);
//! none is bundled. This module reads them: [`catalog`] lists which extension offers which grammar
//! (scope name, language id, file, injection targets) from the `scanExtensions` descriptions, and
//! [`grammar`] parses one grammar file into the raw rule model. The interpreter that runs the rules
//! builds on this model: [`interpreter`] compiles grammars and tokenizes a line at a time.

pub mod catalog;
pub mod grammar;
pub mod interpreter;

pub use catalog::{GrammarCatalog, GrammarEntry};
pub use interpreter::{Grammar, GrammarSet, LineTokens, Scope, StateStack, Token};
pub use grammar::{parse_grammar, read_grammar, GrammarError, RawGrammar, RawRule, MAX_GRAMMAR_BYTES, MAX_RULES};
