//! ember server — the Ember main server (docs/ARCHITECTURE.md §1).
//!
//! Runs agent CLIs headless, stores every session, and pushes updates to clients.

pub mod a2a;
pub mod agents;
pub mod api;
pub mod events;
pub mod session;
pub mod store;
