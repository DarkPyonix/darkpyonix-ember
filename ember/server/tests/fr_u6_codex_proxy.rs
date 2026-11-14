//! SPEC FR-U6 / #145: a Codex member's model traffic goes to the member's proxy endpoint.
//!
//! No real model turn runs and no credit is used. The real `codex exec` is pointed, through the
//! overrides Ember builds, at a fake local upstream that answers 401 to everything; the test
//! asserts the upstream saw the Codex request with the member's key. Skipped when no `codex`
//! binary is installed (`EMBER_CODEX_BIN` picks one). Written without compiling (AGENTS.md).

use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ember_server::agents::codex::{proxy_config_args, CodexAdapter, PROXY_BASE_ENV, PROXY_KEY_ENV};
use ember_server::agents::{AgentAdapter, AgentKind, StartRequest};
use ember_server::events::{AgentEvent, TurnOutcome};
use ember_server::members::{proxy_env, ProxySpec};
use tokio::sync::mpsc;

fn codex_bin() -> std::ffi::OsString {
    std::env::var_os("EMBER_CODEX_BIN").unwrap_or_else(|| "codex".into())
}

/// A fake upstream recording `(request line, authorization header)` of each request.
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
                let mut buf = vec![0u8; 16 * 1024];
                let n = c.read(&mut buf).unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]).to_string();
                let line = head.lines().next().unwrap_or("").to_string();
                let auth = head
                    .lines()
                    .find(|l| l.to_ascii_lowercase().starts_with("authorization:"))
                    .unwrap_or("")
                    .to_string();
                log.lock().unwrap().push((line, auth));
                let body = r#"{"error":{"message":"fake upstream"}}"#;
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
fn fr_u6_codex_member_env_is_not_openai_base_url() {
    // Codex ignores OPENAI_BASE_URL (verified on codex-cli 0.164.0), so it must not be relied on.
    let p = ProxySpec { endpoint: "http://127.0.0.1:9000".into(), key_env: None };
    let env = proxy_env(AgentKind::Codex, &p, |_| None).unwrap();
    assert!(env.iter().any(|(k, v)| k == PROXY_BASE_ENV && v == "http://127.0.0.1:9000"));
    assert!(env.iter().any(|(k, _)| k == PROXY_KEY_ENV));
    assert!(!env.iter().any(|(k, _)| k == "OPENAI_BASE_URL"));
}

#[test]
fn fr_u6_codex_turn_goes_to_the_member_proxy_endpoint() {
    if Command::new(codex_bin()).arg("--version").stdout(Stdio::null()).status().map_or(true, |s| !s.success()) {
        eprintln!("skipped: no codex binary");
        return;
    }
    let (port, seen) = fake_upstream();
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let mut cmd = Command::new(codex_bin());
    cmd.args(["exec", "--skip-git-repo-check", "--ephemeral", "--ignore-user-config"])
        .args(proxy_config_args(&format!("http://127.0.0.1:{port}/v1")))
        .arg("hi")
        .current_dir(work.path())
        .env("CODEX_HOME", home.path())
        .env(PROXY_KEY_ENV, "member-key-145")
        .env_remove("HTTPS_PROXY")
        .env_remove("https_proxy")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = cmd.spawn().unwrap();
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(30) && seen.lock().unwrap().is_empty() {
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = child.kill();
    let _ = child.wait();
    let seen = seen.lock().unwrap();
    assert!(!seen.is_empty(), "the fake upstream saw no request: Codex ignored the proxy endpoint");
    assert!(seen.iter().all(|(line, _)| line.contains("/v1/responses")), "{seen:?}");
    assert!(seen.iter().all(|(_, a)| a.ends_with("Bearer member-key-145")), "{seen:?}");
}

/// A fake Responses API upstream that streams a minimal completed turn, so `CodexAdapter`
/// (`codex app-server`) completes a full turn end-to-end through the member proxy endpoint.
fn fake_streaming_upstream() -> (u16, Arc<Mutex<Vec<(String, String)>>>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        for conn in l.incoming() {
            let Ok(mut c) = conn else { continue };
            let log = log.clone();
            std::thread::spawn(move || {
                let mut buf = vec![0u8; 32 * 1024];
                let n = c.read(&mut buf).unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]).to_string();
                let line = head.lines().next().unwrap_or("").to_string();
                let auth = head
                    .lines()
                    .find(|l| l.to_ascii_lowercase().starts_with("authorization:"))
                    .unwrap_or("")
                    .to_string();
                log.lock().unwrap().push((line, auth));
                let sse = concat!(
                    "event: response.created\n",
                    "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_145\",\"status\":\"in_progress\",\"output\":[]}}\n\n",
                    "event: response.output_item.added\n",
                    "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"id\":\"msg_145\",\"type\":\"message\",\"status\":\"in_progress\",\"role\":\"assistant\",\"content\":[]}}\n\n",
                    "event: response.content_part.added\n",
                    "data: {\"type\":\"response.content_part.added\",\"item_id\":\"msg_145\",\"output_index\":0,\"content_index\":0,\"part\":{\"type\":\"output_text\",\"text\":\"\"}}\n\n",
                    "event: response.output_text.delta\n",
                    "data: {\"type\":\"response.output_text.delta\",\"item_id\":\"msg_145\",\"output_index\":0,\"content_index\":0,\"delta\":\"proxy-ok\"}\n\n",
                    "event: response.output_text.done\n",
                    "data: {\"type\":\"response.output_text.done\",\"item_id\":\"msg_145\",\"output_index\":0,\"content_index\":0,\"text\":\"proxy-ok\"}\n\n",
                    "event: response.output_item.done\n",
                    "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"id\":\"msg_145\",\"type\":\"message\",\"status\":\"completed\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"proxy-ok\"}]}}\n\n",
                    "event: response.completed\n",
                    "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_145\",\"status\":\"completed\",\"output\":[{\"id\":\"msg_145\",\"type\":\"message\",\"status\":\"completed\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"proxy-ok\"}]}],\"usage\":{\"input_tokens\":10,\"input_tokens_details\":{\"cached_tokens\":0},\"output_tokens\":2,\"output_tokens_details\":{\"reasoning_tokens\":0},\"total_tokens\":12}}}\n\n",
                );
                let _ = write!(
                    c,
                    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{sse}",
                    sse.len()
                );
            });
        }
    });
    (port, seen)
}

#[tokio::test]
async fn fr_u6_codex_adapter_app_server_routes_turn_through_member_proxy() {
    if Command::new(codex_bin()).arg("--version").stdout(Stdio::null()).status().map_or(true, |s| !s.success()) {
        eprintln!("skipped: no codex binary");
        return;
    }
    let (port, seen) = fake_streaming_upstream();
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let p = ProxySpec {
        endpoint: format!("http://127.0.0.1:{port}/v1"),
        key_env: Some("K".into()),
    };
    let mut env = proxy_env(AgentKind::Codex, &p, |k| (k == "K").then(|| "member-app-server-key-145".into())).unwrap();
    env.push(("CODEX_HOME".into(), home.path().to_string_lossy().into_owned()));
    env.push(("NO_PROXY".into(), "127.0.0.1,localhost".into()));

    let adapter = CodexAdapter::new(codex_bin());
    let (tx, mut rx) = mpsc::channel(64);
    let mut run = adapter
        .start(
            StartRequest {
                cwd: work.path().to_path_buf(),
                env,
                ..Default::default()
            },
            tx,
        )
        .await
        .expect("start codex app-server with proxy_env");

    run.send("say proxy-ok").await.expect("send turn over codex app-server");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut ended = false;
    let mut assistant_text = String::new();
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(200), rx.recv()).await {
            Ok(Some(AgentEvent::AssistantDelta { text })) => assistant_text.push_str(&text),
            Ok(Some(AgentEvent::AssistantMessage { text })) if assistant_text.is_empty() => {
                assistant_text = text;
            }
            Ok(Some(AgentEvent::TurnEnded { outcome })) => {
                assert_eq!(outcome, TurnOutcome::Completed);
                ended = true;
                break;
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => {
                if !seen.lock().unwrap().is_empty() && ended {
                    break;
                }
            }
        }
    }
    let _ = run.shutdown().await;

    let seen = seen.lock().unwrap();
    assert!(!seen.is_empty(), "codex app-server sent no request to the member proxy endpoint");
    assert!(seen.iter().all(|(line, _)| line.contains("/v1/responses")), "{seen:?}");
    assert!(seen.iter().all(|(_, a)| a.ends_with("Bearer member-app-server-key-145")), "{seen:?}");
    assert!(ended, "codex app-server turn did not complete from the streamed proxy response");
    assert!(assistant_text.contains("proxy-ok"), "expected assistant text from proxy, got {assistant_text:?}");
}
