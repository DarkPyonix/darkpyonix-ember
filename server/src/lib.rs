//! ember server — the Ember main server (docs/ARCHITECTURE.md §1).
//!
//! Runs agent CLIs headless, stores every session, and pushes updates to clients.

pub mod a2a;
pub mod accounts;
pub mod agents;
pub mod api;
pub mod browser;
pub mod chatgpt;
pub mod computers;
pub mod devices;
pub mod events;
pub mod history;
pub mod hub;
pub mod session;
pub mod store;
pub mod transport;
