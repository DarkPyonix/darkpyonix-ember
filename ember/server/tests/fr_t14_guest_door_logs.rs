//! FR-T14: the tracing messages of `serve_capped` that say "guest door", pinned before the
//! function moves into `door/serve.rs` (step C0), where a later step makes the noun a parameter.
//!
//! This test lives in its own file on purpose (issue #549, same device as `fr_c2_enrol_capture.rs`):
//! every integration test file is its own process, and the capture subscriber is installed as the
//! global default before the first event, so the callsite interest cache sees it. Keep this file
//! to ONE test.
//!
//! Expected values were read from `serve_capped` (`tracing::warn!("guest door accept failed: {e}")`,
//! `tracing::debug!("guest door at its connection cap, dropped {peer} ({dropped} dropped so far)")`,
//! `tracing::debug!("guest door connection closed: {e}")`) and from hyper 1.11.1 (`Error`'s Display
//! is its description; a header-read timeout is `"read header from client timeout"`; a server
//! connection whose request head cannot be read ends with `Err`, `proto/h1/dispatch.rs`
//! `poll_read_head` then `poll_catch`, `Server::recv_msg(Err)`). NONE of this has been run when
//! the file was written (nothing was compiled).
//!
//! Driven: the connection cap (DEBUG), a connection that ends in error (DEBUG), twice: a request
//! that is not HTTP, and a header that never completes.
//! NOT driven: `guest door accept failed` (WARN). It needs `accept()` itself to fail (out of
//! descriptors and the like), which cannot be caused from outside without lowering the process
//! file-descriptor limit, which would disturb everything else in the process.
//! The level is asserted by the level column of the formatted line; the target (module path) is
//! not asserted, because the function moves.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::routing::get;
use axum::Router;
use ember_server::guest_door::GuestDoor;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// A tracing writer that collects everything written.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Capture {
    fn lines(&self) -> Vec<String> {
        String::from_utf8_lossy(&self.0.lock().unwrap()).lines().map(str::to_string).collect()
    }
    /// The captured lines that contain `needle`.
    fn with(&self, needle: &str) -> Vec<String> {
        self.lines().into_iter().filter(|l| l.contains(needle)).collect()
    }
}

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

/// Polls `cond` every 10 ms until true; panics naming `what` after `timeout`.
async fn eventually(timeout: Duration, what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while !cond() {
        if Instant::now() >= deadline {
            panic!("not within {timeout:?}: {what}");
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// True when a line containing `needle` is captured within `within` (polled every 10 ms).
async fn seen(cap: &Capture, needle: &str, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    loop {
        if !cap.with(needle).is_empty() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn door() -> GuestDoor {
    GuestDoor::from_lookup(
        |k| (k == "EMBER_GUEST_LISTEN").then(|| "127.0.0.1:9000".to_string()),
        "127.0.0.1:8740".parse::<SocketAddr>().unwrap(),
    )
    .map_err(|e| e.to_string())
    .unwrap()
    .expect("door on")
}

#[tokio::test]
async fn fr_t14_serve_capped_logs_the_cap_and_a_connection_that_ends_in_error() {
    let cap = Capture::default();
    let sub = tracing_subscriber::fmt()
        .with_writer(cap.clone())
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .finish();
    // Global default, once, before any event.
    tracing::subscriber::set_global_default(sub).expect("no other subscriber in this process");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = door().wrap(Router::new().route("/ok", get(|| async { "ok" })));
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    // Cap of 1 and a short header-read timeout (used by the last case only).
    let task = tokio::spawn(GuestDoor::serve_capped(
        listener,
        app,
        async move {
            let _ = rx.await;
        },
        Duration::from_millis(300),
        1,
    ));

    // 1. The cap. c1 is served (so its permit is held while it stays open, keep-alive); c2 is
    // closed at accept and the door logs it at DEBUG with the peer and the running count.
    let mut c1 = tokio::net::TcpStream::connect(addr).await.unwrap();
    c1.write_all(b"GET /ok HTTP/1.1\r\nHost: 127.0.0.1:9000\r\n\r\n").await.unwrap();
    let mut got = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut chunk = [0u8; 1024];
        while !got.ends_with(b"\r\n\r\nok") {
            let n = c1.read(&mut chunk).await.unwrap();
            assert!(n > 0, "c1 closed before the answer: {:?}", String::from_utf8_lossy(&got));
            got.extend_from_slice(&chunk[..n]);
        }
    })
    .await
    .expect("c1 is served");
    let mut c2 = tokio::net::TcpStream::connect(addr).await.unwrap();
    let c2_addr = c2.local_addr().unwrap();
    let mut rest = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(5), c2.read_to_end(&mut rest)).await;
    assert!(rest.is_empty(), "the capped connection got an answer");
    eventually(Duration::from_secs(5), "the connection-cap line is logged", || {
        !cap.with("guest door at its connection cap").is_empty()
    })
    .await;
    let lines = cap.with("guest door at its connection cap");
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("DEBUG"), "level: {}", lines[0]);
    assert!(
        lines[0].contains(&format!("guest door at its connection cap, dropped {c2_addr} (1 dropped so far)")),
        "text: {}",
        lines[0]
    );

    // 2. A connection that ends in error. Free the slot first (the door releases the permit when
    // the connection task ends), then retry until a connection is not dropped at the cap.
    drop(c1);
    drop(c2);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let mut c = tokio::net::TcpStream::connect(addr).await.unwrap();
        let _ = c.write_all(b"\x01\x02 not http\r\n\r\n").await;
        let mut sink = Vec::new();
        let _ = tokio::time::timeout(Duration::from_secs(5), c.read_to_end(&mut sink)).await;
        if seen(&cap, "guest door connection closed", Duration::from_millis(500)).await {
            break;
        }
        assert!(Instant::now() < deadline, "a not-HTTP request never ended its connection in a logged error");
    }
    let lines = cap.with("guest door connection closed");
    for l in &lines {
        assert!(l.contains("DEBUG"), "level: {l}");
        assert!(l.contains("guest door connection closed: "), "text: {l}");
    }

    // 3. A header that never completes: the header-read timeout (300 ms above) ends the
    // connection with hyper's `HeaderTimeout`, whose Display is the description below.
    let needle = "guest door connection closed: read header from client timeout";
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let mut c = tokio::net::TcpStream::connect(addr).await.unwrap();
        c.write_all(b"GET /ok HTTP/1.1\r\nHost: 127.0.0.1:9000\r\nX-Slow: ").await.unwrap();
        if seen(&cap, needle, Duration::from_secs(2)).await {
            break;
        }
        assert!(Instant::now() < deadline, "the header-read timeout was never logged as a closed connection");
    }
    for l in cap.with(needle) {
        assert!(l.contains("DEBUG"), "level: {l}");
    }

    let _ = tx.send(());
    let _ = tokio::time::timeout(Duration::from_secs(5), task).await;
}
