//! FR-S10 listen guard and FR-S9 exempt route walk (INTENT D37, #338).
//!
//! The owner listener speaks plain HTTP, so a non-loopback `EMBER_LISTEN` is refused unless
//! `EMBER_ALLOW_NON_LOOPBACK_LISTEN=1` is set. The decision lives in `auth::check_listen`, so it
//! is tested here without starting a server.

use std::net::SocketAddr;

use axum::http::Method;
use ember_server::auth::{self, ListenCheck};

fn addr(s: &str) -> SocketAddr {
    auth::parse_listen(Some(s)).unwrap()
}

#[test]
fn fr_s10_non_loopback_listen_default_is_loopback() {
    assert_eq!(auth::parse_listen(None).unwrap(), "127.0.0.1:8740".parse::<SocketAddr>().unwrap());
    assert_eq!(auth::parse_listen(Some("")).unwrap(), "127.0.0.1:8740".parse::<SocketAddr>().unwrap());
    assert_eq!(auth::check_listen(auth::parse_listen(None).unwrap(), None), Ok(ListenCheck::Loopback));
}

#[test]
fn fr_s10_non_loopback_listen_loopback_is_accepted_without_opt_in() {
    for s in ["127.0.0.1:8740", "127.0.0.2:9000", "[::1]:8740", "localhost:8740"] {
        assert_eq!(auth::check_listen(addr(s), None), Ok(ListenCheck::Loopback), "{s}");
        // The opt-in changes nothing for loopback (no warning).
        assert_eq!(auth::check_listen(addr(s), Some("1")), Ok(ListenCheck::Loopback), "{s}");
    }
    assert_eq!(addr("localhost:8740"), "127.0.0.1:8740".parse::<SocketAddr>().unwrap());
}

#[test]
fn fr_s10_non_loopback_listen_is_refused_without_opt_in() {
    for s in ["0.0.0.0:8740", "192.168.1.20:8740", "10.0.0.5:8740", "[::]:8740", "[fe80::1]:8740",
        "[::ffff:127.0.0.1]:8740"] {
        for allow in [None, Some(""), Some("0"), Some("true"), Some("yes")] {
            let err = auth::check_listen(addr(s), allow).unwrap_err();
            assert!(err.contains(auth::ALLOW_NON_LOOPBACK_LISTEN), "{s} {allow:?}: {err}");
        }
    }
}

#[test]
fn fr_s10_non_loopback_listen_is_accepted_with_opt_in() {
    for s in ["0.0.0.0:8740", "192.168.1.20:8740", "[::]:8740"] {
        assert_eq!(auth::check_listen(addr(s), Some("1")), Ok(ListenCheck::NonLoopbackAllowed), "{s}");
        assert_eq!(auth::check_listen(addr(s), Some(" 1 ")), Ok(ListenCheck::NonLoopbackAllowed), "{s}");
    }
}

#[test]
fn fr_s10_non_loopback_listen_bad_value_is_an_error() {
    for s in ["nonsense", "localhost", "localhost:x", "example.com:8740", "127.0.0.1"] {
        assert!(auth::parse_listen(Some(s)).is_err(), "{s}");
    }
}

/// Every token-exempt family, with its method, and the near misses that must keep the token.
#[test]
fn fr_s9_every_exempt_route_family_is_listed_and_nothing_else() {
    let nonce = auth::agent_cdp_nonce();
    let cdp = format!("/api/browsers/p1/cdp/{nonce}");
    let cdp_ver = format!("{cdp}/json/version");
    let exempt: Vec<(Method, String)> = vec![
        (Method::GET, "/api/health".into()),
        (Method::GET, "/auth/callback".into()),
        (Method::GET, cdp.clone()),
        (Method::GET, cdp_ver.clone()),
        (Method::POST, "/api/channels/c1/post".into()),
        (Method::POST, "/api/agents/antigravity/hook".into()),
        (Method::GET, "/api/a2a/targets".into()),
        (Method::POST, "/api/a2a/send".into()),
        (Method::GET, "/api/a2a/schedules".into()),
        (Method::GET, "/api/a2a/team".into()),
    ];
    for (m, p) in &exempt {
        assert!(auth::token_exempt(m, p), "{m} {p} should be exempt");
    }
    let kept: Vec<(Method, String)> = vec![
        // Wrong method on an exempt path.
        (Method::POST, "/api/health".into()),
        (Method::POST, "/auth/callback".into()),
        (Method::POST, cdp.clone()),
        (Method::GET, "/api/channels/c1/post".into()),
        // The a2a prefix exemption: only paths under `/api/a2a/`, never settings, never a path
        // that merely shares the prefix.
        (Method::GET, "/api/a2a/settings".into()),
        (Method::PUT, "/api/a2a/settings".into()),
        (Method::GET, "/api/a2a".into()),
        (Method::GET, "/api/a2ax".into()),
        (Method::GET, "/api/a2a-admin/targets".into()),
        (Method::GET, "/api/a2abc/send".into()),
        (Method::GET, "/apix/a2a/targets".into()),
        (Method::GET, "/x/api/a2a/targets".into()),
        // Near misses of the other families.
        (Method::POST, "/api/agents/antigravity/hook/x".into()),
        (Method::POST, "/api/agents/antigravity".into()),
        (Method::GET, "/api/healthz".into()),
        (Method::GET, "/auth/callbackx".into()),
        (Method::GET, "/api/browsers/p1/cdp/wrong-nonce".into()),
        (Method::GET, format!("{cdp}/x")),
        (Method::GET, "/api/browsers//cdp/x".into()),
        // Owner routes.
        (Method::GET, "/api/sessions".into()),
        (Method::GET, "/api/devices".into()),
        (Method::GET, "/api/accounts".into()),
        (Method::GET, "/".into()),
    ];
    for (m, p) in &kept {
        assert!(!auth::token_exempt(m, p), "{m} {p} must keep the server token");
    }
}
