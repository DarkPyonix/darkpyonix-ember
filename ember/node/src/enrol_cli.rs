//! `ember-node enrol <code> --server <url>` (SPEC `FR-C2h`, step N2 of the node enrol design, all
//! [provisional]): redeem a code at an ember server's enrol door, wait for the owner to confirm,
//! make the one trust decision the server cannot make for the node (R1 to R4 of INTENT D39), and
//! then write `enrol.json` (and, only when the server is trusted, one line of `allowed-peers`)
//! through [`crate::enrol::write_enrolment`].
//!
//! N2 is merged (#614, compiled and tested by CI). The follow-up N2b (the preflight before the
//! redeem, the Ctrl-C handling in the poll loop, the NotSynced output, the help wording and the
//! new tests in `ember/server/tests/fr_c2_enrol_node.rs`) is written and not yet compiled.
//!
//! Rules that hold on every path:
//! - Nothing is written before the trust decision, and an aborted run writes nothing at all.
//! - The code, the poll secret and the node token are never passed to [`EnrolIo::say`], to
//!   `tracing`, or into an error text. A response body is never quoted.
//! - One `reqwest` client: no redirects (the code and the poll secret must not follow a redirect
//!   to another host), no proxy (a proxy would see them in clear on http), a request timeout, and
//!   response bodies read with a hard cap of [`MAX_BODY_BYTES`].
//! - The state dir is locked ([`crate::enrol::lock_state_dir`]) before the first request and the
//!   lock is held to the end.
//! - A server name is never resolved here: resolving would decide on an answer the other side may
//!   control.

use std::io::{BufRead, IsTerminal, Write};
use std::net::{Ipv4Addr, Ipv6Addr};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, ensure, Context};
use serde::Deserialize;
use serde_json::json;

use crate::enrol::{is_peer_hex, lock_state_dir, preflight, validate, write_enrolment, Enrolment, WriteError, ENROL_FILE};

/// The most of a response body that is read (64 KiB).
pub const MAX_BODY_BYTES: usize = 64 * 1024;
/// The longest server URL accepted.
pub const MAX_URL_CHARS: usize = 256;
/// The door allows one poll per enrolment per 2 s; the default delay between polls.
pub const DEFAULT_POLL_EVERY: Duration = Duration::from_secs(2);
/// The default timeout of one request.
pub const DEFAULT_HTTP_TIMEOUT: Duration = Duration::from_secs(15);
/// The delay between polls never grows past this (after 429 answers).
const MAX_POLL_DELAY: Duration = Duration::from_secs(30);
/// Consecutive network errors a poll survives.
const MAX_NET_ERRORS: u32 = 5;
/// Added to the confirm deadline the server reported.
const DEADLINE_GRACE_MS: i64 = 30_000;
/// The overall wait never runs shorter or longer than this, whatever the server said (clock skew
/// between the two machines, or a server that names a far deadline).
const MIN_WAIT_MS: i64 = 60_000;
const MAX_WAIT_MS: i64 = 15 * 60_000;
/// The longest enrol id or poll secret the node accepts from the server.
const MAX_SECRET_CHARS: usize = 128;
/// The server caps the host name at 64 characters.
const MAX_HOSTNAME_CHARS: usize = 64;
const MAX_DISPLAY_NAME_CHARS: usize = 64;
const MAX_DISPLAY_ID_CHARS: usize = 128;

pub const USAGE: &str = "usage: ember-node enrol <code|-> --server <url> [--server-fingerprint <id>] \
[--trust-unverified-server] [--url <http://host:port>] [--transport] [--state-dir <dir>] [--yes] \
[--allow-http] [--replace]

A code given as an argument is visible in `ps` and in your shell history for as long as it lives
(it works once and expires after 10 minutes). Prefer `-`, which reads the code from one line of
stdin:
  printf '%s\\n' \"$CODE\" | ember-node enrol - --server <url> --server-fingerprint <id> --yes
With `-` stdin is a pipe, not a terminal: there is no \"Continue?\" prompt (pass --yes) and no
fingerprint question. Without --server-fingerprint (or --trust-unverified-server) the server's
transport id is then not added to allowed-peers; only the token over HTTP is set up.
Ctrl-C while waiting for the owner stops the command; the code is already redeemed by then, so
cancel the pending computer in Ember or let it expire. After the credential arrives, Ctrl-C is
ignored until the files are written: answer the fingerprint question with n, Enter or Ctrl-D.";

/// The exact text printed for every http server (design 4.2).
pub const PLAIN_HTTP_WARNING: &str = "This server speaks plain HTTP. Anyone on the network path can read the enrol code, the poll secret and the credential this node receives, and whoever reads the poll secret can fetch the credential before this node does. Use a network you trust, or an https address.";

/// The only answer to a refused code, whatever the reason.
pub const REFUSED_TEXT: &str = "this enrol code cannot be used";
const RATE_LIMITED_TEXT: &str = "too many attempts, wait and try again";
const TIMEOUT_TEXT: &str = "timed out waiting for the owner";
const FINGERPRINT_PROMPT: &str =
    "Does this match the fingerprint your Ember server prints at start and shows in the app? (n, Enter or Ctrl-D declines) [y/N] ";
const INTERRUPTED_TEXT: &str = "interrupted; nothing was written on this node. The code was already redeemed: ask the owner to cancel the pending computer in Ember, or let it expire.";
const UNVERIFIED_WARNING: &str = "Warning: this adds an unverified server to this node's allowed peers; the transport crosses NAT, so a rogue server could reach this node from anywhere.";

// ---- options and arguments ----------------------------------------------------------------

/// What `ember-node enrol` was asked to do. `Debug` leaves the code out.
#[derive(Clone)]
pub struct Options {
    /// The single-use enrol code. A secret: never printed.
    pub code: String,
    /// The enrol door's base URL.
    pub server: String,
    /// `--server-fingerprint`: the full 64-hex peer id (preferred) or the 16-hex short form.
    pub pin: Option<String>,
    /// `--trust-unverified-server`: trust the delivered id with no pin and no prompt.
    pub trust_unverified: bool,
    /// `--url`: how the server reaches this node.
    pub url: Option<String>,
    /// `--transport`: also report this node's peer id.
    pub transport: bool,
    pub state_dir: PathBuf,
    /// `--yes`: skips the "Continue?" prompt only; it never grants trust.
    pub yes: bool,
    /// `--allow-http`: plain http to a non-private address.
    pub allow_http: bool,
    /// `--replace`: overwrite an existing `enrol.json`.
    pub replace: bool,
    /// The delay between polls (the door allows one per 2 s).
    pub poll_every: Duration,
    /// The overall wait for the owner; `None` derives it from the server's `expires_ms`.
    pub deadline: Option<Duration>,
    pub http_timeout: Duration,
}

impl std::fmt::Debug for Options {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Options")
            .field("code", &"[redacted]")
            .field("server", &self.server)
            .field("pin", &self.pin)
            .field("trust_unverified", &self.trust_unverified)
            .field("url", &self.url)
            .field("transport", &self.transport)
            .field("state_dir", &self.state_dir)
            .field("yes", &self.yes)
            .field("allow_http", &self.allow_http)
            .field("replace", &self.replace)
            .field("poll_every", &self.poll_every)
            .field("deadline", &self.deadline)
            .field("http_timeout", &self.http_timeout)
            .finish()
    }
}

impl Options {
    /// The defaults for `code` at `server`, keeping state in `state_dir`.
    pub fn new(code: String, server: String, state_dir: PathBuf) -> Options {
        Options {
            code,
            server,
            pin: None,
            trust_unverified: false,
            url: None,
            transport: false,
            state_dir,
            yes: false,
            allow_http: false,
            replace: false,
            poll_every: DEFAULT_POLL_EVERY,
            deadline: None,
            http_timeout: DEFAULT_HTTP_TIMEOUT,
        }
    }
}

/// Parses the arguments after `enrol`. A code of `-` reads one line from stdin.
pub fn parse_args(args: &[String], default_state_dir: Option<PathBuf>) -> anyhow::Result<Options> {
    parse_args_with(args, default_state_dir, &mut || {
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line)?;
        Ok(line)
    })
}

/// [`parse_args`] with the stdin line supplied by `read_line` (tests).
pub(crate) fn parse_args_with(
    args: &[String],
    default_state_dir: Option<PathBuf>,
    read_line: &mut dyn FnMut() -> std::io::Result<String>,
) -> anyhow::Result<Options> {
    let mut code: Option<String> = None;
    let mut server: Option<String> = None;
    let mut pin: Option<String> = None;
    let mut url: Option<String> = None;
    let mut state_dir: Option<PathBuf> = None;
    let (mut trust, mut transport, mut yes, mut allow_http, mut replace) = (false, false, false, false, false);
    let mut seen: Vec<&str> = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let a = a.as_str();
        if a.starts_with('-') && a != "-" {
            ensure!(!seen.contains(&a), "{a} was given twice");
            match a {
                "--server" | "--server-fingerprint" | "--url" | "--state-dir" => {
                    let v = it
                        .next()
                        .filter(|v| !v.starts_with("--"))
                        .ok_or_else(|| anyhow!("{a} needs a value"))?
                        .clone();
                    match a {
                        "--server" => server = Some(v),
                        "--server-fingerprint" => pin = Some(v),
                        "--url" => url = Some(v),
                        _ => state_dir = Some(PathBuf::from(v)),
                    }
                }
                "--trust-unverified-server" => trust = true,
                "--transport" => transport = true,
                "--yes" => yes = true,
                "--allow-http" => allow_http = true,
                "--replace" => replace = true,
                other => bail!("unknown argument {other:?}\n{USAGE}"),
            }
            seen.push(a);
        } else {
            ensure!(code.is_none(), "unexpected extra argument\n{USAGE}");
            code = Some(if a == "-" {
                read_line().context("cannot read the enrol code from stdin")?.trim().to_string()
            } else {
                a.to_string()
            });
        }
    }
    let code = code.ok_or_else(|| anyhow!("the enrol code is missing\n{USAGE}"))?;
    ensure!(
        !code.is_empty() && code.len() <= 200 && code.bytes().all(|b| b.is_ascii_graphic()),
        "the enrol code is not valid"
    );
    let server = server.ok_or_else(|| anyhow!("--server is required\n{USAGE}"))?;
    if let Some(p) = &pin {
        parse_pin(p).context("--server-fingerprint")?;
    }
    let state_dir = state_dir
        .or(default_state_dir)
        .ok_or_else(|| anyhow!("no state dir (pass --state-dir, or set EMBER_NODE_STATE_DIR or HOME)"))?;
    let mut o = Options::new(code, server, state_dir);
    o.pin = pin;
    o.trust_unverified = trust;
    o.url = url;
    o.transport = transport;
    o.yes = yes;
    o.allow_http = allow_http;
    o.replace = replace;
    Ok(o)
}

// ---- pure rules -----------------------------------------------------------------------------

/// A server URL after validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerUrl {
    /// `scheme://host[:port]`, host lower-cased, no trailing slash.
    pub base: String,
    pub https: bool,
    /// A loopback, private, link-local or unique-local IP literal, `localhost`, or a `.local`
    /// name. A name is never resolved.
    pub private: bool,
}

enum Host {
    V4(Ipv4Addr),
    V6(Ipv6Addr),
    Name(String),
}

fn split_authority(a: &str) -> anyhow::Result<(Host, Option<u16>)> {
    ensure!(!a.is_empty(), "the URL has no host");
    let (host_str, bracketed, port_str): (&str, bool, Option<&str>) = if let Some(rest) = a.strip_prefix('[') {
        let end = rest.find(']').ok_or_else(|| anyhow!("the URL has an unclosed [ in its host"))?;
        let after = &rest[end + 1..];
        let port = if after.is_empty() {
            None
        } else {
            Some(after.strip_prefix(':').ok_or_else(|| anyhow!("the URL has text after its host"))?)
        };
        (&rest[..end], true, port)
    } else {
        match a.split_once(':') {
            Some((h, p)) => (h, false, Some(p)),
            None => (a, false, None),
        }
    };
    let port = match port_str {
        None => None,
        Some(p) => {
            ensure!(!p.is_empty() && p.len() <= 5 && p.bytes().all(|b| b.is_ascii_digit()), "the URL has a bad port");
            let n: u16 = p.parse().map_err(|_| anyhow!("the URL has a bad port"))?;
            ensure!(n != 0, "the URL has a bad port");
            Some(n)
        }
    };
    let host = if bracketed {
        Host::V6(host_str.parse::<Ipv6Addr>().map_err(|_| anyhow!("the URL has a bad IPv6 address"))?)
    } else if let Ok(v4) = host_str.parse::<Ipv4Addr>() {
        Host::V4(v4)
    } else {
        ensure!(!host_str.is_empty(), "the URL has no host");
        ensure!(
            !host_str.bytes().all(|b| b.is_ascii_digit() || b == b'.'),
            "the URL host looks like an IPv4 address but is not a valid one"
        );
        ensure!(host_str.len() <= 253, "the URL host is too long");
        ensure!(
            host_str.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_')),
            "the URL host has a character a host name cannot have"
        );
        ensure!(
            !host_str.starts_with(['.', '-']) && !host_str.ends_with(['.', '-']) && !host_str.contains(".."),
            "the URL host is malformed"
        );
        Host::Name(host_str.to_ascii_lowercase())
    };
    Ok((host, port))
}

fn is_private_v4(a: Ipv4Addr) -> bool {
    a.is_loopback() || a.is_private() || a.is_link_local()
}

fn is_private_v6(a: Ipv6Addr) -> bool {
    if let Some(v4) = a.to_ipv4_mapped() {
        return is_private_v4(v4);
    }
    let s0 = a.segments()[0];
    a.is_loopback() || (s0 & 0xffc0) == 0xfe80 || (s0 & 0xfe00) == 0xfc00
}

/// Validates a server URL (design section 2, step 1): http or https, a host, no user
/// information, no path beyond `/`, no query, no fragment, no control or space characters, at
/// most [`MAX_URL_CHARS`] characters. Errors never quote the URL.
pub(crate) fn classify_server_url(raw: &str) -> anyhow::Result<ServerUrl> {
    ensure!(!raw.is_empty() && raw.chars().count() <= MAX_URL_CHARS, "the URL must be 1 to {MAX_URL_CHARS} characters");
    ensure!(
        !raw.chars().any(|c| c.is_control() || c.is_whitespace()),
        "the URL has a control or space character"
    );
    let idx = raw.find("://").ok_or_else(|| anyhow!("the URL must start with http:// or https://"))?;
    let https = match raw[..idx].to_ascii_lowercase().as_str() {
        "https" => true,
        "http" => false,
        _ => bail!("the URL must start with http:// or https://"),
    };
    let rest = &raw[idx + 3..];
    ensure!(!rest.contains(['@']), "the URL must not contain user information");
    ensure!(!rest.contains(['?', '#']), "the URL must not have a query or a fragment");
    ensure!(!rest.contains('\\'), "the URL must not contain a backslash");
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    ensure!(path.is_empty() || path == "/", "the URL must not have a path");
    let (host, port) = split_authority(authority)?;
    let (host_text, private) = match &host {
        Host::V4(a) => (a.to_string(), is_private_v4(*a)),
        Host::V6(a) => (format!("[{a}]"), is_private_v6(*a)),
        Host::Name(n) => (n.clone(), n == "localhost" || n.ends_with(".local")),
    };
    let scheme = if https { "https" } else { "http" };
    let base = match port {
        Some(p) => format!("{scheme}://{host_text}:{p}"),
        None => format!("{scheme}://{host_text}"),
    };
    Ok(ServerUrl { base, https, private })
}

/// Plain http to a public host is refused unless `allow_http`. Checked before any request.
pub(crate) fn check_transport_policy(url: &ServerUrl, allow_http: bool) -> anyhow::Result<()> {
    if !url.https && !url.private && !allow_http {
        bail!("plain HTTP to a public address is refused; use an https address, or pass --allow-http");
    }
    Ok(())
}

/// The `--server-fingerprint` value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pin {
    /// 64 lowercase hex characters: the whole peer id.
    Full(String),
    /// The first 16 hex characters (64 bits), lower-cased and without spaces.
    Short(String),
}

pub(crate) fn parse_pin(raw: &str) -> anyhow::Result<Pin> {
    const SHAPE: &str = "a pin is the full 64 lowercase hex peer id, or the 16 hex short form (4 groups of 4)";
    let t = raw.trim();
    if is_peer_hex(t) {
        return Ok(Pin::Full(t.to_string()));
    }
    if t.contains(' ') {
        let parts: Vec<&str> = t.split(' ').collect();
        ensure!(parts.len() == 4 && parts.iter().all(|p| p.len() == 4), "{SHAPE}");
    }
    let compact: String = t.chars().filter(|c| *c != ' ').collect();
    ensure!(compact.len() == 16 && compact.bytes().all(|b| b.is_ascii_hexdigit()), "{SHAPE}");
    Ok(Pin::Short(compact.to_ascii_lowercase()))
}

/// The pin as four groups of four, for display.
fn pin_fingerprint(pin: &Pin) -> String {
    match pin {
        Pin::Full(p) => ember_transport::fingerprint_hex(p).unwrap_or_default(),
        Pin::Short(s) => (0..4).map(|i| &s[i * 4..i * 4 + 4]).collect::<Vec<_>>().join(" "),
    }
}

/// How the delivered peer id compares with the pin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinResult {
    /// Equal; `short` when only the 64-bit short form was compared.
    Match { short: bool },
    Mismatch,
    /// A pin was given and the server delivered no id: a pin cannot be satisfied.
    NoIdDelivered,
}

pub(crate) fn check_pin(delivered: Option<&str>, pin: &Pin) -> PinResult {
    let Some(id) = delivered else { return PinResult::NoIdDelivered };
    match pin {
        Pin::Full(p) => {
            if p == id {
                PinResult::Match { short: false }
            } else {
                PinResult::Mismatch
            }
        }
        Pin::Short(s) => match ember_transport::fingerprint_hex(id) {
            Some(fp) if fp.replace(' ', "") == *s => PinResult::Match { short: true },
            _ => PinResult::Mismatch,
        },
    }
}

/// What to do about the server's transport id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trust {
    /// Add the id to `allowed-peers`.
    Trusted,
    /// Write `enrol.json` only.
    NotTrusted,
    /// Write nothing.
    Abort,
}

/// R1 to R4. `pin` is the result of comparing a given pin, `None` when no pin was given (the
/// caller does not ask when the server delivered no id either: there is nothing to trust).
/// A mismatch or an undeliverable pin aborts; a match trusts. With no pin: interactive and not
/// `yes` asks (`answer` runs once, never otherwise); a non-interactive run, or `yes`, trusts only
/// with `trust_flag`. `yes` answers "Continue?" and never grants trust.
pub(crate) fn decide_trust(
    pin: Option<PinResult>,
    interactive: bool,
    trust_flag: bool,
    yes: bool,
    answer: impl FnOnce() -> bool,
) -> Trust {
    match pin {
        Some(PinResult::Match { .. }) => Trust::Trusted,
        Some(PinResult::Mismatch) | Some(PinResult::NoIdDelivered) => Trust::Abort,
        None => {
            if interactive && !yes {
                if answer() {
                    Trust::Trusted
                } else {
                    Trust::NotTrusted
                }
            } else if trust_flag {
                Trust::Trusted
            } else {
                Trust::NotTrusted
            }
        }
    }
}

/// Control, invisible format and line-separator characters (the server's own set for names).
fn is_forbidden_char(c: char) -> bool {
    let u = c as u32;
    c.is_control()
        || matches!(u,
            0x2028 | 0x2029
            | 0x00AD | 0x0600..=0x0605 | 0x061C | 0x06DD | 0x070F | 0x0890..=0x0891 | 0x08E2 | 0x180E
            | 0x200B..=0x200F | 0x202A..=0x202E | 0x2060..=0x2064 | 0x2066..=0x206F
            | 0xFEFF | 0xFFF9..=0xFFFB | 0x110BD | 0x110CD | 0x13430..=0x1343F | 0x1BCA0..=0x1BCA3
            | 0x1D173..=0x1D17A | 0xE0001 | 0xE0020..=0xE007F)
}

/// A server-supplied string made safe to print: forbidden characters removed, then at most `max`
/// characters.
pub(crate) fn strip_display(s: &str, max: usize) -> String {
    s.chars().filter(|c| !is_forbidden_char(*c)).take(max).collect()
}

fn wall_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// How long to wait for the owner, from the server's `expires_ms` (when it sent one).
fn wait_from_expiry(expires_ms: Option<i64>, now_ms: i64) -> Duration {
    let ms = expires_ms.map(|e| e.saturating_sub(now_ms).saturating_add(DEADLINE_GRACE_MS)).unwrap_or(10 * 60_000 + DEADLINE_GRACE_MS);
    Duration::from_millis(ms.clamp(MIN_WAIT_MS, MAX_WAIT_MS) as u64)
}

/// Up to 250 ms, from the clock (no random-number crate).
fn jitter() -> Duration {
    let n = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
    Duration::from_millis(u64::from(n % 250))
}

fn shape_ok_token(s: &str) -> bool {
    !s.is_empty() && s.chars().count() <= MAX_SECRET_CHARS && s.bytes().all(|b| b.is_ascii_graphic())
}

fn shape_ok_id(s: &str) -> bool {
    shape_ok_token(s) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

// ---- terminal -------------------------------------------------------------------------------

/// What the command needs from the terminal. Tests script it.
pub trait EnrolIo {
    /// Whether stdin and stdout are terminals (a person is there to answer).
    fn interactive(&self) -> bool;
    /// Prints `prompt` and reads one answer; true only for "y" or "yes".
    fn ask(&mut self, prompt: &str) -> bool;
    /// Prints one line. Never given a secret.
    fn say(&mut self, line: &str);
}

/// The real terminal.
pub struct StdIo;

impl EnrolIo for StdIo {
    fn interactive(&self) -> bool {
        std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
    }

    fn ask(&mut self, prompt: &str) -> bool {
        print!("{prompt}");
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        match std::io::stdin().lock().read_line(&mut line) {
            Ok(n) if n > 0 => matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes"),
            _ => false,
        }
    }

    fn say(&mut self, line: &str) {
        println!("{line}");
    }
}

// ---- network --------------------------------------------------------------------------------

enum BodyErr {
    TooLarge,
    Net(String),
}

/// A fixed text for a transport error: reqwest's own text can carry the URL.
fn net_err(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        "the request timed out".into()
    } else if e.is_connect() {
        "cannot connect to the server".into()
    } else {
        "the request failed".into()
    }
}

/// Reads the body in chunks, never holding more than [`MAX_BODY_BYTES`] plus one chunk.
async fn read_capped(mut resp: reqwest::Response) -> Result<Vec<u8>, BodyErr> {
    if resp.content_length().is_some_and(|n| n > MAX_BODY_BYTES as u64) {
        return Err(BodyErr::TooLarge);
    }
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(|e| BodyErr::Net(net_err(&e)))? {
        if buf.len() + chunk.len() > MAX_BODY_BYTES {
            return Err(BodyErr::TooLarge);
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

#[derive(Deserialize)]
struct RedeemAnswer {
    enrol_id: String,
    poll_secret: String,
    #[serde(default)]
    expires_ms: Option<i64>,
}

#[derive(Deserialize)]
struct Credential {
    node_token: String,
    name: String,
    computer_id: String,
    #[serde(default)]
    server_peer_id: Option<String>,
}

#[derive(Deserialize)]
struct PollAnswer {
    status: String,
    #[serde(default)]
    credential: Option<Credential>,
}

enum Polled {
    Answer(PollAnswer),
    NotFound,
    RateLimited,
    Transient(String),
}

async fn poll_once(client: &reqwest::Client, url: &str, secret: &str) -> anyhow::Result<Polled> {
    let resp = match client.get(url).bearer_auth(secret).send().await {
        Ok(r) => r,
        Err(e) => return Ok(Polled::Transient(net_err(&e))),
    };
    match resp.status().as_u16() {
        200 => {}
        404 => return Ok(Polled::NotFound),
        429 => return Ok(Polled::RateLimited),
        n => return Ok(Polled::Transient(format!("the server answered status {n}"))),
    }
    let body = match read_capped(resp).await {
        Ok(b) => b,
        Err(BodyErr::TooLarge) => bail!("the server's answer is too large"),
        Err(BodyErr::Net(m)) => return Ok(Polled::Transient(m)),
    };
    let answer: PollAnswer = serde_json::from_slice(&body).map_err(|_| anyhow!("the server's answer is not valid"))?;
    Ok(Polled::Answer(answer))
}

// ---- the command ----------------------------------------------------------------------------

/// What an enrol that finished left behind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// The computer name the owner chose (control characters removed).
    pub name: String,
    pub computer_id: String,
    /// The server's transport id was trusted (pin matched, owner confirmed it, or
    /// `--trust-unverified-server`).
    pub server_trusted: bool,
    /// The trusted id is listed in `allowed-peers` now.
    pub peer_added: bool,
}

/// The words for a failed final write (SPEC `FR-C2h`). The server consumed the credential when it
/// delivered it, so the token is lost unless it is on disk. Names the computer, never the token
/// or the cause text (which may hold paths only, but is not needed here).
fn write_failure_text(err: &WriteError, name: &str, computer_id: &str, enrol_path: &str) -> String {
    match err {
        WriteError::Refused(_) | WriteError::RolledBack(_) => format!(
            "Nothing was written. The server already handed over this computer's credential once and will not do it again: the token is lost. Remove the computer {name} ({computer_id}) in Ember and enrol again with a new code."
        ),
        WriteError::NewKept(_) => format!(
            "enrol.json now holds the NEW enrolment (the previous file could not be restored): the token is on disk; check {enrol_path}."
        ),
        WriteError::NotSynced(..) => "The files are written, but syncing the directory failed: the token is on disk and works now, but it may not survive a crash or power loss; run sync, or enrol again if the node loses it.".to_string(),
    }
}

/// An error whose text `run` already said through [`EnrolIo::say`]. `run` still returns the text
/// (tests and library callers read it); [`cli`] does not print it a second time.
#[derive(Debug)]
struct Said(String);

impl std::fmt::Display for Said {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Said {}

/// `ember-node enrol …`: parse, run on the real terminal.
pub async fn cli(args: &[String]) -> anyhow::Result<()> {
    let opts = parse_args(args, crate::config::default_state_dir())?;
    let mut io = StdIo;
    match run(opts, &mut io).await {
        Ok(_) => Ok(()),
        // The failure sentence was printed above; the exit status and `error:` line stay.
        Err(e) if e.downcast_ref::<Said>().is_some() => Err(anyhow!("the enrolment did not complete cleanly (see above)")),
        Err(e) => Err(e),
    }
}

/// The whole flow (design section 2). Everything before the final write leaves nothing on disk
/// (apart from the lock file and, with `--transport`, the node's own transport key).
pub async fn run(opts: Options, io: &mut dyn EnrolIo) -> anyhow::Result<Outcome> {
    // 1. Validate, before any file or network access.
    let server = classify_server_url(&opts.server).context("--server")?;
    let pin = opts.pin.as_deref().map(parse_pin).transpose().context("--server-fingerprint")?;
    let node_url = match opts.url.as_deref() {
        Some(u) => Some(classify_server_url(u).context("--url")?.base),
        None => None,
    };
    check_transport_policy(&server, opts.allow_http)?;

    // 2. Say what is about to happen, and ask.
    io.say(&format!("Server: {}", server.base));
    io.say(&format!("TLS: {}", if server.https { "yes" } else { "no" }));
    io.say("This node will accept commands from this server.");
    if !server.https {
        io.say(PLAIN_HTTP_WARNING);
    }
    let interactive = io.interactive();
    if !opts.yes {
        ensure!(interactive, "not running in a terminal: pass --yes to continue without the prompt");
        if !io.ask("Continue? [y/N] ") {
            bail!("cancelled; nothing was changed");
        }
    }

    // 3. Lock the state dir for the whole run, then look at an existing enrolment.
    let lock = lock_state_dir(&opts.state_dir)?;
    let mut replace = opts.replace;
    let enrol_path = opts.state_dir.join(ENROL_FILE);
    if std::fs::symlink_metadata(&enrol_path).is_ok() && !replace {
        if interactive {
            if io.ask(&format!("{} already exists. Replace it? [y/N] ", enrol_path.display())) {
                replace = true;
            } else {
                bail!("cancelled; {} was not changed", enrol_path.display());
            }
        } else {
            bail!("{} already exists; pass --replace to replace it", enrol_path.display());
        }
    }

    // Everything `write_enrolment` would refuse later is refused now: the server hands the
    // credential over once, so a refusal after the delivery loses the token.
    preflight(&lock, replace)?;

    // 4. What this node tells the server about itself.
    let mut hostname = strip_display(crate::envinfo::hostname().trim(), MAX_HOSTNAME_CHARS).trim().to_string();
    if hostname.is_empty() {
        hostname = "node".into();
    }
    let mut info = json!({
        "hostname": hostname,
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "node_version": env!("CARGO_PKG_VERSION"),
    });
    let mut own_fingerprint: Option<String> = None;
    if let Some(obj) = info.as_object_mut() {
        if let Some(u) = &node_url {
            obj.insert("url".into(), json!(u));
        }
        if opts.transport {
            let key = ember_transport::SecretKey::load_or_generate(opts.state_dir.join(crate::transport::KEY_FILE))
                .map_err(|e| anyhow!("cannot load the transport key: {e}"))?;
            let id = key.peer_id().to_string();
            own_fingerprint = ember_transport::fingerprint_hex(&id);
            obj.insert("peer_id".into(), json!(id));
        }
    }

    // 5. Redeem.
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .timeout(opts.http_timeout)
        .build()
        .map_err(|_| anyhow!("cannot build the HTTP client"))?;
    tracing::info!(server = %server.base, "redeeming an enrol code");
    let resp = client
        .post(format!("{}/enrol/redeem", server.base))
        .json(&json!({ "code": opts.code, "node_public_info": info }))
        .send()
        .await
        .map_err(|e| anyhow!("{}", net_err(&e)))?;
    match resp.status().as_u16() {
        200 => {}
        403 => bail!("{REFUSED_TEXT}"),
        429 => bail!("{RATE_LIMITED_TEXT}"),
        n => bail!("the server did not accept the request (status {n})"),
    }
    let body = match read_capped(resp).await {
        Ok(b) => b,
        Err(BodyErr::TooLarge) => bail!("the server's answer is too large"),
        Err(BodyErr::Net(m)) => bail!("{m}"),
    };
    let redeemed: RedeemAnswer =
        serde_json::from_slice(&body).map_err(|_| anyhow!("the server's answer to the redeem is not valid"))?;
    drop(body);
    ensure!(
        shape_ok_id(&redeemed.enrol_id) && shape_ok_token(&redeemed.poll_secret),
        "the server's answer to the redeem is not valid"
    );
    let wait = opts.deadline.unwrap_or_else(|| wait_from_expiry(redeemed.expires_ms, wall_ms()));

    // 6. Poll until the owner decides.
    match &own_fingerprint {
        Some(fp) => io.say(&format!(
            "Waiting for the owner to confirm this computer in Ember (fingerprint of this node: {fp})."
        )),
        None => io.say("Waiting for the owner to confirm this computer in Ember."),
    }
    // From here until the process ends Ctrl-C is handled by this listener and no longer kills the
    // process (registering a tokio signal listener replaces the default SIGINT action for the
    // rest of the process). While polling, the select below turns it into a clean exit. After the
    // delivery nothing reads it any more, so a Ctrl-C can no longer kill the process in the middle
    // of the write, which would lose the token; the cost is that the fingerprint question after
    // the delivery is declined with n, Enter or Ctrl-D (FINGERPRINT_PROMPT says so), not Ctrl-C.
    // `unix::signal` registers now (`ctrl_c()` would register on its first poll, and a signal
    // between two polls would be lost). Not covered by a test: it needs a signal sent to the test
    // process. The server cannot cancel the pending enrolment (no such route, issue #617).
    let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
        .map_err(|_| anyhow!("cannot listen for Ctrl-C"))?;
    let poll_url = format!("{}/enrol/status/{}", server.base, redeemed.enrol_id);
    let started = Instant::now();
    let mut delay = opts.poll_every;
    let mut net_errors = 0u32;
    let cred = loop {
        let elapsed = started.elapsed();
        if elapsed >= wait {
            bail!("{TIMEOUT_TEXT}");
        }
        let polled = tokio::select! {
            r = async {
                tokio::time::sleep((delay + jitter()).min(wait - elapsed)).await;
                if started.elapsed() >= wait {
                    bail!("{TIMEOUT_TEXT}");
                }
                poll_once(&client, &poll_url, &redeemed.poll_secret).await
            } => r?,
            _ = sigint.recv() => bail!("{INTERRUPTED_TEXT}"),
        };
        match polled {
            Polled::Answer(a) => {
                net_errors = 0;
                delay = opts.poll_every;
                match a.status.as_str() {
                    "waiting" | "awaiting_confirmation" => {}
                    "denied" => bail!("the owner denied this computer"),
                    "expired" => bail!("the enrolment expired before the owner confirmed it"),
                    "delivered" => bail!(
                        "the credential was already delivered; ask the owner to remove the computer in Ember and mint a new code"
                    ),
                    "joined" => match a.credential {
                        Some(c) => break c,
                        None => bail!("the server's answer is not valid"),
                    },
                    _ => bail!("the server's answer is not valid"),
                }
            }
            Polled::NotFound => bail!("the server does not know this enrolment (it may have expired)"),
            Polled::RateLimited => delay = (delay * 2).min(MAX_POLL_DELAY),
            Polled::Transient(msg) => {
                net_errors += 1;
                if net_errors >= MAX_NET_ERRORS {
                    bail!("lost contact with the server ({msg})");
                }
            }
        }
    };
    // The poll secret has done its job; it was never printed or logged.
    drop(redeemed);

    // 7. Trust decision, before anything is written.
    let name = strip_display(&cred.name, MAX_DISPLAY_NAME_CHARS);
    let computer_id = strip_display(&cred.computer_id, MAX_DISPLAY_ID_CHARS);
    let delivered_shape_ok = cred.server_peer_id.as_deref().map_or(true, is_peer_hex);
    let delivered: Option<String> = cred.server_peer_id.clone().filter(|_| delivered_shape_ok);
    let enrolment = Enrolment {
        server: server.base.clone(),
        node_token: cred.node_token,
        server_peer_id: delivered.clone(),
        name: name.clone(),
        computer_id: computer_id.clone(),
        enrolled_ms: wall_ms(),
    };
    if let Err(e) = validate(&enrolment) {
        io.say(&format!(
            "The server registered this computer as {name} ({computer_id}) but sent a credential this node cannot use; remove it in Ember."
        ));
        bail!("the server sent an invalid credential ({e}); nothing was written");
    }
    let delivered_fp = delivered.as_deref().and_then(ember_transport::fingerprint_hex);
    if let Some(fp) = &delivered_fp {
        io.say(&format!("The server's transport id: {fp}"));
    }
    let pin_result: Option<PinResult> = match (&pin, delivered_shape_ok) {
        (Some(_), false) => Some(PinResult::Mismatch),
        (Some(p), true) => Some(check_pin(delivered.as_deref(), p)),
        (None, _) => None,
    };
    let mut asked = false;
    let trust = if !delivered_shape_ok {
        Trust::Abort
    } else if pin.is_none() && delivered.is_none() {
        Trust::NotTrusted
    } else {
        decide_trust(pin_result, interactive, opts.trust_unverified, opts.yes, || {
            asked = true;
            io.ask(FINGERPRINT_PROMPT)
        })
    };

    match trust {
        Trust::Abort => {
            let what = if !delivered_shape_ok {
                "the server sent a malformed transport id"
            } else if matches!(pin_result, Some(PinResult::NoIdDelivered)) {
                "a pin was given but the server has no transport id"
            } else {
                "the server's transport id does not match the pin"
            };
            io.say(&format!("Pinned fingerprint:   {}", pin.as_ref().map(pin_fingerprint).unwrap_or_else(|| "(none)".into())));
            io.say(&format!(
                "Delivered fingerprint: {}",
                match (&delivered_fp, delivered_shape_ok) {
                    (Some(fp), _) => fp.clone(),
                    (None, false) => "(malformed)".into(),
                    (None, true) => "(none)".into(),
                }
            ));
            io.say(&format!(
                "The server registered this computer as {name} ({computer_id}) and handed over its credential once, but this node stored nothing. If this was the server you meant, remove it in Ember."
            ));
            bail!("{what}; nothing was written");
        }
        Trust::Trusted | Trust::NotTrusted => {}
    }

    // 8. Write.
    let trusted = trust == Trust::Trusted;
    let allow = if trusted { delivered.as_deref() } else { None };
    // The server wiped the credential when it handed it over, so every outcome other than a full
    // success says what happened to the token, in plain words (never the token itself).
    let mut sync_warning: Option<String> = None;
    let report = match write_enrolment(&lock, &enrolment, allow, replace) {
        Ok(r) => r,
        Err(e) => {
            let path = opts.state_dir.join(ENROL_FILE);
            let text = write_failure_text(&e, &name, &computer_id, &path.display().to_string());
            io.say(&text);
            match e {
                // Both files are in place: the node is set up, so the rest of the report follows
                // and the failure is returned at the end (said once, here, not again by `cli`).
                WriteError::NotSynced(_, report) => {
                    sync_warning = Some(text);
                    report
                }
                _ => return Err(anyhow::Error::new(Said(text))),
            }
        }
    };
    drop(enrolment);

    if sync_warning.is_none() {
        io.say(&format!("Enrolled as {name} ({computer_id})."));
    } else {
        io.say(&format!("Enrolled as {name} ({computer_id}), with the warning above."));
    }
    if trusted {
        if let PinResult::Match { short: true } = pin_result.unwrap_or(PinResult::Mismatch) {
            io.say("short pin: 64 bits (the full 64-hex id is a stronger pin)");
        }
        if pin.is_none() && !asked {
            io.say(UNVERIFIED_WARNING);
        }
        io.say(&format!(
            "This node accepts commands from server {} over the transport, and from anyone holding the token over HTTP at {}.",
            delivered_fp.clone().unwrap_or_default(),
            server.base
        ));
        if report.peer_added {
            io.say("Restart ember-node, or send it SIGHUP, to load the allowed peer.");
        }
    } else {
        io.say(&format!("This node accepts commands from anyone holding the token over HTTP at {}.", server.base));
        if let Some(id) = &delivered {
            io.say(&format!(
                "The server's transport id was not added. To add it, append this id to {} and restart ember-node, or send it SIGHUP: {id}",
                crate::transport::allowed_peers_file(&opts.state_dir).display()
            ));
        }
    }
    io.say("Start the node with: ember-node (add EMBER_NODE_TRANSPORT=1 to serve over the transport).");
    if let Some(text) = sync_warning {
        return Err(anyhow::Error::new(Said(text)));
    }
    Ok(Outcome { name, computer_id, server_trusted: trusted, peer_added: report.peer_added })
}

// ---- tests ----------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    fn id() -> String {
        ember_transport::SecretKey::generate().peer_id().to_string()
    }

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn no_stdin() -> impl FnMut() -> std::io::Result<String> {
        || panic!("stdin must not be read")
    }

    fn parse(v: &[&str]) -> anyhow::Result<Options> {
        parse_args_with(&args(v), Some(PathBuf::from("/state")), &mut no_stdin())
    }

    struct Script {
        interactive: bool,
        answers: VecDeque<bool>,
        lines: Vec<String>,
    }

    impl Script {
        fn new(interactive: bool, answers: &[bool]) -> Script {
            Script { interactive, answers: answers.iter().copied().collect(), lines: Vec::new() }
        }
    }

    impl EnrolIo for Script {
        fn interactive(&self) -> bool {
            self.interactive
        }
        fn ask(&mut self, prompt: &str) -> bool {
            self.lines.push(prompt.to_string());
            self.answers.pop_front().unwrap_or(false)
        }
        fn say(&mut self, line: &str) {
            self.lines.push(line.to_string());
        }
    }

    #[test]
    fn fr_c2_node_cli_classify_accepts_and_classifies_hosts() {
        // (url, base, https, private)
        let table = [
            ("http://10.0.0.5:8741", "http://10.0.0.5:8741", false, true),
            ("http://10.0.0.5:8741/", "http://10.0.0.5:8741", false, true),
            ("http://192.168.1.2", "http://192.168.1.2", false, true),
            ("http://169.254.1.1:1", "http://169.254.1.1:1", false, true),
            ("http://172.16.0.1", "http://172.16.0.1", false, true),
            ("http://172.31.255.255", "http://172.31.255.255", false, true),
            ("http://172.32.0.1", "http://172.32.0.1", false, false),
            ("http://127.0.0.1:9", "http://127.0.0.1:9", false, true),
            ("http://[::1]:8741", "http://[::1]:8741", false, true),
            ("http://[fe80::1]:8741", "http://[fe80::1]:8741", false, true),
            ("http://[fd12:3456::1]", "http://[fd12:3456::1]", false, true),
            ("http://[2001:db8::1]", "http://[2001:db8::1]", false, false),
            ("http://localhost:8741", "http://localhost:8741", false, true),
            ("http://LocalHost", "http://localhost", false, true),
            ("http://host.local", "http://host.local", false, true),
            ("https://example.com", "https://example.com", true, false),
            ("HTTPS://Example.COM:443/", "https://example.com:443", true, false),
            ("http://example.com", "http://example.com", false, false),
            ("http://8.8.8.8", "http://8.8.8.8", false, false),
        ];
        for (url, base, https, private) in table {
            let s = classify_server_url(url).unwrap_or_else(|e| panic!("{url}: {e}"));
            assert_eq!((s.base.as_str(), s.https, s.private), (base, https, private), "{url}");
        }
    }

    #[test]
    fn fr_c2_node_cli_classify_rejects_bad_urls_without_quoting_them() {
        let long = format!("https://{}.example.com", "a".repeat(300));
        let bad = [
            "",
            "ftp://example.com",
            "example.com",
            "https://",
            "https://user@example.com",
            "https://user:pw@example.com",
            "https://example.com/path",
            "https://example.com/a/",
            "https://example.com?x=1",
            "https://example.com/?x=1",
            "https://example.com#frag",
            "https://exa\u{7}mple.com",
            "https://example.com\n",
            "https://exa mple.com",
            "https://example.com:0",
            "https://example.com:99999",
            "https://example.com:",
            "https://example.com:80a",
            "https://[::1",
            "https://[::1]x",
            "https://[nope]",
            "https://127.1",
            "https://256.1.1.1",
            "https://exa_mple..com",
            "https://-bad.example.com",
            "https://example.com\\x",
            long.as_str(),
        ];
        for url in bad {
            let err = classify_server_url(url).expect_err(url);
            let text = format!("{err:#}");
            if url.len() > 3 {
                assert!(!text.contains(url), "{text}");
            }
        }
    }

    #[test]
    fn fr_c2_node_cli_parse_pin_accepts_the_full_id_and_the_short_forms() {
        let full = id();
        assert_eq!(parse_pin(&full).unwrap(), Pin::Full(full.clone()));
        assert_eq!(parse_pin(&format!(" {full} ")).unwrap(), Pin::Full(full.clone()));
        let short = &full[..16];
        let spaced = ember_transport::fingerprint_hex(&full).unwrap();
        assert_eq!(parse_pin(short).unwrap(), Pin::Short(short.to_string()));
        assert_eq!(parse_pin(&spaced).unwrap(), Pin::Short(short.to_string()));
        assert_eq!(parse_pin(&spaced.to_uppercase()).unwrap(), Pin::Short(short.to_string()));
        assert_eq!(parse_pin(&short.to_uppercase()).unwrap(), Pin::Short(short.to_string()));
    }

    #[test]
    fn fr_c2_node_cli_parse_pin_rejects_other_shapes() {
        let full = id();
        let bad: Vec<String> = vec![
            String::new(),
            "   ".to_string(),
            full[..63].to_string(),
            format!("{full}0"),
            "g".repeat(64),
            "g".repeat(16),
            full.to_uppercase(),
            "abab abab abab".to_string(),
            "abab  abab abab abab".to_string(),
            "aba babab abab abab".to_string(),
            format!("{} {}", &full[..8], &full[8..16]),
        ];
        for b in &bad {
            assert!(parse_pin(b).is_err(), "{b:?}");
        }
    }

    #[test]
    fn fr_c2_node_cli_check_pin_table() {
        let a = id();
        let b = id();
        let fa = ember_transport::fingerprint_hex(&a).unwrap().replace(' ', "");
        assert_eq!(check_pin(Some(&a), &Pin::Full(a.clone())), PinResult::Match { short: false });
        assert_eq!(check_pin(Some(&a), &Pin::Short(fa.clone())), PinResult::Match { short: true });
        assert_eq!(check_pin(Some(&a), &Pin::Full(b.clone())), PinResult::Mismatch);
        let fb = ember_transport::fingerprint_hex(&b).unwrap().replace(' ', "");
        assert_eq!(check_pin(Some(&a), &Pin::Short(fb)), PinResult::Mismatch);
        assert_eq!(check_pin(None, &Pin::Full(a.clone())), PinResult::NoIdDelivered);
        assert_eq!(check_pin(None, &Pin::Short(fa.clone())), PinResult::NoIdDelivered);
        assert_eq!(check_pin(Some("not an id"), &Pin::Short(fa)), PinResult::Mismatch);
    }

    #[test]
    fn fr_c2_node_cli_decide_trust_with_a_pin() {
        for interactive in [false, true] {
            for flag in [false, true] {
                for yes in [false, true] {
                    let never = || -> bool { panic!("a pin never asks") };
                    assert_eq!(
                        decide_trust(Some(PinResult::Match { short: false }), interactive, flag, yes, never),
                        Trust::Trusted
                    );
                    assert_eq!(
                        decide_trust(Some(PinResult::Match { short: true }), interactive, flag, yes, never),
                        Trust::Trusted
                    );
                    assert_eq!(decide_trust(Some(PinResult::Mismatch), interactive, flag, yes, never), Trust::Abort);
                    assert_eq!(decide_trust(Some(PinResult::NoIdDelivered), interactive, flag, yes, never), Trust::Abort);
                }
            }
        }
    }

    #[test]
    fn fr_c2_node_cli_decide_trust_without_a_pin() {
        // interactive and not yes: the answer decides, the flag does not.
        for flag in [false, true] {
            assert_eq!(decide_trust(None, true, flag, false, || true), Trust::Trusted);
            assert_eq!(decide_trust(None, true, flag, false, || false), Trust::NotTrusted);
        }
        // yes never trusts, and never asks.
        let never = || -> bool { panic!("must not ask") };
        assert_eq!(decide_trust(None, true, false, true, never), Trust::NotTrusted);
        assert_eq!(decide_trust(None, true, true, true, never), Trust::Trusted);
        // non-interactive: only the flag trusts, and nothing is asked.
        for yes in [false, true] {
            assert_eq!(decide_trust(None, false, false, yes, never), Trust::NotTrusted);
            assert_eq!(decide_trust(None, false, true, yes, never), Trust::Trusted);
        }
    }

    #[test]
    fn fr_c2_node_cli_parse_args_reads_every_flag() {
        let full = id();
        let o = parse(&[
            "dpe_code",
            "--server",
            "https://ember.example",
            "--server-fingerprint",
            &full,
            "--trust-unverified-server",
            "--url",
            "http://10.0.0.9:8741",
            "--transport",
            "--state-dir",
            "/tmp/node",
            "--yes",
            "--allow-http",
            "--replace",
        ])
        .unwrap();
        assert_eq!(o.code, "dpe_code");
        assert_eq!(o.server, "https://ember.example");
        assert_eq!(o.pin.as_deref(), Some(full.as_str()));
        assert!(o.trust_unverified && o.transport && o.yes && o.allow_http && o.replace);
        assert_eq!(o.url.as_deref(), Some("http://10.0.0.9:8741"));
        assert_eq!(o.state_dir, PathBuf::from("/tmp/node"));
        assert_eq!(o.poll_every, Duration::from_secs(2));
        assert_eq!(o.http_timeout, Duration::from_secs(15));
        assert!(o.deadline.is_none());
    }

    #[test]
    fn fr_c2_node_cli_parse_args_defaults() {
        let o = parse(&["--server", "http://10.0.0.1:8741", "dpe_code"]).unwrap();
        assert_eq!(o.state_dir, PathBuf::from("/state"));
        assert!(o.pin.is_none() && o.url.is_none());
        assert!(!o.trust_unverified && !o.transport && !o.yes && !o.allow_http && !o.replace);
        assert!(parse_args_with(&args(&["c", "--server", "http://10.0.0.1"]), None, &mut no_stdin()).is_err());
    }

    #[test]
    fn fr_c2_node_cli_parse_args_errors() {
        let full = id();
        let cases: Vec<Vec<&str>> = vec![
            vec!["c", "--server", "http://10.0.0.1", "--bogus"],
            vec!["c", "--server", "http://10.0.0.1", "-x"],
            vec!["c", "--server", "http://10.0.0.1", "--yes", "--yes"],
            vec!["c", "--server", "http://10.0.0.1", "--server", "http://10.0.0.2"],
            vec!["c", "--server"],
            vec!["c", "--server", "--yes"],
            vec!["c", "--server", "http://10.0.0.1", "--state-dir"],
            vec!["--server", "http://10.0.0.1"],
            vec!["c"],
            vec!["c", "d", "--server", "http://10.0.0.1"],
            vec!["c", "--server", "http://10.0.0.1", "--server-fingerprint", "xyz"],
            vec!["c", "--server", "http://10.0.0.1", "--server-fingerprint", &full[..63]],
        ];
        for c in cases {
            assert!(parse(&c).is_err(), "{c:?}");
        }
    }

    #[test]
    fn fr_c2_node_cli_parse_args_dash_is_only_the_stdin_marker() {
        let mut calls = 0;
        let o = parse_args_with(
            &args(&["-", "--server", "http://10.0.0.1"]),
            Some(PathBuf::from("/s")),
            &mut || {
                calls += 1;
                Ok("dpe_fromstdin\n".to_string())
            },
        )
        .unwrap();
        assert_eq!(o.code, "dpe_fromstdin");
        assert_eq!(calls, 1);
        // As a flag value it is a plain value, and stdin is not read.
        let o = parse(&["c", "--server", "http://10.0.0.1", "--url", "-"]).unwrap();
        assert_eq!(o.url.as_deref(), Some("-"));
        // An empty line is no code.
        let r = parse_args_with(&args(&["-", "--server", "http://10.0.0.1"]), Some(PathBuf::from("/s")), &mut || {
            Ok("\n".to_string())
        });
        assert!(r.is_err());
        // Two codes are an error even when one is the marker.
        let r = parse_args_with(&args(&["-", "c", "--server", "http://10.0.0.1"]), Some(PathBuf::from("/s")), &mut || {
            Ok("x\n".to_string())
        });
        assert!(r.is_err());
    }

    #[test]
    fn fr_c2_node_cli_strip_display_removes_controls_and_caps_the_length() {
        assert_eq!(strip_display("build box", 64), "build box");
        assert_eq!(strip_display("a\nb\x1b[31mc\0d\te", 64), "ab[31mcde");
        assert_eq!(strip_display("a\u{202e}b\u{200b}c\u{2028}d\u{feff}e", 64), "abcde");
        assert_eq!(strip_display(&"x".repeat(200), 64).chars().count(), 64);
        assert_eq!(strip_display("\u{ac00}\u{ac01}\u{ac02}", 2), "\u{ac00}\u{ac01}");
        assert_eq!(strip_display("", 5), "");
    }

    #[test]
    fn fr_c2_node_cli_plain_http_to_a_public_host_is_refused_unless_allowed() {
        let public_http = classify_server_url("http://ember.example:1").unwrap();
        let err = check_transport_policy(&public_http, false).unwrap_err().to_string();
        assert!(err.contains("plain HTTP") && err.contains("--allow-http"), "{err}");
        check_transport_policy(&public_http, true).unwrap();
        check_transport_policy(&classify_server_url("https://ember.example").unwrap(), false).unwrap();
        check_transport_policy(&classify_server_url("http://10.0.0.5").unwrap(), false).unwrap();
        check_transport_policy(&classify_server_url("http://localhost:1").unwrap(), false).unwrap();
    }

    #[test]
    fn fr_c2_node_cli_the_plain_http_warning_says_plain_http() {
        assert!(PLAIN_HTTP_WARNING.contains("plain HTTP"));
        assert!(PLAIN_HTTP_WARNING.contains("poll secret") && PLAIN_HTTP_WARNING.contains("https address"));
        assert!(!PLAIN_HTTP_WARNING.contains('\u{2014}'));
    }

    #[test]
    fn fr_c2_node_cli_wait_from_expiry_is_clamped() {
        let now = 1_000_000_000_000;
        assert_eq!(wait_from_expiry(Some(now + 600_000), now), Duration::from_millis(630_000));
        assert_eq!(wait_from_expiry(None, now), Duration::from_millis(630_000));
        assert_eq!(wait_from_expiry(Some(now - 10_000_000), now), Duration::from_millis(60_000));
        assert_eq!(wait_from_expiry(Some(now + 10_000_000_000), now), Duration::from_millis(15 * 60_000));
    }

    #[tokio::test]
    async fn fr_c2_node_cli_run_refuses_plain_http_to_a_public_host_before_touching_anything() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        let mut o = Options::new("dpe_x".into(), "http://ember.example:1".into(), state.clone());
        o.yes = true;
        let mut io = Script::new(false, &[]);
        let err = run(o, &mut io).await.unwrap_err().to_string();
        assert!(err.contains("plain HTTP"), "{err}");
        assert!(!state.exists(), "not even the state dir or the lock file is made");
        assert!(io.lines.is_empty(), "nothing is said before the refusal: {:?}", io.lines);
    }

    #[tokio::test]
    async fn fr_c2_node_cli_run_needs_yes_when_there_is_no_terminal() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        let o = Options::new("dpe_x".into(), "http://127.0.0.1:1".into(), state.clone());
        let mut io = Script::new(false, &[]);
        let err = run(o, &mut io).await.unwrap_err().to_string();
        assert!(err.contains("--yes"), "{err}");
        assert!(!state.exists());
        // The warning and the facts were shown before the refusal.
        let said = io.lines.join("\n");
        assert!(said.contains("plain HTTP") && said.contains("TLS: no") && said.contains("accept commands"), "{said}");
    }

    #[tokio::test]
    async fn fr_c2_node_cli_run_a_declined_prompt_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        let o = Options::new("dpe_x".into(), "https://ember.example".into(), state.clone());
        let mut io = Script::new(true, &[false]);
        let err = run(o, &mut io).await.unwrap_err().to_string();
        assert!(err.contains("cancelled"), "{err}");
        assert!(!state.exists());
    }

    #[tokio::test]
    async fn fr_c2_node_cli_run_refuses_an_existing_enrol_json_without_replace_before_any_request() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(ENROL_FILE);
        std::fs::write(&path, b"keep me").unwrap();
        let mut o = Options::new("dpe_x".into(), "http://127.0.0.1:1".into(), dir.path().to_path_buf());
        o.yes = true;
        let mut io = Script::new(false, &[]);
        let err = run(o, &mut io).await.unwrap_err().to_string();
        assert!(err.contains("--replace"), "{err}");
        assert_eq!(std::fs::read(&path).unwrap(), b"keep me");
    }

    #[tokio::test]
    async fn fr_c2_node_cli_run_does_not_say_the_code() {
        let dir = tempfile::tempdir().unwrap();
        let o = Options::new("dpe_SECRETCODE".into(), "http://ember.example:1".into(), dir.path().to_path_buf());
        let mut io = Script::new(true, &[false]);
        let err = format!("{:#}", run(o, &mut io).await.unwrap_err());
        assert!(!err.contains("SECRETCODE") && !io.lines.join("\n").contains("SECRETCODE"));
    }

    /// The port is closed: reaching the network would give "cannot connect". The preflight error
    /// must come first.
    #[tokio::test]
    async fn fr_c2_node_cli_run_preflight_refuses_before_any_request() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("real");
        std::fs::write(&target, b"").unwrap();
        std::os::unix::fs::symlink(&target, dir.path().join("allowed-peers")).unwrap();
        let mut o = Options::new("dpe_x".into(), "http://127.0.0.1:1".into(), dir.path().to_path_buf());
        o.yes = true;
        let mut io = Script::new(false, &[]);
        let err = format!("{:#}", run(o, &mut io).await.unwrap_err());
        assert!(err.contains("symbolic link") && err.contains("nothing was redeemed"), "{err}");
        assert!(!err.contains("cannot connect"), "{err}");
        assert!(!dir.path().join(ENROL_FILE).exists());
    }

    #[tokio::test]
    async fn fr_c2_node_cli_run_preflight_refuses_an_unreadable_enrol_json_with_replace() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("elsewhere");
        std::fs::write(&target, b"{}").unwrap();
        std::os::unix::fs::symlink(&target, dir.path().join(ENROL_FILE)).unwrap();
        let mut o = Options::new("dpe_x".into(), "http://127.0.0.1:1".into(), dir.path().to_path_buf());
        o.yes = true;
        o.replace = true;
        let mut io = Script::new(false, &[]);
        let err = format!("{:#}", run(o, &mut io).await.unwrap_err());
        assert!(err.contains("symbolic link"), "{err}");
        assert!(!err.contains("cannot connect"), "{err}");
    }

    #[test]
    fn fr_c2_node_cli_the_help_and_the_prompt_say_what_to_do() {
        assert!(FINGERPRINT_PROMPT.starts_with("Does this match the fingerprint"));
        assert!(FINGERPRINT_PROMPT.contains("(n, Enter or Ctrl-D declines)"));
        assert!(USAGE.contains("visible in `ps`") && USAGE.contains("printf") && USAGE.contains("--yes"));
        assert!(INTERRUPTED_TEXT.contains("nothing was written") && INTERRUPTED_TEXT.contains("cancel the pending computer"));
        assert!(!USAGE.contains("--forget"));
    }

    // Ctrl-C in the poll loop (INTERRUPTED_TEXT) is not unit tested: it needs a SIGINT sent to the
    // test process, which would also reach the other tests of the binary.

    #[test]
    fn fr_c2_node_cli_said_errors_are_recognised_by_cli() {
        let e = anyhow::Error::new(Said("x".into()));
        assert!(e.downcast_ref::<Said>().is_some());
        assert_eq!(format!("{e:#}"), "x");
    }
}
