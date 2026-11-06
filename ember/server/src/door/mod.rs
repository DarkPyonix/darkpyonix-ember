//! The door-agnostic code shared by the doors, moved without change out of `guest_door.rs`:
//! the startup error (here), the hardening pieces (`harden`) and the limit pieces (`limits`).
//! Step C0 moved the mixed items too (`serve`, `config`, the limiter and `limit_layer`, `guard_layer`);
//! they still import the guest constants from `crate::guest_door` until step C1 removes that.
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
