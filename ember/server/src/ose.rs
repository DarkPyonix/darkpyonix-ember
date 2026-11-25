//! The OSE command line of the server's own machine: the local computer's extension installs and
//! uninstalls (SPEC FR-W4, FR-L31.14).
//!
//! The code is `ember_node::ose`, re-exported here so `ember_server::ose::OseManager` and the rest
//! keep their paths. No OSE is started here: the editor server is the DarkPyonix manager's
//! (`INTENT.md` D55, #1064), reached through `ember_node::editor_manager`.

pub use ember_node::ose::*;
