//! The main-server endpoints `ember-client` does not wrap yet: accounts (FR-U1/U2), computers
//! and a session's current computer (FR-X3), "Open IDE" targets (FR-L7), and session creation
//! with an account. Projects and computer assignment (FR-L4), session metadata, search and
//! export (FR-L9, FR-S4) go through `ember_client` (`Client::patch_session`,
//! `Client::assign_computer`, `Client::search`, `Api::export_session`). Shapes mirror `ember/server/src/{accounts,computers}/api.rs` and
//! `ember/server/src/api/ide.rs`, decoded tolerantly (unknown fields ignored, optional ones
//! defaulted) like `ember_client::wire`.
//!
//! TODO(ember-client): move these into `ember_client::api` once the client crate grows them;
//! the UI only depends on the types here.

use serde::{Deserialize, Serialize};
use serde_json::json;

use ember_client::wire::SessionRecord;

#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error("request failed: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("server answered {status}: {message}")]
    Status { status: u16, message: String },
}

pub type ServerResult<T> = Result<T, ServerError>;

/// `GET /api/accounts` entry.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Account {
    pub id: String,
    /// Agent kind, kebab-case (`claude-code`, `codex`, …).
    pub agent: String,
    pub label: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub is_default: bool,
    #[serde(default)]
    pub limited: bool,
}

/// `GET /api/computers` entry (`ComputerStatus`, flattened `ComputerView`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ComputerStatus {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub local: bool,
    /// `None` when not probed.
    #[serde(default)]
    pub reachable: Option<bool>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ComputerView {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub local: bool,
}

/// `GET /api/sessions/{id}/computer`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct CurrentComputer {
    pub session_id: String,
    pub computer: ComputerView,
    #[serde(default)]
    pub implicit: bool,
    #[serde(default)]
    pub notice_pending: bool,
}

/// `PUT /api/sessions/{id}/computer` answer.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SwitchOutcome {
    pub session_id: String,
    pub computer: ComputerView,
    #[serde(default)]
    pub changed: bool,
    #[serde(default)]
    pub notice: Option<String>,
}

/// One "Open IDE" target (`ember/server/src/api/ide.rs` `Target`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct IdeTarget {
    /// `ember`, `vscode` or `gateway`.
    pub kind: String,
    pub available: bool,
    #[serde(default)]
    pub url: Option<String>,
    /// argv that opens `url` on the target computer (Gateway), for ember node to run there.
    #[serde(default)]
    pub command: Option<Vec<String>>,
    #[serde(default)]
    pub launch: Option<serde_json::Value>,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct IdeComputer {
    pub name: String,
    #[serde(default)]
    pub local: bool,
}

/// `GET /api/sessions/{id}/ide`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct IdeLaunch {
    pub session: String,
    pub project: String,
    pub folder: String,
    pub computer: IdeComputer,
    pub targets: Vec<IdeTarget>,
}

impl IdeLaunch {
    pub fn target(&self, kind: &str) -> Option<&IdeTarget> {
        self.targets.iter().find(|t| t.kind == kind)
    }
}

/// `POST /api/sessions` body, including the account (FR-U2) that
/// `ember_client::wire::NewSession` does not have yet.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CreateSession {
    pub project: String,
    pub agent: String,
    pub cwd: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// `None` lets the server's router choose (FR-U3).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ServerApi {
    http: reqwest::Client,
    base: String,
}

impl ServerApi {
    pub fn new(base_url: &str) -> ServerApi {
        ServerApi { http: reqwest::Client::new(), base: base_url.trim_end_matches('/').to_string() }
    }

    fn url(&self, path: &str) -> String {
        format!("{}/api{path}", self.base)
    }

    async fn check(res: reqwest::Response) -> ServerResult<reqwest::Response> {
        let status = res.status();
        if status.is_success() {
            return Ok(res);
        }
        let body = res.text().await.unwrap_or_default();
        let message = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
            .unwrap_or(body);
        Err(ServerError::Status { status: status.as_u16(), message })
    }

    pub async fn accounts(&self) -> ServerResult<Vec<Account>> {
        Ok(Self::check(self.http.get(self.url("/accounts")).send().await?).await?.json().await?)
    }

    /// `probe = false` answers at once with `reachable: None`; `true` asks every node.
    pub async fn computers(&self, probe: bool) -> ServerResult<Vec<ComputerStatus>> {
        let req = self.http.get(self.url("/computers")).query(&[("probe", probe)]);
        Ok(Self::check(req.send().await?).await?.json().await?)
    }

    pub async fn session_computer(&self, id: &str) -> ServerResult<CurrentComputer> {
        let req = self.http.get(self.url(&format!("/sessions/{}/computer", seg(id))));
        Ok(Self::check(req.send().await?).await?.json().await?)
    }

    pub async fn switch_computer(&self, id: &str, computer_id: &str) -> ServerResult<SwitchOutcome> {
        let req = self
            .http
            .put(self.url(&format!("/sessions/{}/computer", seg(id))))
            .json(&json!({ "computer_id": computer_id }));
        Ok(Self::check(req.send().await?).await?.json().await?)
    }

    pub async fn ide_targets(&self, id: &str, computer: Option<&str>) -> ServerResult<IdeLaunch> {
        let mut req = self.http.get(self.url(&format!("/sessions/{}/ide", seg(id))));
        if let Some(c) = computer {
            req = req.query(&[("computer", c)]);
        }
        Ok(Self::check(req.send().await?).await?.json().await?)
    }

    pub async fn create_session(&self, body: &CreateSession) -> ServerResult<SessionRecord> {
        let req = self.http.post(self.url("/sessions")).json(body);
        Ok(Self::check(req.send().await?).await?.json().await?)
    }
}

/// Percent-encode one path segment.
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
    fn decodes_server_shapes() {
        let ide: IdeLaunch = serde_json::from_value(json!({
            "session": "s", "project": "p", "folder": "/w",
            "computer": { "name": "local", "local": true },
            "targets": [
                { "kind": "ember", "available": false, "launch": { "session": "s" }, "reason": "M8" },
                { "kind": "vscode", "available": true, "url": "http://127.0.0.1:8890/?folder=%2Fw" },
                { "kind": "gateway", "available": true, "url": "jetbrains-gateway://connect#type=ssh",
                  "command": ["open", "jetbrains-gateway://connect#type=ssh"] }
            ]
        }))
        .unwrap();
        assert_eq!(ide.target("vscode").unwrap().url.as_deref(), Some("http://127.0.0.1:8890/?folder=%2Fw"));
        assert!(!ide.target("ember").unwrap().available);

        let cs: Vec<ComputerStatus> = serde_json::from_value(json!([
            { "id": "local", "name": "This computer", "url": "", "local": true, "reachable": null },
            { "id": "c1", "name": "studio", "url": "http://studio:8741", "local": false, "reachable": false, "error": "timeout" }
        ]))
        .unwrap();
        assert_eq!(cs[1].reachable, Some(false));

        let a: Vec<Account> = serde_json::from_value(json!([{
            "id": "a1", "agent": "claude-code", "label": "work", "config_dir": "/x", "status": "logged_in",
            "is_default": true, "limited_until": null, "limit_reason": null, "created_at": 1,
            "limited": false, "tokens_today": 3, "login": {}
        }]))
        .unwrap();
        assert_eq!(a[0].label, "work");
    }

    #[test]
    fn create_body_omits_unset_fields() {
        let b = CreateSession {
            project: "p".into(),
            agent: "codex".into(),
            cwd: "/w".into(),
            model: None,
            title: None,
            account: Some("a1".into()),
        };
        assert_eq!(
            serde_json::to_value(&b).unwrap(),
            json!({ "project": "p", "agent": "codex", "cwd": "/w", "account": "a1" })
        );
        assert_eq!(seg("a b/c"), "a%20b%2Fc");
    }
}
