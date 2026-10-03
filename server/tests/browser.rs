//! Remote browser end to end against a real Chrome/Chromium (SPEC FR-R1–FR-R4).
//!
//! Each test skips (with a message) when no browser is found; set `EMBER_CHROME_BIN` to choose one.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse};
use axum::routing::get;
use ember_server::browser::{chrome, BrowserConfig, BrowserInstance, BrowserManager, InputEvent};
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message;

fn manager(dir: &std::path::Path) -> Option<Arc<BrowserManager>> {
    let Some(chrome) = chrome::find_chrome() else {
        eprintln!("skipping: no Chrome/Chromium found (set EMBER_CHROME_BIN)");
        return None;
    };
    let mut cfg = BrowserConfig::from_env(dir);
    cfg.chrome = Some(chrome);
    cfg.headless = true;
    Some(BrowserManager::new(cfg))
}

// ---- a tiny site ----

#[derive(Clone, Default)]
struct Site {
    cookies_seen: Arc<Mutex<Vec<String>>>,
}

const CLICK_PAGE: &str = r#"<!doctype html><html><body style="margin:0">
<button id="b" style="position:absolute;left:0;top:0;width:400px;height:300px">click me</button>
<input id="t" style="position:absolute;left:0;top:320px;width:300px">
<div id="spin" style="position:absolute;left:500px;top:0;width:100px;height:100px;background:red"></div>
<script>
window.clicks = 0; window.lastXY = null;
document.getElementById('b').addEventListener('click', e => { window.clicks++; window.lastXY = [e.clientX, e.clientY]; });
let a = 0; (function f(){ a = (a + 7) % 360; document.getElementById('spin').style.transform = 'rotate(' + a + 'deg)'; requestAnimationFrame(f); })();
</script></body></html>"#;

async fn serve_site(site: Site) -> SocketAddr {
    let app = axum::Router::new()
        .route(
            "/set",
            get(|| async {
                (
                    [("set-cookie", "sid=abc123; Max-Age=3600; Path=/")],
                    Html("<p id=c>set</p>"),
                )
                    .into_response()
            }),
        )
        .route(
            "/",
            get(|axum::extract::State(site): axum::extract::State<Site>, h: HeaderMap| async move {
                let c = h.get("cookie").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
                site.cookies_seen.lock().unwrap().push(c.clone());
                Html(format!("<p id=c>{c}</p>"))
            }),
        )
        .route("/click", get(|| async { Html(CLICK_PAGE) }))
        .with_state(site);
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    addr
}

// ---- a minimal SOCKS5 server that records where it connected ----

async fn socks5_server() -> (SocketAddr, Arc<Mutex<Vec<String>>>) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let seen2 = seen.clone();
    tokio::spawn(async move {
        while let Ok((s, _)) = l.accept().await {
            let seen = seen2.clone();
            tokio::spawn(async move {
                let _ = socks5_conn(s, seen).await;
            });
        }
    });
    (addr, seen)
}

async fn socks5_conn(mut s: TcpStream, seen: Arc<Mutex<Vec<String>>>) -> std::io::Result<()> {
    let mut hdr = [0u8; 2];
    s.read_exact(&mut hdr).await?;
    let mut methods = vec![0u8; hdr[1] as usize];
    s.read_exact(&mut methods).await?;
    s.write_all(&[5, 0]).await?;
    let mut req = [0u8; 4];
    s.read_exact(&mut req).await?;
    let host = match req[3] {
        1 => {
            let mut a = [0u8; 4];
            s.read_exact(&mut a).await?;
            std::net::Ipv4Addr::from(a).to_string()
        }
        3 => {
            let mut n = [0u8; 1];
            s.read_exact(&mut n).await?;
            let mut d = vec![0u8; n[0] as usize];
            s.read_exact(&mut d).await?;
            String::from_utf8_lossy(&d).into_owned()
        }
        _ => {
            let mut a = [0u8; 16];
            s.read_exact(&mut a).await?;
            format!("[{}]", std::net::Ipv6Addr::from(a))
        }
    };
    let mut p = [0u8; 2];
    s.read_exact(&mut p).await?;
    let target = format!("{host}:{}", u16::from_be_bytes(p));
    seen.lock().unwrap().push(target.clone());
    let mut up = match TcpStream::connect(&target).await {
        Ok(u) => u,
        Err(_) => {
            s.write_all(&[5, 5, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
            return Ok(());
        }
    };
    s.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
    tokio::io::copy_bidirectional(&mut s, &mut up).await?;
    Ok(())
}

// ---- helpers ----

async fn goto(b: &BrowserInstance, url: &str) {
    b.input(InputEvent::Navigate { url: url.into() }).await.unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Ok(v) = b.evaluate("location.href + '|' + document.readyState").await {
            if v.as_str() == Some(&format!("{url}|complete")) {
                return;
            }
        }
        assert!(Instant::now() < deadline, "navigation to {url} did not finish");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn text_of_c(b: &BrowserInstance) -> String {
    b.evaluate("document.getElementById('c').textContent").await.unwrap().as_str().unwrap().to_string()
}

fn tempdir() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

// ---- tests ----

/// FR-R1 (egress through the chosen proxy, loopback included), FR-R2 (still logged in after
/// switching egress), FR-R4 (clearing the profile).
/// Browser tests launch a real Chrome each; run them one at a time so they don't starve each
/// other on small CI runners (seen on GitHub's 2-core Linux runner).
static BROWSER_TESTS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test(flavor = "multi_thread")]
async fn cookies_survive_egress_switch_and_clear_removes_them() {
    let _serial = BROWSER_TESTS.lock().await;
    let dir = tempdir();
    let Some(m) = manager(dir.path()) else { return };
    let site = Site::default();
    let addr = serve_site(site.clone()).await;
    let (proxy_a, seen_a) = socks5_server().await;
    let (proxy_b, seen_b) = socks5_server().await;
    let base = format!("http://127.0.0.1:{}", addr.port());

    let b = m.open("egress", Some(Some(format!("socks5://{proxy_a}")))).await.unwrap();
    b.wait_page(Duration::from_secs(30)).await.unwrap();
    goto(&b, &format!("{base}/set")).await;
    let target = format!("127.0.0.1:{}", addr.port());
    assert!(seen_a.lock().unwrap().contains(&target), "egress A not used: {:?}", seen_a.lock().unwrap());
    assert!(seen_b.lock().unwrap().is_empty());

    // Switch egress: restart on the same profile with proxy B.
    b.set_egress(Some(format!("socks5://{proxy_b}"))).await.unwrap();
    assert_eq!(b.egress().as_deref(), Some(format!("socks5://{proxy_b}").as_str()));
    b.wait_page(Duration::from_secs(30)).await.unwrap();
    goto(&b, &format!("{base}/")).await;
    assert!(seen_b.lock().unwrap().contains(&target), "egress B not used");
    assert!(text_of_c(&b).await.contains("sid=abc123"), "cookie lost across egress switch");
    assert!(site.cookies_seen.lock().unwrap().last().unwrap().contains("sid=abc123"));

    // Clear the profile (FR-R4): the cookie is gone, the browser keeps running on egress B.
    m.clear_data("egress").await.unwrap();
    b.wait_page(Duration::from_secs(30)).await.unwrap();
    goto(&b, &format!("{base}/")).await;
    assert_eq!(text_of_c(&b).await, "");
    b.stop().await;
}

/// The view stream over the real HTTP API: hello, JPEG frames with metadata, and a click that
/// reaches the page. Prints local frame-rate and input-latency numbers.
#[tokio::test(flavor = "multi_thread")]
async fn view_stream_frames_and_click() {
    let _serial = BROWSER_TESTS.lock().await;
    let dir = tempdir();
    let Some(m) = manager(dir.path()) else { return };
    let addr = serve_site(Site::default()).await;
    let api = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let api_addr = api.local_addr().unwrap();
    let router = ember_server::browser::api::router(m.clone());
    tokio::spawn(async move { axum::serve(api, router).await.unwrap() });

    let (mut ws, _) =
        tokio_tungstenite::connect_async(format!("ws://{api_addr}/api/v1/browsers/view/view"))
            .await
            .unwrap();
    let hello: Value = match ws.next().await.unwrap().unwrap() {
        Message::Text(t) => serde_json::from_str(t.as_str()).unwrap(),
        other => panic!("expected hello, got {other:?}"),
    };
    assert_eq!(hello["type"], "hello");
    assert_eq!(hello["v"], 1);

    let b = m.get("view").await.unwrap();
    b.wait_page(Duration::from_secs(30)).await.unwrap();
    let url = format!("http://127.0.0.1:{}/click", addr.port());
    let nav = json!({ "type": "navigate", "url": url }).to_string();
    ws.send(Message::Text(nav.into())).await.unwrap();

    // Collect frames for 3 s while the page animates.
    let start = Instant::now();
    let mut frames = 0u32;
    let mut bytes = 0usize;
    let mut first_header: Option<Value> = None;
    while start.elapsed() < Duration::from_secs(3) {
        let msg = match tokio::time::timeout(Duration::from_secs(3), ws.next()).await {
            Ok(Some(Ok(m))) => m,
            _ => break,
        };
        if let Message::Binary(data) = msg {
            let n = u32::from_be_bytes(data[..4].try_into().unwrap()) as usize;
            let header: Value = serde_json::from_slice(&data[4..4 + n]).unwrap();
            let jpeg = &data[4 + n..];
            assert_eq!(&jpeg[..2], &[0xFF, 0xD8], "not a JPEG");
            assert_eq!(header["type"], "frame");
            assert!(header["metadata"]["deviceWidth"].as_f64().unwrap() > 0.0);
            bytes += jpeg.len();
            frames += 1;
            first_header.get_or_insert(header);
        }
    }
    assert!(frames >= 1, "no screencast frame");
    let secs = start.elapsed().as_secs_f64();
    eprintln!(
        "screencast: {frames} frames in {secs:.1} s = {:.1} fps, avg {:.1} KB/frame (1280x800, q70)",
        frames as f64 / secs,
        bytes as f64 / frames as f64 / 1024.0
    );
    assert_eq!(first_header.unwrap()["agent_active"], false);

    // Make sure the click page is loaded, then click through the stream's input protocol.
    goto(&b, &url).await;
    let t0 = Instant::now();
    for ev in ["mousePressed", "mouseReleased"] {
        let msg = json!({ "type": "mouse", "event": ev, "x": 120, "y": 80, "button": "left", "click_count": 1 });
        ws.send(Message::Text(msg.to_string().into())).await.unwrap();
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if b.evaluate("window.clicks").await.unwrap() == json!(1) {
            break;
        }
        assert!(Instant::now() < deadline, "click did not reach the page");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    eprintln!("input: click seen by the page {:.1} ms after sending", t0.elapsed().as_secs_f64() * 1e3);
    assert_eq!(b.evaluate("window.lastXY").await.unwrap(), json!([120, 80]));

    // Typing: focus the input and insert text.
    for ev in ["mousePressed", "mouseReleased"] {
        let msg = json!({ "type": "mouse", "event": ev, "x": 50, "y": 330, "button": "left", "click_count": 1 });
        ws.send(Message::Text(msg.to_string().into())).await.unwrap();
    }
    ws.send(Message::Text(json!({ "type": "text", "text": "hi" }).to_string().into())).await.unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while b.evaluate("document.getElementById('t').value").await.unwrap() != json!("hi") {
        assert!(Instant::now() < deadline, "typed text did not reach the page");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    drop(ws);
    b.stop().await;
}

/// FR-R3: an agent's DevTools connection through the relay marks the agent active, and the user's
/// takeover holds agent commands until released.
#[tokio::test(flavor = "multi_thread")]
async fn agent_relay_activity_and_takeover() {
    let _serial = BROWSER_TESTS.lock().await;
    let dir = tempdir();
    let Some(m) = manager(dir.path()) else { return };
    let api = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let api_addr = api.local_addr().unwrap();
    let router = ember_server::browser::api::router(m.clone());
    tokio::spawn(async move { axum::serve(api, router).await.unwrap() });

    // Discovery document, as browser MCP servers read it.
    let ver: Value = serde_json::from_str(
        &http_get(api_addr, "/api/v1/browsers/agent/cdp/json/version").await,
    )
    .unwrap();
    let ws_url = ver["webSocketDebuggerUrl"].as_str().unwrap().to_string();
    assert_eq!(ws_url, format!("ws://{api_addr}/api/v1/browsers/agent/cdp"));

    let (mut agent, _) = tokio_tungstenite::connect_async(&ws_url).await.unwrap();
    let b = m.get("agent").await.unwrap();
    // The relay counts the connection once it has attached upstream, just after the upgrade.
    let deadline = Instant::now() + Duration::from_secs(5);
    while b.state().borrow().agent_connections != 1 && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(b.state().borrow().agent_connections, 1);

    let call = |id: u64| Message::Text(json!({ "id": id, "method": "Browser.getVersion" }).to_string().into());
    agent.send(call(1)).await.unwrap();
    let r = recv_id(&mut agent, 1, Duration::from_secs(5)).await.expect("relay answered");
    assert!(r["result"]["product"].as_str().unwrap().contains("Chrome"));
    assert!(b.state().borrow().agent_active);

    // User takes over: the agent's next command is held…
    b.input(InputEvent::Takeover { on: true }).await.unwrap();
    assert!(b.state().borrow().takeover);
    agent.send(call(2)).await.unwrap();
    assert!(recv_id(&mut agent, 2, Duration::from_millis(700)).await.is_none(), "not held");
    // …and goes through once the user hands back.
    b.input(InputEvent::Takeover { on: false }).await.unwrap();
    assert!(recv_id(&mut agent, 2, Duration::from_secs(5)).await.is_some());

    // The flag drops once the agent goes quiet.
    let deadline = Instant::now() + Duration::from_secs(6);
    while b.state().borrow().agent_active {
        assert!(Instant::now() < deadline, "agent_active never cleared");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    drop(agent);
    let deadline = Instant::now() + Duration::from_secs(5);
    while b.state().borrow().agent_connections != 0 {
        assert!(Instant::now() < deadline, "agent connection not released");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let cfg: Value =
        serde_json::from_str(&http_get(api_addr, "/api/v1/browsers/agent/agent-config").await).unwrap();
    assert_eq!(cfg["cdp_ws"], ws_url);
    b.stop().await;
}

async fn recv_id(
    ws: &mut tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>,
    id: u64,
    within: Duration,
) -> Option<Value> {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        let m = tokio::time::timeout_at(deadline, ws.next()).await.ok()??.ok()?;
        if let Message::Text(t) = m {
            let v: Value = serde_json::from_str(t.as_str()).ok()?;
            if v["id"] == id {
                return Some(v);
            }
        }
    }
}

async fn http_get(addr: SocketAddr, path: &str) -> String {
    let mut s = TcpStream::connect(addr).await.unwrap();
    let req = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    s.write_all(req.as_bytes()).await.unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).await.unwrap();
    let (head, body) = out.split_once("\r\n\r\n").unwrap();
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    body.to_string()
}
