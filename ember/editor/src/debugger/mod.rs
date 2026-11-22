//! Debugging, the session side: the parts that need no session state.
//!
//! [`launch`] reads `.vscode/launch.json` (JSONC) and substitutes the variables Ember can answer;
//! [`breakpoints`] is the registry of source, function and exception breakpoints and builds the
//! DAP `setBreakpoints` family of arguments from it.

pub mod breakpoints;
pub mod launch;
