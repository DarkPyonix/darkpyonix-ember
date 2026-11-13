//! Owner presence (SPEC `FR-S12`; INTENT D37 "Owner presence", #784).
//!
//! `<data dir>/server-token` is mode 0600. That stops other OS users, not the server user's own
//! processes: an agent's shell can read the file. So the routes that grant authority, admit a
//! party or change a secret ([`routes`]) need a second credential an agent never holds.
//!
//! - **The owner key.** `eok_` plus 32 random bytes in hex, minted per claim. The server keeps
//!   only its SHA-256, in memory ([`Owner`]). The holder keeps the key in memory. It never
//!   reaches a file, a log, a query string, an argument or an environment variable.
//! - **How it is sent.** Header `X-Ember-Owner: <key>` beside the bearer token, on owner routes.
//!   Missing or wrong: 403 `{"code": "owner_required"}`. 403 and not 401, so a client does not
//!   take it for a bad server token and re-read the token file.
//! - **The mode.** Warn by default for now: a call without a key is logged and served, until
//!   the app ships its claim. `EMBER_OWNER_PRESENCE=enforce` refuses it ([`Presence`]).
//! - **How it is claimed.** Over the unix socket `<data dir>/owner.sock` ([`door`]). The key
//!   lives while that connection stays open.
//! - **The transport door** has no owner key: there the device key is the credential, and an
//!   owner route needs a device with scope `full` ([`crate::devices::scope`]). Routes that are
//!   local only (devices, the token, hub admin, enrol codes) stay local only, so a `full` device
//!   approves tool calls but does not manage devices.
//!
//! Voiding a key (its connection closed, or the server stopped) ends its future authority only.
//! Sessions it approved things for keep running and stop only on an explicit stop.

pub mod cli;
#[cfg(unix)]
pub mod door;
pub mod routes;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use chacha20poly1305::aead::{KeyInit, OsRng};
use chacha20poly1305::XChaCha20Poly1305;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

pub use routes::{classify, owner_route, RouteClass, ROUTES};

/// The header that carries the owner key.
pub const OWNER_HEADER: &str = "x-ember-owner";
/// Prefix of every owner key.
pub const KEY_PREFIX: &str = "eok_";
/// The claim socket's name in the data folder.
pub const SOCKET_FILE: &str = "owner.sock";
/// `EMBER_OWNER_PRESENCE=enforce`: an owner route called without a live key gets 403
/// `owner_required`. Unset or any other value: warn, which logs the call and serves it. Warn is
/// the default until the app ships its claim (leader, 2026-11-13); enforce becomes the default
/// in a later change. Where there is no claim socket (Windows, #815) the mode is always warn.
pub const PRESENCE_ENV: &str = "EMBER_OWNER_PRESENCE";
/// `EMBER_OWNER_CONFIRM=off`: a claim that passes the peer checks gets a key with no
/// confirmation (best effort). Unset or any other value: every claim is confirmed.
pub const CONFIRM_ENV: &str = "EMBER_OWNER_CONFIRM";

/// What the owner layer does with an owner route called without a live key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presence {
    /// 403 `owner_required` ([`PRESENCE_ENV`]`=enforce`).
    Enforce,
    /// Log and serve (the default for now).
    Warn,
}

impl Presence {
    /// `Some("enforce")` is [`Presence::Enforce`]; everything else, unset included, warns.
    pub fn from_value(v: Option<&str>) -> Presence {
        match v.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
            Some("enforce") => Presence::Enforce,
            _ => Presence::Warn,
        }
    }

    /// The mode a server runs in for `v` (the value of [`PRESENCE_ENV`]). Without a claim
    /// socket no key can exist, so enforcing would only close the owner routes: such a
    /// platform (Windows until the named-pipe door, #815) always warns.
    pub fn for_platform(v: Option<&str>) -> Presence {
        if cfg!(unix) {
            Presence::from_value(v)
        } else {
            Presence::Warn
        }
    }
}

/// How a claim is confirmed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confirm {
    /// By the code shown on the server console, or by a holder of a live key (the default).
    Strict,
    /// Not at all: the peer checks are the only gate ([`CONFIRM_ENV`]`=off`).
    Off,
}

impl Confirm {
    /// `Some("off")` is [`Confirm::Off`]; everything else is strict.
    pub fn from_value(v: Option<&str>) -> Confirm {
        match v.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
            Some("off") => Confirm::Off,
            _ => Confirm::Strict,
        }
    }
}

/// Who made a claim: what the peer credentials and the process table say, plus the name the
/// claimant gave for itself (shown to the owner, never trusted).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claimant {
    pub pid: u32,
    pub exe: String,
    pub name: String,
}

/// What the holders of a live key are told over their claim connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum OwnerEvent {
    /// A claim waits for confirmation. A holder may approve or deny it.
    ClaimPending { claim: u64, pid: u32, exe: String, name: String },
    /// A pending claim ended (confirmed, denied, timed out or gone).
    ClaimClosed { claim: u64 },
    /// Another claimant got a key, so a foreign claim is visible.
    OwnerClaimed { claim: u64, pid: u32, exe: String, name: String },
    /// A key was voided.
    OwnerGone { claim: u64 },
}

struct Held {
    hash: [u8; 32],
    who: Claimant,
}

/// The live owner keys. In memory only; a restart voids every key.
pub struct Owner {
    presence: Presence,
    keys: Mutex<HashMap<u64, Held>>,
    next: AtomicU64,
    events: tokio::sync::broadcast::Sender<OwnerEvent>,
}

fn hash(key: &str) -> [u8; 32] {
    Sha256::digest(key.as_bytes()).into()
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// 32 bytes from the operating system's random source.
pub(crate) fn random_32() -> [u8; 32] {
    let key = XChaCha20Poly1305::generate_key(&mut OsRng);
    let mut out = [0u8; 32];
    out.copy_from_slice(key.as_slice());
    out
}

impl Owner {
    pub fn new(presence: Presence) -> Arc<Owner> {
        let (events, _) = tokio::sync::broadcast::channel(64);
        Arc::new(Owner { presence, keys: Mutex::new(HashMap::new()), next: AtomicU64::new(1), events })
    }

    /// The mode from [`PRESENCE_ENV`], as [`Presence::for_platform`] reads it.
    pub fn from_env() -> Arc<Owner> {
        Owner::new(Presence::for_platform(std::env::var(PRESENCE_ENV).ok().as_deref()))
    }

    pub fn presence(&self) -> Presence {
        self.presence
    }

    /// A fresh claim id (also used for a pending claim before it has a key).
    pub fn next_claim(&self) -> u64 {
        self.next.fetch_add(1, Ordering::Relaxed)
    }

    /// Mints a key for `who` under `claim` and returns it. Only its hash is kept. The claim is
    /// logged without the key, and the other holders are told.
    pub fn grant(&self, claim: u64, who: Claimant) -> String {
        let key = format!("{KEY_PREFIX}{}", hex(&random_32()));
        tracing::info!(claim, pid = who.pid, exe = %who.exe, name = %who.name, "owner key claimed");
        let event = OwnerEvent::OwnerClaimed { claim, pid: who.pid, exe: who.exe.clone(), name: who.name.clone() };
        self.keys.lock().unwrap_or_else(|e| e.into_inner()).insert(claim, Held { hash: hash(&key), who });
        let _ = self.events.send(event);
        key
    }

    /// Voids the key of `claim`. From then on it gets 403; nothing it started is stopped.
    pub fn void(&self, claim: u64) {
        let gone = self.keys.lock().unwrap_or_else(|e| e.into_inner()).remove(&claim);
        if let Some(h) = gone {
            tracing::info!(claim, pid = h.who.pid, "owner key voided");
            let _ = self.events.send(OwnerEvent::OwnerGone { claim });
        }
    }

    /// Voids every key (the claim socket closed for good).
    pub fn void_all(&self) {
        let ids: Vec<u64> = self.keys.lock().unwrap_or_else(|e| e.into_inner()).keys().copied().collect();
        for id in ids {
            self.void(id);
        }
    }

    /// True when `presented` is a live key. Every held hash is compared without an early exit.
    pub fn check(&self, presented: &str) -> bool {
        if !presented.starts_with(KEY_PREFIX) {
            return false;
        }
        let h = hash(presented);
        let keys = self.keys.lock().unwrap_or_else(|e| e.into_inner());
        let mut ok = false;
        for held in keys.values() {
            let diff = held.hash.iter().zip(h.iter()).fold(0u8, |acc, (x, y)| acc | (x ^ y));
            ok |= diff == 0;
        }
        ok
    }

    /// True when `claim` holds a live key.
    pub fn is_live(&self, claim: u64) -> bool {
        self.keys.lock().unwrap_or_else(|e| e.into_inner()).contains_key(&claim)
    }

    /// The live holders.
    pub fn holders(&self) -> Vec<(u64, Claimant)> {
        let keys = self.keys.lock().unwrap_or_else(|e| e.into_inner());
        let mut v: Vec<(u64, Claimant)> = keys.iter().map(|(id, h)| (*id, h.who.clone())).collect();
        v.sort_by_key(|(id, _)| *id);
        v
    }

    /// Claim events, for the claim connections of the holders.
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<OwnerEvent> {
        self.events.subscribe()
    }

    pub(crate) fn announce(&self, event: OwnerEvent) {
        let _ = self.events.send(event);
    }
}

/// 403 `{"code": "owner_required"}`.
pub fn owner_required() -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(json!({
            "code": "owner_required",
            "error": "this route needs owner presence: the Ember app, or `ember-server owner` in a terminal",
        })),
    )
        .into_response()
}

async fn guard(State(owner): State<Arc<Owner>>, mut req: Request<Body>, next: Next) -> Response {
    // The key goes no further than this layer, whatever the route.
    let presented = req.headers_mut().remove(OWNER_HEADER);
    if !owner_route(req.method(), req.uri().path()) {
        return next.run(req).await;
    }
    let ok = presented.as_ref().and_then(|v| v.to_str().ok()).is_some_and(|k| owner.check(k.trim()));
    if ok {
        return next.run(req).await;
    }
    match owner.presence {
        Presence::Warn => {
            tracing::warn!(method = %req.method(), path = req.uri().path(), "owner route served without owner presence (warn mode; EMBER_OWNER_PRESENCE=enforce refuses it)");
            next.run(req).await
        }
        Presence::Enforce => {
            tracing::info!(method = %req.method(), path = req.uri().path(), "owner_required");
            owner_required()
        }
    }
}

/// Wraps `router` (the local listener's app) so that every owner route needs a live owner key.
/// Apply it inside [`crate::auth::protect`], so the server token is checked first
/// ([`crate::auth::protect_with_owner`] does both).
pub fn require(router: Router, owner: Arc<Owner>) -> Router {
    router.layer(middleware::from_fn_with_state(owner, guard))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn who() -> Claimant {
        Claimant { pid: 1, exe: "app".into(), name: "test".into() }
    }

    #[test]
    fn fr_s12_a_key_is_live_until_voided_and_only_its_hash_is_kept() {
        let o = Owner::new(Presence::Enforce);
        let id = o.next_claim();
        let key = o.grant(id, who());
        assert!(key.starts_with(KEY_PREFIX));
        assert_eq!(key.len(), KEY_PREFIX.len() + 64);
        assert!(o.check(&key));
        assert!(!o.check("eok_nope"));
        assert!(!o.check(""));
        // A second key does not disturb the first.
        let id2 = o.next_claim();
        let key2 = o.grant(id2, who());
        assert_ne!(key, key2);
        assert!(o.check(&key) && o.check(&key2));
        o.void(id);
        assert!(!o.check(&key));
        assert!(o.check(&key2));
        o.void_all();
        assert!(!o.check(&key2));
    }

    #[test]
    fn fr_s12_modes_default_to_strict_confirm_and_warn_presence() {
        assert_eq!(Presence::from_value(None), Presence::Warn);
        assert_eq!(Presence::from_value(Some("off")), Presence::Warn);
        assert_eq!(Presence::from_value(Some("warn")), Presence::Warn);
        assert_eq!(Presence::from_value(Some(" ENFORCE ")), Presence::Enforce);
        // A platform without the claim socket never enforces (#815).
        let enforced = Presence::for_platform(Some("enforce"));
        assert_eq!(enforced, if cfg!(unix) { Presence::Enforce } else { Presence::Warn });
        assert_eq!(Presence::for_platform(None), Presence::Warn);
        assert_eq!(Confirm::from_value(None), Confirm::Strict);
        assert_eq!(Confirm::from_value(Some("1")), Confirm::Strict);
        assert_eq!(Confirm::from_value(Some("off")), Confirm::Off);
    }
}
