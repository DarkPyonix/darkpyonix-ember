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
    /// The server answered 401: the token is missing or wrong (FR-S11). Not retried.
    #[error("authentication failed: the server refused the token ({0})")]
    Unauthorized(String),
    /// The HTTP client could not be built (for example the TLS backend failed to initialise).
    #[error("cannot build the HTTP client: {0}")]
    Client(String),
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

/// The login commands the server returns with an account (`login_instructions`). The command
/// names the account's config directory, so it is shown to the owner and never logged.
#[derive(Clone, PartialEq, Default, Deserialize)]
pub struct Login {
    #[serde(default)]
    pub login: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub note: String,
}

impl std::fmt::Debug for Login {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Login").finish_non_exhaustive()
    }
}

/// `POST /api/accounts` and `POST /api/accounts/{id}/check` answer: the account and its login.
/// [`Account`] itself is decoded unchanged.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct AccountWithLogin {
    #[serde(flatten)]
    pub account: Account,
    #[serde(default)]
    pub login: Login,
}

/// The weekly per-app cap of a ChatGPT account (FR-U4). OpenAI reports no figure, so `known`
/// is false and the view points at where the user sets it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct WeeklyCap {
    #[serde(default)]
    pub known: bool,
    #[serde(default)]
    pub manage_url: String,
    #[serde(default)]
    pub note: String,
}

/// `GET /api/chatgpt/accounts` entry (FR-U4); token material never reaches clients.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ChatGptAccount {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub email: Option<String>,
    /// `signed_in` | `signed_out` | `not_eligible`.
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub limited: bool,
    #[serde(default)]
    pub limited_until: Option<i64>,
    #[serde(default)]
    pub limit_reason: Option<String>,
    #[serde(default)]
    pub tokens_today: u64,
    #[serde(default)]
    pub weekly_cap: Option<WeeklyCap>,
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
    /// The computer designated for the session (FR-X3), read-only.
    #[serde(default)]
    pub designated_computer_id: Option<String>,
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
    pub launch: Option<ember_client::wire::EmberLaunch>,
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

    /// The `(project, folder)` the Ember editor opens (FR-L31); empty until the launch data arrives.
    pub fn editor_target(launch: Option<&Result<IdeLaunch, String>>) -> (String, String) {
        launch
            .and_then(|r| r.as_ref().ok())
            .map(|l| (l.project.clone(), l.folder.clone()))
            .unwrap_or_default()
    }

    /// What the Ember editor window needs to connect: project, folder and the OSE runtime's
    /// `host:port` and connection token when the server started one for the folder.
    pub fn editor_connection(launch: Option<&Result<IdeLaunch, String>>) -> EditorConnection {
        let Some(Ok(l)) = launch else {
            return EditorConnection::default();
        };
        let ember = l.target("ember").and_then(|t| t.launch.as_ref());
        let ose = ember.and_then(|e| e.ose.as_ref());
        EditorConnection {
            project: l.project.clone(),
            folder: l.folder.clone(),
            endpoint: ose.map(|o| host_port(&o.endpoint)),
            token: ose.map(|o| o.connection_token.clone()),
            // The relay path takes the computer's registry id. The display name (`l.computer.name`) is not
            // an id, and writing it into `/api/computers/{id}/terminal` asked for a computer that does not exist.
            computer: ember.map(|e| e.computer.clone()).filter(|c| !c.is_empty()),
            server_url: ember
                .and_then(|e| e.server_url.clone())
                .filter(|u| !u.is_empty())
                .or_else(|| crate::services::try_services().map(|s| s.config.server_url.clone()).filter(|u| !u.is_empty())),
        }
    }
}

/// The editor window's connection data (`endpoint` is `host:port`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EditorConnection {
    pub project: String,
    pub folder: String,
    pub endpoint: Option<String>,
    pub token: Option<String>,
    /// The computer's id for the terminal relay.
    pub computer: Option<String>,
    /// The ember server's base URL for the terminal relay.
    pub server_url: Option<String>,
}

/// `http://127.0.0.1:9000/` becomes `127.0.0.1:9000`; a bare `host:port` is kept.
pub fn host_port(endpoint: &str) -> String {
    let rest = endpoint.split_once("://").map_or(endpoint, |(_, r)| r);
    rest.trim_end_matches('/').to_string()
}

#[cfg(test)]
mod editor_connection_tests {
    use super::*;

    fn launch(ose: Option<(&str, &str)>) -> Result<IdeLaunch, String> {
        Ok(IdeLaunch {
            session: "s".into(),
            project: "p".into(),
            folder: "/w".into(),
            computer: IdeComputer { name: "c".into(), local: true },
            targets: vec![IdeTarget {
                kind: "ember".into(),
                available: true,
                url: None,
                command: None,
                launch: Some(ember_client::wire::EmberLaunch {
                    session: "s".into(),
                    project: "p".into(),
                    computer: "c".into(),
                    folder: "/w".into(),
                    server_url: Some("http://srv:7".into()),
                    ose: ose.map(|(e, t)| ember_client::wire::EmberOseConnection {
                        endpoint: e.into(),
                        connection_token: t.into(),
                    }),
                }),
                reason: None,
            }],
        })
    }

    #[test]
    fn fr_l31_editor_connection_strips_scheme_and_carries_token() {
        let c = IdeLaunch::editor_connection(Some(&launch(Some(("http://127.0.0.1:9000/", "tok")))));
        assert_eq!(c.endpoint.as_deref(), Some("127.0.0.1:9000"));
        assert_eq!(c.token.as_deref(), Some("tok"));
        assert_eq!((c.project.as_str(), c.folder.as_str()), ("p", "/w"));
        assert_eq!(c.computer.as_deref(), Some("c"));
        assert_eq!(c.server_url.as_deref(), Some("http://srv:7"));
    }

    #[test]
    fn fr_l31_editor_connection_never_uses_the_display_name_as_the_computer_id() {
        let mut l = launch(None).unwrap();
        l.targets[0].launch.as_mut().unwrap().computer = String::new();
        l.computer.name = "이 컴퓨터".into();
        let c = IdeLaunch::editor_connection(Some(&Ok(l)));
        assert_eq!(c.computer, None, "the terminal relay path takes an id; a display name asks for a computer that does not exist");
    }

    #[test]
    fn fr_l31_editor_connection_without_ose_has_no_endpoint() {
        let c = IdeLaunch::editor_connection(Some(&launch(None)));
        assert_eq!(c.endpoint, None);
        assert_eq!(IdeLaunch::editor_connection(None), EditorConnection::default());
        assert_eq!(host_port("host:1"), "host:1");
    }
}

/// `GET /api/agents/defaults` and `PUT /api/agents/defaults`: global new-member defaults (`새 팀원 기본값`, FR-L20, #481, #670).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentDefaults {
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
    #[serde(default)]
    pub mode: Option<String>,
}

/// `GET /api/agents/settings` response (FR-L20, FR-S8).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct AgentSettingsResponse {
    #[serde(default)]
    pub settings: std::collections::HashMap<String, AgentHarnessSettings>,
}

/// Per-harness settings in `GET /api/agents/settings` (FR-L20, FR-S8).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct AgentHarnessSettings {
    #[serde(default)]
    pub mem_limit_mb: Option<u64>,
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
    /// Reads the server token once from the environment (FR-S11, `ember_client::token`).
    /// Fails when the HTTP client cannot be built.
    pub fn new(base_url: &str) -> ServerResult<ServerApi> {
        // A server that is not local gets no token and none is read (FR-S11).
        let token = ember_client::token::token_allowed_for(base_url)
            .then(ember_client::token::server_token_from_env)
            .flatten();
        ServerApi::with_token(base_url, token)
    }

    /// Like [`ServerApi::new`] with the token given; `None` sends no `Authorization` header, and
    /// so does a base URL that is not loopback or listed in `EMBER_TOKEN_HOSTS`. Returns an error
    /// when the HTTP client cannot be built, rather than continuing without the header.
    pub fn with_token(base_url: &str, token: Option<String>) -> ServerResult<ServerApi> {
        let auth = token
            .filter(|_| ember_client::token::token_allowed_for(base_url))
            .as_deref()
            .and_then(ember_client::token::auth_headers);
        let mut builder = reqwest::Client::builder();
        if let Some(headers) = auth {
            builder = builder.default_headers(headers);
        }
        Self::from_built(builder.build(), base_url)
    }

    /// Wraps the result of building the HTTP client; a build failure becomes
    /// [`ServerError::Client`].
    fn from_built<E: std::fmt::Display>(
        built: Result<reqwest::Client, E>,
        base_url: &str,
    ) -> ServerResult<ServerApi> {
        let http = built.map_err(|e| ServerError::Client(e.to_string()))?;
        Ok(ServerApi { http, base: base_url.trim_end_matches('/').to_string() })
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
        Err(status_error(status.as_u16(), body))
    }

    pub async fn accounts(&self) -> ServerResult<Vec<Account>> {
        Ok(Self::check(self.http.get(self.url("/accounts")).send().await?).await?.json().await?)
    }

    /// `POST /api/accounts/{id}/default`: the per-agent default account (FR-U1, FR-L17).
    pub async fn set_default_account(&self, id: &str) -> ServerResult<Account> {
        let req = self.http.post(self.url(&default_path(id)));
        Ok(Self::check(req.send().await?).await?.json().await?)
    }

    /// `POST /api/accounts`: a new account with its own config directory and the login command.
    pub async fn create_account(&self, agent: &str, label: &str) -> ServerResult<AccountWithLogin> {
        let req = self.http.post(self.url("/accounts")).json(&create_account_body(agent, label));
        Ok(Self::check(req.send().await?).await?.json().await?)
    }

    /// `POST /api/accounts/{id}/check`: runs the agent's status command, records and returns it.
    pub async fn check_account(&self, id: &str) -> ServerResult<AccountWithLogin> {
        let req = self.http.post(self.url(&check_path(id)));
        Ok(Self::check(req.send().await?).await?.json().await?)
    }

    /// ChatGPT plan accounts; an error (404 when sign-in is off, e.g. hosted) means none.
    pub async fn chatgpt_accounts(&self) -> ServerResult<Vec<ChatGptAccount>> {
        Ok(Self::check(self.http.get(self.url("/chatgpt/accounts")).send().await?).await?.json().await?)
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

    /// `GET /api/agents/defaults`: global new-member defaults (`새 팀원 기본값`, FR-L20, #481, #670).
    pub async fn agent_defaults(&self) -> ServerResult<AgentDefaults> {
        Ok(Self::check(self.http.get(self.url("/agents/defaults")).send().await?).await?.json().await?)
    }

    /// `PUT /api/agents/defaults`: save global new-member defaults (FR-L20, #481, #670).
    pub async fn put_agent_defaults(&self, defaults: &AgentDefaults) -> ServerResult<AgentDefaults> {
        let req = self.http.put(self.url("/agents/defaults")).json(defaults);
        Ok(Self::check(req.send().await?).await?.json().await?)
    }

    /// `GET /api/agents/settings`: per-harness settings (`mem_limit_mb`, FR-L20, FR-S8).
    pub async fn agent_settings(&self) -> ServerResult<AgentSettingsResponse> {
        Ok(Self::check(self.http.get(self.url("/agents/settings")).send().await?).await?.json().await?)
    }

    /// `PUT /api/agents/{agent}/settings`: set or clear (`None` or `Some(0)`) the memory limit in MiB (FR-L20, FR-S8).
    pub async fn put_agent_mem_limit(&self, agent: &str, mem_limit_mb: Option<u64>) -> ServerResult<()> {
        let req = self
            .http
            .put(self.url(&agent_settings_path(agent)))
            .json(&json!({ "mem_limit_mb": mem_limit_mb }));
        Self::check(req.send().await?).await?;
        Ok(())
    }
}

/// The error for a non-success answer: 401 is [`ServerError::Unauthorized`], the rest carry the
/// server's `error` text (or the raw body).
fn status_error(status: u16, body: String) -> ServerError {
    let message = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
        .unwrap_or(body);
    if status == 401 {
        ServerError::Unauthorized(message)
    } else {
        ServerError::Status { status, message }
    }
}

fn default_path(id: &str) -> String {
    format!("/accounts/{}/default", seg(id))
}

fn check_path(id: &str) -> String {
    format!("/accounts/{}/check", seg(id))
}

fn agent_settings_path(agent: &str) -> String {
    format!("/agents/{}/settings", seg(agent))
}

fn create_account_body(agent: &str, label: &str) -> serde_json::Value {
    json!({ "agent": agent, "label": label })
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
    fn fr_l17_choosing_an_account_posts_the_default_endpoint() {
        assert_eq!(default_path("a1"), "/accounts/a1/default");
        assert_eq!(default_path("a/b c"), "/accounts/a%2Fb%20c/default");
        assert_eq!(check_path("a/b"), "/accounts/a%2Fb/check");
        assert_eq!(create_account_body("codex", "work"), json!({ "agent": "codex", "label": "work" }));
    }

    #[test]
    fn fr_u1_decodes_the_login_and_never_prints_the_command() {
        let a: AccountWithLogin = serde_json::from_value(json!({
            "id": "a1", "agent": "codex", "label": "w", "status": "logged_out", "is_default": false,
            "config_dir": "/x", "login": { "env": {"CODEX_HOME": "/x"}, "login": "CODEX_HOME=/x codex login",
            "status": "s", "logout": "l", "note": "n" }
        }))
        .unwrap();
        assert_eq!(a.account.id, "a1");
        assert_eq!(a.login.login, "CODEX_HOME=/x codex login");
        assert!(!format!("{a:?}").contains("codex login"));
        let bare: AccountWithLogin = serde_json::from_value(json!({ "id": "b", "agent": "x", "label": "l" })).unwrap();
        assert_eq!(bare.login, Login::default());
    }

    #[test]
    fn fr_s11_server_api_default_headers_carry_a_sensitive_bearer() {
        let h = ember_client::token::auth_headers("tok").unwrap();
        let v = h.get(reqwest::header::AUTHORIZATION).unwrap();
        assert_eq!(v, "Bearer tok");
        assert!(v.is_sensitive());
        assert!(ember_client::token::auth_headers("bad\ntoken").is_none());
    }

    #[test]
    fn fr_s11_server_api_debug_output_never_holds_the_token() {
        let api = ServerApi::with_token("http://127.0.0.1:1", Some("secret".into())).unwrap();
        assert!(!format!("{api:?}").contains("secret"));
    }

    #[test]
    fn fr_s11_server_api_client_build_failure_is_an_error() {
        let e = ServerApi::from_built(Err("tls backend failed"), "http://127.0.0.1:1").unwrap_err();
        assert!(matches!(&e, ServerError::Client(m) if m == "tls backend failed"));
        assert!(ServerApi::with_token("http://127.0.0.1:1", None).is_ok());
    }

    #[test]
    fn fr_s11_server_api_401_is_the_auth_error() {
        let e = status_error(401, r#"{"error":"unauthorized"}"#.into());
        assert!(matches!(&e, ServerError::Unauthorized(m) if m == "unauthorized"));
        assert!(matches!(status_error(500, "boom".into()), ServerError::Status { status: 500, .. }));
    }

    #[test]
    fn fr_l7_fr_l31_fr_w4_decodes_editor_connection() {
        let ide: IdeLaunch = serde_json::from_value(json!({
            "session": "s", "project": "p", "folder": "/w",
            "computer": { "name": "local", "local": true },
            "targets": [{ "kind": "ember", "available": true, "launch": {
                "session": "s", "project": "p", "computer": "local", "folder": "/w",
                "server_url": "http://localhost:8740",
                "ose": { "endpoint": "http://localhost:9000", "connection_token": "secret" }
            }}]
        })).unwrap();
        let connection = ide.target("ember").unwrap().launch.as_ref().unwrap().ose.as_ref().unwrap();
        assert_eq!(connection.endpoint, "http://localhost:9000");
        assert_eq!(connection.connection_token, "secret");
    }

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
    fn fr_u4_decodes_chatgpt_accounts_with_the_weekly_cap() {
        let a: Vec<ChatGptAccount> = serde_json::from_value(json!([{
            "id": "g1", "kind": "chatgpt", "label": "me", "email": "me@x.dev", "client_id": "c",
            "scopes": [], "plan_usage": true, "status": "signed_in", "access_expires_at": 5,
            "limited_until": null, "limit_reason": null, "created_at": 1, "updated_at": 2,
            "limited": false, "tokens_today": 15,
            "weekly_cap": { "known": false, "manage_url": "https://chatgpt.com/x", "note": "n" }
        }]))
        .unwrap();
        assert_eq!(a[0].tokens_today, 15);
        let cap = a[0].weekly_cap.as_ref().unwrap();
        assert!(!cap.known);
        assert_eq!(cap.manage_url, "https://chatgpt.com/x");
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

    #[test]
    fn fr_l20_defaults_and_settings_wire_shapes() {
        let d = AgentDefaults {
            agent: Some("claude-code".into()),
            model: Some("Opus 4.7".into()),
            effort: Some("High".into()),
            mode: Some("edits".into()),
        };
        let v = serde_json::to_value(&d).unwrap();
        assert_eq!(
            v,
            json!({
                "agent": "claude-code",
                "model": "Opus 4.7",
                "effort": "High",
                "mode": "edits"
            })
        );
        let decoded: AgentDefaults = serde_json::from_value(v).unwrap();
        assert_eq!(decoded, d);

        let empty: AgentDefaults = serde_json::from_value(json!({})).unwrap();
        assert_eq!(empty, AgentDefaults::default());

        let s: AgentSettingsResponse = serde_json::from_value(json!({
            "settings": { "codex": { "mem_limit_mb": 4096 } }
        }))
        .unwrap();
        assert_eq!(s.settings.get("codex").and_then(|c| c.mem_limit_mb), Some(4096));
        assert_eq!(agent_settings_path("claude-code"), "/agents/claude-code/settings");
    }
}
