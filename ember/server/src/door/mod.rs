//! The door-agnostic code shared by the doors, moved without change out of `guest_door.rs`:
//! the startup error (here), the hardening pieces (`harden`) and the limit pieces (`limits`).
//! Step C0 moved the mixed items too (`serve`, `config`, the limiter and `limit_layer`, `guard_layer`).
//! `limits` still imports the guest door's constants from `crate::guest_door`: `LimitConfig::default`
//! and the sweep and strict-path code use them. Step C1 did not remove that import. A door other
//! than the guest door must build its `LimitConfig` field by field and use
//! `DoorLimits::with_strict_path` (the enrol door does).
//! The guest door keeps its constants, its configuration and everything that names the guest.

pub mod config;
pub mod harden;
pub mod limits;
pub mod serve;

pub use harden::with_timeout;
pub use limits::{addr_key, client_ip, TokenBucket};

/// A startup error of the door's configuration.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct DoorError(pub String);

pub(crate) fn err<T>(m: impl Into<String>) -> Result<T, DoorError> {
    Err(DoorError(m.into()))
}
