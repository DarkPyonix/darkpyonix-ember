//! Origin check for WebSocket upgrades (FR-P3, cross-site WebSocket hijacking).
//!
//! Browsers apply no CORS to WebSockets, so a page on any site can open
//! `ws://127.0.0.1:8740/...` and the server would accept it. Native clients send no `Origin`
//! header. A browser always does, so the check is: no `Origin` is allowed; an `Origin` is
//! allowed when its authority equals the request `Host`, or its host is loopback. Anything else
//! (including `null`) is refused with 403 before the upgrade.
//!
//! This is not authentication. Who may attach beyond the loopback trust is open (SPEC FR-P3).

use axum::http::{header, HeaderMap, StatusCode};

/// The host part of an authority (`host`, `host:port`, `[v6]:port`), lower-cased.
pub fn host_of(authority: &str) -> String {
    let a = authority.rsplit('@').next().unwrap_or(authority);
    let h = if let Some(rest) = a.strip_prefix('[') {
        rest.split(']').next().unwrap_or("")
    } else {
        a.split(':').next().unwrap_or("")
    };
    h.to_ascii_lowercase()
}

fn is_loopback(host: &str) -> bool {
    host == "localhost"
        || host.ends_with(".localhost")
        || host == "::1"
        || host.parse::<std::net::Ipv4Addr>().map(|a| a.is_loopback()).unwrap_or(false)
}

/// True when the request may open a WebSocket: no `Origin`, same-host `Origin`, or loopback `Origin`.
pub fn origin_allowed(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(header::ORIGIN) else { return true };
    let Ok(origin) = origin.to_str() else { return false };
    let Some((_scheme, rest)) = origin.split_once("://") else { return false };
    let authority = rest.split('/').next().unwrap_or("");
    if authority.is_empty() {
        return false;
    }
    if let Some(host) = headers.get(header::HOST).and_then(|h| h.to_str().ok()) {
        if host.eq_ignore_ascii_case(authority) {
            return true;
        }
    }
    is_loopback(&host_of(authority))
}

/// `Err(403)` when [`origin_allowed`] is false, for use before `WebSocketUpgrade::on_upgrade`.
pub fn check_origin(headers: &HeaderMap) -> Result<(), StatusCode> {
    if origin_allowed(headers) {
        Ok(())
    } else {
        Err(StatusCode::FORBIDDEN)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(host: &str, origin: Option<&str>) -> HeaderMap {
        let mut m = HeaderMap::new();
        m.insert(header::HOST, host.parse().unwrap());
        if let Some(o) = origin {
            m.insert(header::ORIGIN, o.parse().unwrap());
        }
        m
    }

    #[test]
    fn fr_p3_origin_rules() {
        assert!(origin_allowed(&h("mini.local:8740", None)));
        assert!(origin_allowed(&h("mini.local:8740", Some("http://mini.local:8740"))));
        assert!(origin_allowed(&h("mini.local:8740", Some("http://localhost:3000"))));
        assert!(origin_allowed(&h("127.0.0.1:8740", Some("http://[::1]:5173"))));
        assert!(!origin_allowed(&h("127.0.0.1:8740", Some("https://evil.example"))));
        assert!(!origin_allowed(&h("127.0.0.1:8740", Some("http://127.0.0.1.evil.example"))));
        assert!(!origin_allowed(&h("127.0.0.1:8740", Some("null"))));
    }
}
