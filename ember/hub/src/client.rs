//! Typed HTTP calls to the hub.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use ember_transport::PeerId;
use reqwest::{Method, RequestBuilder, Response, StatusCode};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::json;

use crate::paths;
use crate::types::{
    AddressRecord, ClaimOutcome, Device, DeviceApp, HubInfo, LinkCodeInfo, LinkInfo, LinkRequest, Me, PendingLink,
    Readmission,
};

/// The `code` of a `401` for a removed device's token (`hub.openapi.yaml` `Error.code`).
pub const CODE_DEVICE_REMOVED: &str = "device_removed";
/// The `code` of any other `401` (missing, unknown or expired credential).
pub const CODE_INVALID_CREDENTIALS: &str = "invalid_credentials";

/// How long `GET /config` may take before discovery falls back to the derived URLs.
const CONFIG_TIMEOUT: Duration = Duration::from_secs(5);
/// Added to a long-poll's `wait` for the request timeout (the hub answers `304` at `wait`).
const LONG_POLL_SLACK: Duration = Duration::from_secs(15);
/// The longest `?wait=` the hub accepts (FR-H9: `0..=25`, else `400`).
pub const MAX_WAIT: Duration = Duration::from_secs(25);

/// What can go wrong talking to the hub.
#[derive(Debug, thiserror::Error)]
pub enum HubError {
    /// `401` without a `code` (a hub older than the error codes). With a device token that
    /// worked before, the hub removed the device (tokens do not expire there): see
    /// [`HubError::is_revocation`].
    #[error("the hub did not accept this device's credentials (401): {0}")]
    Unauthorized(String),
    /// `401` with `code: device_removed`: the hub removed this device. Final: stop retrying and
    /// stop using the token (the owner may re-admit the key, FR-H11, after which it links again).
    #[error("the hub removed this device from the account: {0}")]
    DeviceRemoved(String),
    /// `401` with `code: invalid_credentials` (or another code): the credential is missing,
    /// unknown or expired, or a device token was sent where only a resolve token is accepted
    /// (`/pkarr?token=`). The hub does not say the device was removed. Not a revocation.
    #[error("the hub rejected the credentials (401): {0}")]
    InvalidCredentials(String),
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
    /// Any refused credential (`401`).
    pub fn is_unauthorized(&self) -> bool {
        matches!(self, HubError::Unauthorized(_) | HubError::DeviceRemoved(_) | HubError::InvalidCredentials(_))
    }

    /// The hub said explicitly that the device was removed (`device_removed`).
    pub fn is_device_removed(&self) -> bool {
        matches!(self, HubError::DeviceRemoved(_))
    }

    /// The device's registration is gone: `device_removed`, or a `401` without a `code` (only a
    /// hub older than v0.3.0's error codes sends one; there a token that worked before can only
    /// fail by removal). A `401` coded `invalid_credentials` is *not* a revocation.
    pub fn is_revocation(&self) -> bool {
        matches!(self, HubError::Unauthorized(_) | HubError::DeviceRemoved(_))
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
    #[serde(default)]
    code: Option<String>,
}

/// Result of a conditional (and possibly long-polled) `GET /devices`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DevicesPoll {
    /// `304`: the list still matches the `ETag` sent (or nothing changed within `wait`).
    NotModified,
    /// `200`: the current list, with its `ETag` if the hub sent one.
    Changed { devices: Vec<Device>, etag: Option<String> },
}

#[derive(Deserialize)]
struct DeviceList {
    devices: Vec<Device>,
}

#[derive(Deserialize)]
struct Claimed {
    device: Device,
    device_token: String,
    #[serde(default)]
    resolve_token: Option<String>,
}

#[derive(Deserialize)]
struct ResolveToken {
    resolve_token: String,
}

impl HubClient {
    /// A client for the hub at `base` (`https://api.darkpyonix.dev`), without a token.
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
        let (message, code) = match serde_json::from_str::<ErrorBody>(&text) {
            Ok(b) => (b.error, b.code),
            Err(_) => (text, None),
        };
        if code.as_deref() == Some(CODE_DEVICE_REMOVED) {
            return HubError::DeviceRemoved(message);
        }
        match status {
            StatusCode::UNAUTHORIZED if code.is_some() => HubError::InvalidCredentials(message),
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

    // ------------------------------------------------------------------ hub configuration

    /// `GET /config` (public, FR-H8): the hub's relays, directory, link page and API
    /// version. `Ok(None)` when the hub does not serve it (`404`, `405`, `501`: a hub older
    /// than the endpoint).
    pub async fn config(&self) -> Result<Option<HubInfo>, HubError> {
        let resp = Self::send(self.http.get(format!("{}{}", self.base, paths::CONFIG)).timeout(CONFIG_TIMEOUT)).await?;
        match resp.status() {
            StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED | StatusCode::NOT_IMPLEMENTED => Ok(None),
            _ => Self::json::<HubInfo>(resp).await.map(Some),
        }
    }

    // ------------------------------------------------------------------ device links (FR-H1)

    /// `POST /device-links`: ask to join an account. Unauthenticated.
    pub async fn create_link(&self, req: &LinkRequest) -> Result<PendingLink, HubError> {
        let resp = Self::send(self.http.post(format!("{}{}", self.base, paths::DEVICE_LINKS)).json(req)).await?;
        Self::json(resp).await
    }

    /// `POST /device-links/{link_id}/token` with the hex signature over
    /// [`crate::link_message`]. `403` (denied) and `404` (expired / claimed) are errors.
    pub async fn claim(&self, link_id: &str, signature_hex: &str) -> Result<ClaimOutcome, HubError> {
        let resp = Self::send(
            self.http
                .post(format!("{}{}", self.base, paths::device_link_token(link_id)))
                .json(&json!({ "signature": signature_hex })),
        )
        .await?;
        match resp.status() {
            StatusCode::ACCEPTED => Ok(ClaimOutcome::Pending),
            s if s.is_success() => {
                let c: Claimed = resp.json().await.map_err(|e| HubError::Decode(e.to_string()))?;
                Ok(ClaimOutcome::Approved { device: c.device, device_token: c.device_token, resolve_token: c.resolve_token })
            }
            _ => Err(Self::error_for(resp).await),
        }
    }

    /// `GET /device-links/{link_id}` (no credentials): the link's status, for a device that
    /// restarted while waiting. `404` once an expired link was deleted.
    pub async fn link_status(&self, link_id: &str) -> Result<LinkInfo, HubError> {
        let resp = Self::send(self.http.get(format!("{}{}", self.base, paths::device_link(&encode(link_id))))).await?;
        Self::json(resp).await
    }

    /// `GET /link-codes/{user_code}`: what a pending code would let in (account rights: a
    /// main server's token).
    pub async fn link_code(&self, user_code: &str) -> Result<LinkCodeInfo, HubError> {
        let resp = Self::send(self.authed(Method::GET, &paths::link_code(&encode(user_code)))?).await?;
        Self::json(resp).await
    }

    /// `POST /link-codes/{user_code}` `{approve}` (account rights). A device token gets
    /// `403` approving a link that asks for `main_server`, or a re-admitted key's link (FR-H11):
    /// those need a signed-in browser session. Denying them is allowed.
    pub async fn decide_link_code(&self, user_code: &str, approve: bool) -> Result<(), HubError> {
        let req = self
            .authed(Method::POST, &paths::link_code(&encode(user_code)))?
            .json(&json!({ "approve": approve }));
        Self::no_content(Self::send(req).await?).await
    }

    // ------------------------------------------------------------------ account and devices

    /// `GET /me`.
    pub async fn me(&self) -> Result<Me, HubError> {
        Self::json(Self::send(self.authed(Method::GET, paths::ME)?).await?).await
    }

    /// `POST /me/resolve-token` (device token in the header): a new read-only resolve token
    /// (`dpr_...`) for `GET /pkarr/{key}?token=` (NFR-H2). The previous one stops working at
    /// once.
    pub async fn rotate_resolve_token(&self) -> Result<String, HubError> {
        let t: ResolveToken = Self::json(Self::send(self.authed(Method::POST, paths::ME_RESOLVE_TOKEN)?).await?).await?;
        Ok(t.resolve_token)
    }

    /// `GET /devices`: the account's devices, oldest first (removed ones not listed).
    pub async fn devices(&self) -> Result<Vec<Device>, HubError> {
        let list: DeviceList = Self::json(Self::send(self.authed(Method::GET, paths::DEVICES)?).await?).await?;
        Ok(list.devices)
    }

    /// `GET /devices` with `If-None-Match: <etag>` and, with `wait` (capped at 25 s),
    /// `?wait=<seconds>` (FR-H9): the hub holds the request until the list's version differs
    /// from `etag` (`200`) or `wait` passes (`304`). It notices changes about every 2 s, and a
    /// waiting device that is removed gets `401 device_removed`. A hub older than FR-H9 answers
    /// `200` at once. See [`crate::DeviceWatcher`].
    pub async fn devices_since(&self, etag: Option<&str>, wait: Option<Duration>) -> Result<DevicesPoll, HubError> {
        let mut req = match wait {
            Some(w) => self
                .authed(Method::GET, &format!("{}?wait={}", paths::DEVICES, w.min(MAX_WAIT).as_secs().max(1)))?
                .timeout(w + LONG_POLL_SLACK),
            None => self.authed(Method::GET, paths::DEVICES)?,
        };
        if let Some(e) = etag {
            req = req.header(reqwest::header::IF_NONE_MATCH, e);
        }
        let resp = Self::send(req).await?;
        if resp.status() == StatusCode::NOT_MODIFIED {
            return Ok(DevicesPoll::NotModified);
        }
        let etag = resp
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let list: DeviceList = Self::json(resp).await?;
        Ok(DevicesPoll::Changed { devices: list.devices, etag })
    }

    /// `GET /devices/{endpoint_id}`.
    pub async fn device(&self, id: &PeerId) -> Result<Device, HubError> {
        Self::json(Self::send(self.authed(Method::GET, &paths::device(id))?).await?).await
    }

    /// `PATCH /devices/{endpoint_id}` `{name}`: rename (account rights, or the device's own
    /// token).
    pub async fn rename_device(&self, id: &PeerId, name: &str) -> Result<Device, HubError> {
        let req = self.authed(Method::PATCH, &paths::device(id))?.json(&json!({ "name": name }));
        Self::json(Self::send(req).await?).await
    }

    /// `PATCH /devices/{endpoint_id}` `{app}` (FR-H10): what the device runs; `None` clears
    /// it. Only the device's own token may set it (`403` otherwise).
    pub async fn set_app(&self, id: &PeerId, app: Option<&DeviceApp>) -> Result<Device, HubError> {
        let req = self.authed(Method::PATCH, &paths::device(id))?.json(&json!({ "app": app }));
        Self::json(Self::send(req).await?).await
    }

    /// `POST /devices/{endpoint_id}/readmit` (a signed-in session only; device tokens get
    /// `403`, FR-H11): for 15 minutes the removed key may link again, approved by a session.
    pub async fn readmit(&self, id: &PeerId) -> Result<Readmission, HubError> {
        Self::json(Self::send(self.authed(Method::POST, &paths::device_readmit(id))?).await?).await
    }

    /// `DELETE /devices/{endpoint_id}`: account rights, or the device's own token (a device
    /// leaving its account).
    pub async fn remove_device(&self, id: &PeerId) -> Result<(), HubError> {
        Self::no_content(Self::send(self.authed(Method::DELETE, &paths::device(id))?).await?).await
    }

    /// `GET /devices/{endpoint_id}/addresses`: the decoded address record.
    pub async fn addresses(&self, id: &PeerId) -> Result<AddressRecord, HubError> {
        Self::json(Self::send(self.authed(Method::GET, &paths::device_addresses(id))?).await?).await
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
        let c = HubClient::new("https://api.darkpyonix.dev/").with_token("dpd_secret");
        assert_eq!(c.base_url(), "https://api.darkpyonix.dev");
        assert!(!format!("{c:?}").contains("dpd_secret"));
        assert_eq!(encode(" bcdf-ghjk "), "bcdf-ghjk");
        assert_eq!(encode("a/b"), "a%2Fb");
    }
}
