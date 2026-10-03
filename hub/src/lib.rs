//! Ember's side of the darkpyonix.dev hub (SPEC `FR-N2`; hub contract:
//! `darkpyonix-core/docs/api/hub.openapi.yaml`, v0.3.0).
//!
//! - [`HubConfig`]: which hub (`EMBER_HUB_URL`, default `https://darkpyonix.dev`), its relay and
//!   its address directory, turned into a [`ember_transport::TransportConfig`].
//! - [`HubClient`]: typed calls for the part of the hub API Ember uses (device links, link codes,
//!   devices, addresses, `/v1/me`).
//! - [`DeviceLink`]: joining the user's GitHub account, OAuth-device-flow style: start a link,
//!   show the user code and verification URL, poll until approved, keep the device token.
//! - [`Registration`] and [`RegistrationFile`]: the stored result (ember node keeps it in a 0600
//!   file; ember server seals the token in its database).
//! - [`check_registration`] / [`watch_registration`]: notice that the hub removed this device.
//! - `fake` (feature `fake`): an in-process hub for tests.
//!
//! Address publishing and resolving are not HTTP calls made here: the hub's `/pkarr` endpoint is
//! spoken by the transport itself ([`ember_transport::HubDirectory`]), which signs and verifies
//! the records with the endpoint key.

mod client;
mod config;
mod link;
mod registration;
mod types;
pub mod z32;

#[cfg(feature = "fake")]
pub mod fake;

pub use client::{HubClient, HubError};
pub use config::{HubConfig, DEFAULT_HUB_URL, HUB_RELAY_URL_ENV, HUB_URL_ENV};
pub use link::{link_message, DeviceLink, LinkError, LinkPoll};
pub use registration::{
    check_registration, now_secs, watch_registration, Registration, RegistrationFile,
    RegistrationState, REGISTRATION_FILE,
};
pub use types::{AddressRecord, ClaimOutcome, Device, LinkCodeInfo, LinkRequest, Me, PendingLink, Role};
