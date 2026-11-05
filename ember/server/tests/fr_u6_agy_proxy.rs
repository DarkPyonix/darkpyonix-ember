//! SPEC FR-U6 / #146: an Antigravity (`agy`) member's model traffic goes to the member's proxy.
//!
//! No real model turn runs and no credit is used. The real `agy -p` is started with the spawn env
//! and arguments Ember builds, in a temporary HOME (agy keeps its state in `~/.gemini`), against a
//! fake local upstream that answers 401 to everything; the test asserts the upstream saw agy's
//! Gemini API request with the member's key. Skipped when no `agy` binary is installed
//! (`EMBER_AGY_BIN` picks one). Written without compiling (AGENTS.md).

use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ember_server::agents::antigravity::{proxy_args, PROXY_BASE_ENV, PROXY_KEY_ENV};
use ember_server::agents::AgentKind;
use ember_server::members::{proxy_env, ProxySpec};

fn agy_bin() -> std::ffi::OsString {
    std::env::var_os("EMBER_AGY_BIN").unwrap_or_else(|| "agy".into())
}

/// A fake upstream recording `(request line, x-goog-api-key header)` of each request.
fn fake_upstream() -> (u16, Arc<Mutex<Vec<(String, String)>>>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        for conn in l.incoming() {
            let Ok(mut c) = conn else { continue };
            let log = log.clone();
            std::thread::spawn(move || {
                let mut buf = vec![0u8; 64 * 1024];
                let n = c.read(&mut buf).unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]).to_string();
                let line = head.lines().next().unwrap_or("").to_string();
                let key = head
                    .lines()
                    .find(|l| l.to_ascii_lowercase().starts_with("x-goog-api-key:"))
                    .map(|l| l.split_once(':').unwrap().1.trim().to_string())
                    .unwrap_or_default();
                log.lock().unwrap().push((line, key));
                let body = r#"{"error":{"code":401,"message":"fake upstream","status":"UNAUTHENTICATED"}}"#;
                let _ = write!(
                    c,
                    "HTTP/1.1 401 Unauthorized\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
            });
        }
    });
    (port, seen)
}

#[test]
fn fr_u6_agy_member_env_names_the_gemini_base_url_and_key() {
    let p = ProxySpec { endpoint: "http://127.0.0.1:9000".into(), key_env: Some("K".into()) };
    let env = proxy_env(AgentKind::Antigravity, &p, |k| (k == "K").then(|| "member-key".into())).unwrap();
    assert!(env.iter().any(|(k, v)| k == PROXY_BASE_ENV && v == "http://127.0.0.1:9000"));
    assert!(env.iter().any(|(k, v)| k == PROXY_KEY_ENV && v == "member-key"));
    assert_eq!(proxy_args(&env), ["--settings", r#"{"modelProvider":"gemini"}"#]);
}

#[test]
fn fr_u6_agy_turn_goes_to_the_member_proxy_endpoint() {
    if Command::new(agy_bin()).arg("--version").stdout(Stdio::null()).status().map_or(true, |s| !s.success()) {
        eprintln!("skipped: no agy binary");
        return;
    }
    let (port, seen) = fake_upstream();
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let p = ProxySpec { endpoint: format!("http://127.0.0.1:{port}"), key_env: Some("K".into()) };
    let env = proxy_env(AgentKind::Antigravity, &p, |_| Some("member-key-146".into())).unwrap();
    let mut cmd = Command::new(agy_bin());
    cmd.args(["-p", "hi", "--output-format", "stream-json", "--print-timeout", "30s"])
        .args(proxy_args(&env))
        .envs(env.iter().map(|(k, v)| (k, v)))
        .current_dir(work.path())
        .env("HOME", home.path())
        .env_remove("GOOGLE_API_KEY")
        .env_remove("HTTPS_PROXY")
        .env_remove("https_proxy")
        .env_remove("HTTP_PROXY")
        .env_remove("http_proxy")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = cmd.spawn().unwrap();
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(40) && seen.lock().unwrap().is_empty() {
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = child.kill();
    let _ = child.wait();
    let seen = seen.lock().unwrap();
    assert!(!seen.is_empty(), "the fake upstream saw no request: agy ignored the proxy endpoint");
    assert!(seen.iter().all(|(line, _)| line.contains("/v1beta/models/")), "{seen:?}");
    assert!(seen.iter().all(|(_, k)| k == "member-key-146"), "{seen:?}");
}
