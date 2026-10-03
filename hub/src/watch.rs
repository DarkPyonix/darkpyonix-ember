//! Following the account's device list: long-poll when the hub advertises it, a timed
//! conditional poll otherwise.

use std::time::{Duration, Instant};

use ember_transport::PeerId;

use crate::client::{DevicesPoll, HubClient, HubError};
use crate::types::{Device, HubInfo};

/// How long one long-poll asks the hub to hold `GET /v1/devices` (`?wait=`). Below the 30 s
/// many proxies and the Workers runtime are comfortable with.
pub const DEFAULT_WAIT: Duration = Duration::from_secs(25);

/// A long-poll answered faster than this without a change counts as "the hub ignored `wait`".
const EARLY: Duration = Duration::from_millis(500);
/// After this many early answers in a row the watcher falls back to polling.
const EARLY_LIMIT: u32 = 3;
/// Cap of the retry delay after errors in long-poll mode.
const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// Follows `GET /v1/devices` for one credential. [`DeviceWatcher::next`] returns the list the
/// first time, then again each time it changes.
///
/// - **Long-poll** (the hub advertises it, [`HubInfo::supports_devices_wait`]):
///   `GET /v1/devices?wait=25` with `If-None-Match`; the hub answers when the list changes, so a
///   removal is seen within a round trip. A hub that turns out to ignore `wait` (answers
///   immediately without a change, three times in a row) or rejects it (`400`) is polled instead.
/// - **Poll** (fallback): one conditional `GET /v1/devices` every `period` (60 s in ember server
///   and node); a `304` costs no body.
///
/// Errors are returned to the caller (who decides what a revocation means for it); the next call
/// then waits before asking again, so a caller can loop on `next` without spinning.
#[derive(Debug)]
pub struct DeviceWatcher {
    client: HubClient,
    wait: Option<Duration>,
    period: Duration,
    etag: Option<String>,
    last: Option<Vec<Device>>,
    failures: u32,
    early: u32,
    asked_once: bool,
    self_id: Option<PeerId>,
}

impl DeviceWatcher {
    /// A watcher for `client`'s view of the account. Long-polls when `info` advertises it;
    /// otherwise polls every `period`.
    pub fn new(client: HubClient, info: Option<&HubInfo>, period: Duration) -> Self {
        let wait = info.is_some_and(HubInfo::supports_devices_wait).then_some(DEFAULT_WAIT);
        Self { client, wait, period, etag: None, last: None, failures: 0, early: 0, asked_once: false, self_id: None }
    }

    /// The long-poll `wait` (tests use a short one). Ignored in poll mode.
    pub fn with_wait(mut self, wait: Duration) -> Self {
        if self.wait.is_some() {
            self.wait = Some(wait.max(Duration::from_secs(1)));
        }
        self
    }

    /// The device this watcher's token belongs to: a list without it is treated as its removal
    /// ([`HubError::DeviceRemoved`]), for a hub that answers the waiting request with the new
    /// list before refusing the token.
    pub fn expecting(mut self, id: PeerId) -> Self {
        self.self_id = Some(id);
        self
    }

    pub fn is_long_poll(&self) -> bool {
        self.wait.is_some()
    }

    /// The `ETag` of the last list seen.
    pub fn etag(&self) -> Option<&str> {
        self.etag.as_deref()
    }

    fn backoff(&self) -> Duration {
        if self.wait.is_none() {
            return self.period;
        }
        let exp = Duration::from_secs(1u64 << self.failures.min(5));
        exp.min(MAX_BACKOFF).min(self.period.max(Duration::from_millis(10)))
    }

    /// Waits until the device list differs from the last one returned (the first call returns
    /// the current list at once) and returns it. An error is returned as soon as it happens.
    pub async fn next(&mut self) -> Result<Vec<Device>, HubError> {
        loop {
            if self.failures > 0 {
                tokio::time::sleep(self.backoff()).await;
            } else if self.asked_once && self.wait.is_none() {
                tokio::time::sleep(self.period).await;
            }
            // The first request is not held: the caller wants the current list now.
            let wait = if self.asked_once { self.wait } else { None };
            let started = Instant::now();
            let answer = self.client.devices_since(self.etag.as_deref(), wait).await;
            self.asked_once = true;
            let quick = wait.is_some() && started.elapsed() < EARLY;
            match answer {
                Ok(DevicesPoll::NotModified) => {
                    self.failures = 0;
                    if quick {
                        self.note_early();
                    } else {
                        self.early = 0;
                    }
                }
                Ok(DevicesPoll::Changed { devices, etag }) => {
                    self.failures = 0;
                    if let Some(me) = self.self_id {
                        if !devices.iter().any(|d| d.endpoint_id == me) {
                            self.failures = 1;
                            return Err(HubError::DeviceRemoved("this device is no longer in the account's device list".into()));
                        }
                    }
                    self.etag = etag;
                    if self.last.as_ref() == Some(&devices) {
                        // A hub without ETag (or one ignoring If-None-Match) sent the same list.
                        if quick {
                            self.note_early();
                        }
                        continue;
                    }
                    self.early = 0;
                    self.last = Some(devices.clone());
                    return Ok(devices);
                }
                Err(HubError::BadRequest(m)) if wait.is_some() => {
                    tracing::info!(hub = %self.client.base_url(), "the hub refused ?wait= ({m}); polling the device list instead");
                    self.wait = None;
                }
                Err(e) => {
                    self.failures = self.failures.saturating_add(1);
                    return Err(e);
                }
            }
        }
    }

    /// At most [`EARLY_LIMIT`] back-to-back requests before falling back to polling.
    fn note_early(&mut self) {
        self.early += 1;
        if self.early >= EARLY_LIMIT && self.wait.is_some() {
            tracing::info!(hub = %self.client.base_url(), "the hub answers ?wait= at once; polling the device list instead");
            self.wait = None;
        }
    }
}
