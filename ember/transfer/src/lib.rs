//! Ember file transfer between the user's devices (SPEC `FR-F1`..`FR-F10`, `INTENT.md` D24).
//!
//! A device-side library. Peers talk only through `ember-transport` streams (`FR-F6`); the Ember
//! server is not in the path and stores no offer metadata.
//!
//! **First unit (status: written, not compiled).** It covers the offer handshake only:
//!
//! - [`names`]: file and folder name cleaning, never overwriting (`FR-F9`, `FR-F8`).
//! - [`limits`]: the per-offer size limit (`FR-F10`).
//! - [`mode`]: the receive mode and its decision (`FR-F9`).
//! - [`state`]: the transfer state machine with an injected clock (`FR-F3`, `FR-F4`).
//! - [`wire`]: the versioned, length-prefixed messages Offer, Accept, Decline and Cancel.
//! - [`session`]: sending an offer and answering one over a transport stream.
//!
//! **No file bytes move in this unit.** Chunk streaming, resume, hashing and progress are the next
//! unit. The app's `TransferService` trait can be implemented on top of [`session`] later.

pub mod limits;
pub mod mode;
pub mod names;
pub mod received_dir;
pub mod session;
pub mod state;
pub mod store;
pub mod wire;

/// The transport service name a device listens on for transfer offers.
pub const SERVICE: &str = "ember-transfer/1";

pub use ember_transport::PeerId;
