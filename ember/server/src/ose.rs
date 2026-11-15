//! The OSE manager of the server's own machine (SPEC FR-W4).
//!
//! The code is `ember_node::ose`: `ember-node` runs the same manager for the projects of its
//! computer (SPEC FR-W4a, #896), and this crate depends on that one, not the reverse. Everything
//! is re-exported here, so `ember_server::ose::OseManager` and the rest keep their paths.

pub use ember_node::ose::*;
