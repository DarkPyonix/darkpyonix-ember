//! HTTP client for the main server's `/api/v1` (see `server/src/api/mod.rs`).

use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::json;

use crate::wire::{ApprovalDecision, DetectedAgent, NewSession, SessionDetail, SessionRecord, StoredEvent};

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("request failed: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("server answered {status}: {message}")]
    Status { status: u16, message: String },
    #[error("invalid base url {0:?}")]
    BadUrl(String),
}

impl ApiError {
    pub fn status(&self) -> Option<u16> {
        match self {
            ApiError::Status { status, .. } => Some(*status),
            _ => None,
        }
    }
}

pub type ApiResult<T> = Result<T, ApiError>;

/// `GET /health`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Health {
    pub ok: bool,
    pub push_version: u32,
}

/// `POST /sessions/{id}/lease` answer.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Lease {
    pub ttl_secs: u64,
}

/// A main server's HTTP API. Cheap to clone.
#[derive(Debug, Clone)]
pub struct Api {
    http: reqwest::Client,
    /// e.g. `http://127.0.0.1:8740`, no trailing slash.
    base: String,
}

impl Api {
    pub fn new(base_url: &str) -> ApiResult<Api> {
        let base = base_url.trim_end_matches('/').to_string();
        if !(base.starts_with("http://") || base.starts_with("https://")) {
            return Err(ApiError::BadUrl(base_url.to_string()));
        }
        Ok(Api { http: reqwest::Client::new(), base })
    }

    pub fn base_url(&self) -> &str {
        &self.base
    }

    /// The push WebSocket URL (`ws(s)://…/api/v1/push`).
    pub fn push_url(&self) -> String {
        let rest = self.base.strip_prefix("http").unwrap_or(&self.base);
        format!("ws{rest}/api/v1/push")
    }

    fn url(&self, path: &str) -> String {
        format!("{}/api/v1{path}", self.base)
    }

    async fn check(res: reqwest::Response) -> ApiResult<reqwest::Response> {
        let status = res.status();
        if status.is_success() {
            return Ok(res);
        }
        let body = res.text().await.unwrap_or_default();
        let message = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
            .unwrap_or(body);
        Err(ApiError::Status { status: status.as_u16(), message })
    }

    async fn get<T: DeserializeOwned>(&self, path: &str) -> ApiResult<T> {
        let res = Self::check(self.http.get(self.url(path)).send().await?).await?;
        Ok(res.json().await?)
    }

    async fn post(&self, path: &str, body: serde_json::Value) -> ApiResult<reqwest::Response> {
        Self::check(self.http.post(self.url(path)).json(&body).send().await?).await
    }

    pub async fn health(&self) -> ApiResult<Health> {
        self.get("/health").await
    }

    pub async fn agents(&self) -> ApiResult<Vec<DetectedAgent>> {
        self.get("/agents").await
    }

    /// All sessions, or one project's, most recently updated first.
    pub async fn sessions(&self, project: Option<&str>) -> ApiResult<Vec<SessionRecord>> {
        let mut req = self.http.get(self.url("/sessions"));
        if let Some(p) = project {
            req = req.query(&[("project", p)]);
        }
        Ok(Self::check(req.send().await?).await?.json().await?)
    }

    pub async fn session(&self, id: &str) -> ApiResult<SessionDetail> {
        self.get(&format!("/sessions/{}", seg(id))).await
    }

    pub async fn create_session(&self, new: &NewSession) -> ApiResult<SessionRecord> {
        let body = serde_json::to_value(new).expect("NewSession serialises");
        Ok(self.post("/sessions", body).await?.json().await?)
    }

    /// Stored events with `seq > after`, oldest first.
    pub async fn events(&self, id: &str, after: i64) -> ApiResult<Vec<StoredEvent>> {
        let req = self.http.get(self.url(&format!("/sessions/{}/events", seg(id)))).query(&[("after", after)]);
        Ok(Self::check(req.send().await?).await?.json().await?)
    }

    pub async fn send_message(&self, id: &str, text: &str) -> ApiResult<()> {
        self.post(&format!("/sessions/{}/messages", seg(id)), json!({ "text": text })).await?;
        Ok(())
    }

    pub async fn answer(&self, id: &str, approval_id: &str, decision: ApprovalDecision) -> ApiResult<()> {
        let path = format!("/sessions/{}/approvals/{}", seg(id), seg(approval_id));
        self.post(&path, json!({ "decision": decision })).await?;
        Ok(())
    }

    pub async fn interrupt(&self, id: &str) -> ApiResult<()> {
        self.post(&format!("/sessions/{}/interrupt", seg(id)), json!({})).await?;
        Ok(())
    }

    /// Keep `id`'s agent alive while it is being viewed (FR-S6).
    pub async fn lease(&self, id: &str) -> ApiResult<Lease> {
        Ok(self.post(&format!("/sessions/{}/lease", seg(id)), json!({})).await?.json().await?)
    }
}

/// Percent-encode one path segment (ids are UUIDs today, but approval ids come from agents).
fn seg(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
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
    fn urls() {
        let api = Api::new("http://h:1/").unwrap();
        assert_eq!(api.push_url(), "ws://h:1/api/v1/push");
        assert_eq!(Api::new("https://h").unwrap().push_url(), "wss://h/api/v1/push");
        assert!(Api::new("h:1").is_err());
        assert_eq!(seg("a b/c"), "a%20b%2Fc");
    }
}
