//! Owner presence, client side (SPEC `FR-S12`; INTENT D37 "Owner presence", #784).
//!
//! The server token is the non-owner credential. The routes that grant authority, admit a party
//! or change a secret ([`owner_route`]) also need an owner key: `eok_...`, claimed over the
//! server's unix socket `<data dir>/owner.sock` ([`OwnerLink`]) and sent as `X-Ember-Owner`.
//! Without it the server answers 403 `{"code": "owner_required"}`
//! ([`crate::api::ApiError::is_owner_required`]).
//!
//! The key lives in memory only, and only while the claim connection stays open. It has no
//! environment variable and no file: a child process the app starts would inherit a variable,
//! and an agent can read a file. An [`crate::api::Api`] given the key
//! ([`crate::api::Api::set_owner_key`]) sends it on owner routes over plain HTTP only. Over the
//! transport there is no owner key: the device key is the credential, and an owner route needs
//! a device with scope `full`.

use http::Method;

/// The header that carries the owner key.
pub const OWNER_HEADER: &str = "x-ember-owner";
/// The claim socket's name in the server's data folder.
pub const SOCKET_FILE: &str = "owner.sock";

/// The owner routes: `(method, path template)`. The same rows as the `Owner` rows of the
/// server's `owner::ROUTES`; the test `fr_s12_client_owner_routes_match_the_server` compares
/// the two lists and every other route of the server.
pub const OWNER_ROUTES: &[(&str, &str)] = &[
    ("PUT", "/api/a2a/settings"),
    ("POST", "/api/accounts"),
    ("DELETE", "/api/accounts/{id}"),
    ("POST", "/api/accounts/{id}/check"),
    ("POST", "/api/accounts/{id}/clear-limit"),
    ("POST", "/api/accounts/{id}/default"),
    ("PUT", "/api/agents/{agent}/settings"),
    ("PUT", "/api/browsers/{project}/egress"),
    ("POST", "/api/channels/{id}/feed/{approval_id}/answer"),
    ("DELETE", "/api/channels/{id}/guests/{guest_id}"),
    ("DELETE", "/api/channels/{id}/guests/{guest_id}/allow"),
    ("PUT", "/api/channels/{id}/guests/{guest_id}/allow"),
    ("POST", "/api/channels/{id}/invites"),
    ("DELETE", "/api/channels/{id}/invites/{invite_id}"),
    ("DELETE", "/api/chatgpt/accounts/{id}"),
    ("POST", "/api/chatgpt/accounts/{id}/clear-limit"),
    ("POST", "/api/chatgpt/accounts/{id}/refresh"),
    ("POST", "/api/chatgpt/signin"),
    ("POST", "/api/chatgpt/signin/complete"),
    ("POST", "/api/computers"),
    ("POST", "/api/computers/enrol-codes"),
    ("DELETE", "/api/computers/enrol-codes/{id}"),
    ("GET", "/api/computers/enrol-codes/{id}"),
    ("POST", "/api/computers/enrol-codes/{id}/confirm"),
    ("POST", "/api/computers/enrol-codes/{id}/deny"),
    ("DELETE", "/api/computers/{id}"),
    ("GET", "/api/devices"),
    ("POST", "/api/devices"),
    ("DELETE", "/api/devices/{peer}"),
    ("PATCH", "/api/devices/{peer}"),
    ("POST", "/api/hub/check"),
    ("DELETE", "/api/hub/devices/{id}"),
    ("POST", "/api/hub/devices/{id}/computer"),
    ("DELETE", "/api/hub/link"),
    ("POST", "/api/hub/link"),
    ("POST", "/api/hub/link-codes/{code}"),
    ("DELETE", "/api/hub/registration"),
    ("POST", "/api/hub/sync-devices"),
    ("PUT", "/api/hub/sync-devices"),
    ("POST", "/api/mcp"),
    ("DELETE", "/api/mcp/{id}"),
    ("PATCH", "/api/mcp/{id}"),
    ("POST", "/api/providers"),
    ("DELETE", "/api/providers/{id}"),
    ("POST", "/api/server-token/rotate"),
    ("POST", "/api/sessions/{id}/approvals/{approval_id}"),
    ("PUT", "/api/sessions/{id}/approve-all"),
];

/// True when `template` matches `segs`. `{name}` matches one non-empty segment.
fn template_matches(template: &str, segs: &[&str]) -> bool {
    let mut n = 0;
    for (i, t) in template.split('/').enumerate() {
        n = i + 1;
        let Some(s) = segs.get(i) else { return false };
        if t.starts_with('{') {
            if s.is_empty() {
                return false;
            }
        } else if t != *s {
            return false;
        }
    }
    n == segs.len()
}

/// True when `method path` needs owner presence. `path` is the whole request path
/// (`/api/...`); a query string is ignored. `HEAD` is read as `GET`.
///
/// A path that matches an owner template counts as an owner route even where the server has a
/// literal route of the same shape. No such pair exists today (the comparison test walks every
/// server route), and the server decides in any case: this list only chooses where the key is
/// sent.
pub fn owner_route(method: &Method, path: &str) -> bool {
    let method = if method == Method::HEAD { "GET" } else { method.as_str() };
    let path = path.split(['?', '#']).next().unwrap_or_default();
    let segs: Vec<&str> = path.split('/').collect();
    OWNER_ROUTES.iter().any(|(m, t)| *m == method && template_matches(t, &segs))
}

/// The claim socket: `<data_dir>/owner.sock`, else `<home>/.ember/owner.sock`. This is the
/// server's data folder (`EMBER_DATA_DIR` of the server, else `~/.ember`), found the way
/// [`crate::token::server_token`] finds the token file, not the app's own folder.
pub fn socket_path(data_dir: Option<&std::path::Path>, home: Option<&std::path::Path>) -> Option<std::path::PathBuf> {
    match (data_dir, home) {
        (Some(dir), _) => Some(dir.join(SOCKET_FILE)),
        (None, Some(home)) => Some(home.join(".ember").join(SOCKET_FILE)),
        (None, None) => None,
    }
}

/// [`socket_path`] over the process environment (`EMBER_DATA_DIR`, `HOME`).
pub fn socket_path_from_env() -> Option<std::path::PathBuf> {
    let var = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty()).map(std::path::PathBuf::from);
    socket_path(var("EMBER_DATA_DIR").as_deref(), var("HOME").as_deref())
}

/// An owner key. `Debug` never prints it.
#[derive(Clone, PartialEq, Eq)]
pub struct OwnerKey(String);

impl OwnerKey {
    pub fn new(key: impl Into<String>) -> OwnerKey {
        OwnerKey(key.into())
    }

    /// The key, for the `X-Ember-Owner` header. Never log it, never put it in an environment
    /// variable, an argument, a file or a URL.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for OwnerKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OwnerKey(hidden)")
    }
}

/// A line the server sent on the claim socket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnerMessage {
    /// The claim waits: show a field for the code on the server console
    /// ([`OwnerLink::confirm`]). With `holders > 0` an app that already holds a key may approve
    /// it instead.
    Confirm { claim: u64, holders: u64 },
    /// The code was wrong; `left` tries remain.
    WrongCode { left: u64 },
    /// The key. It is live until this link closes.
    Granted { claim: u64, key: OwnerKey },
    /// No key; the server closes the link. `reason` is one of `agent_tree`, `other_user`,
    /// `unknown_process`, `no_peer_pid`, `wrong_code`, `denied`, `timeout`, `busy`,
    /// `bad_request`, `server_stopping`.
    Refused { reason: String },
    /// To a holder: another claim waits. Show who asks and answer with
    /// [`OwnerLink::approve`] or [`OwnerLink::deny`].
    ClaimPending { claim: u64, pid: u64, exe: String, name: String },
    /// To a holder: that pending claim ended (confirmed, denied, timed out or gone).
    ClaimClosed { claim: u64 },
    /// To a holder: another claimant got a key. Show it, so a foreign claim is visible.
    OwnerClaimed { claim: u64, pid: u64, exe: String, name: String },
    /// To a holder: another key was voided.
    OwnerGone { claim: u64 },
    /// A line this client does not know (a newer server). Ignore it.
    Other,
}

impl OwnerMessage {
    /// Reads one line of the claim socket.
    pub fn parse(line: &str) -> Option<OwnerMessage> {
        let v: serde_json::Value = serde_json::from_str(line).ok()?;
        let n = |k: &str| v.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
        let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or_default().to_string();
        let kind = v.get("state").or_else(|| v.get("event")).and_then(|x| x.as_str())?;
        Some(match kind {
            "confirm" => OwnerMessage::Confirm { claim: n("claim"), holders: n("holders") },
            "wrong_code" => OwnerMessage::WrongCode { left: n("left") },
            "granted" => OwnerMessage::Granted { claim: n("claim"), key: OwnerKey(s("key")) },
            "refused" => OwnerMessage::Refused { reason: s("reason") },
            "claim_pending" => {
                OwnerMessage::ClaimPending { claim: n("claim"), pid: n("pid"), exe: s("exe"), name: s("name") }
            }
            "claim_closed" => OwnerMessage::ClaimClosed { claim: n("claim") },
            "owner_claimed" => {
                OwnerMessage::OwnerClaimed { claim: n("claim"), pid: n("pid"), exe: s("exe"), name: s("name") }
            }
            "owner_gone" => OwnerMessage::OwnerGone { claim: n("claim") },
            _ => OwnerMessage::Other,
        })
    }
}

#[cfg(unix)]
pub use link::OwnerLink;

#[cfg(unix)]
mod link {
    use std::io;
    use std::path::Path;

    use serde_json::json;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
    use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
    use tokio::net::UnixStream;

    use super::OwnerMessage;

    /// One claim connection to the server's `owner.sock`. The owner key it is granted lives
    /// exactly as long as this value: drop it and the server voids the key.
    ///
    /// ```text
    /// let mut link = OwnerLink::claim(&socket, "Ember").await?;
    /// loop {
    ///     match link.next().await? {
    ///         Some(OwnerMessage::Confirm { .. }) => { /* ask for the code, then link.confirm(code) */ }
    ///         Some(OwnerMessage::Granted { key, .. }) => api.set_owner_key(Some(&key)),
    ///         Some(OwnerMessage::ClaimPending { claim, .. }) => { /* ask, then link.approve(claim) */ }
    ///         Some(OwnerMessage::Refused { .. }) | None => { api.set_owner_key(None); break }
    ///         _ => {}
    ///     }
    /// }
    /// ```
    pub struct OwnerLink {
        lines: Lines<BufReader<OwnedReadHalf>>,
        w: OwnedWriteHalf,
    }

    impl std::fmt::Debug for OwnerLink {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("OwnerLink")
        }
    }

    impl OwnerLink {
        /// Connects to `socket` and sends the claim. `name` is what the owner is shown for this
        /// claimant (the app's name). The answer comes from [`OwnerLink::next`].
        pub async fn claim(socket: &Path, name: &str) -> io::Result<OwnerLink> {
            let (r, w) = UnixStream::connect(socket).await?.into_split();
            let mut link = OwnerLink { lines: BufReader::new(r).lines(), w };
            // A server that refuses at once may have closed already; its reason is still read.
            let _ = link.send(json!({ "op": "claim", "name": name })).await;
            Ok(link)
        }

        async fn send(&mut self, v: serde_json::Value) -> io::Result<()> {
            let mut line = v.to_string();
            line.push('\n');
            self.w.write_all(line.as_bytes()).await
        }

        /// The next message; `None` when the server closed the link (it stopped, or refused).
        /// The key is void from then on: clear it and claim again with a backoff. Safe to use
        /// in `tokio::select!`.
        pub async fn next(&mut self) -> io::Result<Option<OwnerMessage>> {
            loop {
                match self.lines.next_line().await? {
                    None => return Ok(None),
                    Some(line) => {
                        if let Some(m) = OwnerMessage::parse(&line) {
                            return Ok(Some(m));
                        }
                    }
                }
            }
        }

        /// Sends the code the server shows on its console. The answer is `Granted`, `WrongCode`
        /// or `Refused`, from [`OwnerLink::next`].
        pub async fn confirm(&mut self, code: &str) -> io::Result<()> {
            self.send(json!({ "op": "confirm", "code": code })).await
        }

        /// As a holder: lets the pending claim `claim` have a key.
        pub async fn approve(&mut self, claim: u64) -> io::Result<()> {
            self.send(json!({ "op": "approve", "claim": claim })).await
        }

        /// As a holder: refuses the pending claim `claim`.
        pub async fn deny(&mut self, claim: u64) -> io::Result<()> {
            self.send(json!({ "op": "deny", "claim": claim })).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr_s12_owner_routes_are_matched_by_template_and_method() {
        assert!(owner_route(&Method::POST, "/api/sessions/s1/approvals/a1"));
        assert!(owner_route(&Method::PUT, "/api/sessions/s1/approve-all"));
        assert!(owner_route(&Method::GET, "/api/devices"));
        assert!(owner_route(&Method::HEAD, "/api/devices"));
        assert!(owner_route(&Method::POST, "/api/computers/enrol-codes?x=1"));
        assert!(owner_route(&Method::POST, "/api/channels/c%201/invites"));
        assert!(!owner_route(&Method::GET, "/api/sessions"));
        assert!(!owner_route(&Method::POST, "/api/sessions/s1/messages"));
        assert!(!owner_route(&Method::POST, "/api/sessions/s1/questions/q1/answer"));
        assert!(!owner_route(&Method::GET, "/api/accounts"));
        assert!(!owner_route(&Method::POST, "/api/devices/"));
        assert!(!owner_route(&Method::POST, "/api/sessions//approvals/a1"));
    }

    #[test]
    fn fr_s12_the_socket_is_in_the_servers_data_folder() {
        use std::path::Path;
        assert_eq!(socket_path(Some(Path::new("/d")), Some(Path::new("/h"))), Some("/d/owner.sock".into()));
        assert_eq!(socket_path(None, Some(Path::new("/h"))), Some("/h/.ember/owner.sock".into()));
        assert_eq!(socket_path(None, None), None);
    }

    #[test]
    fn fr_s12_the_key_is_hidden_in_debug_output() {
        let m = OwnerMessage::parse(r#"{"state":"granted","claim":3,"key":"eok_secret"}"#).unwrap();
        assert!(!format!("{m:?}").contains("secret"));
        let OwnerMessage::Granted { claim, key } = m else { panic!("not granted") };
        assert_eq!((claim, key.expose()), (3, "eok_secret"));
    }

    #[test]
    fn fr_s12_socket_lines_are_parsed() {
        assert_eq!(
            OwnerMessage::parse(r#"{"state":"confirm","claim":7,"holders":1}"#),
            Some(OwnerMessage::Confirm { claim: 7, holders: 1 })
        );
        assert_eq!(OwnerMessage::parse(r#"{"state":"wrong_code","left":2}"#), Some(OwnerMessage::WrongCode { left: 2 }));
        assert_eq!(
            OwnerMessage::parse(r#"{"state":"refused","reason":"agent_tree"}"#),
            Some(OwnerMessage::Refused { reason: "agent_tree".into() })
        );
        assert_eq!(
            OwnerMessage::parse(r#"{"event":"claim_pending","claim":8,"pid":41,"exe":"/bin/x","name":"cli"}"#),
            Some(OwnerMessage::ClaimPending { claim: 8, pid: 41, exe: "/bin/x".into(), name: "cli".into() })
        );
        assert_eq!(OwnerMessage::parse(r#"{"event":"owner_gone","claim":8}"#), Some(OwnerMessage::OwnerGone { claim: 8 }));
        assert_eq!(OwnerMessage::parse(r#"{"state":"later"}"#), Some(OwnerMessage::Other));
        assert_eq!(OwnerMessage::parse("not json"), None);
    }
}
