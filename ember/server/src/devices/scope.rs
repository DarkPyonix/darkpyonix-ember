//! Per-device scope on the transport door (SPEC `FR-N8`, `INTENT.md` D37 "Per-device scope",
//! #366).
//!
//! A device admitted by the gate carries a [`Scope`]. [`enforce`] wraps the router the transport
//! serves: it reads the caller's [`PeerId`] (connect info of the transport listener), looks up
//! its scope and refuses a call the scope does not allow with 403 `{"code": "scope_denied"}`
//! before any handler runs. The local listener is not wrapped; it has the server token.
//!
//! Both narrow scopes are allow-lists: a route added later is closed to them until it is placed
//! in a family here.

use std::str::FromStr;
use std::sync::Arc;

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use ember_transport::PeerId;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::Devices;

/// What an admitted device may call over the transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// Every route the transport serves (today's behaviour, the default).
    #[default]
    Full,
    /// `GET` and `HEAD` on the read families; never tokens, terminals, the IDE or the browser.
    ReadOnly,
    /// Channels and messages to existing sessions only.
    ChatOnly,
}

impl Scope {
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Full => "full",
            Scope::ReadOnly => "read_only",
            Scope::ChatOnly => "chat_only",
        }
    }

    /// Whether this scope allows `method` on `path` (the request path, no query).
    ///
    /// An owner route (`FR-S12`, #784) needs `full`: on the transport the device key is the
    /// owner credential, and a narrow scope is the non-owner one. The check is here and not only
    /// in the allow-lists, so a family opened to a narrow scope later cannot open an owner route.
    pub fn allows(self, method: &Method, path: &str) -> bool {
        if self != Scope::Full && crate::owner::owner_route(method, path) {
            return false;
        }
        match self {
            Scope::Full => true,
            Scope::ReadOnly => read_only_allows(method, path),
            Scope::ChatOnly => chat_only_allows(method, path),
        }
    }
}

impl FromStr for Scope {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, ()> {
        match s {
            "full" => Ok(Scope::Full),
            "read_only" => Ok(Scope::ReadOnly),
            "chat_only" => Ok(Scope::ChatOnly),
            _ => Err(()),
        }
    }
}

/// The path split into segments after `/api/`; `None` for anything outside `/api/`, or a path
/// with an empty, `.` or `..` segment (never matched by a scope, so refused).
fn segments(path: &str) -> Option<Vec<&str>> {
    let rest = path.strip_prefix("/api/")?;
    let segs: Vec<&str> = rest.split('/').collect();
    if segs.iter().any(|s| s.is_empty() || *s == "." || *s == "..") {
        return None;
    }
    Some(segs)
}

/// Routes no narrow scope reaches, whatever the method: tokens, device management, shells,
/// the IDE (a code-running editor), the browser's view, input and CDP, ChatGPT sign-in.
fn never_narrow(segs: &[&str]) -> bool {
    match segs {
        ["server-token", ..] | ["devices", ..] | ["chatgpt", ..] => true,
        ["computers", _, "terms", ..] | ["a2a", "terminal", ..] => true,
        ["computers", _, "ide", ..] | ["computers", _, "fs", ..] | ["sessions", _, "ide"] => true,
        ["browsers", _, "view" | "input" | "cdp" | "agent-config", ..] => true,
        ["agents", "antigravity", "hook"] => true,
        _ => false,
    }
}

/// Route families `read_only` may `GET`.
const READ_FAMILIES: &[&str] = &[
    "health", "sessions", "projects", "accounts", "agents", "providers", "usage", "search", "channels",
    "schedules", "a2a", "computers", "transfers", "hub", "mcp", "browsers", "teams", "push", "guest-door",
];

fn read_only_allows(method: &Method, path: &str) -> bool {
    if method != Method::GET && method != Method::HEAD {
        return false;
    }
    let Some(segs) = segments(path) else { return false };
    !never_narrow(&segs) && READ_FAMILIES.contains(&segs[0])
}

fn chat_only_allows(method: &Method, path: &str) -> bool {
    let Some(segs) = segments(path) else { return false };
    if never_narrow(&segs) {
        return false;
    }
    let get = method == Method::GET || method == Method::HEAD;
    let post = method == Method::POST;
    match segs.as_slice() {
        ["health"] | ["push"] => get,
        ["sessions"] => get,
        ["sessions", _] => get,
        ["sessions", _, "events" | "usage"] => get,
        ["sessions", _, "messages" | "interrupt"] => post,
        ["sessions", _, "questions", _, "answer"] => post,
        ["channels"] => get,
        ["channels", _] => get,
        ["channels", _, "messages"] => get || post,
        ["channels", _, "feed"] => get,
        ["channels", _, "read"] => post,
        ["channels", _, "mentions", "preview"] => post,
        ["channels", _, "questions", _, "answer"] => post,
        _ => false,
    }
}

/// 403 `{"code": "scope_denied"}`.
fn denied(scope: Scope) -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(json!({ "code": "scope_denied", "error": format!("this device's scope ({}) does not allow this route", scope.as_str()) })),
    )
        .into_response()
}

async fn check(State(devices): State<Arc<Devices>>, req: Request, next: Next) -> Response {
    // No connect info means the router was served without the transport listener: refuse
    // rather than guess (the transport serve always provides it).
    let Some(ConnectInfo(peer)) = req.extensions().get::<ConnectInfo<PeerId>>().cloned() else {
        return denied(Scope::ChatOnly);
    };
    // A peer with no row was admitted a moment before a revoke; the gate closes it. Treat it as
    // the narrowest scope meanwhile.
    let scope = devices.scope_of(&peer).unwrap_or(Scope::ChatOnly);
    if !scope.allows(req.method(), req.uri().path()) {
        tracing::info!(peer = %peer.fmt_short(), scope = scope.as_str(), method = %req.method(), path = req.uri().path(), "scope denied");
        return denied(scope);
    }
    next.run(req).await
}

/// Wraps `app` (the router the transport serves) with the scope check.
pub fn enforce(app: Router, devices: Arc<Devices>) -> Router {
    app.layer(axum::middleware::from_fn_with_state(devices, check))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_round_trips() {
        for s in [Scope::Full, Scope::ReadOnly, Scope::ChatOnly] {
            assert_eq!(s.as_str().parse::<Scope>(), Ok(s));
        }
        assert!("admin".parse::<Scope>().is_err());
    }

    #[test]
    fn read_only_is_get_only_and_never_tokens_or_shells() {
        let r = Scope::ReadOnly;
        assert!(r.allows(&Method::GET, "/api/sessions"));
        assert!(r.allows(&Method::GET, "/api/projects/p/git/status"));
        assert!(!r.allows(&Method::POST, "/api/sessions"));
        assert!(!r.allows(&Method::GET, "/api/computers/local/terms/t1/attach"));
        assert!(!r.allows(&Method::GET, "/api/sessions/s1/ide"));
        assert!(!r.allows(&Method::GET, "/api/server-token"));
        assert!(!r.allows(&Method::GET, "/api/devices"));
        assert!(!r.allows(&Method::GET, "/api/sessions/../devices"));
        assert!(!r.allows(&Method::GET, "/auth/callback"));
    }

    /// FR-S12: `read_only` may read the `computers` family, but an enrol code's status is an
    /// owner route.
    #[test]
    fn fr_s12_no_narrow_scope_reaches_an_owner_route() {
        for (m, template, class) in crate::owner::ROUTES {
            if *class != crate::owner::RouteClass::Owner {
                continue;
            }
            let method = Method::from_bytes(m.as_bytes()).unwrap();
            let path = template.replace('{', "").replace('}', "");
            assert!(crate::owner::owner_route(&method, &path), "{m} {path}");
            assert!(Scope::Full.allows(&method, &path), "{m} {path}");
            for s in [Scope::ReadOnly, Scope::ChatOnly] {
                assert!(!s.allows(&method, &path), "{} reaches {m} {path}", s.as_str());
            }
        }
    }

    #[test]
    fn chat_only_is_channels_and_existing_sessions() {
        let c = Scope::ChatOnly;
        assert!(c.allows(&Method::POST, "/api/sessions/s1/messages"));
        assert!(c.allows(&Method::POST, "/api/channels/c1/messages"));
        assert!(!c.allows(&Method::POST, "/api/sessions"));
        assert!(!c.allows(&Method::POST, "/api/sessions/s1/fork"));
        assert!(!c.allows(&Method::POST, "/api/sessions/s1/approvals/a1"));
        assert!(!c.allows(&Method::POST, "/api/channels"));
        assert!(!c.allows(&Method::POST, "/api/computers/local/fs/read"));
        assert!(!c.allows(&Method::POST, "/api/computers/local/terms"));
        assert!(!c.allows(&Method::GET, "/api/projects"));
    }
}
