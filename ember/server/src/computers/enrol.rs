//! One-time enrol codes for adding a computer (SPEC FR-C1, FR-C2 first unit, `INTENT.md` D39,
//! all [provisional]).
//!
//! What is here: minting a code from the local admin router, the redeem core, and a status route
//! by enrol id. What is not here: any route that redeems a code (where an unauthenticated redeem
//! route lives is an open security decision, see `docs/design/COMPUTERS.md`, "Adding a computer
//! with an enrol code"), `ember-node enrol`, and the registration step that calls
//! [`Enrol::complete`].
//!
//! Secrets: a code is `dpe_` plus 244 random bits, made like the channel invite secrets
//! ([`crate::channels::invites::new_secret`], two v4 UUIDs from the OS CSPRNG). Only the SHA-256
//! of the code is stored, as a BLOB. Redeem looks the digest up in a UNIQUE index inside SQLite,
//! so no secret is compared byte by byte in this code and there is no constant-time compare to
//! get wrong; the attacker does not control the digest, so index timing says nothing about the
//! code. The code appears once, in the answer to minting, and in no log: [`EnrolCode`] and
//! [`Minted`] print a redacted `Debug`.
//!
//! Refusal: an unknown, used, expired, cancelled or malformed code all return the same
//! [`Refused::Generic`] from one `UPDATE ... WHERE used_ms IS NULL AND expires_ms > now`, so the
//! answer is not an oracle. After 10 failures from one source key within a minute the source gets
//! [`Refused::RateLimited`] before the code is looked at, so that answer tells nothing about the
//! code either.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use super::{LOCAL, LOCAL_NAME};
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

/// Code prefix; a leaked value is recognizable.
pub const CODE_PREFIX: &str = "dpe_";
/// Default lifetime of a code.
pub const DEFAULT_TTL: Duration = Duration::from_secs(10 * 60);
/// Longest lifetime of a code; a larger request is clamped to it.
pub const MAX_TTL: Duration = Duration::from_secs(10 * 60);
/// Failed redeems one source key may make within [`FAIL_WINDOW_MS`] before it is rate-limited.
pub const MAX_FAILURES: usize = 10;
/// The failure window in milliseconds.
pub const FAIL_WINDOW_MS: i64 = 60_000;
/// Most source keys tracked by the limiter; the least recently failing is evicted for a new one.
pub const MAX_TRACKED_SOURCES: usize = 4096;
/// Longest computer name in characters.
pub const MAX_NAME_CHARS: usize = 64;
/// Expired rows are swept on mint once they have been expired this long, so a project screen can
/// still read "expired" for a while.
pub const SWEEP_GRACE_MS: i64 = 60 * 60 * 1000;
/// The one answer for every code that cannot be redeemed.
pub const REFUSED_MESSAGE: &str = "this enrol code cannot be used";
/// The answer when a source key has failed too often.
pub const RATE_LIMITED_MESSAGE: &str = "too many attempts, try again later";

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

/// A successful redeem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redeemed {
    pub name: String,
    pub enrol_id: String,
}

/// Why a redeem was refused. [`Refused::Generic`] covers unknown, used, expired, cancelled and
/// malformed codes alike.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    Generic,
    RateLimited,
}

impl Refused {
    pub fn message(self) -> &'static str {
        match self {
            Refused::Generic => REFUSED_MESSAGE,
            Refused::RateLimited => RATE_LIMITED_MESSAGE,
        }
    }
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for Refused {}

/// The state of an enrol code by id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EnrolStatus {
    /// Not joined yet and not expired (the code may or may not be redeemed already).
    Waiting,
    /// A computer was registered with it ([`Enrol::complete`]).
    Joined,
    /// Past its expiry without a computer.
    Expired,
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
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl From<rusqlite::Error> for EnrolError {
    fn from(e: rusqlite::Error) -> Self {
        EnrolError::Other(e.into())
    }
}

fn digest(code: &str) -> Vec<u8> {
    Sha256::digest(code.as_bytes()).to_vec()
}

/// A computer name as the registry takes it, plus the shape rules the guest names use.
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

/// The server URL that goes into the command: http(s), no whitespace, quotes or shell characters.
fn check_base_url(url: &str) -> Result<String, EnrolError> {
    let url = url.trim().trim_end_matches('/');
    let ok_scheme = url.starts_with("http://") || url.starts_with("https://");
    let ok_chars = url
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '/' | '.' | '-' | '_' | '[' | ']' | '%' | '~'));
    if !ok_scheme || !ok_chars || url.len() > 300 {
        return Err(EnrolError::BadRequest("the server URL for the enrol command is not a plain http(s) URL".into()));
    }
    Ok(url.to_string())
}

/// The enrol-code service: store plus the in-memory failure limiter.
pub struct Enrol {
    store: Arc<Store>,
    /// Failure times (Unix ms) per source key.
    failures: Mutex<HashMap<String, VecDeque<i64>>>,
}

impl Enrol {
    pub fn new(store: Arc<Store>) -> Arc<Enrol> {
        Arc::new(Enrol { store, failures: Mutex::new(HashMap::new()) })
    }

    /// Mint a code for a computer to be called `name`. `ttl` defaults to 10 minutes and is
    /// clamped to 10 minutes; zero is refused. `base_url` is the server's externally reachable
    /// URL, which the server does not know by itself: the caller passes it. Expired rows older
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
        let command = format!("ember-node enrol {code} --server {base}");
        Ok(Minted { id, code: EnrolCode(Zeroizing::new(code)), expires_ms, command })
    }

    /// Redeem `code` at `now_ms` for the caller identified by `source_key` (the remote address;
    /// the transport that carries the call decides what it is). One SQL statement marks the code
    /// used only if it is unused and unexpired, so of two concurrent redeems one wins.
    pub fn redeem(&self, code: &str, now_ms: i64, source_key: &str) -> Result<Redeemed, Refused> {
        if self.rate_limited(source_key, now_ms) {
            return Err(Refused::RateLimited);
        }
        let won = self.store.conn().query_row(
            "UPDATE enrol_codes SET used_ms = ?1
             WHERE code_hash = ?2 AND used_ms IS NULL AND expires_ms > ?1
             RETURNING id, name",
            params![now_ms, digest(code)],
            |r| Ok(Redeemed { enrol_id: r.get(0)?, name: r.get(1)? }),
        );
        match won {
            Ok(r) => Ok(r),
            // No row, or a database error: the caller sees the same refusal either way.
            Err(_) => {
                self.record_failure(source_key, now_ms);
                Err(Refused::Generic)
            }
        }
    }

    fn rate_limited(&self, source: &str, now_ms: i64) -> bool {
        let mut all = self.failures.lock().unwrap_or_else(|e| e.into_inner());
        match all.get_mut(source) {
            Some(q) => {
                while q.front().is_some_and(|t| now_ms - *t >= FAIL_WINDOW_MS) {
                    q.pop_front();
                }
                q.len() >= MAX_FAILURES
            }
            None => false,
        }
    }

    fn record_failure(&self, source: &str, now_ms: i64) {
        let mut all = self.failures.lock().unwrap_or_else(|e| e.into_inner());
        if !all.contains_key(source) && all.len() >= MAX_TRACKED_SOURCES {
            all.retain(|_, q| q.back().is_some_and(|t| now_ms - *t < FAIL_WINDOW_MS));
            if all.len() >= MAX_TRACKED_SOURCES {
                if let Some(oldest) = all.iter().min_by_key(|(_, q)| q.back().copied()).map(|(k, _)| k.clone()) {
                    all.remove(&oldest);
                }
            }
        }
        // Keys are bounded in length so an attacker cannot grow memory per entry.
        let key: String = source.chars().take(64).collect();
        all.entry(key).or_default().push_back(now_ms);
    }

    /// The state of the code `id` at `now_ms`. A missing id is [`EnrolError::NotFound`].
    pub fn status(&self, id: &str, now_ms: i64) -> Result<EnrolStatus, EnrolError> {
        let row: Option<(i64, Option<String>)> = self
            .store
            .conn()
            .query_row("SELECT expires_ms, computer_id FROM enrol_codes WHERE id = ?1", params![id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()?;
        match row {
            None => Err(EnrolError::NotFound(id.into())),
            Some((_, Some(_))) => Ok(EnrolStatus::Joined),
            Some((exp, None)) if now_ms >= exp => Ok(EnrolStatus::Expired),
            Some(_) => Ok(EnrolStatus::Waiting),
        }
    }

    /// Record that the computer `computer_id` registered with the redeemed code `enrol_id`. The
    /// later registration step calls this. It works once, and only on a redeemed code.
    pub fn complete(&self, enrol_id: &str, computer_id: &str) -> Result<(), EnrolError> {
        let n = self.store.conn().execute(
            "UPDATE enrol_codes SET computer_id = ?2
             WHERE id = ?1 AND used_ms IS NOT NULL AND computer_id IS NULL",
            params![enrol_id, computer_id],
        )?;
        if n == 1 {
            Ok(())
        } else {
            Err(EnrolError::NotFound(enrol_id.into()))
        }
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
// Admin routes (local, token-protected router only; never the guest door)
// ---------------------------------------------------------------------------------------------

#[derive(Clone)]
struct AppState {
    enrol: Arc<Enrol>,
    base_url: String,
}

/// `POST /api/computers/enrol-codes`, `GET` and `DELETE /api/computers/enrol-codes/{id}`.
/// `base_url` is the server's externally reachable URL for the printed command. There is no
/// redeem route. Merge it only into the token-protected local router.
pub fn router(enrol: Arc<Enrol>, base_url: String) -> Router {
    Router::new()
        .route("/api/computers/enrol-codes", post(mint))
        .route("/api/computers/enrol-codes/{id}", get(status).delete(cancel))
        .with_state(AppState { enrol, base_url })
}

struct ApiError(EnrolError);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let code = match &self.0 {
            EnrolError::BadRequest(_) => StatusCode::BAD_REQUEST,
            EnrolError::NotFound(_) => StatusCode::NOT_FOUND,
            EnrolError::Used(_) => StatusCode::CONFLICT,
            EnrolError::Other(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        // An internal error does not echo database text.
        let msg = match &self.0 {
            EnrolError::Other(_) => "internal error".to_string(),
            e => e.to_string(),
        };
        (code, Json(json!({ "error": msg }))).into_response()
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

async fn mint(State(s): State<AppState>, Json(b): Json<MintBody>) -> Result<Response, ApiError> {
    let m = s.enrol.mint(
        &b.name,
        crate::store::now_ms(),
        b.ttl_secs.map(Duration::from_secs),
        &s.base_url,
    )?;
    tracing::info!(enrol = %m.id, "enrol code minted");
    let body = json!({
        "id": m.id,
        "code": m.code.expose(),
        "expires_ms": m.expires_ms,
        "command": m.command,
        "warning": format!("The code is shown once. Check that {} is reachable from the other machine.", s.base_url.trim_end_matches('/')),
    });
    let mut res = (StatusCode::CREATED, Json(body)).into_response();
    res.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(res)
}

async fn status(State(s): State<AppState>, Path(id): Path<String>) -> Result<impl IntoResponse, ApiError> {
    let st = s.enrol.status(&id, crate::store::now_ms())?;
    Ok(Json(json!({ "status": st })))
}

async fn cancel(State(s): State<AppState>, Path(id): Path<String>) -> Result<impl IntoResponse, ApiError> {
    s.enrol.cancel(&id)?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;

    const BASE: &str = "http://192.168.0.5:8740";
    const T0: i64 = 1_000_000_000;

    fn svc() -> (Arc<Store>, Arc<Enrol>) {
        let store = Arc::new(Store::open_in_memory().unwrap());
        (store.clone(), Enrol::new(store))
    }

    fn mint(e: &Enrol, now: i64) -> Minted {
        e.mint("laptop", now, None, BASE).unwrap()
    }

    #[test]
    fn fr_c1_migration_creates_the_table() {
        let (store, _) = svc();
        let n: i64 = store.conn().query_row("SELECT count(*) FROM enrol_codes", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
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
        // Dump every cell of the table as text: the code is in none of them.
        let conn = store.conn();
        let mut stmt = conn
            .prepare("SELECT id, hex(code_hash), name, created_ms, expires_ms, used_ms, computer_id FROM enrol_codes")
            .unwrap();
        let dump: String = stmt
            .query_map([], |r| {
                let mut s = String::new();
                for i in 0..7 {
                    s.push_str(&format!("{:?}|", r.get::<_, rusqlite::types::Value>(i)?));
                }
                Ok(s)
            })
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert!(!dump.contains(&code));
        assert!(!dump.contains(code.trim_start_matches(CODE_PREFIX)));
    }

    #[test]
    fn fr_c1_the_code_is_absent_from_the_database_files() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(&dir.path().join("ember.db")).unwrap());
        let e = Enrol::new(store.clone());
        let m = mint(&e, T0);
        e.redeem(m.code.expose(), T0 + 1, "a").unwrap();
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

    #[test]
    fn fr_c1_redeem_ok_once_then_refused() {
        let (_, e) = svc();
        let m = mint(&e, T0);
        let r = e.redeem(m.code.expose(), T0 + 1, "a").unwrap();
        assert_eq!(r, Redeemed { name: "laptop".into(), enrol_id: m.id.clone() });
        assert_eq!(e.redeem(m.code.expose(), T0 + 2, "a"), Err(Refused::Generic));
    }

    #[test]
    fn fr_c1_expired_unknown_and_used_get_the_same_refusal() {
        let (_, e) = svc();
        let used = mint(&e, T0);
        e.redeem(used.code.expose(), T0 + 1, "a").unwrap();
        let expired = mint(&e, T0);
        let fresh = mint(&e, T0);
        let results = [
            e.redeem(used.code.expose(), T0 + 2, "b"),
            e.redeem(expired.code.expose(), T0 + 600_000, "c"),
            e.redeem("dpe_00000000000000000000000000000000000000000000000000000000000000ff", T0 + 2, "d"),
            e.redeem("", T0 + 2, "e"),
            e.redeem("not a code at all", T0 + 2, "f"),
        ];
        for r in &results {
            assert_eq!(*r, Err(Refused::Generic));
            assert_eq!(r.as_ref().unwrap_err().message(), REFUSED_MESSAGE);
        }
        // The expiry instant itself is expired; one millisecond earlier is fine.
        assert!(e.redeem(fresh.code.expose(), T0 + 599_999, "g").is_ok());
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
                    e.redeem(&code, T0 + 1, &format!("src{i}")).is_ok()
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
            assert_eq!(e.redeem("dpe_wrong", T0 + i as i64, "1.2.3.4"), Err(Refused::Generic));
        }
        // The 11th attempt is limited, and so is a CORRECT code from the same source: the answer
        // does not depend on the code.
        assert_eq!(e.redeem("dpe_wrong", T0 + 20, "1.2.3.4"), Err(Refused::RateLimited));
        assert_eq!(e.redeem(m.code.expose(), T0 + 21, "1.2.3.4"), Err(Refused::RateLimited));
        // Another source is unaffected, and the limited attempt did not burn the code.
        assert!(e.redeem(m.code.expose(), T0 + 22, "5.6.7.8").is_ok());
    }

    #[test]
    fn fr_c1_rate_limit_window_slides_and_success_does_not_count() {
        let (_, e) = svc();
        for i in 0..MAX_FAILURES {
            let _ = e.redeem("dpe_wrong", T0 + i as i64, "s");
        }
        assert_eq!(e.redeem("dpe_wrong", T0 + 100, "s"), Err(Refused::RateLimited));
        let m = mint(&e, T0 + FAIL_WINDOW_MS + 100);
        assert!(e.redeem(m.code.expose(), T0 + FAIL_WINDOW_MS + 101, "s").is_ok());
        // Ten successes from one source never limit it.
        for _ in 0..12 {
            let m = mint(&e, T0 + 1_000_000);
            assert!(e.redeem(m.code.expose(), T0 + 1_000_001, "ok-source").is_ok());
        }
    }

    #[test]
    fn fr_c1_status_transitions() {
        let (_, e) = svc();
        let m = mint(&e, T0);
        assert_eq!(e.status(&m.id, T0 + 1).unwrap(), EnrolStatus::Waiting);
        let r = e.redeem(m.code.expose(), T0 + 2, "a").unwrap();
        // Redeemed but not yet registered: still waiting.
        assert_eq!(e.status(&m.id, T0 + 3).unwrap(), EnrolStatus::Waiting);
        e.complete(&r.enrol_id, "computer-1").unwrap();
        assert_eq!(e.status(&m.id, T0 + 4).unwrap(), EnrolStatus::Joined);
        assert!(e.complete(&r.enrol_id, "computer-2").is_err());
        // Joined stays joined after the expiry time.
        assert_eq!(e.status(&m.id, T0 + 700_000).unwrap(), EnrolStatus::Joined);

        let lapsed = mint(&e, T0);
        assert_eq!(e.status(&lapsed.id, T0 + 600_000).unwrap(), EnrolStatus::Expired);
        assert!(matches!(e.status("nope", T0), Err(EnrolError::NotFound(_))));
    }

    #[test]
    fn fr_c1_complete_needs_a_redeemed_code() {
        let (_, e) = svc();
        let m = mint(&e, T0);
        assert!(e.complete(&m.id, "c").is_err());
        assert!(e.complete("nope", "c").is_err());
    }

    #[test]
    fn fr_c1_cancel_unredeemed_only() {
        let (_, e) = svc();
        let m = mint(&e, T0);
        e.cancel(&m.id).unwrap();
        assert_eq!(e.redeem(m.code.expose(), T0 + 1, "a"), Err(Refused::Generic));
        assert!(matches!(e.cancel(&m.id), Err(EnrolError::NotFound(_))));
        let used = mint(&e, T0);
        e.redeem(used.code.expose(), T0 + 1, "a").unwrap();
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
            format!("{:?}", e.redeem(&code, T0, "a")),
            format!("{:?}", e.redeem(&code, T0, "a")),
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
            let _ = e.redeem(&code, T0 + 1, "a");
            let _ = e.redeem(&code, T0 + 2, "a");
            code
        });
        let out = String::from_utf8(cap.0.lock().unwrap().clone()).unwrap();
        assert!(!out.contains(code.trim_start_matches(CODE_PREFIX)), "{out}");
        assert!(out.contains("redacted"));
    }
}
