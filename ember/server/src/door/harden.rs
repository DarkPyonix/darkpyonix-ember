//! The door-agnostic hardening pieces: the request timeout, the Host and Origin guard parts, the
//! response headers and the bare 404. Moved without change from `guest_door.rs`; the tests in
//! `ember/server/tests/fr_t14_guest_door_characterization.rs` pin them.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::Router;
use serde_json::json;

use crate::api::origin::host_of;

/// Answers 408 when the request (handler included) exceeds `limit`.
pub fn with_timeout(app: Router, limit: Duration) -> Router {
    app.layer(middleware::from_fn_with_state(limit, timeout_layer))
}

async fn timeout_layer(State(limit): State<Duration>, req: Request, next: Next) -> Response {
    match tokio::time::timeout(limit, next.run(req)).await {
        Ok(r) => r,
        Err(_) => (StatusCode::REQUEST_TIMEOUT, axum::Json(json!({ "error": "request timed out", "code": "timeout" })))
            .into_response(),
    }
}

#[derive(Clone)]
pub(crate) struct Guard {
    pub(crate) hosts: Arc<HashSet<String>>,
}

pub(crate) fn forbid(why: &str) -> Response {
    (StatusCode::FORBIDDEN, axum::Json(json!({ "error": why, "code": "forbidden" }))).into_response()
}

fn is_loopback_name(host: &str) -> bool {
    host == "localhost"
        || host.ends_with(".localhost")
        || host.parse::<std::net::IpAddr>().map(|a| a.is_loopback()).unwrap_or(false)
}

pub(crate) fn request_host(req: &Request) -> Option<String> {
    let mut all = req.headers().get_all(header::HOST).iter();
    match (all.next(), all.next()) {
        (Some(v), None) => v.to_str().ok().map(str::to_string),
        (None, _) => req.uri().authority().map(|a| a.to_string()),
        _ => None,
    }
}

/// Absent is allowed. Present: its authority equals `Host` (case-insensitive), or its host is
/// an allowed name that is not loopback. `null`, a bad value, another site, and a loopback
/// Origin different from `Host` are refused.
pub(crate) fn origin_ok(headers: &HeaderMap, host: &str, allowed: &HashSet<String>) -> bool {
    let mut all = headers.get_all(header::ORIGIN).iter();
    let Some(first) = all.next() else { return true };
    if all.next().is_some() {
        return false;
    }
    let Ok(origin) = first.to_str() else { return false };
    let Some((scheme, authority)) = origin.split_once("://") else { return false };
    if (scheme != "http" && scheme != "https") || authority.is_empty() || authority.contains(['/', '@', ' ', '\\']) {
        return false;
    }
    if authority.eq_ignore_ascii_case(host) {
        return true;
    }
    let h = host_of(authority);
    !is_loopback_name(&h) && allowed.contains(&h)
}

/// Headers on every response of the door.
pub(crate) async fn response_headers(req: Request, next: Next) -> Response {
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res
}

pub(crate) async fn not_found() -> Response {
    let mut r = Response::new(Body::empty());
    *r.status_mut() = StatusCode::NOT_FOUND;
    r
}
