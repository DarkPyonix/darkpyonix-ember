//! The shell hands its owner key to the editor windows it starts itself (SPEC FR-S12, leader
//! 2026-11-23, #1042), without the 허용/거부 sheet.
//!
//! The shell makes a one-time token per editor window, passes it in [`HANDOFF_ENV`] (never in
//! argv) and records it with the child's PID. The editor window takes the variable out of its
//! environment at start ([`take_from_env`]) and claims at the server's door with the name
//! [`claim_name`]. The shell, a holder, approves that pending claim only when the name carries a
//! recorded token and the door's PID for the claim (read by the server from the connection) is
//! the recorded one, within [`HANDOFF_TTL`]; the token is spent at the first claim that names it.
//! Anything else goes to the sheet.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// The environment variable that carries the token to the editor window.
pub const HANDOFF_ENV: &str = "EMBER_OWNER_HANDOFF";
/// How long a token waits for its claim.
pub const HANDOFF_TTL: Duration = Duration::from_secs(60);
/// The claim name of an editor window, before its token.
pub const EDITOR_CLAIM_PREFIX: &str = "Ember editor ";

/// A fresh token: 16 random bytes in hex.
pub fn new_token() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("the OS random source answers");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The door name an editor window claims with: [`EDITOR_CLAIM_PREFIX`] and its token, or the
/// app's own name without one.
pub fn claim_name(token: Option<&str>) -> String {
    match token.filter(|t| !t.is_empty()) {
        Some(t) => format!("{EDITOR_CLAIM_PREFIX}{t}"),
        None => crate::owner_claim::CLAIMANT_NAME.to_string(),
    }
}

/// The tokens the shell gave out, by token: the child's PID and when it was given.
#[derive(Default)]
pub struct Handoffs {
    issued: HashMap<String, (u64, Instant)>,
}

impl Handoffs {
    /// Records `token` for the child `pid` at `now`.
    pub fn record(&mut self, token: String, pid: u64, now: Instant) {
        self.issued.retain(|_, (_, at)| now.duration_since(*at) < HANDOFF_TTL);
        self.issued.insert(token, (pid, now));
    }

    /// Whether a pending claim by `pid` named `name` is one of this shell's editor windows. The
    /// token is spent here whether it matched or not; a name without a recorded token is no.
    pub fn take(&mut self, pid: u64, name: &str, now: Instant) -> bool {
        let Some(token) = name.strip_prefix(EDITOR_CLAIM_PREFIX) else { return false };
        match self.issued.remove(token) {
            Some((want, at)) => want == pid && now.duration_since(at) < HANDOFF_TTL,
            None => false,
        }
    }
}

/// The shell's tokens.
pub fn registry() -> &'static Mutex<Handoffs> {
    static REG: OnceLock<Mutex<Handoffs>> = OnceLock::new();
    REG.get_or_init(Default::default)
}

/// This process's token, taken out of the environment by [`take_from_env`].
static OWN: OnceLock<Option<String>> = OnceLock::new();

/// Reads [`HANDOFF_ENV`] once and removes it from the environment, so nothing this process
/// starts inherits it. Call first thing in `main`, while the process has one thread.
pub fn take_from_env() {
    let token = std::env::var(HANDOFF_ENV).ok().filter(|t| !t.is_empty());
    // SAFETY: called at the start of `main`, before any other thread exists (the update hooks
    // and the runtime start after it), so no thread reads the environment concurrently.
    unsafe { std::env::remove_var(HANDOFF_ENV) };
    let _ = OWN.set(token);
}

/// The token [`take_from_env`] took, if any.
pub fn own_token() -> Option<&'static str> {
    OWN.get().and_then(|t| t.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr_s12_handoff_tokens_are_fresh_hex() {
        let (a, b) = (new_token(), new_token());
        assert_eq!(a.len(), 32);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
        // The door keeps 64 characters of a name.
        assert!(claim_name(Some(&a)).len() <= 64);
        assert_eq!(claim_name(None), crate::owner_claim::CLAIMANT_NAME);
        assert_eq!(claim_name(Some("")), crate::owner_claim::CLAIMANT_NAME);
    }

    #[test]
    fn fr_s12_handoff_is_spent_once_and_needs_its_pid() {
        let now = Instant::now();
        let mut h = Handoffs::default();
        h.record("t1".into(), 41, now);
        assert!(h.take(41, &claim_name(Some("t1")), now), "the child's own claim");
        assert!(!h.take(41, &claim_name(Some("t1")), now), "spent");

        h.record("t2".into(), 42, now);
        assert!(!h.take(99, &claim_name(Some("t2")), now), "another PID");
        assert!(!h.take(42, &claim_name(Some("t2")), now), "spent by the wrong claim too");

        h.record("t3".into(), 43, now);
        assert!(!h.take(43, "cli", now), "a name without a token");
        assert!(!h.take(43, &claim_name(Some("other")), now), "an unknown token");
        assert!(!h.take(43, "Ember", now));
        assert!(h.take(43, &claim_name(Some("t3")), now), "still there for its own claim");
    }

    #[test]
    fn fr_s12_handoff_expires() {
        let now = Instant::now();
        let mut h = Handoffs::default();
        h.record("t".into(), 7, now);
        assert!(!h.take(7, &claim_name(Some("t")), now + HANDOFF_TTL), "after 60 s");
        // Old tokens are dropped when a new one is recorded.
        h.record("old".into(), 8, now);
        h.record("new".into(), 9, now + HANDOFF_TTL);
        assert!(!h.issued.contains_key("old"));
    }
}
