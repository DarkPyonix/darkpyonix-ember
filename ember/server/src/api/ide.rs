//! "Open IDE" launch targets for a session.
//!
//! `GET /api/sessions/{id}/ide[?computer=<name>]` returns, for the session's project and
//! current computer, how to open each IDE. The server only returns data; it never opens or
//! renders anything:
//!
//! | `kind`    | What it is                                    | What we return                    |
//! |-----------|-----------------------------------------------|-----------------------------------|
//! | `ember`   | Ember's own editor-core IDE (dioxus-compose, M8) | launch info only (no URL)       |
//! | `vscode`  | VS Code Web wrapped by `ember/proxy/` on that computer | the proxy URL with `?folder=`   |
//! | `gateway` | JetBrains Gateway                             | a `jetbrains-gateway://` link and the command that opens it on that computer |
//!
//! **Computers are not wired yet**: a session has no current-computer
//! attribute in the store. Until it does, the computer is `?computer=` or the configured
//! default, and each computer's IDE details come from [`IdeConfig`] (`EMBER_IDE_COMPUTERS`).
//! The folder is the session's `cwd`, taken as a path on that computer.
//!
//! TODO(computers): sessions now have a current computer (`crate::computers`,
//! `GET /api/sessions/{id}/computer`) and computers are registered there. Default
//! `?computer=` to the session's current computer and map registered computers to IDE targets
//! instead of the separate `EMBER_IDE_COMPUTERS` list.
//!
//! Link formats:
//! - Gateway: `jetbrains-gateway://connect#type=ssh&host=…&port=…&user=…&projectPath=…`
//!   `[&idePath=…&deploy=false]`, as in JetBrains' "Connect and work with JetBrains Gateway"
//!   (<https://www.jetbrains.com/help/idea/remote-development-a.html>). JetBrains is moving
//!   Gateway into the Toolbox App with a `jetbrains://` scheme and only "limited backward
//!   compatibility" for these links (<https://www.jetbrains.com/help/toolbox-app/jetbrains-gateway-migrations-guide.html>).

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::ose::OseManager;
use crate::session::Sessions;
use crate::store::SessionRecord;

/// One computer's IDE endpoints.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct ComputerIde {
    /// The computer the main server runs on.
    #[serde(default)]
    pub local: bool,
    /// The wrapped VS Code Web on that computer: the URL `python -m dpx.serve` announces.
    pub ide_url: Option<String>,
    /// SSH destination host for Gateway.
    pub ssh_host: Option<String>,
    pub ssh_user: Option<String>,
    pub ssh_port: Option<u16>,
    /// An IDE backend already installed on the computer (Gateway's `idePath`). Without it
    /// Gateway asks which IDE to use.
    pub jetbrains_ide_path: Option<String>,
    /// `macos`, `linux` or `windows`: decides the command that opens a link there.
    pub os: Option<String>,
}

/// `EMBER_IDE_COMPUTERS`: inline JSON (starting with `{`) or a path to a JSON file:
///
/// ```json
/// { "default_computer": "mini",
///   "server_url": "http://mini.local:8740",
///   "computers": {
///     "mini":   { "local": true, "ide_url": "http://127.0.0.1:8890/", "os": "macos" },
///     "studio": { "ide_url": "http://studio.local:8890/", "ssh_host": "studio.local",
///                 "ssh_user": "me", "os": "linux" } } }
/// ```
///
/// Unset: one local computer named `local`, whose `ide_url` is `EMBER_IDE_URL`.
/// `server_url` (what the Ember editor connects to) defaults to `EMBER_PUBLIC_URL`, else the
/// request's `Host`.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct IdeConfig {
    pub default_computer: String,
    #[serde(default)]
    pub server_url: Option<String>,
    pub computers: BTreeMap<String, ComputerIde>,
}

impl Default for IdeConfig {
    fn default() -> Self {
        let mut computers = BTreeMap::new();
        computers.insert("local".to_string(), ComputerIde { local: true, ..Default::default() });
        IdeConfig { default_computer: "local".into(), server_url: None, computers }
    }
}

impl IdeConfig {
    pub fn from_env() -> anyhow::Result<IdeConfig> {
        let mut cfg = match std::env::var("EMBER_IDE_COMPUTERS") {
            Ok(v) if !v.trim().is_empty() => Self::parse(&v)?,
            _ => {
                let mut c = IdeConfig::default();
                if let Some(local) = c.computers.get_mut("local") {
                    local.ide_url = std::env::var("EMBER_IDE_URL").ok().filter(|u| !u.is_empty());
                }
                c
            }
        };
        if cfg.server_url.is_none() {
            cfg.server_url = std::env::var("EMBER_PUBLIC_URL").ok().filter(|u| !u.is_empty());
        }
        Ok(cfg)
    }

    /// Inline JSON or a file path.
    pub fn parse(value: &str) -> anyhow::Result<IdeConfig> {
        let text = if value.trim_start().starts_with('{') {
            value.to_string()
        } else {
            std::fs::read_to_string(value.trim())
                .map_err(|e| anyhow::anyhow!("EMBER_IDE_COMPUTERS: cannot read {value}: {e}"))?
        };
        let cfg: IdeConfig = serde_json::from_str(&text)
            .map_err(|e| anyhow::anyhow!("EMBER_IDE_COMPUTERS: invalid JSON: {e}"))?;
        if !cfg.computers.contains_key(&cfg.default_computer) {
            anyhow::bail!(
                "EMBER_IDE_COMPUTERS: default_computer {:?} is not in computers",
                cfg.default_computer
            );
        }
        Ok(cfg)
    }
}

/// What the Ember editor needs to open a project (it is a native app, not a URL).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct EmberLaunch {
    pub session: String,
    pub project: String,
    pub computer: String,
    pub folder: String,
    /// The main server the editor connects to.
    pub server_url: Option<String>,
    /// Connection details when an OSE endpoint is reachable by the editor.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ose: Option<EmberOseConnection>,
}

#[derive(Clone, Serialize, PartialEq)]
pub struct EmberOseConnection {
    pub endpoint: String,
    pub connection_token: String,
}

impl std::fmt::Debug for EmberOseConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmberOseConnection").field("endpoint", &self.endpoint)
            .field("connection_token", &"[REDACTED]").finish()
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Target {
    /// `ember`, `vscode` or `gateway`.
    pub kind: &'static str,
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// argv that opens `url` on the target computer (Gateway), for ember node to run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub launch: Option<EmberLaunch>,
    /// Why the target is unavailable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl Target {
    fn unavailable(kind: &'static str, reason: String) -> Target {
        Target { kind, available: false, url: None, command: None, launch: None, reason: Some(reason) }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ComputerRef {
    pub name: String,
    pub local: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct IdeLaunch {
    pub session: String,
    pub project: String,
    pub folder: String,
    pub computer: ComputerRef,
    /// Always in the order ember, vscode, gateway.
    pub targets: Vec<Target>,
}

/// Percent-encodes everything but RFC 3986 unreserved characters (and `/` when asked).
pub fn percent_encode(s: &str, keep_slash: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        let keep = b.is_ascii_alphanumeric()
            || matches!(b, b'-' | b'.' | b'_' | b'~')
            || (keep_slash && b == b'/');
        if keep {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The wrapped VS Code Web URL for `folder`: `<ide_url>/?folder=<folder>`.
pub fn vscode_web_url(ide_url: &str, folder: &str) -> String {
    format!("{}/?folder={}", ide_url.trim_end_matches('/'), percent_encode(folder, true))
}

/// `jetbrains-gateway://connect#…` for an SSH connection, or why one cannot be built.
pub fn gateway_url(c: &ComputerIde, folder: &str) -> Result<String, String> {
    let host = c.ssh_host.as_deref().filter(|h| !h.is_empty()).ok_or_else(|| {
        "no ssh_host configured for this computer; Gateway connects over SSH".to_string()
    })?;
    let mut params = vec![
        ("type", "ssh".to_string()),
        ("host", host.to_string()),
        ("port", c.ssh_port.unwrap_or(22).to_string()),
    ];
    if let Some(user) = c.ssh_user.as_deref().filter(|u| !u.is_empty()) {
        params.push(("user", user.to_string()));
    }
    params.push(("projectPath", folder.to_string()));
    if let Some(ide) = c.jetbrains_ide_path.as_deref().filter(|p| !p.is_empty()) {
        params.push(("idePath", ide.to_string()));
        params.push(("deploy", "false".to_string()));
    }
    let fragment: Vec<String> =
        params.iter().map(|(k, v)| format!("{k}={}", percent_encode(v, false))).collect();
    Ok(format!("jetbrains-gateway://connect#{}", fragment.join("&")))
}

/// The argv that hands a URL to its registered handler on a computer of this OS.
pub fn open_command(os: Option<&str>, url: &str) -> Option<Vec<String>> {
    let argv: Vec<&str> = match os? {
        "macos" => vec!["open", url],
        "linux" => vec!["xdg-open", url],
        "windows" => vec!["cmd", "/c", "start", "", url],
        _ => return None,
    };
    Some(argv.into_iter().map(String::from).collect())
}

/// The launch targets for one session on one computer.
pub fn launch_targets(
    rec: &SessionRecord,
    computer_name: &str,
    computer: &ComputerIde,
    server_url: Option<&str>,
    ose: &OseManager,
) -> IdeLaunch {
    let folder = rec.cwd.clone();

    let connection = if computer.local {
        ose.connection(std::path::Path::new(&folder))
    } else {
        Err("OSE server is only available on the local computer".into())
    };
    let ember = Target {
        kind: "ember",
        available: connection.is_ok(),
        url: None,
        command: None,
        launch: Some(EmberLaunch {
            session: rec.id.clone(),
            project: rec.project.clone(),
            computer: computer_name.to_string(),
            folder: folder.clone(),
            server_url: server_url.map(String::from),
            ose: connection.as_ref().ok().cloned(),
        }),
        reason: connection.err(),
    };

    let vscode = match computer.ide_url.as_deref().filter(|u| !u.is_empty()) {
        Some(base) => Target {
            kind: "vscode",
            available: true,
            url: Some(vscode_web_url(base, &folder)),
            command: None,
            launch: None,
            reason: None,
        },
        None => Target::unavailable(
            "vscode",
            format!("no ide_url configured for computer {computer_name:?} (run `python -m dpx.serve` there)"),
        ),
    };

    let gateway = match gateway_url(computer, &folder) {
        Ok(url) => Target {
            kind: "gateway",
            available: true,
            command: open_command(computer.os.as_deref(), &url),
            url: Some(url),
            launch: None,
            reason: None,
        },
        Err(reason) => Target::unavailable("gateway", reason),
    };

    IdeLaunch {
        session: rec.id.clone(),
        project: rec.project.clone(),
        folder,
        computer: ComputerRef { name: computer_name.to_string(), local: computer.local },
        targets: vec![ember, vscode, gateway],
    }
}

#[derive(Clone)]
struct IdeState {
    sessions: Arc<Sessions>,
    config: Arc<IdeConfig>,
    ose: Arc<OseManager>,
}

pub fn router(sessions: Arc<Sessions>, config: Arc<IdeConfig>) -> Router {
    router_with_ose(sessions, config, Arc::new(OseManager::default()))
}

/// Like [`router`] with the manager shared with the extension route (`ide_extensions`).
pub fn router_with_ose(sessions: Arc<Sessions>, config: Arc<IdeConfig>, ose: Arc<OseManager>) -> Router {
    Router::new()
        .route("/api/sessions/{id}/ide", get(ide_targets))
        .with_state(IdeState { sessions, config, ose })
}

#[derive(Deserialize)]
struct IdeQuery {
    computer: Option<String>,
}

fn error(code: StatusCode, msg: String) -> Response {
    (code, Json(json!({ "error": msg }))).into_response()
}

async fn ide_targets(
    State(st): State<IdeState>,
    Path(id): Path<String>,
    Query(q): Query<IdeQuery>,
    headers: HeaderMap,
) -> Response {
    let rec = match st.sessions.store().session(&id) {
        Ok(Some(rec)) => rec,
        Ok(None) => return error(StatusCode::NOT_FOUND, format!("session not found: {id}")),
        Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")),
    };
    let name = q.computer.unwrap_or_else(|| st.config.default_computer.clone());
    let Some(computer) = st.config.computers.get(&name) else {
        return error(StatusCode::NOT_FOUND, format!("unknown computer: {name}"));
    };
    let server_url = st.config.server_url.clone().or_else(|| {
        headers
            .get(header::HOST)
            .and_then(|h| h.to_str().ok())
            .map(|h| format!("http://{h}"))
    });
    Json(launch_targets(&rec, &name, computer, server_url.as_deref(), &st.ose)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr_l7_fr_l31_fr_w4_serializes_editor_connection() {
        let launch = EmberLaunch {
            session: "s".into(), project: "p".into(), computer: "local".into(),
            folder: "/w".into(), server_url: None,
            ose: Some(EmberOseConnection { endpoint: "http://localhost:9000".into(), connection_token: "secret".into() }),
        };
        let value = serde_json::to_value(launch).unwrap();
        assert_eq!(value["ose"]["endpoint"], "http://localhost:9000");
        assert_eq!(value["ose"]["connection_token"], "secret");
    }

    fn record(folder: &std::path::Path) -> SessionRecord {
        SessionRecord {
            id: "s".into(), project: "p".into(), agent: crate::agents::AgentKind::Scripted,
            cwd: folder.to_string_lossy().into_owned(), model: None, native_id: None,
            status: crate::events::SessionStatus::Idle, title: String::new(), created_at: 0,
            updated_at: 0, last_seq: 0, account_id: None, account_reason: None,
            pinned: false, archived: false,
        }
    }

    #[test]
    fn fr_l7_fr_w4_missing_server_omits_connection() {
        let dir = tempfile::tempdir().unwrap();
        let manager = OseManager::new(None);
        let targets = launch_targets(&record(dir.path()), "local", &ComputerIde { local: true, ..Default::default() }, None, &manager);
        let ember = &targets.targets[0];
        assert!(!ember.available);
        assert!(ember.reason.as_ref().unwrap().contains("not installed"));
        assert!(serde_json::to_value(ember).unwrap()["launch"].get("ose").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn fr_l7_fr_w4_launch_starts_and_reuses_server() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("server");
        std::fs::write(&script, r##"#!/bin/sh
exec python3 -c 'import socket,sys; p=int(sys.argv[sys.argv.index("--port")+1]); s=socket.socket(); s.bind(("127.0.0.1",p)); s.listen();
while True: s.accept()[0].close()' "$@"
"##).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let manager = OseManager::with_data_root(Some(script), dir.path().join("ose-data"));
        let computer = ComputerIde { local: true, ..Default::default() };
        let first = launch_targets(&record(dir.path()), "local", &computer, None, &manager);
        let second = launch_targets(&record(dir.path()), "local", &computer, None, &manager);
        assert!(first.targets[0].available);
        let connection = first.targets[0].launch.as_ref().unwrap().ose.as_ref().unwrap();
        assert_eq!(Some(connection), second.targets[0].launch.as_ref().unwrap().ose.as_ref());
        assert!(!connection.connection_token.is_empty());
        let value = serde_json::to_value(&first).unwrap();
        assert_eq!(value["targets"][0]["launch"]["ose"]["connection_token"], connection.connection_token);
        assert!(!format!("{first:?}").contains(&connection.connection_token));
    }

    #[test]
    fn percent_encoding() {
        assert_eq!(percent_encode("/a b/ü", true), "/a%20b/%C3%BC");
        assert_eq!(percent_encode("/a b", false), "%2Fa%20b");
        assert_eq!(percent_encode("a&b=c#d", true), "a%26b%3Dc%23d");
    }

    #[test]
    fn vscode_url_joins_cleanly() {
        assert_eq!(
            vscode_web_url("http://127.0.0.1:8890/", "/Users/me/my app"),
            "http://127.0.0.1:8890/?folder=/Users/me/my%20app"
        );
        assert_eq!(vscode_web_url("http://h:1", "/x"), "http://h:1/?folder=/x");
    }

    #[test]
    fn gateway_url_matches_documented_form() {
        let c = ComputerIde {
            ssh_host: Some("ec2.example.com".into()),
            ssh_user: Some("ubuntu".into()),
            jetbrains_ide_path: Some("/home/ubuntu/.cache/JetBrains/RemoteDev/dist/idea".into()),
            ..Default::default()
        };
        assert_eq!(
            gateway_url(&c, "/home/ubuntu/development/spring-petclinic").unwrap(),
            "jetbrains-gateway://connect#type=ssh&host=ec2.example.com&port=22&user=ubuntu\
             &projectPath=%2Fhome%2Fubuntu%2Fdevelopment%2Fspring-petclinic\
             &idePath=%2Fhome%2Fubuntu%2F.cache%2FJetBrains%2FRemoteDev%2Fdist%2Fidea&deploy=false"
        );
    }

    #[test]
    fn gateway_without_ide_path_or_user() {
        let c = ComputerIde { ssh_host: Some("h".into()), ssh_port: Some(2222), ..Default::default() };
        assert_eq!(
            gateway_url(&c, "/p").unwrap(),
            "jetbrains-gateway://connect#type=ssh&host=h&port=2222&projectPath=%2Fp"
        );
        assert!(gateway_url(&ComputerIde::default(), "/p").is_err());
    }

    #[test]
    fn open_commands_per_os() {
        assert_eq!(open_command(Some("macos"), "u"), Some(vec!["open".into(), "u".into()]));
        assert_eq!(open_command(Some("linux"), "u"), Some(vec!["xdg-open".into(), "u".into()]));
        assert_eq!(open_command(Some("windows"), "u").unwrap().len(), 5);
        assert_eq!(open_command(None, "u"), None);
        assert_eq!(open_command(Some("plan9"), "u"), None);
    }

    #[test]
    fn config_parse_inline_and_default_check() {
        let cfg = IdeConfig::parse(
            r#"{"default_computer":"mini","computers":{"mini":{"local":true,"ide_url":"http://x/"}}}"#,
        )
        .unwrap();
        assert!(cfg.computers["mini"].local);
        assert_eq!(cfg.computers["mini"].ide_url.as_deref(), Some("http://x/"));
        assert!(IdeConfig::parse(r#"{"default_computer":"nope","computers":{}}"#).is_err());
        assert!(IdeConfig::parse("{not json").is_err());
    }
}
