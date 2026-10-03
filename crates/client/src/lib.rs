//! Ember native client core: everything below the UI (SPEC §L, PR-1, FR-S6).
//!
//! - [`wire`]: the main server's JSON types.
//! - [`api`]: HTTP client for `/api/v1`.
//! - [`push`]: push-message decoding (with version check) and reconnect backoff.
//! - [`transcript`] and [`state`]: the pure reducer a UI binds to: projects, sessions with
//!   launcher status (incl. finished-unread), transcripts, computers, connection state.
//! - [`cache`]: the cold-start snapshot file.
//! - [`client`]: the sync engine tying them together.
//!
//! A UI holds a [`Client`], reads with [`Client::read`], and redraws on
//! [`Client::subscribe`] / [`Client::watch_revision`].

pub mod api;
pub mod cache;
pub mod client;
pub mod push;
pub mod state;
pub mod transcript;
pub mod wire;

pub use client::{Client, ClientConfig};
pub use state::{Changes, ConnectionState, Input, LauncherStatus, State};
