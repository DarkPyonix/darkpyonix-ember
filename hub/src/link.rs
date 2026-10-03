//! Joining the user's account: the device-link flow (hub `FR-H1`), the shape of the OAuth
//! device authorization grant (RFC 8628) with proof of key possession added.
//!
//! 1. [`DeviceLink::start`]: `POST /v1/device-links {endpoint_id, name, role}` returns a user
//!    code, a verification URL and a challenge.
//! 2. The person opens the verification URL (signed in with GitHub) and approves the code — or
//!    types the code into their ember server, which approves it with its own device token.
//! 3. [`DeviceLink::poll`] / [`DeviceLink::wait`]: the device signs
//!    `darkpyonix-hub/v2/link\n<link_id>\n<challenge>` with its endpoint key and posts it every
//!    `interval` seconds until it is approved (device token, shown once), denied or expired.

use std::time::Duration;

use ember_transport::SecretKey;

use crate::client::{HubClient, HubError};
use crate::registration::{now_secs, Registration};
use crate::types::{ClaimOutcome, LinkRequest, PendingLink, Role};

/// The bytes signed to claim a link.
pub fn link_message(link_id: &str, challenge: &str) -> Vec<u8> {
    format!("darkpyonix-hub/v2/link\n{link_id}\n{challenge}").into_bytes()
}

/// Why a link did not produce a registration.
#[derive(Debug, thiserror::Error)]
pub enum LinkError {
    #[error("the link was denied")]
    Denied,
    #[error("the link expired before it was approved (user code {0})")]
    Expired(String),
    #[error("this endpoint is already registered with the hub (or was removed; removed keys are not reused): {0}")]
    AlreadyRegistered(String),
    #[error(transparent)]
    Hub(#[from] HubError),
}

/// One poll's outcome.
#[derive(Debug)]
pub enum LinkPoll {
    Pending,
    Approved(Registration),
}

/// A started link, waiting for approval.
#[derive(Debug)]
pub struct DeviceLink {
    client: HubClient,
    key: SecretKey,
    pending: PendingLink,
    /// Overrides the hub's `interval` (tests).
    poll_interval: Option<Duration>,
}

impl DeviceLink {
    /// Asks the hub to let this endpoint (`key`) join an account as `name` / `role`.
    pub async fn start(client: &HubClient, key: &SecretKey, name: &str, role: Role) -> Result<Self, LinkError> {
        let req = LinkRequest { endpoint_id: key.peer_id(), name: name.trim().to_string(), role };
        let pending = client.create_link(&req).await.map_err(|e| match e {
            HubError::Conflict(m) => LinkError::AlreadyRegistered(m),
            e => LinkError::Hub(e),
        })?;
        tracing::info!(user_code = %pending.user_code, url = %pending.verification_uri_complete, "hub device link started");
        Ok(Self { client: client.without_token(), key: key.clone(), pending, poll_interval: None })
    }

    /// Resumes polling a link started earlier (e.g. ember server after a restart).
    pub fn resume(client: &HubClient, key: &SecretKey, pending: PendingLink) -> Self {
        Self { client: client.without_token(), key: key.clone(), pending, poll_interval: None }
    }

    pub fn with_poll_interval(mut self, interval: Duration) -> Self {
        self.poll_interval = Some(interval);
        self
    }

    /// What to show the person: the user code and where to enter it.
    pub fn pending(&self) -> &PendingLink {
        &self.pending
    }

    pub fn user_code(&self) -> &str {
        &self.pending.user_code
    }

    pub fn is_expired(&self) -> bool {
        now_secs() > self.pending.expires_at
    }

    fn interval(&self) -> Duration {
        self.poll_interval.unwrap_or_else(|| Duration::from_secs(self.pending.interval.max(1)))
    }

    /// One claim attempt.
    pub async fn poll(&self) -> Result<LinkPoll, LinkError> {
        let sig = self.key.sign(&link_message(&self.pending.link_id, &self.pending.challenge));
        match self.client.claim(&self.pending.link_id, &hex::encode(sig)).await {
            Ok(ClaimOutcome::Pending) => Ok(LinkPoll::Pending),
            Ok(ClaimOutcome::Approved { device, device_token }) => {
                if device.endpoint_id != self.key.peer_id() {
                    return Err(HubError::Decode(format!(
                        "hub registered {} instead of this endpoint {}",
                        device.endpoint_id,
                        self.key.peer_id()
                    ))
                    .into());
                }
                tracing::info!(name = %device.name, role = %device.role, "registered with the hub");
                Ok(LinkPoll::Approved(Registration {
                    hub_url: self.client.base_url().to_string(),
                    device,
                    device_token,
                    registered_at: now_secs(),
                    revoked_at: None,
                }))
            }
            Err(HubError::Forbidden(_)) => Err(LinkError::Denied),
            Err(HubError::NotFound(_)) => Err(LinkError::Expired(self.pending.user_code.clone())),
            Err(HubError::Conflict(m)) => Err(LinkError::AlreadyRegistered(m)),
            Err(e) => Err(e.into()),
        }
    }

    /// Polls every `interval` until approved, denied or expired. Transient network errors are
    /// retried until the link expires.
    pub async fn wait(&self) -> Result<Registration, LinkError> {
        let mut interval = self.interval();
        loop {
            match self.poll().await {
                Ok(LinkPoll::Approved(r)) => return Ok(r),
                Ok(LinkPoll::Pending) => {}
                Err(LinkError::Hub(HubError::RateLimited)) => interval = (interval * 2).min(Duration::from_secs(60)),
                Err(LinkError::Hub(e @ (HubError::Unreachable(_) | HubError::Status { .. }))) => {
                    tracing::debug!(error = %e, "hub link poll failed; retrying");
                }
                Err(e) => return Err(e),
            }
            if self.is_expired() {
                return Err(LinkError::Expired(self.pending.user_code.clone()));
            }
            tokio::time::sleep(interval).await;
        }
    }
}
