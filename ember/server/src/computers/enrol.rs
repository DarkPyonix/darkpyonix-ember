//! One-time enrol codes and confirm at join for adding a computer (SPEC FR-C1, FR-C2, FR-C2a to
//! FR-C2d, FR-C2f, FR-C2g, `INTENT.md` D39 "Confirm at join", all [provisional]).
//!
//! What is here: minting a code from the local admin router, the redeem core (which now only
//! opens an enrolment and hands back a poll secret), the owner's confirm and deny, the node's
//! one-time delivery of its credential, the status routes, and two failure limiters. What is not
//! here: the enrol door (a listener of its own), the redeem and poll routes on it, and
//! `ember-node enrol` (build order steps 2 to 4 in `docs/design/COMPUTERS.md`). The core is
//! driven directly by tests until then.
//!
//! # States (FR-C2a)
//!
//! ```text
//! waiting --redeem--> awaiting_confirmation --confirm--> confirmed --delivery--> joined
//!    |                    |    \--deny--> denied              \--lost or late--> expired
//!    \--code ttl--> expired \--deadline--> expired
//! ```
//!
//! [`State::TRANSITIONS`] is the whole table; [`Enrol::transition`] refuses any other move with
//! [`EnrolError::IllegalTransition`]. `confirmed` is internal: the admin still reads
//! `awaiting_confirmation` ([`EnrolStatus`]) until the node has received its credential. The
//! deadline is [`CONFIRM_WINDOW_MS`] after the redeem (equal to the code lifetime). A `confirmed`
//! enrolment whose credential is not taken within [`DELIVERY_GRACE_MS`] of the confirm, or whose
//! credential is no longer in memory, becomes `expired`.
//!
//! # Secrets
//!
//! A code is `dpe_` plus 244 random bits, made like the channel invite secrets
//! ([`crate::channels::invites::new_secret`], two v4 UUIDs from the OS CSPRNG). Only the SHA-256
//! of the code is stored, as a BLOB. Redeem looks the digest up in a UNIQUE index inside SQLite,
//! so no secret is compared byte by byte in this code.
//!
//! The poll secret (`dps_`, three v4 UUIDs, 366 bits) is made at redeem, returned to the caller
//! once in [`Redeemed`], and stored only as its SHA-256. [`Enrol::take_delivery`] hashes the
//! presented secret and compares the digests without an early exit, and does the same work for
//! an unknown enrol id, so a wrong secret and an unknown id answer alike ([`Delivery::Unknown`]).
//!
//! The node token (`dpn_`, 366 bits) is minted by the server at confirm, never before, and the
//! node never chooses it. The registry keeps it in clear (it presents it as a bearer, like every
//! computer). The pending copy for delivery is kept in MEMORY only (`Enrol::pending`), never in
//! SQLite: it is handed to the node once by [`Enrol::take_delivery`] and wiped. A server restart
//! between the confirm and the delivery loses it; the next look at the enrolment reads `expired`,
//! the computer stays registered with a token nobody holds, and the owner removes it and mints a
//! new code. There is no retry path (SPEC FR-C2d).
//!
//! Nothing secret reaches a log: [`EnrolCode`] and [`Secret`] print a redacted `Debug`, and so do
//! the types that hold them ([`Minted`], [`Redeemed`], [`Delivery`]).
//!
//! # Untrusted display data
//!
//! [`NodeInfo`] (the host name and other facts a node reports) and the observed source address
//! are written by the other machine or by the network. They are length-capped and cleaned of
//! control and format characters, but they stay untrusted: the app MUST render them as plain
//! text, never as HTML or markup, and nothing in the core decides anything from them except the
//! address a confirmed computer is registered at.
//!
//! # Refusal and limits
//!
//! An unknown, used, expired, cancelled or malformed code all return the same
//! [`Refused::Generic`] from one `UPDATE ... WHERE used_ms IS NULL AND expires_ms > now`. Before
//! the code is looked at, a redeem is refused with [`Refused::RateLimited`] when its source key
//! has failed [`LimitConfig::per_source_failures`] times within the window, or when ALL sources
//! together have failed [`LimitConfig::global_failures`] times within the global window
//! (FR-C2f). The global counter is a separate queue that the per-source map's eviction cannot
//! touch, so rotating source keys does not reset it. A success spends neither budget.

use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use axum::extract::{Path, State as AxumState};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use super::{Computers, LOCAL, LOCAL_NAME};
use crate::channels::invites::{is_forbidden_name_char, new_secret};
use crate::store::Store;

/// Store migration (appended to `store::MIGRATIONS`): enrol codes. `code_hash` is the SHA-256
/// digest (32 bytes); the code itself is never stored. Times are Unix milliseconds.
pub const MIGRATION: &str = "
CREATE TABLE enrol_codes (
    id          TEXT PRIMARY KEY,
    code_hash   BLOB NOT NULL UNIQUE,
    name        TEXT NOT NULL,
    created_ms  INTEGER NOT NULL,
    expires_ms  INTEGER NOT NULL,
    used_ms     INTEGER,
    computer_id TEXT
);
";

/// Store migration (appended to `store::MIGRATIONS` after [`MIGRATION`]): confirm at join
/// (FR-C2a to FR-C2d). Adds the state and what the redeem and the confirm record. `poll_hash` is
/// the SHA-256 of the poll secret (the secret is never stored); `node_info` is the JSON of the
/// validated [`NodeInfo`], untrusted display data; there is deliberately NO column for the node
/// token: the pending token lives in memory only. Rows that existed before are settled: a
/// completed one is `joined`, a redeemed but incomplete one is `expired` (it has no poll
/// secret, so it could never be confirmed), an unredeemed one stays `waiting`.
pub const CONFIRM_MIGRATION: &str = "
ALTER TABLE enrol_codes ADD COLUMN state TEXT NOT NULL DEFAULT 'waiting';
ALTER TABLE enrol_codes ADD COLUMN poll_hash BLOB;
ALTER TABLE enrol_codes ADD COLUMN node_info TEXT;
ALTER TABLE enrol_codes ADD COLUMN observed_addr TEXT;
ALTER TABLE enrol_codes ADD COLUMN confirm_deadline_ms INTEGER;
ALTER TABLE enrol_codes ADD COLUMN confirmed_ms INTEGER;
ALTER TABLE enrol_codes ADD COLUMN delivered_ms INTEGER;
ALTER TABLE enrol_codes ADD COLUMN denied_ms INTEGER;
ALTER TABLE enrol_codes ADD COLUMN final_name TEXT;
UPDATE enrol_codes SET state = 'joined' WHERE computer_id IS NOT NULL;
UPDATE enrol_codes SET state = 'expired' WHERE computer_id IS NULL AND used_ms IS NOT NULL;
";

/// Code prefix; a leaked value is recognizable.
pub const CODE_PREFIX: &str = "dpe_";
/// Poll secret prefix.
pub const POLL_PREFIX: &str = "dps_";
/// Node token prefix.
pub const TOKEN_PREFIX: &str = "dpn_";
/// Characters after [`CODE_PREFIX`] in a code: two v4 UUIDs without dashes, 32 lower-case hex each.
pub const CODE_BODY_CHARS: usize = 64;
/// Default lifetime of a code.
pub const DEFAULT_TTL: Duration = Duration::from_secs(10 * 60);
/// Longest lifetime of a code; a larger request is clamped to it.
pub const MAX_TTL: Duration = Duration::from_secs(10 * 60);
/// How long the owner has to confirm after the redeem: equal to the code lifetime (FR-C2a).
pub const CONFIRM_WINDOW_MS: i64 = MAX_TTL.as_millis() as i64;
/// How long a confirmed credential waits for the node to take it before the enrolment expires.
pub const DELIVERY_GRACE_MS: i64 = 10 * 60 * 1000;
/// Longest limiter key, in characters (see [`Enrol::source_key`]).
const SOURCE_KEY_CHARS: usize = 64;
/// Failed redeems one source key may make within [`FAIL_WINDOW_MS`] before it is rate-limited.
pub const MAX_FAILURES: usize = 10;
/// The per-source failure window in milliseconds.
pub const FAIL_WINDOW_MS: i64 = 60_000;
/// Most source keys tracked by the limiter; the least recently failing is evicted for a new one.
pub const MAX_TRACKED_SOURCES: usize = 4096;
/// Failed redeems across all sources within [`GLOBAL_FAIL_WINDOW_MS`] before every source is
/// rate-limited (FR-C2f).
pub const GLOBAL_MAX_FAILURES: usize = 60;
/// The global failure window in milliseconds (10 minutes).
pub const GLOBAL_FAIL_WINDOW_MS: i64 = 10 * 60 * 1000;
/// Longest computer name in characters.
pub const MAX_NAME_CHARS: usize = 64;
/// Longest text field of [`NodeInfo`] in characters (host name, os, arch, version).
pub const MAX_INFO_CHARS: usize = 64;
/// Longest node URL in [`NodeInfo`] in characters.
pub const MAX_INFO_URL_CHARS: usize = 300;
/// Longest observed address kept, in characters.
pub const MAX_ADDR_CHARS: usize = 64;
/// Largest serialized [`NodeInfo`] in bytes.
pub const MAX_INFO_BYTES: usize = 4096;
/// The node API port the server proposes when a node reports no address ([provisional, pT/pX]).
pub const PROPOSED_NODE_PORT: u16 = 8741;
/// Expired rows are swept on mint once they have been expired this long, so a project screen can
/// still read "expired" for a while.
pub const SWEEP_GRACE_MS: i64 = 60 * 60 * 1000;
/// The one answer for every code that cannot be redeemed.
pub const REFUSED_MESSAGE: &str = "this enrol code cannot be used";
/// The answer when a source key, or all sources together, have failed too often.
pub const RATE_LIMITED_MESSAGE: &str = "too many attempts, try again later";
/// The answer when the node information is not acceptable.
pub const BAD_NODE_INFO_MESSAGE: &str = "invalid node information";

/// The failure limits of [`Enrol::redeem`] (FR-C2f), a configurable struct like the guest door's
/// `LimitConfig`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LimitConfig {
    /// Failures one source key may make within [`LimitConfig::per_source_window_ms`].
    pub per_source_failures: usize,
    pub per_source_window_ms: i64,
    /// Most source keys tracked at once.
    pub max_tracked_sources: usize,
    /// Failures across all sources within [`LimitConfig::global_window_ms`].
    pub global_failures: usize,
    pub global_window_ms: i64,
}

impl Default for LimitConfig {
    fn default() -> Self {
        LimitConfig {
            per_source_failures: MAX_FAILURES,
            per_source_window_ms: FAIL_WINDOW_MS,
            max_tracked_sources: MAX_TRACKED_SOURCES,
            global_failures: GLOBAL_MAX_FAILURES,
            global_window_ms: GLOBAL_FAIL_WINDOW_MS,
        }
    }
}

/// A code, redacted when printed. Serializes to the string (for the one minting answer only).
#[derive(Clone)]
pub struct EnrolCode(Zeroizing<String>);

impl Serialize for EnrolCode {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl EnrolCode {
    /// The code text. Call it only to hand the code to its one reader.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for EnrolCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("EnrolCode(<redacted>)")
    }
}

/// A poll secret or a node token: zeroized on drop, redacted when printed, serializes to the
/// string (for the one answer that hands it over). Equality is constant-time.
#[derive(Clone)]
pub struct Secret(Zeroizing<String>);

impl Secret {
    fn new(s: String) -> Secret {
        Secret(Zeroizing::new(s))
    }

    /// The secret text. Call it only to hand the secret to its one reader.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl PartialEq for Secret {
    fn eq(&self, other: &Self) -> bool {
        crate::auth::token_eq(&self.0, &other.0)
    }
}

impl Serialize for Secret {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

/// The result of minting. The code and the command that contains it exist only here.
#[derive(Clone, Serialize)]
pub struct Minted {
    pub id: String,
    pub code: EnrolCode,
    pub expires_ms: i64,
    /// The exact line to run on the other machine.
    pub command: String,
}

impl std::fmt::Debug for Minted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `command` holds the code, so it is not printed.
        f.debug_struct("Minted")
            .field("id", &self.id)
            .field("code", &self.code)
            .field("expires_ms", &self.expires_ms)
            .field("command", &"<redacted>")
            .finish()
    }
}

/// What a node says about itself at redeem. UNTRUSTED display data: the other machine wrote it.
/// Everything is length-capped by [`NodeInfo::validated`] and free of control, format (bidi,
/// zero-width) and line-separator characters, and the app MUST render it as plain text, never
/// as HTML. The core uses none of it for a decision; a confirmed computer is registered at `url`
/// when given (else at the observed address plus [`PROPOSED_NODE_PORT`], shown to the owner).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeInfo {
    /// Host name the node reports, 1 to [`MAX_INFO_CHARS`] characters.
    pub hostname: String,
    #[serde(default)]
    pub os: String,
    #[serde(default)]
    pub arch: String,
    #[serde(default)]
    pub node_version: String,
    /// `http(s)://host:port` where the server should reach the node API.
    #[serde(default)]
    pub url: Option<String>,
    /// The node's transport peer id, 64 hex characters.
    #[serde(default)]
    pub peer_id: Option<String>,
}

impl NodeInfo {
    /// A node that reports only a host name (tests, and nodes with nothing else to say).
    pub fn minimal(hostname: &str) -> NodeInfo {
        NodeInfo {
            hostname: hostname.into(),
            os: String::new(),
            arch: String::new(),
            node_version: String::new(),
            url: None,
            peer_id: None,
        }
    }

    /// The cleaned copy, or `None` when any field breaks its cap or carries a control, format
    /// or line-separator character. The check does not depend on any code or enrolment.
    pub fn validated(&self) -> Option<NodeInfo> {
        fn text(s: &str, min: usize, max: usize) -> Option<String> {
            let t = s.trim();
            let n = t.chars().count();
            if n < min || n > max || t.chars().any(is_forbidden_name_char) {
                return None;
            }
            Some(t.to_string())
        }
        let url = match &self.url {
            None => None,
            Some(u) => {
                let u = text(u, 1, MAX_INFO_URL_CHARS)?;
                let scheme = u.starts_with("http://") || u.starts_with("https://");
                let clean = u.chars().all(|c| !c.is_whitespace() && !matches!(c, '"' | '\'' | '<' | '>' | '\\' | '`'));
                if !scheme || !clean {
                    return None;
                }
                Some(u)
            }
        };
        let peer_id = match &self.peer_id {
            None => None,
            Some(p) if p.len() == 64 && p.chars().all(|c| c.is_ascii_hexdigit()) => Some(p.clone()),
            Some(_) => return None,
        };
        let out = NodeInfo {
            hostname: text(&self.hostname, 1, MAX_INFO_CHARS)?,
            os: text(&self.os, 0, MAX_INFO_CHARS)?,
            arch: text(&self.arch, 0, MAX_INFO_CHARS)?,
            node_version: text(&self.node_version, 0, MAX_INFO_CHARS)?,
            url,
            peer_id,
        };
        (serde_json::to_string(&out).ok()?.len() <= MAX_INFO_BYTES).then_some(out)
    }

    /// A short, readable form of the peer id for the owner to compare, `None` without one.
    pub fn fingerprint(&self) -> Option<String> {
        // `peer_id` passed validation as 64 hex digits of either case; the shared function wants lowercase.
        ember_transport::fingerprint_hex(&self.peer_id.as_ref()?.to_ascii_lowercase())
    }
}

/// The address the server proposes for a node that reported none: the observed source address
/// with [`PROPOSED_NODE_PORT`]. `None` when the observed text is not an address. The app shows
/// it to the owner for editing; it is never trusted automatically.
pub fn observed_node_url(observed: &str) -> Option<String> {
    let ip = match observed.parse::<SocketAddr>() {
        Ok(sa) => sa.ip(),
        Err(_) => observed.parse::<IpAddr>().ok()?,
    };
    Some(match ip {
        IpAddr::V4(a) => format!("http://{a}:{PROPOSED_NODE_PORT}"),
        IpAddr::V6(a) => format!("http://[{a}]:{PROPOSED_NODE_PORT}"),
    })
}

/// A successful redeem. The enrolment now waits for the owner; no credential exists yet. The
/// poll secret is returned here once and stored only as a hash.
#[derive(Debug, Clone, PartialEq)]
pub struct Redeemed {
    pub enrol_id: String,
    pub poll_secret: Secret,
    /// The confirm deadline (Unix ms): the redeem time plus [`CONFIRM_WINDOW_MS`].
    pub expires_ms: i64,
}

/// Why a redeem was refused. [`Refused::Generic`] covers unknown, used, expired, cancelled and
/// malformed codes alike. [`Refused::BadNodeInfo`] depends only on the node information, never
/// on the code, and burns nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    Generic,
    RateLimited,
    BadNodeInfo,
}

impl Refused {
    pub fn message(self) -> &'static str {
        match self {
            Refused::Generic => REFUSED_MESSAGE,
            Refused::RateLimited => RATE_LIMITED_MESSAGE,
            Refused::BadNodeInfo => BAD_NODE_INFO_MESSAGE,
        }
    }
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for Refused {}

/// The stored state of an enrolment (FR-C2a). [`State::TRANSITIONS`] is the complete table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Waiting,
    AwaitingConfirmation,
    /// Internal: confirmed by the owner, credential not yet delivered to the node.
    Confirmed,
    Joined,
    Denied,
    Expired,
}

impl State {
    /// Every legal move. `joined`, `denied` and `expired` are final.
    pub const TRANSITIONS: &'static [(State, State)] = &[
        (State::Waiting, State::AwaitingConfirmation),
        (State::Waiting, State::Expired),
        (State::AwaitingConfirmation, State::Confirmed),
        (State::AwaitingConfirmation, State::Denied),
        (State::AwaitingConfirmation, State::Expired),
        (State::Confirmed, State::Joined),
        (State::Confirmed, State::Expired),
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            State::Waiting => "waiting",
            State::AwaitingConfirmation => "awaiting_confirmation",
            State::Confirmed => "confirmed",
            State::Joined => "joined",
            State::Denied => "denied",
            State::Expired => "expired",
        }
    }

    pub fn parse(s: &str) -> Option<State> {
        Some(match s {
            "waiting" => State::Waiting,
            "awaiting_confirmation" => State::AwaitingConfirmation,
            "confirmed" => State::Confirmed,
            "joined" => State::Joined,
            "denied" => State::Denied,
            "expired" => State::Expired,
            _ => return None,
        })
    }

    pub fn can_move_to(self, to: State) -> bool {
        State::TRANSITIONS.contains(&(self, to))
    }
}

/// The state of an enrolment as the admin sees it. `confirmed` reads `awaiting_confirmation`
/// until the node has received its credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EnrolStatus {
    /// Minted, not redeemed, not expired.
    Waiting,
    /// Redeemed; the owner has not confirmed, or the node has not yet taken its credential.
    AwaitingConfirmation,
    /// The node received its credential and the computer is registered.
    Joined,
    /// The owner denied it.
    Denied,
    /// Past its expiry or deadline without a result.
    Expired,
}

impl From<State> for EnrolStatus {
    fn from(s: State) -> Self {
        match s {
            State::Waiting => EnrolStatus::Waiting,
            State::AwaitingConfirmation | State::Confirmed => EnrolStatus::AwaitingConfirmation,
            State::Joined => EnrolStatus::Joined,
            State::Denied => EnrolStatus::Denied,
            State::Expired => EnrolStatus::Expired,
        }
    }
}

/// What the admin route answers for an enrolment. `node_info`, `observed_addr` and the rest are
/// untrusted display data: render them as plain text.
#[derive(Debug, Clone, Serialize)]
pub struct StatusDetail {
    pub status: EnrolStatus,
    /// The name chosen at mint.
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_info: Option<NodeInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_addr: Option<String>,
    /// The address the server proposes when the node reported no `url`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proposed_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confirm_deadline_ms: Option<i64>,
    /// How a confirm would register this node: `"url"` (its reported url, even when it also
    /// reported a peer id), `"peer"` (its peer id over the transport) or `"observed"` (the
    /// address the door saw). Present only while the optional detail fields are, and only when
    /// a registrar is set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registered_by: Option<&'static str>,
}

/// What the node gets when it cancels its own enrolment with its poll secret (FR-C2m, #617).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeCancel {
    /// A wrong secret or an unknown id: the same answer as a poll's [`Delivery::Unknown`].
    Unknown,
    /// The enrolment was awaiting confirmation and now reads `expired`; the owner cannot confirm it.
    Cancelled,
    /// The enrolment is past the owner's answer (`confirmed`, `joined`, `denied`, `expired`);
    /// nothing changed.
    TooLate(State),
}

/// What the node gets when it asks for its credential (FR-C2d). Every wrong secret and every
/// unknown enrol id is [`Delivery::Unknown`].
#[derive(Debug)]
pub enum Delivery {
    /// Still waiting for the owner.
    Pending,
    /// The credential, handed over once and wiped from the server's memory.
    /// `server_peer_id` is the server's own public transport id, `None` when the transport is off.
    Joined { token: Secret, name: String, computer_id: String, server_peer_id: Option<String> },
    /// The credential was already delivered.
    Delivered,
    Denied,
    Expired,
    Unknown,
}

/// What a confirm did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmOutcome {
    pub computer_id: String,
    pub name: String,
}

/// Errors of the admin side.
#[derive(Debug, thiserror::Error)]
pub enum EnrolError {
    #[error("{0}")]
    BadRequest(String),
    #[error("enrol code {0} not found")]
    NotFound(String),
    #[error("enrol code {0} was already used")]
    Used(String),
    /// The computer name is already in the registry. The enrolment stays awaiting confirmation.
    #[error("a computer named {0:?} already exists; choose another name")]
    NameTaken(String),
    #[error("enrolment {id} is {state}, not awaiting confirmation")]
    WrongState { id: String, state: &'static str },
    #[error("enrolment {0} has expired")]
    Expired(String),
    /// The `expect_fingerprint` the owner gave is not the fingerprint the node reported (or the
    /// node reported none). Nothing was registered and the enrolment stays awaiting confirmation.
    #[error("The fingerprint does not match the one the other machine reported; nothing was registered")]
    FingerprintMismatch,
    /// The computer would be registered by its peer id, so the owner must say which fingerprint
    /// they compared. Nothing was registered.
    #[error("This computer is registered by its peer id; confirm with expect_fingerprint set to the fingerprint shown on the other machine")]
    FingerprintRequired,
    #[error("illegal enrolment transition {from} to {to}")]
    IllegalTransition { from: &'static str, to: &'static str },
    /// The enrol door is off: no code is minted, because a command could not work.
    #[error("The enrol door is off: set EMBER_ENROL_LISTEN (and EMBER_ENROL_PUBLIC_URL) and restart")]
    DoorOff,
    /// The door is on but the server has no address to print for it.
    #[error("Set EMBER_ENROL_PUBLIC_URL to the address the other machine uses")]
    DoorUrlUnknown,
    /// The door opens at mint (auto-close, the default, #539) and its socket could not be
    /// bound. The code just minted is cancelled, so no unusable code is shown.
    #[error("The enrol door could not be opened; no code was made")]
    DoorUnavailable,
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl From<rusqlite::Error> for EnrolError {
    fn from(e: rusqlite::Error) -> Self {
        EnrolError::Other(e.into())
    }
}

/// Why a [`Registrar`] could not register a computer.
#[derive(Debug)]
pub enum RegisterError {
    NameTaken,
    /// A fixed text, never one that could carry the token.
    Failed(String),
}

/// The registry as the confirm step needs it. The real implementation is [`ComputersRegistrar`];
/// tests use a fake. The door never holds one: registering a computer is an owner act.
pub trait Registrar: Send + Sync {
    /// True when a computer of that name exists (compared without regard to case).
    fn name_taken(&self, name: &str) -> bool;
    /// Register the computer with the freshly minted `token`; returns its computer id.
    /// `info` and `observed_addr` are untrusted; the address comes from [`NodeInfo::url`] or
    /// [`observed_node_url`].
    fn register(&self, name: &str, token: &str, info: &NodeInfo, observed_addr: &str) -> Result<String, RegisterError>;
    /// Undo a registration (when recording it failed). Best effort.
    fn remove(&self, computer_id: &str);
    /// How [`Registrar::register`] would address this node: `Some("url")`, `Some("peer")` or
    /// `Some("observed")`, `None` when it would refuse. The default is `None` (a registrar that
    /// cannot say), which makes neither the confirm view nor the peer-path rule apply.
    fn registers_by(&self, _info: &NodeInfo, _observed_addr: &str) -> Option<&'static str> {
        None
    }
}

/// Where a confirmed node is registered: by HTTP URL, or by its transport peer id.
#[derive(Debug, PartialEq)]
pub enum Target {
    Url(String),
    Peer(ember_transport::PeerId),
}

/// The address rule for a confirmed node (FR-C2c), pure so it is testable without a transport.
/// 1. `info.url` present: by URL (it wins over a peer id).
/// 2. No url, a peer id, and `transport_on`: by peer.
/// 3. No url, a peer id, no transport: the observed URL (a LAN node enrolled with `--transport`
///    against a server without the transport still works).
/// 4. Neither: the observed URL, or the "no address" error when none can be derived.
///
/// A peer id equal to `server_id` (the node claims to be this server) is refused in every case.
/// The peer id is the node's own claim; the owner compares its fingerprint at confirm.
fn register_target(
    info: &NodeInfo,
    observed: &str,
    transport_on: bool,
    server_id: Option<&str>,
) -> Result<Target, RegisterError> {
    let peer = info.peer_id.as_ref().map(|p| p.parse::<ember_transport::PeerId>().ok());
    if let (Some(Some(p)), Some(own)) = (&peer, server_id) {
        if p.to_string() == own.to_ascii_lowercase() {
            return Err(RegisterError::Failed("the node's peer id is this server's own".into()));
        }
    }
    if let Some(url) = &info.url {
        return Ok(Target::Url(url.clone()));
    }
    if transport_on {
        match peer {
            Some(Some(p)) => return Ok(Target::Peer(p)),
            Some(None) => return Err(RegisterError::Failed("the node's peer id is not valid".into())),
            None => {}
        }
    }
    observed_node_url(observed)
        .map(Target::Url)
        .ok_or_else(|| RegisterError::Failed("the node reported no address and none could be derived".into()))
}

/// [`Registrar`] over the computers registry.
pub struct ComputersRegistrar {
    computers: Arc<Computers>,
    /// This server's own transport peer id, `None` when the transport is off.
    server_peer_id: Option<String>,
}

impl ComputersRegistrar {
    pub fn new(computers: Arc<Computers>) -> ComputersRegistrar {
        ComputersRegistrar { computers, server_peer_id: None }
    }

    /// With the server's own peer id, so a node that claims it is refused at register.
    pub fn with_server_peer_id(mut self, server_peer_id: Option<String>) -> ComputersRegistrar {
        self.server_peer_id = server_peer_id;
        self
    }
}

impl Registrar for ComputersRegistrar {
    fn name_taken(&self, name: &str) -> bool {
        // A registry that cannot be read counts as taken: fail closed.
        self.computers
            .registry()
            .list()
            .map(|l| l.iter().any(|c| c.name.eq_ignore_ascii_case(name)))
            .unwrap_or(true)
    }

    fn register(&self, name: &str, token: &str, info: &NodeInfo, observed_addr: &str) -> Result<String, RegisterError> {
        let target = register_target(info, observed_addr, self.computers.dialer().is_some(), self.server_peer_id.as_deref())?;
        let result = match target {
            Target::Url(url) => self.computers.register(name, &url, token),
            Target::Peer(peer) => self.computers.register_peer(name, &ember_transport::PeerAddr::from(peer), token),
        };
        match result {
            Ok(c) => {
                // FR-C2c: probe the registered computer once without blocking confirm.
                if let Ok(handle) = tokio::runtime::Handle::try_current() {
                    let computers = self.computers.clone();
                    let cid = c.id.clone();
                    handle.spawn(async move {
                        let _ = computers.status(&cid).await;
                    });
                }
                Ok(c.id)
            }
            Err(_) if self.name_taken(name) => Err(RegisterError::NameTaken),
            Err(super::ComputerError::BadRequest(m)) if m == super::DUPLICATE_PEER => Err(RegisterError::Failed(m)),
            Err(_) => Err(RegisterError::Failed("the computer could not be registered at that address".into())),
        }
    }

    fn registers_by(&self, info: &NodeInfo, observed_addr: &str) -> Option<&'static str> {
        let target =
            register_target(info, observed_addr, self.computers.dialer().is_some(), self.server_peer_id.as_deref()).ok()?;
        Some(match target {
            Target::Peer(_) => "peer",
            Target::Url(_) if info.url.is_some() => "url",
            Target::Url(_) => "observed",
        })
    }

    fn remove(&self, computer_id: &str) {
        // Idempotent: a computer that is already gone is not a failure. Any other failure is
        // logged with the computer id and the error text, which holds no token (the call carries none).
        match self.computers.remove(computer_id) {
            Ok(()) | Err(super::ComputerError::NotFound(_)) => {}
            Err(e) => tracing::warn!(
                computer = %computer_id,
                error = %e,
                "could not remove the computer of an expired enrolment"
            ),
        }
    }
}

/// Whether `code` has the shape [`Enrol::mint`] makes: [`CODE_PREFIX`] and then exactly
/// [`CODE_BODY_CHARS`] lower-case hex digits.
fn code_has_shape(code: &str) -> bool {
    code.strip_prefix(CODE_PREFIX)
        .is_some_and(|b| b.len() == CODE_BODY_CHARS && b.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f')))
}

fn digest(code: &str) -> Vec<u8> {
    Sha256::digest(code.as_bytes()).to_vec()
}

/// Equal-length byte compare without an early exit.
fn digests_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// A secret of three v4 UUIDs (366 random bits) behind `prefix`.
fn long_secret(prefix: &str) -> String {
    format!(
        "{prefix}{}{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

/// The observed address as stored: control and format characters removed, at most
/// [`MAX_ADDR_CHARS`], `unknown` when nothing is left.
fn clean_addr(source: &str) -> String {
    let s: String = source.chars().filter(|c| !is_forbidden_name_char(*c)).take(MAX_ADDR_CHARS).collect();
    if s.is_empty() {
        "unknown".into()
    } else {
        s
    }
}

/// A computer name as the registry takes it, plus the shape rules the guest names use.
/// An owner-typed fingerprint as 16 lowercase hex digits: spaces are dropped and case is
/// ignored. Anything else is a 400, so a check the owner asked for is never skipped.
fn normalise_fingerprint(fp: &str) -> Result<String, EnrolError> {
    let n: String = fp.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_ascii_lowercase();
    if n.len() == 16 && n.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(n)
    } else {
        Err(EnrolError::BadRequest("expect_fingerprint must be 16 hex digits, as 4 groups of 4".into()))
    }
}

fn check_name(name: &str) -> Result<String, EnrolError> {
    let name = name.trim();
    let chars = name.chars().count();
    if name.is_empty()
        || name == LOCAL
        || name == LOCAL_NAME
        || chars > MAX_NAME_CHARS
        || name.chars().any(is_forbidden_name_char)
    {
        return Err(EnrolError::BadRequest(format!(
            "invalid computer name: use 1 to {MAX_NAME_CHARS} characters, no control characters, not {LOCAL:?} or {LOCAL_NAME:?}"
        )));
    }
    Ok(name.to_string())
}

/// The server URL that goes into the command: `http(s)://host[:port][/path]`, nothing else.
/// Allowed: ASCII letters, digits, `.`, `-`, `_`, `:` and `/`, and one pair of brackets only as an
/// IPv6 literal host (`[` right after `://`, hex digits and `:` inside). `%`, `~`, `[` and `]`
/// elsewhere are refused (#526): the command is pasted into a shell, and zsh globs on `[ ]`,
/// expands a leading `~`, and `%` starts a percent escape or a zone id the node does not need.
fn check_base_url(url: &str) -> Result<String, EnrolError> {
    let bad = || EnrolError::BadRequest("the server URL for the enrol command is not a plain http(s) URL".into());
    let url = url.trim().trim_end_matches('/');
    let rest = url.strip_prefix("http://").or_else(|| url.strip_prefix("https://")).ok_or_else(bad)?;
    if rest.is_empty() || url.len() > 300 {
        return Err(bad());
    }
    let plain = |c: char| c.is_ascii_alphanumeric() || matches!(c, ':' | '/' | '.' | '-' | '_');
    let after_host = match rest.strip_prefix('[') {
        Some(v6) => {
            let close = v6.find(']').ok_or_else(bad)?;
            let inside = &v6[..close];
            if inside.is_empty() || !inside.chars().all(|c| c.is_ascii_hexdigit() || c == ':') {
                return Err(bad());
            }
            let after = &v6[close + 1..];
            if !(after.is_empty() || after.starts_with(':') || after.starts_with('/')) {
                return Err(bad());
            }
            after
        }
        // A host name must come first: `http:///x` and `http://:80` have none.
        None if rest.starts_with('/') || rest.starts_with(':') => return Err(bad()),
        None => rest,
    };
    if !after_host.chars().all(plain) {
        return Err(bad());
    }
    Ok(url.to_string())
}

/// One row of `enrol_codes`, as the state machine reads it.
struct Row {
    name: String,
    state: State,
    poll_hash: Option<Vec<u8>>,
    node_info: Option<String>,
    observed_addr: Option<String>,
    confirm_deadline_ms: Option<i64>,
    final_name: Option<String>,
    computer_id: Option<String>,
}

const ROW_SQL: &str = "SELECT name, state, poll_hash, node_info, observed_addr, confirm_deadline_ms, final_name, computer_id
                       FROM enrol_codes WHERE id = ?1";

/// The enrol-code service: store, the in-memory failure limiters and the pending credentials.
pub struct Enrol {
    store: Arc<Store>,
    limits: LimitConfig,
    /// Failure times (Unix ms) per source key. Bounded by `limits.max_tracked_sources`.
    failures: Mutex<HashMap<String, VecDeque<i64>>>,
    /// Failure times (Unix ms) of all sources together. Never evicted by source, at most
    /// `limits.global_failures` long.
    global_failures: Mutex<VecDeque<i64>>,
    /// Node tokens minted at confirm and not yet delivered, by enrol id. MEMORY ONLY, never in
    /// SQLite; lost on restart (the enrolment then reads `expired`).
    pending: Mutex<HashMap<String, Secret>>,
    /// The registry used to remove the computer of a `confirmed` enrolment that expired
    /// undelivered. Set by [`Enrol::set_registrar`] (the router does it).
    registrar: Mutex<Option<Arc<dyn Registrar>>>,
    /// Computer ids whose enrolment expired undelivered and that are not removed yet (no
    /// registrar was set at the time). Ids only, never a secret.
    orphans: Mutex<Vec<String>>,
    /// Serializes every state change after the redeem (confirm, deny, delivery, expiry), so
    /// exactly one of two racing calls wins and no registration is made for the loser.
    gate: Mutex<()>,
    /// The server's own transport peer id (public, 64 lowercase hex), set once at construction by
    /// [`Enrol::with_server_peer_id`] and `None` when the transport is off. Never per request.
    server_peer_id: Option<String>,
}

impl Enrol {
    pub fn new(store: Arc<Store>) -> Arc<Enrol> {
        Self::with_limits(store, LimitConfig::default())
    }

    pub fn with_limits(store: Arc<Store>, limits: LimitConfig) -> Arc<Enrol> {
        Arc::new(Enrol {
            store,
            limits,
            failures: Mutex::new(HashMap::new()),
            global_failures: Mutex::new(VecDeque::new()),
            pending: Mutex::new(HashMap::new()),
            registrar: Mutex::new(None),
            orphans: Mutex::new(Vec::new()),
            gate: Mutex::new(()),
            server_peer_id: None,
        })
    }

    /// Like [`Enrol::new`] (default limits), with the server's transport peer id for the `joined` delivery
    /// (FR-C2). The id is kept only when it is exactly 64 lowercase hex chars (the shape
    /// [`ember_transport::fingerprint_hex`] accepts); a malformed value is dropped to `None`, never
    /// stored. `None` is the transport being off.
    pub fn with_server_peer_id(store: Arc<Store>, server_peer_id: Option<String>) -> Arc<Enrol> {
        let server_peer_id = server_peer_id.filter(|id| ember_transport::fingerprint_hex(id).is_some());
        let mut e = Self::new(store);
        Arc::get_mut(&mut e).expect("a fresh Arc has one owner").server_peer_id = server_peer_id;
        e
    }

    /// The server's transport peer id (64 lowercase hex), `None` when the transport is off. The
    /// mint pins it in the command (`--server-fingerprint`).
    pub fn server_peer_id(&self) -> Option<&str> {
        self.server_peer_id.as_deref()
    }

    /// Give the service the registry, so a computer registered by a confirm whose credential was
    /// never delivered (the node did not poll in time, or the server restarted) is removed when
    /// the enrolment expires. Orphans found before this call are removed at the next settle.
    pub fn set_registrar(&self, registrar: Arc<dyn Registrar>) {
        *self.registrar.lock().unwrap_or_else(|e| e.into_inner()) = Some(registrar);
    }

    /// Settle every enrolment at `now_ms` without reading one. The router calls it at startup so
    /// a `confirmed` row left by a restart is expired and its computer removed at once.
    pub fn sweep(&self, now_ms: i64) -> Result<(), EnrolError> {
        let g = self.gate();
        self.settle(&g, now_ms)
    }

    /// Whether any enrolment still needs the enrol door at `now_ms` (#539): a code not redeemed
    /// yet (`waiting`), a redeem the owner has not answered (`awaiting_confirmation`), or a
    /// confirm whose credential the node has not fetched (`confirmed`). Settles first, so an
    /// expired one does not count. The auto-closing door ([`crate::enrol_door::DoorGate`]) closes
    /// when this is false.
    pub fn has_live_enrolment(&self, now_ms: i64) -> Result<bool, EnrolError> {
        let g = self.gate();
        self.settle(&g, now_ms)?;
        let n: i64 = self.store.conn().query_row(
            "SELECT COUNT(*) FROM enrol_codes WHERE state IN ('waiting', 'awaiting_confirmation', 'confirmed')",
            [],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    fn gate(&self) -> MutexGuard<'_, ()> {
        self.gate.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Mint a code for a computer to be called `name`. `ttl` defaults to 10 minutes and is
    /// clamped to 10 minutes; zero is refused. `base_url` is the URL of the enrol DOOR as the other
    /// machine reaches it (not the owner API address); the caller passes it. When the service
    /// holds the server's peer id, the command ends in `--server-fingerprint <full 64 hex id>`
    /// (the pin the node checks; the short form is for human screens only). Expired rows older
    /// than [`SWEEP_GRACE_MS`] are deleted.
    pub fn mint(&self, name: &str, now_ms: i64, ttl: Option<Duration>, base_url: &str) -> Result<Minted, EnrolError> {
        let name = check_name(name)?;
        let base = check_base_url(base_url)?;
        let ttl = ttl.unwrap_or(DEFAULT_TTL);
        if ttl.is_zero() {
            return Err(EnrolError::BadRequest("ttl must be more than zero".into()));
        }
        let ttl = ttl.min(MAX_TTL);
        let code = new_secret(CODE_PREFIX);
        let id = uuid::Uuid::new_v4().to_string();
        let expires_ms = now_ms + ttl.as_millis() as i64;
        {
            let conn = self.store.conn();
            conn.execute("DELETE FROM enrol_codes WHERE expires_ms < ?1", params![now_ms - SWEEP_GRACE_MS])?;
            conn.execute(
                "INSERT INTO enrol_codes (id, code_hash, name, created_ms, expires_ms) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![id, digest(&code), name, now_ms, expires_ms],
            )?;
        }
        let mut command = format!("ember-node enrol {code} --server {base}");
        if let Some(id) = &self.server_peer_id {
            command.push_str(" --server-fingerprint ");
            command.push_str(id);
        }
        Ok(Minted { id, code: EnrolCode(Zeroizing::new(code)), expires_ms, command })
    }

    /// Redeem `code` at `now_ms` for the caller identified by `source_key` (the remote address;
    /// the transport that carries the call decides what it is), reporting `info`. Order: the two
    /// failure limiters, then the node information (independent of the code), then one SQL
    /// statement that moves the code from `waiting` to `awaiting_confirmation` only if it is
    /// unused and unexpired, so of two concurrent redeems one wins. No credential exists after
    /// it: the answer is the enrol id, the poll secret (once) and the confirm deadline.
    pub fn redeem(&self, code: &str, info: &NodeInfo, now_ms: i64, source_key: &str) -> Result<Redeemed, Refused> {
        if self.rate_limited(source_key, now_ms) {
            return Err(Refused::RateLimited);
        }
        let info = info.validated().ok_or(Refused::BadNodeInfo)?;
        // A code with a shape no mint makes is never looked up in SQLite. It is a failure like
        // any other: the same refusal, and it spends the same failure budgets, so it can neither
        // probe for free nor reveal the format.
        if !code_has_shape(code) {
            self.record_failure(source_key, now_ms);
            return Err(Refused::Generic);
        }
        let info_json = serde_json::to_string(&info).map_err(|_| Refused::BadNodeInfo)?;
        let observed = clean_addr(source_key);
        let secret = long_secret(POLL_PREFIX);
        let deadline = now_ms + CONFIRM_WINDOW_MS;
        let won = self.store.conn().query_row(
            "UPDATE enrol_codes
             SET used_ms = ?1, state = 'awaiting_confirmation', poll_hash = ?3, node_info = ?4,
                 observed_addr = ?5, confirm_deadline_ms = ?6
             WHERE code_hash = ?2 AND used_ms IS NULL AND state = 'waiting' AND expires_ms > ?1
             RETURNING id",
            params![now_ms, digest(code), digest(&secret), info_json, observed, deadline],
            |r| r.get::<_, String>(0),
        );
        match won {
            Ok(enrol_id) => Ok(Redeemed { enrol_id, poll_secret: Secret::new(secret), expires_ms: deadline }),
            // No row, or a database error: the caller sees the same refusal either way.
            Err(_) => {
                self.record_failure(source_key, now_ms);
                Err(Refused::Generic)
            }
        }
    }

    /// The limiter key of a source: its first [`SOURCE_KEY_CHARS`] characters. Lookup and record
    /// both use it, so a long source is limited per source too. Two sources that share their
    /// first 64 characters share one bucket, on purpose: the door passes an IP address, so keys
    /// are short in practice, and a bound on key length bounds memory per entry.
    fn source_key(source: &str) -> String {
        source.chars().take(SOURCE_KEY_CHARS).collect()
    }

    fn rate_limited(&self, source: &str, now_ms: i64) -> bool {
        {
            let mut g = self.global_failures.lock().unwrap_or_else(|e| e.into_inner());
            while g.front().is_some_and(|t| now_ms - *t >= self.limits.global_window_ms) {
                g.pop_front();
            }
            if g.len() >= self.limits.global_failures {
                return true;
            }
        }
        let mut all = self.failures.lock().unwrap_or_else(|e| e.into_inner());
        match all.get_mut(&Self::source_key(source)) {
            Some(q) => {
                while q.front().is_some_and(|t| now_ms - *t >= self.limits.per_source_window_ms) {
                    q.pop_front();
                }
                q.len() >= self.limits.per_source_failures
            }
            None => false,
        }
    }

    fn record_failure(&self, source: &str, now_ms: i64) {
        {
            let mut g = self.global_failures.lock().unwrap_or_else(|e| e.into_inner());
            // Once full every source is limited, so nothing more is recorded; the queue stays
            // bounded and the window slides as the oldest entries age out.
            if g.len() < self.limits.global_failures {
                g.push_back(now_ms);
            }
        }
        let window = self.limits.per_source_window_ms;
        let key = Self::source_key(source);
        let mut all = self.failures.lock().unwrap_or_else(|e| e.into_inner());
        if !all.contains_key(&key) && all.len() >= self.limits.max_tracked_sources {
            all.retain(|_, q| q.back().is_some_and(|t| now_ms - *t < window));
            if all.len() >= self.limits.max_tracked_sources {
                if let Some(oldest) = all.iter().min_by_key(|(_, q)| q.back().copied()).map(|(k, _)| k.clone()) {
                    all.remove(&oldest);
                }
            }
        }
        all.entry(key).or_default().push_back(now_ms);
    }

    fn load(&self, id: &str) -> Result<Option<Row>, EnrolError> {
        let row = self
            .store
            .conn()
            .query_row(ROW_SQL, params![id], |r| {
                let state: String = r.get(1)?;
                Ok(Row {
                    name: r.get(0)?,
                    state: State::parse(&state).ok_or_else(|| {
                        rusqlite::Error::InvalidColumnType(1, "state".into(), rusqlite::types::Type::Text)
                    })?,
                    poll_hash: r.get(2)?,
                    node_info: r.get(3)?,
                    observed_addr: r.get(4)?,
                    confirm_deadline_ms: r.get(5)?,
                    final_name: r.get(6)?,
                    computer_id: r.get(7)?,
                })
            })
            .optional()?;
        Ok(row)
    }

    fn load_or_not_found(&self, id: &str) -> Result<Row, EnrolError> {
        self.load(id)?.ok_or_else(|| EnrolError::NotFound(id.into()))
    }

    /// Persist the expiries that time has caused and drop credentials that can no longer be
    /// delivered. The `_gate` argument proves the caller holds the gate.
    fn settle(&self, _gate: &MutexGuard<'_, ()>, now_ms: i64) -> Result<(), EnrolError> {
        // Computer ids of confirmed enrolments that expire in this call. `UPDATE ... RETURNING`
        // makes each row leave `confirmed` exactly once, so each id is queued exactly once.
        let mut orphaned: Vec<String> = Vec::new();
        let confirmed_ids: Vec<String> = {
            let conn = self.store.conn();
            // Every statement below is a move of the table: waiting, awaiting and confirmed to expired.
            conn.execute("UPDATE enrol_codes SET state = 'expired' WHERE state = 'waiting' AND expires_ms <= ?1", params![now_ms])?;
            conn.execute(
                "UPDATE enrol_codes SET state = 'expired' WHERE state = 'awaiting_confirmation' AND confirm_deadline_ms <= ?1",
                params![now_ms],
            )?;
            let mut late = conn.prepare(
                "UPDATE enrol_codes SET state = 'expired' WHERE state = 'confirmed' AND confirmed_ms + ?2 <= ?1
                 RETURNING computer_id",
            )?;
            let ids = late
                .query_map(params![now_ms, DELIVERY_GRACE_MS], |r| r.get::<_, Option<String>>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            orphaned.extend(ids.into_iter().flatten());
            let mut stmt = conn.prepare("SELECT id FROM enrol_codes WHERE state = 'confirmed'")?;
            let ids = stmt.query_map([], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
            ids
        };
        // A confirmed enrolment whose credential is not in memory (a restart) can never be
        // delivered: it expires.
        let lost: Vec<String> = {
            let p = self.pending.lock().unwrap_or_else(|e| e.into_inner());
            confirmed_ids.iter().filter(|id| !p.contains_key(*id)).cloned().collect()
        };
        for id in &lost {
            let conn = self.store.conn();
            let computer: Option<Option<String>> = conn
                .query_row(
                    "UPDATE enrol_codes SET state = 'expired' WHERE id = ?1 AND state = 'confirmed' RETURNING computer_id",
                    params![id],
                    |r| r.get::<_, Option<String>>(0),
                )
                .optional()?;
            orphaned.extend(computer.flatten());
        }
        // Wipe every pending credential whose enrolment is no longer confirmed (expired, or
        // joined elsewhere). The gate is held, so no confirm is half done.
        self.pending.lock().unwrap_or_else(|e| e.into_inner()).retain(|id, _| confirmed_ids.contains(id));
        self.reap(orphaned);
        Ok(())
    }

    /// Remove the computers of enrolments that expired undelivered (nobody can ever use their
    /// credential). A `joined` enrolment never reaches here. Ids wait in `orphans` until a
    /// registrar is set. The state change has already happened, and a failing removal is the
    /// registrar's to log (without any secret); it never undoes the state.
    fn reap(&self, found: Vec<String>) {
        let mut queue = self.orphans.lock().unwrap_or_else(|e| e.into_inner());
        queue.extend(found);
        if queue.is_empty() {
            return;
        }
        let registrar = self.registrar.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(registrar) = registrar {
            for id in queue.drain(..) {
                registrar.remove(&id);
            }
        }
    }

    /// Move `id` from `from` to `to` if it is still in `from`. An illegal pair is an error;
    /// `Ok(false)` means the row was no longer in `from`. `computer_id` and `final_name` are
    /// recorded when given.
    fn transition(
        &self,
        id: &str,
        from: State,
        to: State,
        now_ms: i64,
        computer_id: Option<&str>,
        final_name: Option<&str>,
    ) -> Result<bool, EnrolError> {
        if !from.can_move_to(to) {
            return Err(EnrolError::IllegalTransition { from: from.as_str(), to: to.as_str() });
        }
        let n = self.store.conn().execute(
            "UPDATE enrol_codes SET
                state = ?3,
                confirmed_ms = CASE WHEN ?3 = 'confirmed' THEN ?4 ELSE confirmed_ms END,
                delivered_ms = CASE WHEN ?3 = 'joined' THEN ?4 ELSE delivered_ms END,
                denied_ms = CASE WHEN ?3 = 'denied' THEN ?4 ELSE denied_ms END,
                computer_id = COALESCE(?5, computer_id),
                final_name = COALESCE(?6, final_name)
             WHERE id = ?1 AND state = ?2",
            params![id, from.as_str(), to.as_str(), now_ms, computer_id, final_name],
        )?;
        Ok(n == 1)
    }

    /// The admin state of the enrolment `id` at `now_ms`. A missing id is [`EnrolError::NotFound`].
    pub fn status(&self, id: &str, now_ms: i64) -> Result<EnrolStatus, EnrolError> {
        let g = self.gate();
        self.settle(&g, now_ms)?;
        Ok(self.load_or_not_found(id)?.state.into())
    }

    /// The state plus, once redeemed, the untrusted node information for the owner's card.
    pub fn status_detail(&self, id: &str, now_ms: i64) -> Result<StatusDetail, EnrolError> {
        let g = self.gate();
        self.settle(&g, now_ms)?;
        let row = self.load_or_not_found(id)?;
        let showing = matches!(row.state, State::AwaitingConfirmation | State::Confirmed);
        let info: Option<NodeInfo> =
            if showing { row.node_info.as_deref().and_then(|j| serde_json::from_str(j).ok()) } else { None };
        let observed = if showing { row.observed_addr.clone() } else { None };
        let proposed_url = match (&info, &observed) {
            (Some(i), Some(o)) if i.url.is_none() => observed_node_url(o),
            _ => None,
        };
        let registrar = self.registrar.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let registered_by = match (&registrar, &info, &observed) {
            (Some(r), Some(i), o) => r.registers_by(i, o.as_deref().unwrap_or("unknown")),
            _ => None,
        };
        Ok(StatusDetail {
            registered_by,
            status: row.state.into(),
            name: row.name,
            fingerprint: info.as_ref().and_then(|i| i.fingerprint()),
            node_info: info,
            observed_addr: observed,
            proposed_url,
            confirm_deadline_ms: if showing { row.confirm_deadline_ms } else { None },
        })
    }

    /// The owner confirms the enrolment `enrol_id`, optionally under another name. Only from
    /// `awaiting_confirmation` and before the deadline. In order: the name is checked against
    /// the registry (a clash is [`EnrolError::NameTaken`] and the state is untouched, so the
    /// owner can call again with a new name), the node token is minted, the computer is
    /// registered through `registrar`, and the enrolment records it (`confirmed`, shown as
    /// `awaiting_confirmation` until the node takes the credential). If recording fails the
    /// registration is removed again.
    ///
    /// The registry name check and `register` are separate steps, and another writer to the
    /// registry can take the name in between. [`Registrar::register`] answering
    /// [`RegisterError::NameTaken`] is the authoritative answer: it is [`EnrolError::NameTaken`]
    /// (409 `name_taken`) with the state unchanged, and the owner can confirm again with a new
    /// name. The early check only saves minting a token in the common case.
    ///
    /// `expect_fingerprint` is the fingerprint the owner read on the other machine. When given
    /// it must be 16 hex digits (spaces and case are ignored, else 400) and equal the node's
    /// reported fingerprint, else [`EnrolError::FingerprintMismatch`]; a node that reported no
    /// peer id never matches. When the registrar would register the node by its peer id
    /// (`registers_by` is `"peer"`), it is required ([`EnrolError::FingerprintRequired`]).
    /// Both checks run before the token is minted and before anything is registered.
    pub fn confirm(
        &self,
        enrol_id: &str,
        name_override: Option<&str>,
        expect_fingerprint: Option<&str>,
        now_ms: i64,
        registrar: &dyn Registrar,
    ) -> Result<ConfirmOutcome, EnrolError> {
        let g = self.gate();
        self.settle(&g, now_ms)?;
        let row = self.load_or_not_found(enrol_id)?;
        match row.state {
            State::AwaitingConfirmation => {}
            State::Expired => return Err(EnrolError::Expired(enrol_id.into())),
            other => return Err(EnrolError::WrongState { id: enrol_id.into(), state: other.as_str() }),
        }
        if row.confirm_deadline_ms.map_or(true, |d| now_ms >= d) {
            return Err(EnrolError::Expired(enrol_id.into()));
        }
        let expected = expect_fingerprint.map(normalise_fingerprint).transpose()?;
        let name = check_name(name_override.unwrap_or(&row.name))?;
        if registrar.name_taken(&name) {
            return Err(EnrolError::NameTaken(name));
        }
        let info: NodeInfo = row
            .node_info
            .as_deref()
            .and_then(|j| serde_json::from_str(j).ok())
            .ok_or_else(|| EnrolError::Other(anyhow::anyhow!("enrolment {enrol_id} has no node information")))?;
        let observed = row.observed_addr.clone().unwrap_or_else(|| "unknown".into());
        match &expected {
            Some(want) => {
                let reported = info.fingerprint().map(|f| f.replace(' ', "").to_ascii_lowercase());
                if reported.as_ref() != Some(want) {
                    return Err(EnrolError::FingerprintMismatch);
                }
            }
            None if registrar.registers_by(&info, &observed) == Some("peer") => {
                return Err(EnrolError::FingerprintRequired);
            }
            None => {}
        }
        let token = Secret::new(long_secret(TOKEN_PREFIX));
        let computer_id = match registrar.register(&name, token.expose(), &info, &observed) {
            Ok(id) => id,
            Err(RegisterError::NameTaken) => return Err(EnrolError::NameTaken(name)),
            Err(RegisterError::Failed(msg)) => return Err(EnrolError::BadRequest(msg)),
        };
        // The credential is in memory before the row says `confirmed`, so a settle can never
        // see a confirmed row without it.
        self.pending.lock().unwrap_or_else(|e| e.into_inner()).insert(enrol_id.to_string(), token);
        let recorded = self.transition(
            enrol_id,
            State::AwaitingConfirmation,
            State::Confirmed,
            now_ms,
            Some(&computer_id),
            Some(&name),
        );
        match recorded {
            Ok(true) => Ok(ConfirmOutcome { computer_id, name }),
            other => {
                self.pending.lock().unwrap_or_else(|e| e.into_inner()).remove(enrol_id);
                registrar.remove(&computer_id);
                match other {
                    Err(e) => Err(e),
                    _ => Err(EnrolError::WrongState { id: enrol_id.into(), state: "changed" }),
                }
            }
        }
    }

    /// The owner denies the enrolment: `awaiting_confirmation` to `denied`. Nothing was
    /// registered, so there is nothing to undo.
    pub fn deny(&self, enrol_id: &str, now_ms: i64) -> Result<(), EnrolError> {
        let g = self.gate();
        self.settle(&g, now_ms)?;
        let row = self.load_or_not_found(enrol_id)?;
        match row.state {
            State::AwaitingConfirmation => {}
            State::Expired => return Err(EnrolError::Expired(enrol_id.into())),
            other => return Err(EnrolError::WrongState { id: enrol_id.into(), state: other.as_str() }),
        }
        if self.transition(enrol_id, State::AwaitingConfirmation, State::Denied, now_ms, None, None)? {
            Ok(())
        } else {
            Err(EnrolError::WrongState { id: enrol_id.into(), state: "changed" })
        }
    }

    /// The node gives up while it waits for the owner (FR-C2m, #617). The secret check is the
    /// poll's ([`Enrol::row_for_secret`]), so a wrong secret and an unknown id are the same
    /// [`NodeCancel::Unknown`]. Only `awaiting_confirmation` moves, to `expired` (a legal move;
    /// no new state, so the owner's app needs no change), and only before the owner answered.
    pub fn cancel_by_node(&self, enrol_id: &str, poll_secret: &str, now_ms: i64) -> Result<NodeCancel, EnrolError> {
        let g = self.gate();
        self.settle(&g, now_ms)?;
        let Some(row) = self.row_for_secret(enrol_id, poll_secret)? else {
            return Ok(NodeCancel::Unknown);
        };
        Ok(match row.state {
            State::Waiting => NodeCancel::Unknown,
            State::AwaitingConfirmation => {
                if self.transition(enrol_id, State::AwaitingConfirmation, State::Expired, now_ms, None, None)? {
                    NodeCancel::Cancelled
                } else {
                    NodeCancel::TooLate(State::Expired)
                }
            }
            other => NodeCancel::TooLate(other),
        })
    }

    /// The row of `enrol_id` when `poll_secret` is its poll secret: the secret is hashed and
    /// compared in constant effort, and an unknown id, a row without a poll hash (never redeemed)
    /// and a wrong secret all do the same work and give `None`. The caller holds the gate and has
    /// settled. Shared by [`Enrol::take_delivery`] and [`Enrol::verify_poll`], so they cannot
    /// disagree on who gets past the secret check.
    fn row_for_secret(&self, enrol_id: &str, poll_secret: &str) -> Result<Option<Row>, EnrolError> {
        let presented = digest(poll_secret);
        let row = self.load(enrol_id)?;
        let zero = vec![0u8; 32];
        let stored = row.as_ref().and_then(|r| r.poll_hash.as_ref()).unwrap_or(&zero);
        let matches = digests_eq(&presented, stored);
        let known = row.as_ref().is_some_and(|r| r.poll_hash.is_some());
        Ok(row.filter(|_| known && matches))
    }

    /// Whether `poll_secret` would get past the secret check of [`Enrol::take_delivery`], with no
    /// delivery: `true` exactly when `take_delivery` would not answer [`Delivery::Unknown`] (the
    /// row exists, has a poll hash, the digests match and the state is not `waiting`). It never
    /// touches `pending`, never transitions a row and never hands out a token; the only write is
    /// the time-based settle that every core call does first. The door uses it to limit only the
    /// polls whose secret verified, so a caller without a valid secret takes no limiter slot.
    pub fn verify_poll(&self, enrol_id: &str, poll_secret: &str, now_ms: i64) -> Result<bool, EnrolError> {
        let g = self.gate();
        self.settle(&g, now_ms)?;
        Ok(self.row_for_secret(enrol_id, poll_secret)?.is_some_and(|r| r.state != State::Waiting))
    }

    /// The node asks for its credential with its poll secret. The presented secret is hashed and
    /// compared in constant effort, and an unknown enrol id does the same work, so every bad
    /// secret and every unknown id is [`Delivery::Unknown`]. The token is returned once and
    /// wiped; a second call is [`Delivery::Delivered`].
    pub fn take_delivery(&self, enrol_id: &str, poll_secret: &str, now_ms: i64) -> Result<Delivery, EnrolError> {
        let g = self.gate();
        self.settle(&g, now_ms)?;
        let Some(row) = self.row_for_secret(enrol_id, poll_secret)? else {
            return Ok(Delivery::Unknown);
        };
        Ok(match row.state {
            State::Waiting => Delivery::Unknown,
            State::AwaitingConfirmation => Delivery::Pending,
            State::Joined => Delivery::Delivered,
            State::Denied => Delivery::Denied,
            State::Expired => Delivery::Expired,
            State::Confirmed => {
                let token = self.pending.lock().unwrap_or_else(|e| e.into_inner()).remove(enrol_id);
                match token {
                    Some(token) if self.transition(enrol_id, State::Confirmed, State::Joined, now_ms, None, None)? => {
                        Delivery::Joined {
                            token,
                            name: row.final_name.unwrap_or(row.name),
                            computer_id: row.computer_id.unwrap_or_default(),
                            server_peer_id: self.server_peer_id.clone(),
                        }
                    }
                    _ => {
                        let _ = self.transition(enrol_id, State::Confirmed, State::Expired, now_ms, None, None);
                        Delivery::Expired
                    }
                }
            }
        })
    }

    /// Cancel an unredeemed code by deleting it. A redeemed code cannot be cancelled
    /// ([`EnrolError::Used`]); remove the computer instead.
    pub fn cancel(&self, id: &str) -> Result<(), EnrolError> {
        let conn = self.store.conn();
        let n = conn.execute("DELETE FROM enrol_codes WHERE id = ?1 AND used_ms IS NULL", params![id])?;
        if n == 1 {
            return Ok(());
        }
        let exists: Option<i64> =
            conn.query_row("SELECT 1 FROM enrol_codes WHERE id = ?1", params![id], |r| r.get(0)).optional()?;
        Err(if exists.is_some() { EnrolError::Used(id.into()) } else { EnrolError::NotFound(id.into()) })
    }
}

// ---------------------------------------------------------------------------------------------
// Admin routes (local, token-protected router only; never the guest door, never the enrol door)
// ---------------------------------------------------------------------------------------------

/// What the owner router knows about the enrol door, to print a working command (FR-C1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DoorUrl {
    /// `EMBER_ENROL_LISTEN` is unset: no door, so no code is minted.
    Off,
    /// The door is on, but its address is unspecified (0.0.0.0, ::) and `EMBER_ENROL_PUBLIC_URL`
    /// is unset, so the server cannot know what the other machine should type.
    Unknown,
    /// The door URL the other machine uses (`http(s)://host[:port]`).
    Url(String),
}

#[derive(Clone)]
struct AppState {
    enrol: Arc<Enrol>,
    door: DoorUrl,
    registrar: Arc<dyn Registrar>,
    /// The auto-closing door (#539), `None` when the door is always open or off.
    gate: Option<Arc<crate::enrol_door::DoorGate>>,
}

/// `POST /api/computers/enrol-codes`, `GET` and `DELETE /api/computers/enrol-codes/{id}`, and
/// the owner's `POST /api/computers/enrol-codes/{id}/confirm` and `.../deny`. `base_url` is the
/// enrol door's URL for the printed command (the door port, not the owner API address; `Off` and
/// `Unknown` make the mint answer 409); `registrar` registers a confirmed computer. There is no redeem route. Merge it only into the token-protected local router.
pub fn router(enrol: Arc<Enrol>, door: DoorUrl, registrar: Arc<dyn Registrar>) -> Router {
    router_with_gate(enrol, door, registrar, None)
}

/// As [`router`], with the auto-closing door (#539): the mint opens `gate` before it answers, the
/// answer says `door: "open"`, and a cancel or deny nudges the gate so it closes at once when no
/// enrolment needs it any more.
pub fn router_with_gate(
    enrol: Arc<Enrol>,
    door: DoorUrl,
    registrar: Arc<dyn Registrar>,
    gate: Option<Arc<crate::enrol_door::DoorGate>>,
) -> Router {
    // The registry is needed to remove the computer of an enrolment that expires undelivered.
    // The first sweep covers a `confirmed` row left by a restart.
    enrol.set_registrar(registrar.clone());
    if enrol.sweep(crate::store::now_ms()).is_err() {
        tracing::warn!("the startup sweep of enrol codes failed");
    }
    Router::new()
        .route("/api/computers/enrol-codes", post(mint))
        .route("/api/computers/enrol-codes/{id}", get(status).delete(cancel))
        .route("/api/computers/enrol-codes/{id}/confirm", post(confirm))
        .route("/api/computers/enrol-codes/{id}/deny", post(deny))
        .with_state(AppState { enrol, door, registrar, gate })
}

struct ApiError(EnrolError);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (code, key) = match &self.0 {
            EnrolError::BadRequest(_) => (StatusCode::BAD_REQUEST, None),
            EnrolError::NotFound(_) => (StatusCode::NOT_FOUND, None),
            EnrolError::Used(_) => (StatusCode::CONFLICT, None),
            EnrolError::NameTaken(_) => (StatusCode::CONFLICT, Some("name_taken")),
            EnrolError::WrongState { .. } | EnrolError::IllegalTransition { .. } => {
                (StatusCode::CONFLICT, Some("wrong_state"))
            }
            EnrolError::Expired(_) => (StatusCode::GONE, Some("expired")),
            EnrolError::FingerprintMismatch => (StatusCode::CONFLICT, Some("fingerprint_mismatch")),
            EnrolError::FingerprintRequired => (StatusCode::CONFLICT, Some("fingerprint_required")),
            EnrolError::DoorOff => (StatusCode::CONFLICT, Some("door_off")),
            EnrolError::DoorUrlUnknown => (StatusCode::CONFLICT, Some("door_url_unknown")),
            EnrolError::DoorUnavailable => (StatusCode::SERVICE_UNAVAILABLE, Some("door_unavailable")),
            EnrolError::Other(_) => (StatusCode::INTERNAL_SERVER_ERROR, None),
        };
        // An internal error does not echo database text.
        let msg = match &self.0 {
            EnrolError::Other(_) => "internal error".to_string(),
            e => e.to_string(),
        };
        let body = match key {
            Some(k) => json!({ "error": k, "message": msg }),
            None => json!({ "error": msg }),
        };
        (code, Json(body)).into_response()
    }
}

impl From<EnrolError> for ApiError {
    fn from(e: EnrolError) -> Self {
        ApiError(e)
    }
}

#[derive(Deserialize)]
struct MintBody {
    name: String,
    /// Optional lifetime in seconds; clamped to 600, zero refused.
    #[serde(default)]
    ttl_secs: Option<u64>,
}

async fn mint(AxumState(s): AxumState<AppState>, Json(b): Json<MintBody>) -> Result<Response, ApiError> {
    // No door, no code: the owner is never handed a secret that cannot be redeemed.
    let door_url = match &s.door {
        DoorUrl::Off => return Err(EnrolError::DoorOff.into()),
        DoorUrl::Unknown => return Err(EnrolError::DoorUrlUnknown.into()),
        DoorUrl::Url(u) => check_base_url(u)?,
    };
    let m = s.enrol.mint(&b.name, crate::store::now_ms(), b.ttl_secs.map(Duration::from_secs), &door_url)?;
    // An auto-closing door opens now, before the code is shown. A bind failure cancels the code.
    let door_state = match &s.gate {
        None => "on",
        Some(g) => match g.open().await {
            Ok(()) => "open",
            Err(e) => {
                tracing::warn!(enrol = %m.id, "the enrol door could not be opened: {e}");
                let _ = s.enrol.cancel(&m.id);
                return Err(EnrolError::DoorUnavailable.into());
            }
        },
    };
    tracing::info!(enrol = %m.id, door = door_state, "enrol code minted");
    let mut warning = format!("The code is shown once. Check that {door_url} is reachable from the other machine.");
    if door_url.starts_with("http://") {
        warning.push_str(
            " The enrol door speaks plain HTTP. Anyone on the network path can read the code and the credential. \
             Use it on a trusted network, or put TLS in front of it and set EMBER_ENROL_PUBLIC_URL to the https address.",
        );
    }
    let mut body = json!({
        "id": m.id,
        "code": m.code.expose(),
        "expires_ms": m.expires_ms,
        "command": m.command,
        "warning": warning,
        "door": door_state,
        "server": door_url,
    });
    // Both keys are absent, never null, when the transport is off.
    if let (Some(id), Some(map)) = (s.enrol.server_peer_id(), body.as_object_mut()) {
        map.insert("server_peer_id".into(), json!(id));
        if let Some(short) = ember_transport::fingerprint_hex(id) {
            map.insert("server_fingerprint".into(), json!(short));
        }
    }
    let mut res = (StatusCode::CREATED, Json(body)).into_response();
    res.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(res)
}

/// `{status, name, node_info?, observed_addr?, proposed_url?, fingerprint?, confirm_deadline_ms?}`.
/// The optional fields are untrusted display data and appear while the owner can still decide.
async fn status(AxumState(s): AxumState<AppState>, Path(id): Path<String>) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(s.enrol.status_detail(&id, crate::store::now_ms())?))
}

async fn cancel(AxumState(s): AxumState<AppState>, Path(id): Path<String>) -> Result<impl IntoResponse, ApiError> {
    s.enrol.cancel(&id)?;
    if let Some(g) = &s.gate {
        g.nudge();
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Unknown fields are refused (422) rather than ignored, so a client cannot believe a check it
/// asked for was made. `expect_fingerprint` is the check for the peer id (FR-C2c).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfirmBody {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    expect_fingerprint: Option<String>,
}

async fn confirm(
    AxumState(s): AxumState<AppState>,
    Path(id): Path<String>,
    body: Option<Json<ConfirmBody>>,
) -> Result<impl IntoResponse, ApiError> {
    let (name, expect) = body.map(|Json(b)| (b.name, b.expect_fingerprint)).unwrap_or_default();
    let out = s.enrol.confirm(&id, name.as_deref(), expect.as_deref(), crate::store::now_ms(), &*s.registrar)?;
    tracing::info!(enrol = %id, computer = %out.computer_id, "enrol confirmed");
    Ok(Json(json!({ "computer_id": out.computer_id, "name": out.name })))
}

async fn deny(AxumState(s): AxumState<AppState>, Path(id): Path<String>) -> Result<impl IntoResponse, ApiError> {
    s.enrol.deny(&id, crate::store::now_ms())?;
    if let Some(g) = &s.gate {
        g.nudge();
    }
    tracing::info!(enrol = %id, "enrol denied");
    Ok(Json(json!({ "status": "denied" })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Barrier;

    const BASE: &str = "http://192.168.0.5:8740";
    const T0: i64 = 1_000_000_000;
    const SRC: &str = "10.0.0.7:5000";

    fn svc() -> (Arc<Store>, Arc<Enrol>) {
        let store = Arc::new(Store::open_in_memory().unwrap());
        (store.clone(), Enrol::new(store))
    }

    fn mint(e: &Enrol, now: i64) -> Minted {
        e.mint("laptop", now, None, BASE).unwrap()
    }

    fn info() -> NodeInfo {
        NodeInfo::minimal("laptop-host")
    }

    /// Redeem with a valid node description.
    fn rd(e: &Enrol, code: &str, now: i64, src: &str) -> Result<Redeemed, Refused> {
        e.redeem(code, &info(), now, src)
    }

    /// A registry that lives in a Vec.
    #[derive(Default)]
    struct Fake {
        names: Mutex<Vec<String>>,
        /// (name, token) in registration order.
        registered: Mutex<Vec<(String, String)>>,
        removed: Mutex<Vec<String>>,
    }

    impl Fake {
        fn count(&self) -> usize {
            self.registered.lock().unwrap().len()
        }
        fn token(&self, i: usize) -> String {
            self.registered.lock().unwrap()[i].1.clone()
        }
    }

    impl Registrar for Fake {
        fn name_taken(&self, name: &str) -> bool {
            self.names.lock().unwrap().iter().any(|n| n.eq_ignore_ascii_case(name))
        }
        fn register(&self, name: &str, token: &str, _i: &NodeInfo, _o: &str) -> Result<String, RegisterError> {
            if self.name_taken(name) {
                return Err(RegisterError::NameTaken);
            }
            self.names.lock().unwrap().push(name.to_string());
            let mut r = self.registered.lock().unwrap();
            r.push((name.to_string(), token.to_string()));
            Ok(format!("computer-{}", r.len()))
        }
        fn remove(&self, computer_id: &str) {
            self.removed.lock().unwrap().push(computer_id.to_string());
            // Idempotent: the name is freed once, a second call finds nothing.
            let idx: Option<usize> = computer_id.strip_prefix("computer-").and_then(|n| n.parse().ok());
            let name = idx.and_then(|n| self.registered.lock().unwrap().get(n - 1).map(|r| r.0.clone()));
            if let Some(name) = name {
                self.names.lock().unwrap().retain(|n| !n.eq_ignore_ascii_case(&name));
            }
        }
    }

    /// Mint and redeem; returns (enrol id, poll secret).
    fn open(e: &Enrol, now: i64) -> (String, String) {
        let m = mint(e, now);
        let r = rd(e, m.code.expose(), now + 1, SRC).unwrap();
        (r.enrol_id, r.poll_secret.expose().to_string())
    }

    // ---- C3b: the server peer id in the joined delivery ----

    /// Confirm and take the delivery of one enrolment on `e`.
    fn joined_of(e: &Enrol, now: i64) -> Delivery {
        let fake = Fake::default();
        let (id, secret) = open(e, now);
        e.confirm(&id, None, None, now + 10, &fake).unwrap();
        e.take_delivery(&id, &secret, now + 12).unwrap()
    }

    fn server_id_of(d: Delivery) -> Option<String> {
        match d {
            Delivery::Joined { server_peer_id, .. } => server_peer_id,
            other => panic!("{other:?}"),
        }
    }

    fn enrol_with(id: Option<&str>) -> Arc<Enrol> {
        let store = Arc::new(Store::open_in_memory().unwrap());
        Enrol::with_server_peer_id(store, id.map(String::from))
    }

    #[test]
    fn fr_c2_joined_carries_the_server_peer_id_when_set() {
        let id = "ab".repeat(32);
        let e = enrol_with(Some(&id));
        assert_eq!(server_id_of(joined_of(&e, T0)), Some(id));
    }

    #[test]
    fn fr_c2_joined_has_none_without_a_transport() {
        let (_, e) = svc();
        assert_eq!(server_id_of(joined_of(&e, T0)), None);
        let e = enrol_with(None);
        assert_eq!(server_id_of(joined_of(&e, T0)), None);
    }

    #[test]
    fn fr_c2_a_malformed_server_peer_id_is_dropped_never_stored() {
        for bad in ["zz".repeat(32), "a".repeat(63), "a".repeat(65), "AB".repeat(32), String::new(), " ".repeat(64)] {
            let e = enrol_with(Some(&bad));
            assert_eq!(e.server_peer_id, None, "{bad:?}");
            assert_eq!(server_id_of(joined_of(&e, T0)), None, "{bad:?}");
        }
    }

    #[test]
    fn fr_c2_the_server_peer_id_is_not_per_request() {
        let id = "cd".repeat(32);
        let e1 = enrol_with(Some(&id));
        let e2 = enrol_with(Some(&id));
        let a = server_id_of(joined_of(&e1, T0));
        let b = server_id_of(joined_of(&e2, T0));
        assert_eq!(a, b);
        assert_eq!(a, Some(id.clone()));
        // A second delivery on the same instance gives the same value.
        assert_eq!(server_id_of(joined_of(&e1, T0 + 1_000_000)), Some(id));
    }

    #[test]
    fn fr_c2_fingerprint_of_hex_matches_nodeinfo_fingerprint() {
        for id in ["ab".repeat(32), format!("0123456789abcdef{}", "e".repeat(48))] {
            let mut i = info();
            i.peer_id = Some(id.clone());
            assert_eq!(ember_transport::fingerprint_hex(&id), i.fingerprint());
            assert!(i.fingerprint().is_some());
        }
        assert_eq!(info().fingerprint(), None);
    }

    #[test]
    fn fr_c1_migration_creates_the_table() {
        let (store, _) = svc();
        let n: i64 = store.conn().query_row("SELECT count(*) FROM enrol_codes", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn fr_c2_confirm_migration_is_appended_after_the_first() {
        let first = crate::store::migration_position(MIGRATION).expect("enrol migration is registered");
        let second = crate::store::migration_position(CONFIRM_MIGRATION).expect("confirm migration is registered");
        assert!(second > first);
        // No column carries a node token.
        let (store, _) = svc();
        let conn = store.conn();
        let mut stmt = conn.prepare("SELECT name FROM pragma_table_info('enrol_codes')").unwrap();
        let cols: Vec<String> = stmt.query_map([], |r| r.get(0)).unwrap().map(|c| c.unwrap()).collect();
        assert!(cols.iter().all(|c| !c.contains("token")), "{cols:?}");
        for want in ["state", "poll_hash", "node_info", "observed_addr", "confirm_deadline_ms"] {
            assert!(cols.iter().any(|c| c == want), "{want} in {cols:?}");
        }
    }

    #[test]
    fn fr_c1_mint_stores_only_the_hash() {
        let (store, e) = svc();
        let m = mint(&e, T0);
        let code = m.code.expose().to_string();
        let (hash, name): (Vec<u8>, String) = store
            .conn()
            .query_row("SELECT code_hash, name FROM enrol_codes WHERE id = ?1", params![m.id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(hash, Sha256::digest(code.as_bytes()).to_vec());
        assert_eq!(name, "laptop");
        assert!(!dump(&store).contains(&code));
        assert!(!dump(&store).contains(code.trim_start_matches(CODE_PREFIX)));
    }

    /// Every cell of the table as text.
    fn dump(store: &Store) -> String {
        let conn = store.conn();
        let mut stmt = conn
            .prepare(
                "SELECT id, hex(code_hash), name, created_ms, expires_ms, used_ms, computer_id, state, hex(poll_hash),
                        node_info, observed_addr, confirm_deadline_ms, confirmed_ms, delivered_ms, denied_ms, final_name
                 FROM enrol_codes",
            )
            .unwrap();
        stmt.query_map([], |r| {
            let mut s = String::new();
            for i in 0..16 {
                s.push_str(&format!("{:?}|", r.get::<_, rusqlite::types::Value>(i)?));
            }
            Ok(s)
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
    }

    #[test]
    fn fr_c1_the_code_is_absent_from_the_database_files() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(&dir.path().join("ember.db")).unwrap());
        let e = Enrol::new(store.clone());
        let m = mint(&e, T0);
        rd(&e, m.code.expose(), T0 + 1, "a").unwrap();
        let code = m.code.expose().to_string();
        for entry in std::fs::read_dir(dir.path()).unwrap() {
            let bytes = std::fs::read(entry.unwrap().path()).unwrap();
            assert!(!bytes.windows(code.len()).any(|w| w == code.as_bytes()));
        }
    }

    #[test]
    fn fr_c1_code_format_and_entropy() {
        let (_, e) = svc();
        let m = mint(&e, T0);
        let c = m.code.expose();
        let body = c.strip_prefix("dpe_").expect("prefix");
        assert!(body.chars().all(|ch| ch.is_ascii_hexdigit()));
        // 244 bits need at least 61 hex characters.
        assert!(body.len() * 4 >= 244, "{} bits", body.len() * 4);
        let other = mint(&e, T0);
        assert_ne!(c, other.code.expose());
        assert_eq!(m.command, format!("ember-node enrol {c} --server {BASE}"));
    }

    // ---- C3c: the command names the door and pins the full server peer id ----

    #[test]
    fn fr_c2_command_has_the_full_server_peer_id_as_the_pin() {
        let id = "ab".repeat(32);
        let e = enrol_with(Some(&id));
        let m = mint(&e, T0);
        let c = m.code.expose();
        assert_eq!(m.command, format!("ember-node enrol {c} --server {BASE} --server-fingerprint {id}"));
        // The full 64 hex, never the short human form.
        let short = ember_transport::fingerprint_hex(&id).unwrap();
        assert!(!m.command.contains(&short), "{}", m.command);
        assert_eq!(m.command.rsplit(' ').next().unwrap().len(), 64);
    }

    #[test]
    fn fr_c2_command_is_the_old_string_without_a_server_peer_id() {
        let e = enrol_with(None);
        let m = mint(&e, T0);
        assert_eq!(m.command, format!("ember-node enrol {} --server {BASE}", m.code.expose()));
        assert!(!m.command.contains("--server-fingerprint"));
        assert_eq!(e.server_peer_id(), None);
    }

    #[test]
    fn fr_c2_server_peer_id_accessor_returns_the_stored_id() {
        let id = "cd".repeat(32);
        assert_eq!(enrol_with(Some(&id)).server_peer_id(), Some(id.as_str()));
    }

    /// The owner router with a given door state, plus the store to count rows.
    fn owner_app(enrol: Arc<Enrol>, door: DoorUrl) -> Router {
        struct Nop;
        impl Registrar for Nop {
            fn name_taken(&self, _n: &str) -> bool {
                false
            }
            fn register(&self, _n: &str, _t: &str, _i: &NodeInfo, _o: &str) -> Result<String, RegisterError> {
                Err(RegisterError::Failed("unused".into()))
            }
            fn remove(&self, _id: &str) {}
        }
        router(enrol, door, Arc::new(Nop))
    }

    async fn post_mint(app: &Router, body: serde_json::Value) -> (StatusCode, serde_json::Value, String, Option<String>) {
        use http_body_util::BodyExt;
        use tower::ServiceExt;
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/api/computers/enrol-codes")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(body.to_string()))
            .unwrap();
        let r = app.clone().oneshot(req).await.unwrap();
        let st = r.status();
        let cc = r.headers().get(header::CACHE_CONTROL).and_then(|v| v.to_str().ok()).map(String::from);
        let bytes = r.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&bytes).to_string();
        (st, serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null), text, cc)
    }

    fn row_count(store: &Store) -> i64 {
        store.conn().query_row("SELECT COUNT(*) FROM enrol_codes", [], |r| r.get(0)).unwrap()
    }

    #[tokio::test]
    async fn fr_c2_mint_answer_names_the_door_and_the_server_id() {
        let id = "0f".repeat(32);
        let store = Arc::new(Store::open_in_memory().unwrap());
        let e = Enrol::with_server_peer_id(store.clone(), Some(id.clone()));
        let door = "http://192.168.0.5:8741";
        let app = owner_app(e, DoorUrl::Url(door.into()));
        let (st, b, text, cc) = post_mint(&app, json!({"name": "laptop"})).await;
        assert_eq!(st, StatusCode::CREATED, "{text}");
        assert_eq!(cc.as_deref(), Some("no-store"));
        assert_eq!(b["door"], "on");
        assert_eq!(b["server"], door);
        assert_eq!(b["server_peer_id"], id);
        assert_eq!(b["server_fingerprint"], ember_transport::fingerprint_hex(&id).unwrap());
        let cmd = b["command"].as_str().unwrap();
        assert!(cmd.starts_with("ember-node enrol dpe_"), "{cmd}");
        assert!(cmd.contains(&format!(" --server {door} ")), "{cmd}");
        assert!(cmd.ends_with(&format!(" --server-fingerprint {id}")), "{cmd}");
        let w = b["warning"].as_str().unwrap();
        assert!(w.contains("shown once") && w.contains(door) && w.contains("plain HTTP"), "{w}");
        assert_eq!(row_count(&store), 1);
    }

    #[tokio::test]
    async fn fr_c2_mint_answer_omits_the_id_keys_without_a_transport() {
        let store = Arc::new(Store::open_in_memory().unwrap());
        let app = owner_app(Enrol::new(store), DoorUrl::Url("https://enrol.example.com".into()));
        let (st, b, text, _) = post_mint(&app, json!({"name": "laptop"})).await;
        assert_eq!(st, StatusCode::CREATED, "{text}");
        assert!(b.get("server_peer_id").is_none() && b.get("server_fingerprint").is_none(), "{text}");
        assert!(!text.contains("server_peer_id") && !text.contains("server_fingerprint"), "{text}");
        assert_eq!(b["door"], "on");
        assert_eq!(b["server"], "https://enrol.example.com");
        // https: no plain-HTTP sentence.
        assert!(!b["warning"].as_str().unwrap().contains("plain HTTP"));
        assert!(!b["command"].as_str().unwrap().contains("--server-fingerprint"));
    }

    #[tokio::test]
    async fn fr_c2_mint_with_the_door_off_is_409_and_creates_no_row() {
        let store = Arc::new(Store::open_in_memory().unwrap());
        let app = owner_app(Enrol::new(store.clone()), DoorUrl::Off);
        let (st, b, text, _) = post_mint(&app, json!({"name": "laptop"})).await;
        assert_eq!(st, StatusCode::CONFLICT, "{text}");
        assert_eq!(b["error"], "door_off");
        assert_eq!(
            b["message"],
            "The enrol door is off: set EMBER_ENROL_LISTEN (and EMBER_ENROL_PUBLIC_URL) and restart"
        );
        assert!(!text.contains("dpe_") && b.get("code").is_none() && b.get("command").is_none(), "{text}");
        assert_eq!(row_count(&store), 0);
    }

    #[tokio::test]
    async fn fr_c2_mint_with_an_unknown_door_url_is_409_and_creates_no_row() {
        let store = Arc::new(Store::open_in_memory().unwrap());
        let app = owner_app(Enrol::new(store.clone()), DoorUrl::Unknown);
        let (st, b, text, _) = post_mint(&app, json!({"name": "laptop"})).await;
        assert_eq!(st, StatusCode::CONFLICT, "{text}");
        assert_eq!(b["error"], "door_url_unknown");
        assert_eq!(b["message"], "Set EMBER_ENROL_PUBLIC_URL to the address the other machine uses");
        assert!(!text.contains("dpe_") && b.get("code").is_none(), "{text}");
        assert_eq!(row_count(&store), 0);
    }

    #[test]
    fn fr_c1_ttl_default_clamp_and_zero() {
        let (_, e) = svc();
        assert_eq!(mint(&e, T0).expires_ms, T0 + 600_000);
        let big = e.mint("a", T0, Some(Duration::from_secs(86_400)), BASE).unwrap();
        assert_eq!(big.expires_ms, T0 + 600_000);
        let short = e.mint("b", T0, Some(Duration::from_secs(30)), BASE).unwrap();
        assert_eq!(short.expires_ms, T0 + 30_000);
        assert!(matches!(e.mint("c", T0, Some(Duration::ZERO), BASE), Err(EnrolError::BadRequest(_))));
    }

    #[test]
    fn fr_c1_name_and_base_url_are_validated() {
        let (_, e) = svc();
        let long = "n".repeat(65);
        for bad in ["", "   ", "local", "this server", "a\nb", "x\u{200b}y", long.as_str()] {
            assert!(matches!(e.mint(bad, T0, None, BASE), Err(EnrolError::BadRequest(_))), "{bad:?}");
        }
        for bad in ["", "ftp://x", "http://a b", "http://x;rm -rf", "http://x'y", "javascript:1"] {
            assert!(matches!(e.mint("ok", T0, None, bad), Err(EnrolError::BadRequest(_))), "{bad:?}");
        }
        let m = e.mint(" ok ", T0, None, "https://ember.example/").unwrap();
        assert!(m.command.ends_with("--server https://ember.example"));
    }

    /// #526 item 5: no character in the printed command that zsh or bash would expand.
    #[test]
    fn fr_c1_base_url_refuses_glob_tilde_and_percent() {
        let (_, e) = svc();
        for bad in [
            "http://x[1]",
            "http://host/a[b]",
            "http://[::1]x",
            "http://[zz::1]:9300",
            "http://[]:9300",
            "http://[fe80::1%25en0]:9300",
            "http://host/~user",
            "http://host/%2e",
            "http://ho%73t",
            "http://:9300",
            "http:///path",
            "http://",
            "http://x*",
            "http://x?y",
        ] {
            assert!(matches!(e.mint("ok", T0, None, bad), Err(EnrolError::BadRequest(_))), "{bad:?}");
        }
        for good in ["http://[::1]:9300", "http://[fd00::12]", "https://enrol.example.com:443/ember", "http://10.0.0.5:9300"] {
            let m = e.mint("ok", T0, None, good).unwrap_or_else(|err| panic!("{good:?}: {err}"));
            assert!(m.command.contains(&format!("--server {good}")), "{good:?}");
        }
    }

    #[test]
    fn fr_c1_redeem_ok_once_then_refused() {
        let (_, e) = svc();
        let m = mint(&e, T0);
        let r = rd(&e, m.code.expose(), T0 + 1, "a").unwrap();
        assert_eq!(r.enrol_id, m.id);
        assert_eq!(r.expires_ms, T0 + 1 + CONFIRM_WINDOW_MS);
        assert_eq!(rd(&e, m.code.expose(), T0 + 2, "a"), Err(Refused::Generic));
    }

    #[test]
    fn fr_c2_a_code_with_the_wrong_shape_is_refused_and_recorded_as_a_failure() {
        let (_, e) = svc();
        let good = format!("{CODE_PREFIX}{}", "a".repeat(CODE_BODY_CHARS));
        assert!(code_has_shape(&good));
        let upper = format!("{CODE_PREFIX}{}", "A".repeat(CODE_BODY_CHARS));
        let short = format!("{CODE_PREFIX}{}", "a".repeat(CODE_BODY_CHARS - 1));
        let long = format!("{CODE_PREFIX}{}", "a".repeat(CODE_BODY_CHARS + 1));
        let nonhex = format!("{CODE_PREFIX}{}", "g".repeat(CODE_BODY_CHARS));
        for bad in ["", "dpe_", "dpe_wrong", upper.as_str(), short.as_str(), long.as_str(), nonhex.as_str()] {
            assert!(!code_has_shape(bad), "{bad:?}");
        }
        // Ten malformed codes spend the per-source budget like ten unknown ones, the eleventh is limited.
        for i in 0..MAX_FAILURES {
            assert_eq!(rd(&e, "dpe_wrong", T0 + i as i64, "m"), Err(Refused::Generic));
        }
        assert_eq!(rd(&e, "dpe_wrong", T0 + 20, "m"), Err(Refused::RateLimited));
        // A minted code has the shape.
        assert!(code_has_shape(mint(&e, T0).code.expose()));
    }

    /// Both kinds of refusal are the same `Refused::Generic` and spend the same budgets, so the
    /// only difference from outside is that a well-shaped unknown code goes to the store and a
    /// wrong-shaped one never does. The test makes that visible: it holds the store's connection
    /// lock. A wrong-shaped code must still be answered at once; a well-shaped one must wait for
    /// the lock (it can only finish after the lock is released). A `code_has_shape` that always
    /// said "yes" would block the first call.
    #[test]
    fn fr_c2_a_wrong_shaped_code_never_touches_the_store_but_a_well_shaped_one_does() {
        use std::sync::mpsc;
        let (store, e) = svc();
        let wrong = "dpe_wrong".to_string();
        let well_shaped_unknown = format!("{CODE_PREFIX}{}", "a".repeat(CODE_BODY_CHARS));
        assert!(code_has_shape(&well_shaped_unknown) && !code_has_shape(&wrong));

        let guard = store.conn();
        let (tx, rx) = mpsc::channel::<(&'static str, Result<Redeemed, Refused>)>();
        for (label, code) in [("wrong", wrong), ("shaped", well_shaped_unknown)] {
            let (e, tx) = (e.clone(), tx.clone());
            std::thread::spawn(move || {
                let _ = tx.send((label, e.redeem(&code, &info(), T0, "m")));
            });
        }
        let (label, result) = rx.recv_timeout(std::time::Duration::from_secs(5)).expect("the wrong-shaped code is answered while the store is locked");
        assert_eq!(label, "wrong", "the first answer comes from the code that never touches the store");
        assert_eq!(result, Err(Refused::Generic));
        assert!(
            rx.recv_timeout(std::time::Duration::from_millis(300)).is_err(),
            "the well-shaped code needs the store, which is locked"
        );
        drop(guard);
        let (label, result) = rx.recv_timeout(std::time::Duration::from_secs(5)).expect("answered once the lock is free");
        assert_eq!(label, "shaped");
        assert_eq!(result, Err(Refused::Generic), "the same refusal in the end");
    }

    #[test]
    fn fr_c1_expired_unknown_and_used_get_the_same_refusal() {
        let (_, e) = svc();
        let used = mint(&e, T0);
        rd(&e, used.code.expose(), T0 + 1, "a").unwrap();
        let expired = mint(&e, T0);
        let fresh = mint(&e, T0);
        let results = [
            rd(&e, used.code.expose(), T0 + 2, "b"),
            rd(&e, expired.code.expose(), T0 + 600_000, "c"),
            rd(&e, "dpe_00000000000000000000000000000000000000000000000000000000000000ff", T0 + 2, "d"),
            rd(&e, "", T0 + 2, "e"),
            rd(&e, "not a code at all", T0 + 2, "f"),
        ];
        for r in &results {
            assert_eq!(*r, Err(Refused::Generic));
            assert_eq!(r.as_ref().unwrap_err().message(), REFUSED_MESSAGE);
        }
        // The expiry instant itself is expired; one millisecond earlier is fine.
        assert!(rd(&e, fresh.code.expose(), T0 + 599_999, "g").is_ok());
    }

    #[test]
    fn fr_c1_one_winner_under_concurrency() {
        let (_, e) = svc();
        let m = mint(&e, T0);
        let code = m.code.expose().to_string();
        let n = 16;
        let barrier = Arc::new(Barrier::new(n));
        let handles: Vec<_> = (0..n)
            .map(|i| {
                let (e, code, barrier) = (e.clone(), code.clone(), barrier.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    rd(&e, &code, T0 + 1, &format!("src{i}")).is_ok()
                })
            })
            .collect();
        let wins = handles.into_iter().map(|h| h.join().unwrap()).filter(|w| *w).count();
        assert_eq!(wins, 1);
    }

    #[test]
    fn fr_c1_rate_limit_after_ten_failures_per_source() {
        let (_, e) = svc();
        let m = mint(&e, T0);
        for i in 0..MAX_FAILURES {
            assert_eq!(rd(&e, "dpe_wrong", T0 + i as i64, "1.2.3.4"), Err(Refused::Generic));
        }
        // The 11th attempt is limited, and so is a CORRECT code from the same source: the answer
        // does not depend on the code.
        assert_eq!(rd(&e, "dpe_wrong", T0 + 20, "1.2.3.4"), Err(Refused::RateLimited));
        assert_eq!(rd(&e, m.code.expose(), T0 + 21, "1.2.3.4"), Err(Refused::RateLimited));
        // Another source is unaffected, and the limited attempt did not burn the code.
        assert!(rd(&e, m.code.expose(), T0 + 22, "5.6.7.8").is_ok());
    }

    #[test]
    fn fr_c1_rate_limit_window_slides_and_success_does_not_count() {
        let (_, e) = svc();
        for i in 0..MAX_FAILURES {
            let _ = rd(&e, "dpe_wrong", T0 + i as i64, "s");
        }
        assert_eq!(rd(&e, "dpe_wrong", T0 + 100, "s"), Err(Refused::RateLimited));
        let m = mint(&e, T0 + FAIL_WINDOW_MS + 100);
        assert!(rd(&e, m.code.expose(), T0 + FAIL_WINDOW_MS + 101, "s").is_ok());
        // Many successes from one source never limit it (nor the global budget).
        for _ in 0..70 {
            let m = mint(&e, T0 + 1_000_000);
            assert!(rd(&e, m.code.expose(), T0 + 1_000_001, "ok-source").is_ok());
        }
        assert_eq!(rd(&e, "dpe_wrong", T0 + 1_000_002, "x"), Err(Refused::Generic));
    }

    #[test]
    fn fr_c1_cancel_unredeemed_only() {
        let (_, e) = svc();
        let m = mint(&e, T0);
        e.cancel(&m.id).unwrap();
        assert_eq!(rd(&e, m.code.expose(), T0 + 1, "a"), Err(Refused::Generic));
        assert!(matches!(e.cancel(&m.id), Err(EnrolError::NotFound(_))));
        let used = mint(&e, T0);
        rd(&e, used.code.expose(), T0 + 1, "a").unwrap();
        assert!(matches!(e.cancel(&used.id), Err(EnrolError::Used(_))));
    }

    #[test]
    fn fr_c1_expired_rows_are_swept_on_mint_after_the_grace() {
        let (store, e) = svc();
        let old = mint(&e, T0);
        let _ = mint(&e, T0 + SWEEP_GRACE_MS + 600_001);
        let n: i64 = store
            .conn()
            .query_row("SELECT count(*) FROM enrol_codes WHERE id = ?1", params![old.id], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn fr_c1_debug_of_the_code_types_is_redacted() {
        let (_, e) = svc();
        let m = mint(&e, T0);
        let code = m.code.expose().to_string();
        let body = code.trim_start_matches(CODE_PREFIX).to_string();
        for s in [format!("{:?}", m), format!("{:#?}", m), format!("{:?}", m.code)] {
            assert!(!s.contains(&body), "{s}");
        }
        for s in [
            format!("{:?}", rd(&e, &code, T0, "a")),
            format!("{:?}", rd(&e, &code, T0, "a")),
            format!("{:?}", EnrolError::Used(m.id.clone())),
        ] {
            assert!(!s.contains(&body), "{s}");
        }
    }

    /// A tracing writer that collects everything written.
    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for Capture {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
        type Writer = Capture;
        fn make_writer(&'a self) -> Capture {
            self.clone()
        }
    }

    #[test]
    fn fr_c1_the_code_is_in_no_tracing_output() {
        let cap = Capture::default();
        let sub = tracing_subscriber::fmt()
            .with_writer(cap.clone())
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .finish();
        let code = tracing::subscriber::with_default(sub, || {
            let (_, e) = svc();
            let m = mint(&e, T0);
            tracing::info!(minted = ?m, "after mint");
            tracing::info!(code = ?m.code, "code field");
            let code = m.code.expose().to_string();
            let _ = rd(&e, &code, T0 + 1, "a");
            let _ = rd(&e, &code, T0 + 2, "a");
            code
        });
        let out = String::from_utf8(cap.0.lock().unwrap().clone()).unwrap();
        assert!(!out.contains(code.trim_start_matches(CODE_PREFIX)), "{out}");
        assert!(out.contains("redacted"));
    }

    // ---- FR-C2: confirm at join ----------------------------------------------------------

    #[test]
    fn fr_c2_redeem_moves_to_awaiting_confirmation_and_stores_only_the_poll_hash() {
        let (store, e) = svc();
        let m = mint(&e, T0);
        assert_eq!(e.status(&m.id, T0).unwrap(), EnrolStatus::Waiting);
        let r = rd(&e, m.code.expose(), T0 + 1, SRC).unwrap();
        assert_eq!(e.status(&m.id, T0 + 2).unwrap(), EnrolStatus::AwaitingConfirmation);
        let secret = r.poll_secret.expose().to_string();
        assert!(secret.starts_with(POLL_PREFIX));
        assert!(secret.trim_start_matches(POLL_PREFIX).len() * 4 >= 256);
        assert_eq!(r.expires_ms, T0 + 1 + 600_000);

        let (state, hash, deadline, observed): (String, Vec<u8>, i64, String) = store
            .conn()
            .query_row(
                "SELECT state, poll_hash, confirm_deadline_ms, observed_addr FROM enrol_codes WHERE id = ?1",
                params![m.id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(state, "awaiting_confirmation");
        assert_eq!(hash, Sha256::digest(secret.as_bytes()).to_vec());
        assert_eq!(deadline, T0 + 1 + 600_000);
        assert_eq!(observed, SRC);
        // The secret is in no cell.
        let d = dump(&store);
        assert!(!d.contains(&secret) && !d.contains(secret.trim_start_matches(POLL_PREFIX)));
        // A second redeem of the same code is refused and does not change the first.
        assert_eq!(rd(&e, m.code.expose(), T0 + 3, SRC), Err(Refused::Generic));
        // Two enrolments never share a poll secret.
        let (_, other) = open(&e, T0);
        assert_ne!(secret, other);
    }

    #[test]
    fn fr_c2_no_credential_exists_before_confirm() {
        let (store, e) = svc();
        let (id, secret) = open(&e, T0);
        assert!(matches!(e.take_delivery(&id, &secret, T0 + 5).unwrap(), Delivery::Pending));
        assert!(e.pending.lock().unwrap().is_empty());
        let fake = Fake::default();
        assert_eq!(fake.count(), 0);
        assert!(!dump(&store).contains(TOKEN_PREFIX));
    }

    #[test]
    fn fr_c2_confirm_registers_once_and_the_token_is_delivered_once() {
        let (store, e) = svc();
        let fake = Fake::default();
        let (id, secret) = open(&e, T0);
        let out = e.confirm(&id, None, None, T0 + 10, &fake).unwrap();
        assert_eq!(out, ConfirmOutcome { computer_id: "computer-1".into(), name: "laptop".into() });
        assert_eq!(fake.count(), 1);
        // The owner still reads awaiting_confirmation until the node has the credential.
        assert_eq!(e.status(&id, T0 + 11).unwrap(), EnrolStatus::AwaitingConfirmation);
        assert_eq!(e.pending.lock().unwrap().len(), 1);
        let token = fake.token(0);
        assert!(token.starts_with(TOKEN_PREFIX) && token.trim_start_matches(TOKEN_PREFIX).len() * 4 >= 256);
        // The token is in no SQLite cell, before or after delivery.
        assert!(!dump(&store).contains(&token));

        match e.take_delivery(&id, &secret, T0 + 12).unwrap() {
            Delivery::Joined { token: t, name, computer_id, .. } => {
                assert_eq!(t.expose(), token);
                assert_eq!(name, "laptop");
                assert_eq!(computer_id, "computer-1");
            }
            other => panic!("{other:?}"),
        }
        assert!(e.pending.lock().unwrap().is_empty());
        assert_eq!(e.status(&id, T0 + 13).unwrap(), EnrolStatus::Joined);
        assert!(matches!(e.take_delivery(&id, &secret, T0 + 14).unwrap(), Delivery::Delivered));
        assert!(matches!(e.take_delivery(&id, &secret, T0 + 15).unwrap(), Delivery::Delivered));
        assert_eq!(fake.count(), 1);
        assert!(!dump(&store).contains(&token));
    }

    #[test]
    fn fr_c2_a_second_confirm_is_wrong_state() {
        let (_, e) = svc();
        let fake = Fake::default();
        let (id, secret) = open(&e, T0);
        e.confirm(&id, None, None, T0 + 10, &fake).unwrap();
        let again = e.confirm(&id, Some("other"), None, T0 + 11, &fake);
        assert!(matches!(again, Err(EnrolError::WrongState { .. })), "{again:?}");
        // After delivery too, and a deny after a confirm.
        let _ = e.take_delivery(&id, &secret, T0 + 12).unwrap();
        assert!(matches!(e.confirm(&id, None, None, T0 + 13, &fake), Err(EnrolError::WrongState { .. })));
        assert!(matches!(e.deny(&id, T0 + 14), Err(EnrolError::WrongState { .. })));
        assert_eq!(fake.count(), 1);
        assert!(matches!(e.confirm("nope", None, None, T0, &fake), Err(EnrolError::NotFound(_))));
        // A code that was never redeemed cannot be confirmed.
        let m = mint(&e, T0);
        assert!(matches!(e.confirm(&m.id, None, None, T0 + 1, &fake), Err(EnrolError::WrongState { .. })));
    }

    #[test]
    fn fr_c2_deny_registers_nothing_and_the_node_sees_denied() {
        let (_, e) = svc();
        let fake = Fake::default();
        let (id, secret) = open(&e, T0);
        e.deny(&id, T0 + 10).unwrap();
        assert!(matches!(e.take_delivery(&id, &secret, T0 + 11).unwrap(), Delivery::Denied));
        assert_eq!(e.status(&id, T0 + 12).unwrap(), EnrolStatus::Denied);
        assert!(matches!(e.confirm(&id, None, None, T0 + 13, &fake), Err(EnrolError::WrongState { .. })));
        assert!(matches!(e.deny(&id, T0 + 14), Err(EnrolError::WrongState { .. })));
        assert_eq!(fake.count(), 0);
        assert!(e.pending.lock().unwrap().is_empty());
        // The code was burned by the redeem: a denied enrolment needs a new mint.
        let m = e.mint("laptop", T0, None, BASE).unwrap();
        assert!(rd(&e, m.code.expose(), T0 + 20, SRC).is_ok());
    }

    #[test]
    fn fr_c2_confirm_after_the_deadline_is_refused() {
        let (_, e) = svc();
        let fake = Fake::default();
        let (id, secret) = open(&e, T0);
        let redeemed_at = T0 + 1;
        let late = redeemed_at + CONFIRM_WINDOW_MS;
        assert!(matches!(e.confirm(&id, None, None, late, &fake), Err(EnrolError::Expired(_))));
        assert_eq!(fake.count(), 0);
        assert!(matches!(e.take_delivery(&id, &secret, late + 1).unwrap(), Delivery::Expired));
        assert_eq!(e.status(&id, late + 2).unwrap(), EnrolStatus::Expired);
        assert!(matches!(e.deny(&id, late + 3), Err(EnrolError::Expired(_))));
        // One millisecond before the deadline still works.
        let (id2, _) = open(&e, T0);
        assert!(e.confirm(&id2, None, None, T0 + 1 + CONFIRM_WINDOW_MS - 1, &fake).is_ok());
    }

    #[test]
    fn fr_c2_name_clash_keeps_awaiting_confirmation_and_a_rename_succeeds() {
        let (_, e) = svc();
        let fake = Fake::default();
        fake.names.lock().unwrap().push("Laptop".into());
        let (id, secret) = open(&e, T0);
        let r = e.confirm(&id, None, None, T0 + 10, &fake);
        assert!(matches!(r, Err(EnrolError::NameTaken(ref n)) if n == "laptop"), "{r:?}");
        assert_eq!(e.status(&id, T0 + 11).unwrap(), EnrolStatus::AwaitingConfirmation);
        assert_eq!(fake.count(), 0);
        assert!(e.pending.lock().unwrap().is_empty());
        assert!(matches!(e.take_delivery(&id, &secret, T0 + 12).unwrap(), Delivery::Pending));
        // A bad replacement name is a bad request and changes nothing.
        assert!(matches!(e.confirm(&id, Some("a\nb"), None, T0 + 13, &fake), Err(EnrolError::BadRequest(_))));
        assert!(matches!(e.confirm(&id, Some("laptop"), None, T0 + 14, &fake), Err(EnrolError::NameTaken(_))));
        let ok = e.confirm(&id, Some("laptop-2"), None, T0 + 15, &fake).unwrap();
        assert_eq!(ok.name, "laptop-2");
        match e.take_delivery(&id, &secret, T0 + 16).unwrap() {
            Delivery::Joined { name, .. } => assert_eq!(name, "laptop-2"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn fr_c2_a_bad_poll_secret_is_indistinguishable_from_an_unknown_enrol_id() {
        let (_, e) = svc();
        let fake = Fake::default();
        let (id, secret) = open(&e, T0);
        let waiting = mint(&e, T0);
        let wrong = format!("{POLL_PREFIX}{}", "0".repeat(96));
        let cases = [
            e.take_delivery(&id, &wrong, T0 + 5).unwrap(),
            e.take_delivery(&id, "", T0 + 5).unwrap(),
            e.take_delivery("no-such-id", &secret, T0 + 5).unwrap(),
            e.take_delivery("no-such-id", &wrong, T0 + 5).unwrap(),
            // A code that was minted but never redeemed has no poll secret.
            e.take_delivery(&waiting.id, &secret, T0 + 5).unwrap(),
            e.take_delivery(&waiting.id, waiting.code.expose(), T0 + 5).unwrap(),
        ];
        for c in &cases {
            assert!(matches!(c, Delivery::Unknown), "{c:?}");
        }
        // The right secret answers; the same wrong secret still does not, in every later state.
        assert!(matches!(e.take_delivery(&id, &secret, T0 + 6).unwrap(), Delivery::Pending));
        e.confirm(&id, None, None, T0 + 7, &fake).unwrap();
        assert!(matches!(e.take_delivery(&id, &wrong, T0 + 8).unwrap(), Delivery::Unknown));
        assert!(e.pending.lock().unwrap().len() == 1, "a wrong secret takes nothing");
        // Another enrolment's secret does not open this one.
        let (_, other_secret) = open(&e, T0);
        assert!(matches!(e.take_delivery(&id, &other_secret, T0 + 9).unwrap(), Delivery::Unknown));
        assert!(matches!(e.take_delivery(&id, &secret, T0 + 10).unwrap(), Delivery::Joined { .. }));
    }

    #[test]
    fn fr_c2_verify_poll_has_no_side_effect() {
        let (_, e) = svc();
        let fake = Fake::default();
        let (id, secret) = open(&e, T0);
        let wrong = format!("{POLL_PREFIX}{}", "0".repeat(96));
        e.confirm(&id, None, None, T0 + 5, &fake).unwrap();
        assert_eq!(e.pending.lock().unwrap().len(), 1);
        assert!(e.verify_poll(&id, &secret, T0 + 6).unwrap());
        assert!(e.verify_poll(&id, &secret, T0 + 7).unwrap(), "verifying twice changes nothing");
        assert_eq!(e.pending.lock().unwrap().len(), 1, "the token is still pending");
        assert_eq!(e.status(&id, T0 + 8).unwrap(), EnrolStatus::AwaitingConfirmation);
        assert!(!e.verify_poll(&id, &wrong, T0 + 9).unwrap());
        assert!(!e.verify_poll(&id, "", T0 + 9).unwrap());
        assert!(!e.verify_poll("no-such-id", &secret, T0 + 9).unwrap());
        assert_eq!(e.pending.lock().unwrap().len(), 1, "a wrong secret or unknown id takes nothing");
        match e.take_delivery(&id, &secret, T0 + 10).unwrap() {
            Delivery::Joined { .. } => {}
            other => panic!("{other:?}"),
        }
        assert!(e.pending.lock().unwrap().is_empty());
        assert!(e.verify_poll(&id, &secret, T0 + 11).unwrap(), "joined still verifies");
        assert!(matches!(e.take_delivery(&id, &secret, T0 + 12).unwrap(), Delivery::Delivered));
    }

    #[test]
    fn fr_c2_verify_poll_agrees_with_take_delivery_on_who_gets_past_the_secret_check() {
        let wrong = format!("{POLL_PREFIX}{}", "0".repeat(96));
        let check = |e: &Enrol, id: &str, secret: &str, now: i64, label: &str| {
            let v = e.verify_poll(id, secret, now).unwrap();
            let d = e.take_delivery(id, secret, now).unwrap();
            assert_eq!(v, !matches!(d, Delivery::Unknown), "{label}: verify_poll {v}, take_delivery {d:?}");
        };
        let fake = Fake::default();
        // Waiting: minted, never redeemed, no poll hash.
        let (_, e) = svc();
        let m = mint(&e, T0);
        check(&e, &m.id, m.code.expose(), T0 + 1, "waiting");
        assert!(!e.verify_poll(&m.id, m.code.expose(), T0 + 1).unwrap());
        // AwaitingConfirmation.
        let (id, secret) = open(&e, T0);
        check(&e, &id, &secret, T0 + 5, "awaiting_confirmation");
        assert!(e.verify_poll(&id, &secret, T0 + 5).unwrap());
        // Wrong secret and unknown id.
        check(&e, &id, &wrong, T0 + 6, "wrong secret");
        check(&e, &id, "", T0 + 6, "empty secret");
        check(&e, "no-such-id", &secret, T0 + 6, "unknown id");
        // Confirmed (verify first, then take_delivery inside check delivers; Joined follows).
        let (id2, secret2) = open(&e, T0);
        e.confirm(&id2, None, None, T0 + 7, &fake).unwrap();
        check(&e, &id2, &secret2, T0 + 8, "confirmed");
        // Joined (already delivered by the check above).
        check(&e, &id2, &secret2, T0 + 9, "joined");
        // Denied.
        let (id3, secret3) = open(&e, T0);
        e.deny(&id3, T0 + 7).unwrap();
        check(&e, &id3, &secret3, T0 + 8, "denied");
        // Expired.
        let (id4, secret4) = open(&e, T0);
        check(&e, &id4, &secret4, T0 + 1 + CONFIRM_WINDOW_MS + 1, "expired");
    }

    #[test]
    fn fr_c2_concurrent_confirm_and_deny_exactly_one_wins() {
        for round in 0..20 {
            let (_, e) = svc();
            let fake = Arc::new(Fake::default());
            let (id, _) = open(&e, T0);
            let barrier = Arc::new(Barrier::new(2));
            let c = {
                let (e, fake, id, b) = (e.clone(), fake.clone(), id.clone(), barrier.clone());
                std::thread::spawn(move || {
                    b.wait();
                    e.confirm(&id, None, None, T0 + 10, &*fake).is_ok()
                })
            };
            let d = {
                let (e, id, b) = (e.clone(), id.clone(), barrier.clone());
                std::thread::spawn(move || {
                    b.wait();
                    e.deny(&id, T0 + 10).is_ok()
                })
            };
            let (confirmed, denied) = (c.join().unwrap(), d.join().unwrap());
            assert!(confirmed ^ denied, "round {round}: confirm {confirmed}, deny {denied}");
            // The loser left nothing behind: a registration exists exactly when confirm won.
            assert_eq!(fake.count(), usize::from(confirmed), "round {round}");
            assert!(fake.removed.lock().unwrap().is_empty());
            let st = e.status(&id, T0 + 11).unwrap();
            assert_eq!(st, if confirmed { EnrolStatus::AwaitingConfirmation } else { EnrolStatus::Denied });
            assert_eq!(e.pending.lock().unwrap().len(), usize::from(confirmed));
        }
    }

    #[test]
    fn fr_c2_node_info_is_validated_at_redeem() {
        let (_, e) = svc();
        let m = mint(&e, T0);
        let long = "h".repeat(65);
        let mut bad: Vec<NodeInfo> = Vec::new();
        for host in ["", "   ", "a\nb", "tab\there", "nul\u{0}", "x\u{202e}y", "z\u{200b}w", "l\u{2028}s", long.as_str()] {
            bad.push(NodeInfo::minimal(host));
        }
        let mut with = |f: &dyn Fn(&mut NodeInfo)| {
            let mut i = info();
            f(&mut i);
            bad.push(i);
        };
        with(&|i| i.os = "li\nnux".into());
        with(&|i| i.arch = "a".repeat(65));
        with(&|i| i.node_version = "1\u{202e}".into());
        with(&|i| i.url = Some("ftp://10.0.0.1:8741".into()));
        with(&|i| i.url = Some("http://10.0.0.1 :8741".into()));
        with(&|i| i.url = Some("http://10.0.0.1/\"x".into()));
        with(&|i| i.url = Some(format!("http://{}", "a".repeat(300))));
        with(&|i| i.peer_id = Some("zz".repeat(32)));
        with(&|i| i.peer_id = Some("ab".repeat(31)));
        for i in &bad {
            // Refused whether the code is right or wrong: the answer does not depend on the code.
            assert_eq!(e.redeem(m.code.expose(), i, T0 + 1, SRC), Err(Refused::BadNodeInfo), "{i:?}");
            assert_eq!(e.redeem("dpe_wrong", i, T0 + 1, SRC), Err(Refused::BadNodeInfo), "{i:?}");
        }
        // None of that burned the code or spent a failure budget.
        for n in 0..100 {
            assert_eq!(e.redeem("dpe_wrong", &bad[0], T0 + 1, &format!("flood{n}")), Err(Refused::BadNodeInfo));
        }
        let good = NodeInfo {
            hostname: "h".repeat(64),
            os: "linux".into(),
            arch: "aarch64".into(),
            node_version: "0.1.0".into(),
            url: Some("http://10.0.0.7:8741".into()),
            peer_id: Some("ab".repeat(32)),
        };
        let r = e.redeem(m.code.expose(), &good, T0 + 2, SRC).unwrap();
        let d = e.status_detail(&r.enrol_id, T0 + 3).unwrap();
        assert_eq!(d.node_info.unwrap(), good);
        assert_eq!(d.fingerprint.as_deref(), Some("abab abab abab abab"));
        assert_eq!(d.proposed_url, None);
    }

    #[test]
    fn fr_c2_the_observed_address_is_stored_clean_and_proposes_a_url() {
        let (_, e) = svc();
        let m = mint(&e, T0);
        let r = e.redeem(m.code.expose(), &info(), T0 + 1, "192.168.0.9:50123").unwrap();
        let d = e.status_detail(&r.enrol_id, T0 + 2).unwrap();
        assert_eq!(d.observed_addr.as_deref(), Some("192.168.0.9:50123"));
        assert_eq!(d.proposed_url.as_deref(), Some("http://192.168.0.9:8741"));
        assert_eq!(d.name, "laptop");
        assert_eq!(d.confirm_deadline_ms, Some(T0 + 1 + CONFIRM_WINDOW_MS));
        assert_eq!(observed_node_url("[fe80::1]:9").as_deref(), Some("http://[fe80::1]:8741"));
        assert_eq!(observed_node_url("not an address"), None);
        // Control characters and length are cleaned; an empty address becomes "unknown".
        let m = mint(&e, T0);
        let r = e.redeem(m.code.expose(), &info(), T0 + 1, &format!("a\nb\u{202e}{}", "9".repeat(100))).unwrap();
        let d = e.status_detail(&r.enrol_id, T0 + 2).unwrap();
        let o = d.observed_addr.unwrap();
        assert!(o.chars().count() <= MAX_ADDR_CHARS && !o.contains('\n') && !o.contains('\u{202e}'));
        let m = mint(&e, T0);
        let r = e.redeem(m.code.expose(), &info(), T0 + 1, "").unwrap();
        assert_eq!(e.status_detail(&r.enrol_id, T0 + 2).unwrap().observed_addr.as_deref(), Some("unknown"));
        // After the result the details are no longer shown.
        e.deny(&r.enrol_id, T0 + 3).unwrap();
        let d = e.status_detail(&r.enrol_id, T0 + 4).unwrap();
        assert!(d.node_info.is_none() && d.observed_addr.is_none() && d.confirm_deadline_ms.is_none());
    }

    #[test]
    fn fr_c2_global_failure_cap_blocks_every_source_even_after_key_rotation() {
        // Defaults: 60 failures from 60 different sources lock out the 61st source and a valid
        // code, then 5000 more sources (more than the 4096 tracked keys) stay locked out.
        let (_, e) = svc();
        let m = mint(&e, T0);
        for i in 0..GLOBAL_MAX_FAILURES {
            assert_eq!(rd(&e, "dpe_wrong", T0 + i as i64, &format!("src{i}")), Err(Refused::Generic));
        }
        assert_eq!(rd(&e, m.code.expose(), T0 + 100, "fresh"), Err(Refused::RateLimited));
        for n in 0..(MAX_TRACKED_SOURCES + 904) {
            assert_eq!(rd(&e, m.code.expose(), T0 + 101, &format!("rot{n}")), Err(Refused::RateLimited));
        }
        // The window slides: the oldest failures age out and a valid code works again.
        let later = mint(&e, T0 + GLOBAL_FAIL_WINDOW_MS + 50);
        assert!(rd(&e, later.code.expose(), T0 + GLOBAL_FAIL_WINDOW_MS + 100, "fresh").is_ok());
    }

    #[test]
    fn fr_c2_per_source_eviction_cannot_reset_the_global_counter() {
        // Only 4 source keys are tracked, so each new failing source evicts an old key.
        let cfg = LimitConfig { max_tracked_sources: 4, global_failures: 20, ..LimitConfig::default() };
        let store = Arc::new(Store::open_in_memory().unwrap());
        let e = Enrol::with_limits(store, cfg);
        let m = mint(&e, T0);
        for i in 0..20 {
            assert_eq!(rd(&e, "dpe_wrong", T0 + i, &format!("s{i}")), Err(Refused::Generic), "attempt {i}");
        }
        assert!(e.failures.lock().unwrap().len() <= 4);
        // s0 was evicted from the per-source map; it and a fresh source are still locked out.
        assert_eq!(rd(&e, m.code.expose(), T0 + 30, "s0"), Err(Refused::RateLimited));
        assert_eq!(rd(&e, m.code.expose(), T0 + 30, "never-seen"), Err(Refused::RateLimited));
        assert!(e.global_failures.lock().unwrap().len() <= 20);
        // After the window the cap lifts for everyone.
        let later = mint(&e, T0 + GLOBAL_FAIL_WINDOW_MS + 20);
        assert!(rd(&e, later.code.expose(), T0 + GLOBAL_FAIL_WINDOW_MS + 30, "never-seen").is_ok());
    }

    #[test]
    fn fr_c2_a_restart_loses_the_pending_token_and_the_enrolment_reads_expired() {
        let (store, e1) = svc();
        let fake = Fake::default();
        let (id, secret) = open(&e1, T0);
        e1.confirm(&id, None, None, T0 + 10, &fake).unwrap();
        assert_eq!(e1.pending.lock().unwrap().len(), 1);
        // A new service over the same store has no pending token.
        let e2 = Enrol::new(store.clone());
        assert!(e2.pending.lock().unwrap().is_empty());
        assert!(matches!(e2.take_delivery(&id, &secret, T0 + 11).unwrap(), Delivery::Expired));
        assert_eq!(e2.status(&id, T0 + 12).unwrap(), EnrolStatus::Expired);
        // The old instance sees the same row and wipes its copy of the credential.
        assert_eq!(e1.status(&id, T0 + 13).unwrap(), EnrolStatus::Expired);
        assert!(e1.pending.lock().unwrap().is_empty());
        assert!(matches!(e1.take_delivery(&id, &secret, T0 + 14).unwrap(), Delivery::Expired));
        // These services have no registrar, so nothing was removed (the orphan waits for one;
        // see the fr_c2_*_removes_the_computer tests for the cleanup).
        assert_eq!(fake.count(), 1);
        assert!(fake.removed.lock().unwrap().is_empty());
    }

    #[test]
    fn fr_c2_confirmed_then_expired_removes_the_computer() {
        let (_, e) = svc();
        let fake = Arc::new(Fake::default());
        e.set_registrar(fake.clone());
        let (id, _secret) = open(&e, T0);
        e.confirm(&id, None, None, T0 + 10, &*fake).unwrap();
        assert!(fake.removed.lock().unwrap().is_empty());
        // Inside the grace nothing happens.
        assert_eq!(e.status(&id, T0 + 11).unwrap(), EnrolStatus::AwaitingConfirmation);
        assert!(fake.removed.lock().unwrap().is_empty());
        // Past the grace the node never polled: expired, and the computer is removed.
        let late = T0 + 10 + DELIVERY_GRACE_MS;
        assert_eq!(e.status(&id, late).unwrap(), EnrolStatus::Expired);
        assert_eq!(fake.removed.lock().unwrap().as_slice(), ["computer-1"]);
        assert!(!fake.name_taken("laptop"));
        // A second sweep neither removes again nor fails.
        e.sweep(late + 1).unwrap();
        assert_eq!(e.status(&id, late + 2).unwrap(), EnrolStatus::Expired);
        assert_eq!(fake.removed.lock().unwrap().len(), 1);
        // The same name can be enrolled again.
        let (id2, _) = open(&e, late + 3);
        assert!(e.confirm(&id2, None, None, late + 10, &*fake).is_ok());
    }

    #[test]
    fn fr_c2_a_restart_expires_the_confirmed_row_and_removes_its_computer() {
        let (store, e1) = svc();
        let fake = Arc::new(Fake::default());
        let (id, _secret) = open(&e1, T0);
        e1.confirm(&id, None, None, T0 + 10, &*fake).unwrap();
        // A new service over the same store: no token in memory. The first read settles.
        let e2 = Enrol::new(store.clone());
        e2.set_registrar(fake.clone());
        assert_eq!(e2.status(&id, T0 + 11).unwrap(), EnrolStatus::Expired);
        assert_eq!(fake.removed.lock().unwrap().as_slice(), ["computer-1"]);
        assert!(!fake.name_taken("laptop"));
        assert_eq!(e2.status(&id, T0 + 12).unwrap(), EnrolStatus::Expired);
        assert_eq!(fake.removed.lock().unwrap().len(), 1);
    }

    #[test]
    fn fr_c2_a_startup_sweep_removes_the_orphan_without_any_read() {
        let (store, e1) = svc();
        let fake = Arc::new(Fake::default());
        let (id, _) = open(&e1, T0);
        e1.confirm(&id, None, None, T0 + 10, &*fake).unwrap();
        let e2 = Enrol::new(store.clone());
        e2.set_registrar(fake.clone());
        e2.sweep(T0 + 11).unwrap();
        assert_eq!(fake.removed.lock().unwrap().as_slice(), ["computer-1"]);
    }

    #[test]
    fn fr_c2_an_orphan_found_before_the_registrar_is_set_is_removed_later() {
        let (store, e1) = svc();
        let fake = Arc::new(Fake::default());
        let (id, _) = open(&e1, T0);
        e1.confirm(&id, None, None, T0 + 10, &*fake).unwrap();
        let e2 = Enrol::new(store.clone());
        e2.sweep(T0 + 11).unwrap();
        assert!(fake.removed.lock().unwrap().is_empty());
        e2.set_registrar(fake.clone());
        e2.sweep(T0 + 12).unwrap();
        assert_eq!(fake.removed.lock().unwrap().as_slice(), ["computer-1"]);
    }

    #[test]
    fn fr_c2_a_joined_enrolment_is_never_removed() {
        let (_, e) = svc();
        let fake = Arc::new(Fake::default());
        e.set_registrar(fake.clone());
        let (id, secret) = open(&e, T0);
        e.confirm(&id, None, None, T0 + 10, &*fake).unwrap();
        assert!(matches!(e.take_delivery(&id, &secret, T0 + 11).unwrap(), Delivery::Joined { .. }));
        let far = T0 + 10 * DELIVERY_GRACE_MS;
        assert_eq!(e.status(&id, far).unwrap(), EnrolStatus::Joined);
        e.sweep(far + 1).unwrap();
        // A restart does not touch it either.
        let e2 = Enrol::new(e.store.clone());
        e2.set_registrar(fake.clone());
        assert_eq!(e2.status(&id, far + 2).unwrap(), EnrolStatus::Joined);
        assert!(fake.removed.lock().unwrap().is_empty());
        assert!(fake.name_taken("laptop"));
    }

    #[test]
    fn fr_c2_denied_and_unconfirmed_expiry_remove_nothing() {
        let (_, e) = svc();
        let fake = Arc::new(Fake::default());
        e.set_registrar(fake.clone());
        let (id, _) = open(&e, T0);
        let (id2, _) = open(&e, T0);
        e.deny(&id, T0 + 5).unwrap();
        e.sweep(T0 + CONFIRM_WINDOW_MS + 100).unwrap();
        assert_eq!(e.status(&id2, T0 + CONFIRM_WINDOW_MS + 101).unwrap(), EnrolStatus::Expired);
        assert!(fake.removed.lock().unwrap().is_empty());
    }

    #[test]
    fn fr_c2_a_rate_limit_key_longer_than_64_chars_is_limited_per_source() {
        let (_, e) = svc();
        let long = "a".repeat(200);
        for i in 0..MAX_FAILURES {
            assert_eq!(rd(&e, "dpe_wrong", T0 + i as i64, &long), Err(Refused::Generic));
        }
        assert_eq!(rd(&e, "dpe_wrong", T0 + 100, &long), Err(Refused::RateLimited));
        // Another source is not limited.
        assert_eq!(rd(&e, "dpe_wrong", T0 + 101, "9.9.9.9"), Err(Refused::Generic));
    }

    #[test]
    fn fr_c2_sources_sharing_their_first_64_chars_share_one_bucket() {
        // Intended: the door passes an IP address, so keys are short in practice, and the cut
        // bounds memory per entry.
        let (_, e) = svc();
        let (a, b) = (format!("{}x", "k".repeat(64)), format!("{}y", "k".repeat(64)));
        for i in 0..MAX_FAILURES {
            let _ = rd(&e, "dpe_wrong", T0 + i as i64, &a);
        }
        assert_eq!(rd(&e, "dpe_wrong", T0 + 100, &b), Err(Refused::RateLimited));
    }

    #[test]
    fn fr_c2_register_saying_name_taken_after_a_clear_check_is_409_and_the_state_stays() {
        // Another registry writer takes the name between the check and `register`.
        struct Lying(Fake);
        impl Registrar for Lying {
            fn name_taken(&self, _n: &str) -> bool {
                false
            }
            fn register(&self, n: &str, t: &str, i: &NodeInfo, o: &str) -> Result<String, RegisterError> {
                if n == "laptop" {
                    return Err(RegisterError::NameTaken);
                }
                self.0.register(n, t, i, o)
            }
            fn remove(&self, id: &str) {
                self.0.remove(id)
            }
        }
        let (_, e) = svc();
        let reg = Lying(Fake::default());
        let (id, secret) = open(&e, T0);
        let r = e.confirm(&id, None, None, T0 + 10, &reg);
        assert!(matches!(r, Err(EnrolError::NameTaken(ref n)) if n == "laptop"), "{r:?}");
        assert_eq!(e.status(&id, T0 + 11).unwrap(), EnrolStatus::AwaitingConfirmation);
        assert!(e.pending.lock().unwrap().is_empty());
        assert!(matches!(e.take_delivery(&id, &secret, T0 + 12).unwrap(), Delivery::Pending));
        assert!(e.confirm(&id, Some("laptop-2"), None, T0 + 13, &reg).is_ok());
    }

    #[test]
    fn fr_c2_an_undelivered_credential_expires_and_is_wiped() {
        let (_, e) = svc();
        let fake = Fake::default();
        let (id, secret) = open(&e, T0);
        e.confirm(&id, None, None, T0 + 10, &fake).unwrap();
        let late = T0 + 10 + DELIVERY_GRACE_MS;
        assert!(matches!(e.take_delivery(&id, &secret, late).unwrap(), Delivery::Expired));
        assert!(e.pending.lock().unwrap().is_empty());
    }

    #[test]
    fn fr_c2_only_hashes_are_stored_in_the_database_files() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(&dir.path().join("ember.db")).unwrap());
        let e = Enrol::new(store);
        let fake = Fake::default();
        let (id, secret) = open(&e, T0);
        e.confirm(&id, None, None, T0 + 10, &fake).unwrap();
        let token = fake.token(0);
        let scan = |when: &str| {
            for entry in std::fs::read_dir(dir.path()).unwrap() {
                let bytes = std::fs::read(entry.unwrap().path()).unwrap();
                for needle in [&secret, &token] {
                    assert!(!bytes.windows(needle.len()).any(|w| w == needle.as_bytes()), "{when}");
                }
            }
        };
        scan("before delivery");
        let _ = e.take_delivery(&id, &secret, T0 + 11).unwrap();
        scan("after delivery");
    }

    #[test]
    fn fr_c2_the_transition_table_is_explicit_and_illegal_moves_are_typed_errors() {
        use State::*;
        let all = [Waiting, AwaitingConfirmation, Confirmed, Joined, Denied, Expired];
        let legal = [
            (Waiting, AwaitingConfirmation),
            (Waiting, Expired),
            (AwaitingConfirmation, Confirmed),
            (AwaitingConfirmation, Denied),
            (AwaitingConfirmation, Expired),
            (Confirmed, Joined),
            (Confirmed, Expired),
        ];
        for a in all {
            for b in all {
                assert_eq!(a.can_move_to(b), legal.contains(&(a, b)), "{a:?} -> {b:?}");
            }
            assert_eq!(State::parse(a.as_str()), Some(a));
        }
        for fin in [Joined, Denied, Expired] {
            assert!(all.iter().all(|b| !fin.can_move_to(*b)), "{fin:?} is final");
        }
        let (_, e) = svc();
        let err = e.transition("x", Joined, Waiting, T0, None, None);
        assert!(matches!(err, Err(EnrolError::IllegalTransition { from: "joined", to: "waiting" })), "{err:?}");
        let err = e.transition("x", Waiting, Joined, T0, None, None);
        assert!(matches!(err, Err(EnrolError::IllegalTransition { .. })));
        // A legal move on a row that is not in `from` is Ok(false), not a change.
        assert_eq!(e.transition("x", Waiting, Expired, T0, None, None).unwrap(), false);
    }

    #[test]
    fn fr_c2_status_values_waiting_awaiting_joined_denied_expired() {
        let (_, e) = svc();
        let fake = Fake::default();
        let json = |s: EnrolStatus| serde_json::to_value(s).unwrap();
        let w = mint(&e, T0);
        assert_eq!(json(e.status(&w.id, T0).unwrap()), "waiting");
        assert_eq!(json(e.status(&w.id, T0 + 600_000).unwrap()), "expired");
        let (a, secret) = open(&e, T0);
        assert_eq!(json(e.status(&a, T0 + 5).unwrap()), "awaiting_confirmation");
        e.confirm(&a, None, None, T0 + 6, &fake).unwrap();
        assert_eq!(json(e.status(&a, T0 + 7).unwrap()), "awaiting_confirmation");
        let _ = e.take_delivery(&a, &secret, T0 + 8).unwrap();
        assert_eq!(json(e.status(&a, T0 + 9).unwrap()), "joined");
        // Joined stays joined after every deadline.
        assert_eq!(json(e.status(&a, T0 + 10_000_000).unwrap()), "joined");
        let (d, _) = open(&e, T0);
        e.deny(&d, T0 + 5).unwrap();
        assert_eq!(json(e.status(&d, T0 + 6).unwrap()), "denied");
        let (x, _) = open(&e, T0);
        assert_eq!(json(e.status(&x, T0 + 1 + CONFIRM_WINDOW_MS).unwrap()), "expired");
        assert!(matches!(e.status("nope", T0), Err(EnrolError::NotFound(_))));
    }

    #[test]
    fn fr_c2_the_secret_types_print_redacted() {
        let (_, e) = svc();
        let fake = Fake::default();
        let (id, secret) = open(&e, T0);
        let r = rd(&e, "dpe_wrong", T0, "z");
        e.confirm(&id, None, None, T0 + 10, &fake).unwrap();
        let token = fake.token(0);
        let delivery = e.take_delivery(&id, &secret, T0 + 11).unwrap();
        let m = mint(&e, T0);
        let redeemed = rd(&e, m.code.expose(), T0 + 1, "q").unwrap();
        let rbody = redeemed.poll_secret.expose().trim_start_matches(POLL_PREFIX).to_string();
        for s in [
            format!("{r:?}"),
            format!("{delivery:?}"),
            format!("{delivery:#?}"),
            format!("{redeemed:?}"),
            format!("{:?}", redeemed.poll_secret),
        ] {
            assert!(!s.contains(token.trim_start_matches(TOKEN_PREFIX)), "{s}");
            assert!(!s.contains(secret.trim_start_matches(POLL_PREFIX)), "{s}");
            assert!(!s.contains(&rbody), "{s}");
        }
        assert!(format!("{redeemed:?}").contains("redacted"));
    }

    #[test]
    fn fr_c2_the_secrets_are_in_no_tracing_output() {
        let cap = Capture::default();
        let sub = tracing_subscriber::fmt()
            .with_writer(cap.clone())
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .finish();
        let (secret, token, code) = tracing::subscriber::with_default(sub, || {
            let (_, e) = svc();
            let fake = Fake::default();
            let m = mint(&e, T0);
            let r = rd(&e, m.code.expose(), T0 + 1, SRC).unwrap();
            tracing::info!(redeemed = ?r, "after redeem");
            e.confirm(&r.enrol_id, None, None, T0 + 2, &fake).unwrap();
            let d = e.take_delivery(&r.enrol_id, r.poll_secret.expose(), T0 + 3);
            tracing::info!(delivery = ?d, "after delivery");
            let _ = e.deny(&r.enrol_id, T0 + 4);
            (r.poll_secret.expose().to_string(), fake.token(0), m.code.expose().to_string())
        });
        let out = String::from_utf8(cap.0.lock().unwrap().clone()).unwrap();
        for s in [secret.trim_start_matches(POLL_PREFIX), token.trim_start_matches(TOKEN_PREFIX), code.trim_start_matches(CODE_PREFIX)] {
            assert!(!out.contains(s), "{out}");
        }
        assert!(out.contains("redacted"));
    }

    #[test]
    fn fr_c2_a_failed_record_rolls_the_registration_back() {
        // A row that moved on between the check and the record (here: expired by a second
        // service over the same store) makes the record fail; the registration is removed.
        struct Racing {
            inner: Fake,
            store: Arc<Store>,
            hits: AtomicUsize,
        }
        impl Registrar for Racing {
            fn name_taken(&self, n: &str) -> bool {
                self.inner.name_taken(n)
            }
            fn register(&self, n: &str, t: &str, i: &NodeInfo, o: &str) -> Result<String, RegisterError> {
                self.hits.fetch_add(1, Ordering::SeqCst);
                self.store.conn().execute("UPDATE enrol_codes SET state = 'denied'", []).unwrap();
                self.inner.register(n, t, i, o)
            }
            fn remove(&self, id: &str) {
                self.inner.remove(id)
            }
        }
        let (store, e) = svc();
        let reg = Racing { inner: Fake::default(), store: store.clone(), hits: AtomicUsize::new(0) };
        let (id, _) = open(&e, T0);
        let r = e.confirm(&id, None, None, T0 + 10, &reg);
        assert!(matches!(r, Err(EnrolError::WrongState { .. })), "{r:?}");
        assert_eq!(reg.hits.load(Ordering::SeqCst), 1);
        assert_eq!(reg.inner.removed.lock().unwrap().as_slice(), ["computer-1"]);
        assert!(e.pending.lock().unwrap().is_empty());
    }

    // ---- C4: register by the node's peer id (FR-C2c) ----

    fn real_id() -> String {
        ember_transport::SecretKey::generate().peer_id().to_string()
    }

    fn node(url: Option<&str>, peer: Option<&str>) -> NodeInfo {
        let mut i = info();
        i.url = url.map(String::from);
        i.peer_id = peer.map(String::from);
        i
    }

    #[test]
    fn fr_c2_target_url_wins_over_a_peer_id() {
        let id = real_id();
        let i = node(Some("http://10.1.1.1:8741"), Some(&id));
        for on in [true, false] {
            assert_eq!(register_target(&i, SRC, on, None).unwrap(), Target::Url("http://10.1.1.1:8741".into()));
        }
    }

    #[test]
    fn fr_c2_target_peer_only_with_transport_registers_by_peer() {
        let id = real_id();
        let i = node(None, Some(&id));
        let want: ember_transport::PeerId = id.parse().unwrap();
        assert_eq!(register_target(&i, SRC, true, Some(&real_id())).unwrap(), Target::Peer(want));
        // Upper-case hex from the node parses to the same peer.
        let up = node(None, Some(&id.to_ascii_uppercase()));
        assert_eq!(register_target(&up, SRC, true, None).unwrap(), Target::Peer(want));
    }

    #[test]
    fn fr_c2_target_peer_only_without_transport_falls_back_to_the_observed_url() {
        let i = node(None, Some(&real_id()));
        assert_eq!(register_target(&i, "192.168.0.9:5000", false, None).unwrap(), Target::Url("http://192.168.0.9:8741".into()));
    }

    #[test]
    fn fr_c2_target_neither_uses_the_observed_url_or_the_old_error() {
        let i = node(None, None);
        for on in [true, false] {
            assert_eq!(register_target(&i, "10.0.0.7:5000", on, None).unwrap(), Target::Url("http://10.0.0.7:8741".into()));
            match register_target(&i, "unknown", on, None) {
                Err(RegisterError::Failed(m)) => {
                    assert_eq!(m, "the node reported no address and none could be derived")
                }
                other => panic!("{other:?}"),
            }
        }
    }

    #[test]
    fn fr_c2_target_refuses_the_servers_own_peer_id() {
        let own = real_id();
        for i in [node(None, Some(&own)), node(None, Some(&own.to_ascii_uppercase())), node(Some("http://10.1.1.1:8741"), Some(&own))] {
            match register_target(&i, SRC, true, Some(&own)) {
                Err(RegisterError::Failed(m)) => assert_eq!(m, "the node's peer id is this server's own"),
                other => panic!("{other:?}"),
            }
        }
    }

    #[test]
    fn fr_c2_target_an_invalid_peer_id_is_an_error_not_a_panic() {
        // 64 hex digits that are not an Ed25519 point of large order (all 0xff: y >= p, refused).
        for bad in ["ff".repeat(32), "zz".repeat(32), "ab".repeat(31)] {
            let i = node(None, Some(&bad));
            match register_target(&i, SRC, true, None) {
                Err(RegisterError::Failed(m)) => assert_eq!(m, "the node's peer id is not valid", "{bad}"),
                other => panic!("{other:?}"),
            }
            // Without the transport the peer id is not used at all.
            assert!(matches!(register_target(&i, SRC, false, None), Ok(Target::Url(_))), "{bad}");
        }
    }

    #[test]
    fn fr_c2_redeem_registers_nothing_only_confirm_does() {
        let (_, e) = svc();
        let fake = Fake::default();
        let m = mint(&e, T0);
        let r = e.redeem(m.code.expose(), &node(None, Some(&real_id())), T0 + 1, SRC).unwrap();
        e.status_detail(&r.enrol_id, T0 + 2).unwrap();
        assert_eq!(fake.count(), 0);
        e.deny(&r.enrol_id, T0 + 3).unwrap();
        assert_eq!(fake.count(), 0);
        let (id, _) = open(&e, T0 + 10);
        assert_eq!(fake.count(), 0);
        e.confirm(&id, None, None, T0 + 12, &fake).unwrap();
        assert_eq!(fake.count(), 1);
    }

    #[test]
    fn fr_c2_confirm_view_carries_the_node_reported_fingerprint_only_while_awaiting() {
        let (_, e) = svc();
        let id = real_id();
        let m = mint(&e, T0);
        let r = e.redeem(m.code.expose(), &node(None, Some(&id)), T0 + 1, SRC).unwrap();
        let d = e.status_detail(&r.enrol_id, T0 + 2).unwrap();
        assert_eq!(d.status, EnrolStatus::AwaitingConfirmation);
        assert_eq!(d.fingerprint, ember_transport::fingerprint_hex(&id));
        assert_eq!(d.node_info.unwrap().peer_id.as_deref(), Some(id.as_str()));
        e.deny(&r.enrol_id, T0 + 3).unwrap();
        let d = e.status_detail(&r.enrol_id, T0 + 4).unwrap();
        assert!(d.fingerprint.is_none() && d.node_info.is_none());
    }
    // ---- C5: confirm fingerprint, registered_by, duplicate peer, register failure ----

    /// A registrar that answers `registers_by` with a fixed value, over a [`Fake`].
    struct By {
        inner: Fake,
        by: Option<&'static str>,
    }
    impl Registrar for By {
        fn name_taken(&self, n: &str) -> bool {
            self.inner.name_taken(n)
        }
        fn register(&self, n: &str, t: &str, i: &NodeInfo, o: &str) -> Result<String, RegisterError> {
            self.inner.register(n, t, i, o)
        }
        fn remove(&self, id: &str) {
            self.inner.remove(id)
        }
        fn registers_by(&self, _i: &NodeInfo, _o: &str) -> Option<&'static str> {
            self.by
        }
    }

    fn by(by: Option<&'static str>) -> By {
        By { inner: Fake::default(), by }
    }

    /// Redeem `info` on a fresh code; returns (enrol id, poll secret).
    fn open_node(e: &Enrol, info: &NodeInfo, now: i64) -> (String, String) {
        let m = mint(e, now);
        let r = e.redeem(m.code.expose(), info, now + 1, SRC).unwrap();
        (r.enrol_id, r.poll_secret.expose().to_string())
    }

    #[test]
    fn fr_c2_confirm_with_a_matching_fingerprint_ignores_case_and_spaces() {
        let id = real_id();
        let fp = ember_transport::fingerprint_hex(&id).unwrap();
        for given in [fp.clone(), fp.to_ascii_uppercase(), fp.replace(' ', ""), format!("  {}  ", fp.to_ascii_uppercase())] {
            let (_, e) = svc();
            let reg = by(Some("url"));
            let (eid, _) = open_node(&e, &node(None, Some(&id)), T0);
            let out = e.confirm(&eid, None, Some(&given), T0 + 10, &reg);
            assert!(out.is_ok(), "{given:?}: {out:?}");
            assert_eq!(reg.inner.count(), 1);
        }
    }

    #[test]
    fn fr_c2_confirm_with_a_wrong_fingerprint_is_a_mismatch_and_changes_nothing() {
        let (_, e) = svc();
        let reg = by(Some("url"));
        let (eid, secret) = open_node(&e, &node(None, Some(&real_id())), T0);
        let other = ember_transport::fingerprint_hex(&real_id()).unwrap();
        let r = e.confirm(&eid, None, Some(&other), T0 + 10, &reg);
        assert!(matches!(r, Err(EnrolError::FingerprintMismatch)), "{r:?}");
        assert_eq!(reg.inner.count(), 0);
        assert!(e.pending.lock().unwrap().is_empty());
        assert_eq!(e.status(&eid, T0 + 11).unwrap(), EnrolStatus::AwaitingConfirmation);
        assert!(matches!(e.take_delivery(&eid, &secret, T0 + 12).unwrap(), Delivery::Pending));
        // The right one still confirms afterwards.
        let fp = e.status_detail(&eid, T0 + 13).unwrap().fingerprint.unwrap();
        assert!(e.confirm(&eid, None, Some(&fp), T0 + 14, &reg).is_ok());
    }

    #[test]
    fn fr_c2_a_node_without_a_peer_id_never_matches_an_expected_fingerprint() {
        let (_, e) = svc();
        let reg = by(Some("url"));
        let (eid, _) = open_node(&e, &node(Some("http://10.1.1.1:8741"), None), T0);
        let fp = ember_transport::fingerprint_hex(&real_id()).unwrap();
        let r = e.confirm(&eid, None, Some(&fp), T0 + 10, &reg);
        assert!(matches!(r, Err(EnrolError::FingerprintMismatch)), "{r:?}");
        assert_eq!(reg.inner.count(), 0);
        assert_eq!(e.status(&eid, T0 + 11).unwrap(), EnrolStatus::AwaitingConfirmation);
    }

    #[test]
    fn fr_c2_a_malformed_expected_fingerprint_is_a_bad_request_and_changes_nothing() {
        let (_, e) = svc();
        let reg = by(Some("url"));
        let id = real_id();
        let (eid, _) = open_node(&e, &node(None, Some(&id)), T0);
        let good = ember_transport::fingerprint_hex(&id).unwrap();
        let too_long = format!("{good}0");
        let too_short = good[..good.len() - 1].to_string();
        let bad = ["", "abab", "zzzz zzzz zzzz zzzz", too_long.as_str(), too_short.as_str(), "ab ab"];
        for b in bad {
            let r = e.confirm(&eid, None, Some(b), T0 + 10, &reg);
            assert!(matches!(r, Err(EnrolError::BadRequest(_))), "{b:?}: {r:?}");
        }
        assert_eq!(reg.inner.count(), 0);
        assert_eq!(e.status(&eid, T0 + 11).unwrap(), EnrolStatus::AwaitingConfirmation);
    }

    #[test]
    fn fr_c2_the_peer_path_requires_the_fingerprint() {
        let (_, e) = svc();
        let reg = by(Some("peer"));
        let id = real_id();
        let (eid, secret) = open_node(&e, &node(None, Some(&id)), T0);
        let r = e.confirm(&eid, None, None, T0 + 10, &reg);
        assert!(matches!(r, Err(EnrolError::FingerprintRequired)), "{r:?}");
        assert_eq!(reg.inner.count(), 0);
        assert!(e.pending.lock().unwrap().is_empty());
        assert_eq!(e.status(&eid, T0 + 11).unwrap(), EnrolStatus::AwaitingConfirmation);
        assert!(matches!(e.take_delivery(&eid, &secret, T0 + 12).unwrap(), Delivery::Pending));
        let fp = ember_transport::fingerprint_hex(&id).unwrap().to_ascii_uppercase();
        assert!(e.confirm(&eid, None, Some(&fp), T0 + 13, &reg).is_ok());
        assert_eq!(reg.inner.count(), 1);
    }

    #[test]
    fn fr_c2_the_url_and_observed_paths_do_not_require_the_fingerprint() {
        for path in [Some("url"), Some("observed"), None] {
            let (_, e) = svc();
            let reg = by(path);
            let (eid, _) = open_node(&e, &node(None, Some(&real_id())), T0);
            assert!(e.confirm(&eid, None, None, T0 + 10, &reg).is_ok(), "{path:?}");
        }
    }

    #[test]
    fn fr_c2_status_shows_how_the_node_would_be_registered_only_while_awaiting() {
        let id = real_id();
        let cases: [(NodeInfo, &'static str); 4] = [
            // The silently-dropped case: a url wins over the peer id.
            (node(Some("http://10.1.1.1:8741"), Some(&id)), "url"),
            (node(Some("http://10.1.1.1:8741"), None), "url"),
            (node(None, Some(&id)), "peer"),
            (node(None, None), "observed"),
        ];
        for (n, want) in cases {
            let (_, e) = svc();
            // A stand-in for the real rule, which `register_target` tests cover.
            let reg = Arc::new(by(Some(want)));
            e.set_registrar(reg.clone());
            let (eid, _) = open_node(&e, &n, T0);
            let d = e.status_detail(&eid, T0 + 5).unwrap();
            assert_eq!(d.registered_by, Some(want));
            let v = serde_json::to_value(&d).unwrap();
            assert_eq!(v["registered_by"], want);
            if want == "url" && n.peer_id.is_some() {
                assert_eq!(v["node_info"]["peer_id"], id.as_str());
                assert_eq!(v["node_info"]["url"], "http://10.1.1.1:8741");
            }
            e.deny(&eid, T0 + 6).unwrap();
            let d = e.status_detail(&eid, T0 + 7).unwrap();
            assert_eq!(d.registered_by, None);
            assert!(serde_json::to_value(&d).unwrap().get("registered_by").is_none());
        }
    }

    #[test]
    fn fr_c2_status_has_no_registered_by_without_a_registrar() {
        let (_, e) = svc();
        let (eid, _) = open_node(&e, &node(None, Some(&real_id())), T0);
        let d = e.status_detail(&eid, T0 + 5).unwrap();
        assert_eq!(d.registered_by, None);
        assert!(serde_json::to_value(&d).unwrap().get("registered_by").is_none());
        // A registrar that cannot say (the trait default) leaves the key out too.
        e.set_registrar(Arc::new(Fake::default()));
        let d = e.status_detail(&eid, T0 + 6).unwrap();
        assert!(serde_json::to_value(&d).unwrap().get("registered_by").is_none());
    }

    #[test]
    fn fr_c2_a_failed_register_leaves_the_enrolment_confirmable_with_no_token_left() {
        // `register` fails after the name check. Pinned behaviour: the token is held only in a
        // local until `register` succeeds, so a failure leaves no pending credential, nothing
        // registered, the state `awaiting_confirmation`, and a 400 for the owner.
        struct Failing {
            inner: Fake,
            fail: AtomicUsize,
        }
        impl Registrar for Failing {
            fn name_taken(&self, n: &str) -> bool {
                self.inner.name_taken(n)
            }
            fn register(&self, n: &str, t: &str, i: &NodeInfo, o: &str) -> Result<String, RegisterError> {
                if self.fail.fetch_add(1, Ordering::SeqCst) == 0 {
                    return Err(RegisterError::Failed("a computer with this peer id is already registered".into()));
                }
                self.inner.register(n, t, i, o)
            }
            fn remove(&self, id: &str) {
                self.inner.remove(id)
            }
        }
        let (store, e) = svc();
        let reg = Failing { inner: Fake::default(), fail: AtomicUsize::new(0) };
        let (eid, secret) = open_node(&e, &node(None, None), T0);
        let r = e.confirm(&eid, None, None, T0 + 10, &reg);
        assert!(
            matches!(r, Err(EnrolError::BadRequest(ref m)) if m == "a computer with this peer id is already registered"),
            "{r:?}"
        );
        assert_eq!(reg.inner.count(), 0);
        assert!(reg.inner.removed.lock().unwrap().is_empty());
        assert!(e.pending.lock().unwrap().is_empty());
        assert_eq!(e.status(&eid, T0 + 11).unwrap(), EnrolStatus::AwaitingConfirmation);
        // Nothing could be delivered, and the row records no computer.
        assert!(matches!(e.take_delivery(&eid, &secret, T0 + 12).unwrap(), Delivery::Pending));
        let n: i64 = store
            .conn()
            .query_row("SELECT COUNT(*) FROM enrol_codes WHERE computer_id IS NOT NULL", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
        // The same enrolment confirms on the next try.
        assert!(e.confirm(&eid, None, None, T0 + 13, &reg).is_ok());
        assert_eq!(reg.inner.count(), 1);
    }

    #[tokio::test]
    async fn fr_c2_the_real_registrar_maps_a_duplicate_peer_to_failed_and_reports_peer_by() {
        // A `Computers` with a transport dialer, so the peer path is live.
        let net = ember_transport::mem::MemNetwork::new();
        let st = crate::transport::ServerTransport::new(net.transport());
        let store = Arc::new(Store::open_in_memory().unwrap());
        let computers = Computers::with_transport(
            crate::computers::Registry::new(store),
            crate::computers::connector(Some(st.dialer.clone())),
            None,
            Some(st.dialer.clone()),
        );
        let reg = ComputersRegistrar::new(computers.clone());
        let id = real_id();
        let n = node(None, Some(&id));
        assert_eq!(reg.registers_by(&n, SRC), Some("peer"));
        assert_eq!(reg.registers_by(&node(Some("http://10.1.1.1:8741"), Some(&id)), SRC), Some("url"));
        assert_eq!(reg.registers_by(&node(None, None), SRC), Some("observed"));
        assert_eq!(reg.registers_by(&node(None, None), "unknown"), None);
        reg.register("first", "tok", &n, SRC).unwrap();
        match reg.register("second", "tok", &n, SRC) {
            Err(RegisterError::Failed(m)) => assert_eq!(m, "a computer with this peer id is already registered"),
            other => panic!("{other:?}"),
        }
        assert_eq!(computers.registry().list().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn fr_c2_without_a_transport_a_peer_only_node_registers_by_its_observed_address() {
        // Mutation M12: a real registrar over a `Computers` with no dialer. The mirror (the same
        // node with a dialer gives "peer") is pinned by
        // `fr_c2_the_real_registrar_maps_a_duplicate_peer_to_failed_and_reports_peer_by`; it is
        // repeated here only for the stored shape.
        let store = Arc::new(Store::open_in_memory().unwrap());
        let computers = Computers::new(crate::computers::Registry::new(store), crate::computers::connector(None));
        assert!(computers.dialer().is_none());
        let reg = ComputersRegistrar::new(computers.clone());
        let n = node(None, Some(&real_id()));
        assert_eq!(reg.registers_by(&n, SRC), Some("observed"));
        let id = reg.register("plain", "tok", &n, SRC).unwrap();
        let c = computers.registry().get(&id).unwrap().unwrap();
        assert!(c.peer.is_none(), "{c:?}");
        assert!(!c.url.is_empty(), "{c:?}");

        // With the dialer the same node is registered by peer.
        let net = ember_transport::mem::MemNetwork::new();
        let st = crate::transport::ServerTransport::new(net.transport());
        let store = Arc::new(Store::open_in_memory().unwrap());
        let on = Computers::with_transport(
            crate::computers::Registry::new(store),
            crate::computers::connector(Some(st.dialer.clone())),
            None,
            Some(st.dialer.clone()),
        );
        let reg = ComputersRegistrar::new(on.clone());
        assert_eq!(reg.registers_by(&n, SRC), Some("peer"));
        let id = reg.register("viapeer", "tok", &n, SRC).unwrap();
        let c = on.registry().get(&id).unwrap().unwrap();
        assert!(c.peer.is_some() && c.url.is_empty(), "{c:?}");
    }
}
