//! FR-T14: the guest page served at `GET /join` (written test first, not yet compiled or run).
//! The page is one inline script and one style block; the policy allows exactly those by SHA-256.
//! Browser behaviour is checked by the owner by hand (see the PR text), not here.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use base64::Engine;
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::agents::AgentAdapter;
use ember_server::channels::guest::{guest_router, GUEST_ROUTES};
use ember_server::guest_door::{join_page, GuestDoor, JOIN_ROUTE};
use ember_server::session::Sessions;
use ember_server::store::Store;
use http_body_util::BodyExt;
use sha2::{Digest, Sha256};
use tower::ServiceExt;

fn app() -> Router {
    let m: HashMap<String, String> = [("EMBER_GUEST_LISTEN".to_string(), "127.0.0.1:9000".to_string())].into();
    let d = GuestDoor::from_lookup(|k| m.get(k).cloned(), "127.0.0.1:8740".parse::<SocketAddr>().unwrap())
        .unwrap()
        .unwrap();
    let store = Arc::new(Store::open_in_memory().unwrap());
    let sessions = Sessions::new(store.clone(), vec![Arc::new(ScriptedAdapter) as Arc<dyn AgentAdapter>]);
    d.wrap(guest_router(store, sessions))
}

async fn get_join() -> (StatusCode, axum::http::HeaderMap, String) {
    let req = Request::builder().uri("/join").header("host", "127.0.0.1:9000").body(Body::empty()).unwrap();
    let res = app().oneshot(req).await.unwrap();
    let (s, h) = (res.status(), res.headers().clone());
    (s, h, String::from_utf8(res.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap())
}

fn between<'a>(page: &'a str, open: &str, close: &str) -> &'a str {
    let s = page.find(open).unwrap() + open.len();
    let e = page[s..].find(close).unwrap() + s;
    &page[s..e]
}

fn source(text: &str) -> String {
    format!("'sha256-{}'", base64::engine::general_purpose::STANDARD.encode(Sha256::digest(text.as_bytes())))
}

#[tokio::test]
async fn fr_t14_join_csp_hashes_equal_the_served_script_and_style() {
    let (s, h, page) = get_join().await;
    assert_eq!(s, StatusCode::OK);
    let csp = h.get("content-security-policy").unwrap().to_str().unwrap().to_string();
    // Recompute from the served text, not from the constants the server used.
    let script = between(&page, "<script>", "</script>");
    let style = between(&page, "<style>", "</style>");
    assert!(csp.contains(&format!("script-src {};", source(script))), "{csp}");
    assert!(csp.contains(&format!("style-src {};", source(style))), "{csp}");
    assert_eq!(script, join_page().script);
    assert_eq!(csp, join_page().csp);
    assert_eq!(
        csp,
        format!(
            "default-src 'none'; script-src {}; style-src {}; connect-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
            source(script),
            source(style)
        )
    );
    assert_eq!(h.get("referrer-policy").unwrap(), "no-referrer");
    assert_eq!(h.get("x-content-type-options").unwrap(), "nosniff");
    assert_eq!(h.get("cache-control").unwrap(), "no-store");
}

#[tokio::test]
async fn fr_t14_join_csp_has_no_unsafe_wildcard_or_data_sources() {
    let (_, h, _) = get_join().await;
    let csp = h.get("content-security-policy").unwrap().to_str().unwrap();
    for bad in ["unsafe-inline", "unsafe-eval", "unsafe-hashes", "data:", "blob:", "*", "http:", "https:", "'self' 'unsafe"] {
        assert!(!csp.contains(bad), "csp contains {bad}: {csp}");
    }
    for want in ["default-src 'none'", "connect-src 'self'", "base-uri 'none'", "form-action 'none'", "frame-ancestors 'none'"] {
        assert!(csp.contains(want), "{want}");
    }
    // script-src and style-src carry a hash and nothing else.
    for dir in ["script-src", "style-src"] {
        let part = csp.split("; ").find(|p| p.starts_with(dir)).unwrap();
        let rest: Vec<&str> = part.split(' ').skip(1).collect();
        assert_eq!(rest.len(), 1, "{part}");
        assert!(rest[0].starts_with("'sha256-") && rest[0].ends_with('\''), "{part}");
    }
}

#[tokio::test]
async fn fr_t14_join_page_has_no_forbidden_dom_or_code_apis() {
    let (_, _, page) = get_join().await;
    let script = between(&page, "<script>", "</script>");
    let forbidden = [
        "innerHTML",
        "outerHTML",
        "insertAdjacentHTML",
        "document.write",
        "eval(",
        "new Function",
        "Function(",
        "setTimeout(\"",
        "setTimeout('",
        "setInterval(\"",
        "setInterval('",
        "localStorage",
        "document.cookie",
        "XMLHttpRequest",
        "WebSocket",
        "EventSource",
        "sendBeacon",
        "importScripts",
        "postMessage",
        "window.open",
        "location.search",
    ];
    for f in forbidden {
        assert!(!page.contains(f), "page contains {f}");
        assert!(!script.contains(f), "script contains {f}");
    }
    // Timers are only ever given a function name.
    assert!(script.contains("setTimeout(poll"));
}

#[tokio::test]
async fn fr_t14_join_script_removes_the_token_from_the_address_bar() {
    let (_, _, page) = get_join().await;
    let script = between(&page, "<script>", "</script>");
    assert!(script.contains("location.hash"));
    assert!(script.contains("history.replaceState(null, \"\", window.location.pathname)") || script.contains("history.replaceState(null"));
    // The hash is read before it is removed.
    assert!(script.find("location.hash").unwrap() < script.find("replaceState").unwrap());
    assert!(!script.contains("location.search"));
    assert!(script.contains("credentials: \"omit\""));
    assert!(script.contains("sessionStorage"));
    assert!(script.contains("textContent"));
    // The token is sent only in the JSON body of accept.
    assert!(script.contains("{ token: token, name: name }"));
    assert!(!script.contains("?token") && !script.contains("&token") && !script.contains("#\" +"));
}

#[tokio::test]
async fn fr_t14_join_page_makes_no_external_requests() {
    let (_, _, page) = get_join().await;
    assert!(!page.contains("http:") && !page.contains("https:") && !page.contains("//cdn") && !page.contains("ws:"));
    for attr in [" src=", " href=", " action=", " srcset=", "<link", "<img", "<iframe", "<object", "<embed", "<form action", "@import", "url("] {
        assert!(!page.contains(attr), "page contains {attr}");
    }
    // Every fetch path is one of the four guest routes.
    let script = between(&page, "<script>", "</script>");
    let mut paths: Vec<&str> = script.match_indices("\"/").map(|(i, _)| &script[i + 1..]).map(|t| &t[..t.find('"').unwrap()]).collect();
    paths.retain(|p| p.starts_with("/api"));
    paths.sort();
    paths.dedup();
    assert!(!paths.is_empty());
    for p in paths {
        assert!(p.starts_with("/api/guest/"), "{p}");
        let known = GUEST_ROUTES.iter().any(|(_, r)| {
            let prefix = r.split("{id}").next().unwrap();
            p.starts_with(prefix.trim_end_matches('/')) || *r == p
        });
        assert!(known, "{p}");
    }
}

#[tokio::test]
async fn fr_t14_join_page_has_one_inline_script_and_no_inline_handlers() {
    let (_, _, page) = get_join().await;
    assert_eq!(page.matches("<script").count(), 1);
    assert_eq!(page.matches("</script>").count(), 1);
    assert_eq!(page.matches("<style").count(), 1);
    assert!(!page.contains("<script src") && !page.contains("<script type=\"module\""));
    // No on...= attribute and no javascript: URL anywhere outside the script text.
    let script = between(&page, "<script>", "</script>");
    let outside = page.replace(script, "");
    assert!(!outside.contains("javascript:") && !page.contains("javascript:"));
    let lower = outside.to_ascii_lowercase();
    for (i, _) in lower.match_indices(" on") {
        let rest = &lower[i + 3..];
        let name: String = rest.chars().take_while(|c| c.is_ascii_lowercase()).collect();
        assert!(!(rest[name.len()..].starts_with('=') && !name.is_empty()), "inline handler on{name}");
    }
    assert!(!lower.contains(" style=") && !outside.contains("<style>\n@import"));
    assert!(script.contains("addEventListener"));
}

#[tokio::test]
async fn fr_t14_join_page_is_korean_and_mobile_friendly() {
    let (_, h, page) = get_join().await;
    assert!(h.get("content-type").unwrap().to_str().unwrap().starts_with("text/html"));
    assert!(page.contains("lang=\"ko\""));
    assert!(page.contains("width=device-width"));
    assert!(page.contains("prefers-color-scheme:dark"));
    for s in ["Ember 채널에 참여", "이름", "보내기", "초대가 종료되었습니다", "메시지는 8192바이트까지"] {
        assert!(page.contains(s), "{s}");
    }
    assert!(!page.contains('\u{2014}'));
}

#[tokio::test]
async fn fr_t14_join_page_stays_get_only_and_in_the_route_table() {
    assert_eq!(JOIN_ROUTE, ("GET", "/join"));
    // The page is inline: no asset route exists, so /join.js is not served.
    let a = app();
    for (method, path, want) in [
        ("HEAD", "/join", StatusCode::METHOD_NOT_ALLOWED),
        ("TRACE", "/join", StatusCode::METHOD_NOT_ALLOWED),
        ("POST", "/join", StatusCode::METHOD_NOT_ALLOWED),
        ("GET", "/join.js", StatusCode::NOT_FOUND),
        ("GET", "/join.html", StatusCode::NOT_FOUND),
    ] {
        let req = Request::builder().method(method).uri(path).header("host", "127.0.0.1:9000").body(Body::empty()).unwrap();
        assert_eq!(a.clone().oneshot(req).await.unwrap().status(), want, "{method} {path}");
    }
}
