//! Typed HTTP calls to the hub.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use ember_transport::PeerId;
use reqwest::{Method, RequestBuilder, Response, StatusCode};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::json;

use crate::types::{AddressRecord, ClaimOutcome, Device, LinkCodeInfo, LinkRequest, Me, PendingLink};

/// What can go wrong talking to the hub.
#[derive(Debug, thiserror::Error)]
pub enum HubError {
    /// `401`: no valid credential. With a device token that worked before, the hub removed the
    /// device (tokens do not expire): see [`crate::check_registration`].
    #[error("the hub did not accept this device's credentials (401): {0}")]
    Unauthorized(String),
    #[error("forbidden by the hub (403): {0}")]
    Forbidden(String),
    #[error("not found on the hub (404): {0}")]
    NotFound(String),
    #[error("conflict on the hub (409): {0}")]
    Conflict(String),
    #[error("rejected by the hub (400): {0}")]
    BadRequest(String),
    #[error("rate limited by the hub (429)")]
    RateLimited,
    #[error("hub answered {status}: {message}")]
    Status { status: u16, message: String },
    #[error("cannot reach the hub: {0}")]
    Unreachable(String),
    #[error("unexpected answer from the hub: {0}")]
    Decode(String),
    #[error("no device token: this device is not registered with the hub")]
    NotRegistered,
}

impl HubError {
    pub fn is_unauthorized(&self) -> bool {
        matches!(self, HubError::Unauthorized(_))
    }
}

/// A client for one hub, optionally carrying this device's token. Cheap to clone.
#[derive(Clone)]
pub struct HubClient {
    base: Arc<str>,
    http: reqwest::Client,
    token: Option<Arc<str>>,
}

impl fmt::Debug for HubClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HubClient")
            .field("base", &self.base)
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

#[derive(Deserialize)]
struct ErrorBody {
    error: String,
}

#[derive(Deserialize)]
struct DeviceList {
    devices: Vec<Device>,
}

#[derive(Deserialize)]
struct Claimed {
    device: Device,
    device_token: String,
}

impl HubClient {
    /// A client for the hub at `base` (`https://darkpyonix.dev`), without a token.
    pub fn new(base: &str) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .user_agent(concat!("ember/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("reqwest client builds");
        Self { base: base.trim_end_matches('/').into(), http, token: None }
    }

    /// The same hub, authenticated with a device token (`Authorization: Bearer`).
    pub fn with_token(&self, token: &str) -> Self {
        Self { token: Some(token.into()), ..self.clone() }
    }

    /// The same hub without a token (device-link calls are unauthenticated).
    pub fn without_token(&self) -> Self {
        Self { token: None, ..self.clone() }
    }

    pub fn base_url(&self) -> &str {
        &self.base
    }

    pub fn has_token(&self) -> bool {
        self.token.is_some()
    }

    fn request(&self, method: Method, path: &str) -> RequestBuilder {
        let req = self.http.request(method, format!("{}{path}", self.base));
        match &self.token {
            Some(t) => req.bearer_auth(t),
            None => req,
        }
    }

    fn authed(&self, method: Method, path: &str) -> Result<RequestBuilder, HubError> {
        if self.token.is_none() {
            return Err(HubError::NotRegistered);
        }
        Ok(self.request(method, path))
    }

    async fn send(req: RequestBuilder) -> Result<Response, HubError> {
        req.send().await.map_err(|e| HubError::Unreachable(e.to_string()))
    }

    /// Maps a non-success status to an error, reading the hub's `{"error": "..."}` body.
    async fn error_for(resp: Response) -> HubError {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        let message = serde_json::from_str::<ErrorBody>(&text).map(|b| b.error).unwrap_or(text);
        match status {
            StatusCode::UNAUTHORIZED => HubError::Unauthorized(message),
            StatusCode::FORBIDDEN => HubError::Forbidden(message),
            StatusCode::NOT_FOUND => HubError::NotFound(message),
            StatusCode::CONFLICT => HubError::Conflict(message),
            StatusCode::BAD_REQUEST => HubError::BadRequest(message),
            StatusCode::TOO_MANY_REQUESTS => HubError::RateLimited,
            s => HubError::Status { status: s.as_u16(), message },
        }
    }

    async fn json<T: DeserializeOwned>(resp: Response) -> Result<T, HubError> {
        if !resp.status().is_success() {
            return Err(Self::error_for(resp).await);
        }
        resp.json().await.map_err(|e| HubError::Decode(e.to_string()))
    }

    async fn no_content(resp: Response) -> Result<(), HubError> {
        if !resp.status().is_success() {
            return Err(Self::error_for(resp).await);
        }
        Ok(())
    }

    // ------------------------------------------------------------------ device links (FR-H1)

    /// `POST /v1/device-links`: ask to join an account. Unauthenticated.
    pub async fn create_link(&self, req: &LinkRequest) -> Result<PendingLink, HubError> {
        let resp = Self::send(self.http.post(format!("{}/v1/device-links", self.base)).json(req)).await?;
        Self::json(resp).await
    }

    /// `POST /v1/device-links/{link_id}/token` with the hex signature over
    /// [`crate::link_message`]. `403` (denied) and `404` (expired / claimed) are errors.
    pub async fn claim(&self, link_id: &str, signature_hex: &str) -> Result<ClaimOutcome, HubError> {
        let resp = Self::send(
            self.http
                .post(format!("{}/v1/device-links/{link_id}/token", self.base))
                .json(&json!({ "signature": signature_hex })),
        )
        .await?;
        match resp.status() {
            StatusCode::ACCEPTED => Ok(ClaimOutcome::Pending),
            s if s.is_success() => {
                let c: Claimed = resp.json().await.map_err(|e| HubError::Decode(e.to_string()))?;
                Ok(ClaimOutcome::Approved { device: c.device, device_token: c.device_token })
            }
            _ => Err(Self::error_for(resp).await),
        }
    }

    /// `GET /v1/link-codes/{user_code}`: what a pending code would let in (account rights: a
    /// main server's token).
    pub async fn link_code(&self, user_code: &str) -> Result<LinkCodeInfo, HubError> {
        let resp = Self::send(self.authed(Method::GET, &format!("/v1/link-codes/{}", encode(user_code)))?).await?;
        Self::json(resp).await
    }

    /// `POST /v1/link-codes/{user_code}` `{approve}` (account rights).
    pub async fn decide_link_code(&self, user_code: &str, approve: bool) -> Result<(), HubError> {
        let req = self
            .authed(Method::POST, &format!("/v1/link-codes/{}", encode(user_code)))?
            .json(&json!({ "approve": approve }));
        Self::no_content(Self::send(req).await?).await
    }

    // ------------------------------------------------------------------ account and devices

    /// `GET /v1/me`.
    pub async fn me(&self) -> Result<Me, HubError> {
        Self::json(Self::send(self.authed(Method::GET, "/v1/me")?).await?).await
    }

    /// `GET /v1/devices`: the account's devices, oldest first (removed ones not listed).
    pub async fn devices(&self) -> Result<Vec<Device>, HubError> {
        let list: DeviceList = Self::json(Self::send(self.authed(Method::GET, "/v1/devices")?).await?).await?;
        Ok(list.devices)
    }

    /// `GET /v1/devices/{endpoint_id}`.
    pub async fn device(&self, id: &PeerId) -> Result<Device, HubError> {
        Self::json(Self::send(self.authed(Method::GET, &format!("/v1/devices/{id}"))?).await?).await
    }

    /// `DELETE /v1/devices/{endpoint_id}` (account rights).
    pub async fn remove_device(&self, id: &PeerId) -> Result<(), HubError> {
        Self::no_content(Self::send(self.authed(Method::DELETE, &format!("/v1/devices/{id}"))?).await?).await
    }

    /// `GET /v1/devices/{endpoint_id}/addresses`: the decoded address record.
    pub async fn addresses(&self, id: &PeerId) -> Result<AddressRecord, HubError> {
        Self::json(Self::send(self.authed(Method::GET, &format!("/v1/devices/{id}/addresses"))?).await?).await
    }
}

/// Percent-encodes a path segment (user codes are `[A-Z]{4}-[A-Z]{4}`, but they are typed by
/// people).
fn encode(segment: &str) -> String {
    let mut out = String::new();
    for b in segment.trim().bytes() {
        if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_is_not_in_debug_output() {
        let c = HubClient::new("https://darkpyonix.dev/").with_token("dpd_secret");
        assert_eq!(c.base_url(), "https://darkpyonix.dev");
        assert!(!format!("{c:?}").contains("dpd_secret"));
        assert_eq!(encode(" bcdf-ghjk "), "bcdf-ghjk");
        assert_eq!(encode("a/b"), "a%2Fb");
    }
}
