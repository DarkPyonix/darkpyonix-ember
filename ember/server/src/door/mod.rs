//! The door-agnostic code shared by the doors, moved without change out of `guest_door.rs`:
//! the startup error (here), the hardening pieces (`harden`) and the limit pieces (`limits`).
//! The guest door keeps its constants, its configuration and everything that names the guest.

pub mod harden;
pub mod limits;

pub use harden::with_timeout;
pub use limits::{addr_key, client_ip, TokenBucket};

/// A startup error of the door's configuration.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct DoorError(pub String);

pub(crate) fn err<T>(m: impl Into<String>) -> Result<T, DoorError> {
    Err(DoorError(m.into()))
}
