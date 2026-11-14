//! Owner presence, client side (SPEC `FR-S12`; INTENT D37 "Owner presence", #784).
//!
//! The server token is the non-owner credential. The routes that grant authority, admit a party
//! or change a secret ([`owner_route`]) also need an owner key: `eok_...`, claimed over the
//! server's unix socket `<data dir>/owner.sock`, or on Windows its named pipe
//! `\\.\pipe\ember-owner-<hash>` ([`OwnerEndpoint`], [`OwnerLink`]), and sent as `X-Ember-Owner`.
//! Without it the server answers 403 `{"code": "owner_required"}`
//! ([`crate::api::ApiError::is_owner_required`]).
//!
//! The key lives in memory only, and only while the claim connection stays open. It has no
//! environment variable and no file: a child process the app starts would inherit a variable,
//! and an agent can read a file. An [`crate::api::Api`] given the key
//! ([`crate::api::Api::set_owner_key`]) sends it on owner routes over plain HTTP only. Over the
//! transport there is no owner key: the device key is the credential, and an owner route needs
//! a device with scope `full`.

use std::path::{Path, PathBuf};

use http::Method;
use sha2::{Digest, Sha256};

/// The header that carries the owner key.
pub const OWNER_HEADER: &str = "x-ember-owner";
/// The claim socket's name in the server's data folder.
pub const SOCKET_FILE: &str = "owner.sock";
/// The start of the claim pipe's name on Windows; [`pipe_name`] adds the data folder's hash.
pub const PIPE_PREFIX: &str = r"\\.\pipe\ember-owner-";

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

/// The server's data folder: `data_dir`, else `<home>/.ember` (`EMBER_DATA_DIR` of the server,
/// else `~/.ember`), found the way [`crate::token::server_token`] finds the token file, not the
/// app's own folder.
fn server_dir(data_dir: Option<&Path>, home: Option<&Path>) -> Option<PathBuf> {
    match (data_dir, home) {
        (Some(dir), _) => Some(dir.to_path_buf()),
        (None, Some(home)) => Some(home.join(".ember")),
        (None, None) => None,
    }
}

/// The claim socket: `<data_dir>/owner.sock`, else `<home>/.ember/owner.sock` (unix).
pub fn socket_path(data_dir: Option<&Path>, home: Option<&Path>) -> Option<PathBuf> {
    server_dir(data_dir, home).map(|d| d.join(SOCKET_FILE))
}

/// [`socket_path`] over the process environment (`EMBER_DATA_DIR`, `HOME`).
pub fn socket_path_from_env() -> Option<PathBuf> {
    socket_path(env_dir("EMBER_DATA_DIR").as_deref(), env_dir("HOME").as_deref())
}

fn env_dir(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// The claim pipe for the data folder whose canonical path is `canonical` (Windows):
/// [`PIPE_PREFIX`] plus the first 8 bytes of the SHA-256 of the lower-cased path, in hex. The
/// server's `owner::pipe_name_of` is the same function; the test
/// `fr_s12_client_pipe_name_matches_the_server` compares the two.
pub fn pipe_name_of(canonical: &str) -> String {
    let digest = Sha256::digest(canonical.to_lowercase().as_bytes());
    let mut s = String::from(PIPE_PREFIX);
    for b in &digest[..8] {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// The claim pipe of `data_dir`: [`pipe_name_of`] its canonical path, or of the path as given
/// when it cannot be canonicalised.
pub fn pipe_name(data_dir: &Path) -> String {
    let dir = std::fs::canonicalize(data_dir).unwrap_or_else(|_| data_dir.to_path_buf());
    pipe_name_of(&dir.to_string_lossy())
}

/// Where the server takes claims.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnerEndpoint {
    /// Unix: the socket `<data dir>/owner.sock`.
    Socket(PathBuf),
    /// Windows: the named pipe `\\.\pipe\ember-owner-<hash>` ([`pipe_name`]).
    Pipe(String),
}

impl OwnerEndpoint {
    /// This platform's endpoint for the server whose data folder is `data_dir`, else
    /// `<home>/.ember`.
    pub fn for_dir(data_dir: Option<&Path>, home: Option<&Path>) -> Option<OwnerEndpoint> {
        let dir = server_dir(data_dir, home)?;
        Some(if cfg!(windows) { OwnerEndpoint::Pipe(pipe_name(&dir)) } else { OwnerEndpoint::Socket(dir.join(SOCKET_FILE)) })
    }

    /// [`OwnerEndpoint::for_dir`] over the process environment (`EMBER_DATA_DIR`, `HOME`), the
    /// variables the server reads for its own folder.
    pub fn from_env() -> Option<OwnerEndpoint> {
        OwnerEndpoint::for_dir(env_dir("EMBER_DATA_DIR").as_deref(), env_dir("HOME").as_deref())
    }
}

impl std::fmt::Display for OwnerEndpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OwnerEndpoint::Socket(p) => write!(f, "{}", p.display()),
            OwnerEndpoint::Pipe(n) => f.write_str(n),
        }
    }
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

#[cfg(any(unix, windows))]
pub use link::OwnerLink;

#[cfg(any(unix, windows))]
mod link {
    use std::io;
    use std::pin::Pin;

    use serde_json::json;
    use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, Lines};

    use super::{OwnerEndpoint, OwnerMessage};

    type Reader = Pin<Box<dyn AsyncRead + Send + Sync>>;
    type Writer = Pin<Box<dyn AsyncWrite + Send + Sync>>;

    /// `ERROR_PIPE_BUSY`: every pipe instance is taken; the server makes a new one at once.
    #[cfg(windows)]
    const ERROR_PIPE_BUSY: i32 = 231;

    #[cfg(unix)]
    async fn connect(endpoint: &OwnerEndpoint) -> io::Result<(Reader, Writer)> {
        match endpoint {
            OwnerEndpoint::Socket(path) => {
                let (r, w) = tokio::net::UnixStream::connect(path).await?.into_split();
                let (r, w): (Reader, Writer) = (Box::pin(r), Box::pin(w));
                Ok((r, w))
            }
            OwnerEndpoint::Pipe(_) => {
                Err(io::Error::new(io::ErrorKind::Unsupported, "a named pipe is a Windows endpoint"))
            }
        }
    }

    #[cfg(windows)]
    async fn connect(endpoint: &OwnerEndpoint) -> io::Result<(Reader, Writer)> {
        use tokio::net::windows::named_pipe::ClientOptions;
        let OwnerEndpoint::Pipe(name) = endpoint else {
            return Err(io::Error::new(io::ErrorKind::Unsupported, "a unix socket is not a Windows endpoint"));
        };
        // ClientOptions asks for SECURITY_IDENTIFICATION by default: the server learns who
        // connects but cannot act as this process.
        let mut tries = 0;
        let pipe = loop {
            match ClientOptions::new().open(name) {
                Ok(p) => break p,
                Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) && tries < 100 => {
                    tries += 1;
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
                Err(e) => return Err(e),
            }
        };
        let (r, w) = tokio::io::split(pipe);
        let (r, w): (Reader, Writer) = (Box::pin(r), Box::pin(w));
        Ok((r, w))
    }

    /// One claim connection to the server's claim door ([`OwnerEndpoint`]). The owner key it
    /// is granted lives exactly as long as this value: drop it and the server voids the key.
    ///
    /// ```text
    /// let mut link = OwnerLink::claim(&OwnerEndpoint::from_env()?, "Ember").await?;
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
        lines: Lines<BufReader<Reader>>,
        w: Writer,
    }

    impl std::fmt::Debug for OwnerLink {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("OwnerLink")
        }
    }

    impl OwnerLink {
        /// Connects to `endpoint` and sends the claim. `name` is what the owner is shown for
        /// this claimant (the app's name). The answer comes from [`OwnerLink::next`].
        pub async fn claim(endpoint: &OwnerEndpoint, name: &str) -> io::Result<OwnerLink> {
            let (r, w) = connect(endpoint).await?;
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
    fn fr_s12_the_endpoint_is_this_platforms_door() {
        let dir = tempfile::tempdir().unwrap();
        let got = OwnerEndpoint::for_dir(Some(dir.path()), Some(Path::new("/h"))).unwrap();
        if cfg!(windows) {
            assert_eq!(got, OwnerEndpoint::Pipe(pipe_name(dir.path())));
        } else {
            assert_eq!(got, OwnerEndpoint::Socket(dir.path().join(SOCKET_FILE)));
        }
        assert_eq!(OwnerEndpoint::for_dir(None, None), None);
        let p = pipe_name_of(r"\\?\C:\Users\me\.ember");
        assert_eq!(p.len(), PIPE_PREFIX.len() + 16);
        assert_eq!(p, pipe_name_of(r"\\?\c:\users\ME\.ember"));
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
