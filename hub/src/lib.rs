//! Ember's side of the darkpyonix.dev hub (SPEC `FR-N2`; hub contract:
//! `darkpyonix-core/docs/api/hub.openapi.yaml` v0.3.0 with the FR-H1/H8–H11/NFR-H2 additions of
//! darkpyonix-core PR #34, branch `feat/m4-hub-ember-gaps`).
//!
//! - [`HubConfig`]: which hub (`EMBER_HUB_URL`, default `https://hub.darkpyonix.dev`), its relay and
//!   its address directory, turned into a [`ember_transport::TransportConfig`].
//! - [`HubClient`]: typed calls for the part of the hub API Ember uses (device links, link codes,
//!   devices, addresses, `/v1/me`).
//! - [`DeviceLink`]: joining the user's GitHub account, OAuth-device-flow style: start a link,
//!   show the user code and verification URL, poll until approved, keep the device token.
//! - [`Registration`] and [`RegistrationFile`]: the stored result (ember node keeps it in a 0600
//!   file; ember server seals the token in its database).
//! - [`check_registration`] / [`watch_registration`] / [`watch_registration_with`]: notice that
//!   the hub removed this device (`401 device_removed`), by long-poll when the hub offers it.
//! - [`ensure_resolve_token`]: the read-only `dpr_` token the address resolver puts in its URL
//!   (device tokens are refused in URLs, NFR-H2).
//! - [`DeviceWatcher`]: follow the account's device list (`ETag` + `?wait=` long-poll, or a
//!   timed conditional poll as the fallback).
//! - [`HubConfig::discover`]: the relay and directory from `GET /v1/config`, falling back to
//!   `https://relay.<host>` and `<hub>/pkarr` when the hub does not serve it.
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
mod watch;
pub mod z32;

#[cfg(feature = "fake")]
pub mod fake;

pub use client::{DevicesPoll, HubClient, HubError, CODE_DEVICE_REMOVED, CODE_INVALID_CREDENTIALS, MAX_WAIT};
pub use config::{HubConfig, RelaySource, DEFAULT_HUB_URL, HUB_RELAY_URL_ENV, HUB_URL_ENV};
pub use link::{link_message, DeviceLink, LinkError, LinkPoll};
pub use registration::{
    check_registration, ensure_resolve_token, now_secs, watch_registration, watch_registration_with, Registration,
    RegistrationFile,
    RegistrationState, REGISTRATION_FILE,
};
pub use types::{
    service_label, AddressRecord, ClaimOutcome, Device, DeviceApp, HubInfo, LinkCodeInfo, LinkInfo, LinkRequest,
    LinkStatus, Me, PendingLink, Readmission, Role, LONG_POLL_API_VERSION,
};
pub use watch::{DeviceWatcher, DEFAULT_WAIT};
