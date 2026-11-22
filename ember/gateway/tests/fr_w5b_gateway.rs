//! SPEC FR-W5b (#80): the in-app gateway. A loopback listener with a per-launch token that pipes
//! each accepted request to the computer's OSE on its own relay stream. The OSE here is a fake
//! in-process HTTP and WebSocket server reached through a fake [`Dialer`]: each dial is one
//! in-memory stream, counted, and every request head the fake receives is recorded, so the tests
//! see exactly what the gateway would put on the relay.

use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ember_gateway::{Dialer, Gateway, GatewayConfig, GatewayError, OSE_TOKEN_COOKIE, TOKEN_COOKIE, TOKEN_HEADER};
use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tokio_tungstenite::tungstenite::protocol::Role;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_tungstenite::WebSocketStream;

const WAIT: Duration = Duration::from_secs(10);
const WORKBENCH: &str = "<!DOCTYPE html><html><body>fake workbench</body></html>";
/// The OSE launch's placeholder token (64 characters, like the node's).
const PLACEHOLDER: &str = "abcdabcdabcdabcdabcdabcdabcdabcdabcdabcdabcdabcdabcdabcdabcdabcd";

/// The fake OSE behind the fake relay: counts dials and records each request head it receives.
#[derive(Clone, Default)]
struct FakeOse {
    dials: Arc<AtomicUsize>,
    heads: Arc<Mutex<Vec<String>>>,
}

impl FakeOse {
    fn dials(&self) -> usize {
        self.dials.load(Ordering::SeqCst)
    }
    fn heads(&self) -> Vec<String> {
        self.heads.lock().unwrap().clone()
    }
}

impl Dialer for FakeOse {
    type Stream = DuplexStream;
    fn dial(&self) -> impl Future<Output = io::Result<DuplexStream>> + Send {
        let me = self.clone();
        async move {
            me.dials.fetch_add(1, Ordering::SeqCst);
            let (near, far) = tokio::io::duplex(64 * 1024);
            tokio::spawn(serve_ose(far, me.heads.clone()));
            Ok(near)
        }
    }
}

async fn read_head<S: AsyncReadExt + Unpin>(s: &mut S) -> String {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if s.read(&mut byte).await.unwrap_or(0) == 0 {
            break;
        }
        head.push(byte[0]);
    }
    String::from_utf8_lossy(&head).into_owned()
}

fn header<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.split("\r\n").skip(1).find_map(|line| {
        let (n, v) = line.split_once(':')?;
        n.trim().eq_ignore_ascii_case(name).then(|| v.trim())
    })
}

/// One relay stream's worth of OSE: a WebSocket upgrade becomes an echo socket; any other request
/// gets the workbench page, and the stream closes after it only if the request said
/// `Connection: close` (otherwise it waits for a next request that never comes).
async fn serve_ose(mut s: DuplexStream, heads: Arc<Mutex<Vec<String>>>) {
    let head = read_head(&mut s).await;
    heads.lock().unwrap().push(head.clone());
    if header(&head, "upgrade").is_some_and(|v| v.eq_ignore_ascii_case("websocket")) {
        let key = header(&head, "sec-websocket-key").unwrap_or_default();
        let answer = format!(
            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n",
            derive_accept_key(key.as_bytes())
        );
        s.write_all(answer.as_bytes()).await.unwrap();
        let mut ws = WebSocketStream::from_raw_socket(s, Role::Server, None).await;
        while let Some(Ok(m)) = ws.next().await {
            if m.is_binary() || m.is_text() {
                if ws.send(m).await.is_err() {
                    break;
                }
            } else if m.is_close() {
                break;
            }
        }
        return;
    }
    let close = header(&head, "connection").is_some_and(|v| v.eq_ignore_ascii_case("close"));
    let answer = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n{}\r\n{WORKBENCH}",
        WORKBENCH.len(),
        if close { "Connection: close\r\n" } else { "" }
    );
    s.write_all(answer.as_bytes()).await.unwrap();
    if close {
        let _ = s.shutdown().await;
    } else {
        let _ = s.read_to_end(&mut Vec::new()).await;
    }
}

struct Answer {
    status: u16,
    head: String,
    body: String,
}

/// Sends `raw` on a new connection and reads until the gateway closes it.
async fn send_raw(addr: SocketAddr, raw: &str) -> String {
    let mut s = TcpStream::connect(addr).await.unwrap();
    s.write_all(raw.as_bytes()).await.unwrap();
    let mut out = Vec::new();
    tokio::time::timeout(WAIT, s.read_to_end(&mut out)).await.expect("the gateway closes the connection").unwrap();
    String::from_utf8_lossy(&out).into_owned()
}

/// A browser-like GET (it asks for keep-alive) with `extra` header lines.
async fn get(addr: SocketAddr, path: &str, extra: &[(&str, &str)]) -> Answer {
    let mut raw = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: keep-alive\r\n");
    for (n, v) in extra {
        raw.push_str(&format!("{n}: {v}\r\n"));
    }
    raw.push_str("\r\n");
    let text = send_raw(addr, &raw).await;
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let status = head.split(' ').nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
    Answer { status, head: format!("{head}\r\n\r\n"), body: body.to_owned() }
}

fn cookie(token: &str) -> String {
    format!("{TOKEN_COOKIE}={token}")
}

async fn start(ose: &FakeOse) -> Gateway {
    let config = GatewayConfig { ose_token: Some(PLACEHOLDER.to_owned()), ..GatewayConfig::default() };
    Gateway::start(ose.clone(), config).await.unwrap()
}

/// (a) The first page load carries the token as `tkn`: the gateway answers it itself with a
/// redirect that drops `tkn` from the address and sets the cookie, then the page comes through
/// one stream. A header token also works. The OSE never sees the gateway's token; it gets the
/// placeholder as its own cookie, right after the request line.
#[tokio::test]
async fn fr_w5b_a_request_with_the_token_gets_the_workbench() {
    let ose = FakeOse::default();
    let gw = start(&ose).await;
    let (addr, token) = (gw.addr(), gw.token().to_owned());
    assert_eq!(gw.url("/?folder=%2Fp"), format!("http://{addr}/?folder=%2Fp&tkn={token}"));
    assert_eq!(gw.url("/"), format!("http://{addr}/?tkn={token}"));

    let first = get(addr, &format!("/?folder=%2Fp&tkn={token}"), &[]).await;
    assert_eq!(first.status, 302, "{}", first.head);
    assert_eq!(header(&first.head, "location"), Some("/?folder=%2Fp"));
    let set = header(&first.head, "set-cookie").expect("the gateway sets its cookie");
    assert!(set.starts_with(&format!("{}; ", cookie(&token))), "{set}");
    for attr in ["HttpOnly", "SameSite=Strict", "Path=/"] {
        assert!(set.split("; ").any(|a| a == attr), "{attr} in {set}");
    }
    assert_eq!(ose.dials(), 0, "the redirect opens no stream");

    let page = get(addr, "/?folder=%2Fp", &[("Cookie", &cookie(&token))]).await;
    assert_eq!(page.status, 200, "{}", page.head);
    assert_eq!(page.body, WORKBENCH);
    assert_eq!(ose.dials(), 1);

    let by_header = get(addr, "/", &[(TOKEN_HEADER, &token)]).await;
    assert_eq!(by_header.status, 200, "{}", by_header.head);
    assert_eq!(by_header.body, WORKBENCH);

    let heads = ose.heads();
    assert_eq!(heads.len(), 2);
    let lines: Vec<&str> = heads[0].split("\r\n").collect();
    assert_eq!(lines[0], "GET /?folder=%2Fp HTTP/1.1");
    assert_eq!(lines[1], format!("Cookie: {OSE_TOKEN_COOKIE}={PLACEHOLDER}"), "the placeholder comes first");
    for head in &heads {
        assert!(!head.contains(&token), "the gateway token never reaches the OSE: {head}");
        assert!(!head.to_ascii_lowercase().contains(&TOKEN_HEADER.to_ascii_lowercase()), "{head}");
    }
}

/// (b) No token, a wrong one, or one of the wrong length, in any of the three places: 403 and no
/// stream, for a page and for a WebSocket upgrade alike.
#[tokio::test]
async fn fr_w5b_a_request_without_the_right_token_is_403_and_opens_no_stream() {
    let ose = FakeOse::default();
    let gw = start(&ose).await;
    let (addr, token) = (gw.addr(), gw.token().to_owned());
    let mut wrong = token.clone().into_bytes();
    wrong[0] = if wrong[0] == b'0' { b'1' } else { b'0' };
    let wrong = String::from_utf8(wrong).unwrap();
    let short = &token[..token.len() - 1];

    let tries: Vec<(String, Vec<(&str, String)>)> = vec![
        ("/".into(), vec![]),
        (format!("/?tkn={wrong}"), vec![]),
        (format!("/?tkn={short}"), vec![]),
        ("/?tkn=".into(), vec![]),
        ("/".into(), vec![("Cookie", cookie(&wrong))]),
        ("/".into(), vec![("Cookie", format!("{OSE_TOKEN_COOKIE}={token}"))]),
        ("/".into(), vec![(TOKEN_HEADER, wrong.clone())]),
        ("/".into(), vec![(TOKEN_HEADER, short.to_owned())]),
    ];
    for (path, extra) in &tries {
        let extra: Vec<(&str, &str)> = extra.iter().map(|(n, v)| (*n, v.as_str())).collect();
        let a = get(addr, path, &extra).await;
        assert_eq!(a.status, 403, "{path} {extra:?}: {}", a.head);
        assert!(!a.body.contains(WORKBENCH));
    }

    let upgrade = format!("ws://{addr}/socket").into_client_request().unwrap();
    let tcp = TcpStream::connect(addr).await.unwrap();
    match tokio::time::timeout(WAIT, tokio_tungstenite::client_async(upgrade, tcp)).await.unwrap() {
        Err(WsError::Http(resp)) => assert_eq!(resp.status().as_u16(), 403),
        other => panic!("an upgrade without the token is refused, got {:?}", other.map(|(_, r)| r.status())),
    }
    assert_eq!(ose.dials(), 0, "no refused request opens a stream");
}

/// (c) After the first page load the cookie alone works, also among the page's other cookies;
/// the gateway's cookie is taken out before the request goes on, the others stay.
#[tokio::test]
async fn fr_w5b_after_the_first_load_the_cookie_alone_works() {
    let ose = FakeOse::default();
    let gw = start(&ose).await;
    let (addr, token) = (gw.addr(), gw.token().to_owned());

    let first = get(addr, &format!("/?tkn={token}"), &[]).await;
    assert_eq!(first.status, 302);
    let set = header(&first.head, "set-cookie").unwrap();
    let jar = set.split("; ").next().unwrap().to_owned();
    assert_eq!(jar, cookie(&token));

    for path in ["/", "/static/out/vs/workbench/workbench.web.main.js", "/manifest.json"] {
        let a = get(addr, path, &[("Cookie", &jar)]).await;
        assert_eq!(a.status, 200, "{path}: {}", a.head);
    }
    let mixed = get(addr, "/", &[("Cookie", &format!("a=1; {jar}; {OSE_TOKEN_COOKIE}=stale; b=2"))]).await;
    assert_eq!(mixed.status, 200, "{}", mixed.head);

    let heads = ose.heads();
    assert_eq!(heads.len(), 4);
    let last = heads.last().unwrap();
    assert_eq!(header(last, "cookie"), Some(format!("{OSE_TOKEN_COOKIE}={PLACEHOLDER}; a=1; b=2").as_str()), "{last}");
    assert!(!last.contains(&token) && !last.contains("stale"), "{last}");
}

/// (d) A WebSocket upgrade (the workbench's management and extension host connections) is one
/// stream for the life of the socket, and bytes round-trip both ways through it.
#[tokio::test]
async fn fr_w5b_a_websocket_upgrade_round_trips_bytes() {
    let ose = FakeOse::default();
    let gw = start(&ose).await;
    let (addr, token) = (gw.addr(), gw.token().to_owned());

    let mut upgrade = format!("ws://{addr}/?reconnectionToken=r1&reconnection=false&skipWebSocketFrames=false")
        .into_client_request()
        .unwrap();
    upgrade.headers_mut().insert("Cookie", cookie(&token).parse().unwrap());
    let tcp = TcpStream::connect(addr).await.unwrap();
    let (mut ws, resp) = tokio::time::timeout(WAIT, tokio_tungstenite::client_async(upgrade, tcp)).await.unwrap().unwrap();
    assert_eq!(resp.status().as_u16(), 101);

    let big: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
    for m in [Message::Binary(vec![1, 2, 3].into()), Message::Binary(big.into()), Message::text("auth")] {
        ws.send(m.clone()).await.unwrap();
        let back = tokio::time::timeout(WAIT, ws.next()).await.unwrap().unwrap().unwrap();
        assert_eq!(back, m);
    }
    ws.close(None).await.unwrap();
    assert_eq!(ose.dials(), 1, "one stream for the whole socket");

    let head = &ose.heads()[0];
    assert!(head.starts_with("GET /?reconnectionToken=r1&reconnection=false&skipWebSocketFrames=false HTTP/1.1\r\n"), "{head}");
    assert_eq!(header(head, "upgrade"), Some("websocket"));
    assert!(header(head, "connection").is_some_and(|v| v.to_ascii_lowercase().contains("upgrade")), "{head}");
    assert!(!head.contains(&token), "{head}");
}

/// (e) The gateway binds loopback only, and each launch has its own token.
#[tokio::test]
async fn fr_w5b_a_non_loopback_bind_is_refused() {
    for bind in ["0.0.0.0:0", "192.0.2.10:0", "[::]:0"] {
        let config = GatewayConfig { bind: bind.parse().unwrap(), ..GatewayConfig::default() };
        match Gateway::start(FakeOse::default(), config).await {
            Err(GatewayError::NotLoopback(a)) => assert_eq!(a, bind.parse::<SocketAddr>().unwrap()),
            Err(e) => panic!("{bind}: expected NotLoopback, got {e}"),
            Ok(gw) => panic!("{bind}: bound at {}", gw.addr()),
        }
    }
    let a = Gateway::start(FakeOse::default(), GatewayConfig::default()).await.unwrap();
    let b = Gateway::start(FakeOse::default(), GatewayConfig::default()).await.unwrap();
    assert_eq!(a.addr().ip(), std::net::Ipv4Addr::LOCALHOST);
    assert_ne!(a.addr().port(), 0);
    assert_eq!(a.token().len(), 64);
    assert!(a.token().bytes().all(|c| c.is_ascii_hexdigit()));
    assert_ne!(a.token(), b.token(), "a token per launch");
}

/// (f) One stream per request: the gateway asks the OSE for `Connection: close` (the browser asked
/// for keep-alive), so each request is the first bytes of its own relay stream, where the node
/// swaps the placeholder. A second request pipelined on the same connection is not forwarded.
#[tokio::test]
async fn fr_w5b_one_stream_per_request_with_connection_close() {
    let ose = FakeOse::default();
    let gw = start(&ose).await;
    let (addr, token) = (gw.addr(), gw.token().to_owned());
    let jar = cookie(&token);

    for i in 1..=3 {
        let a = get(addr, &format!("/static/{i}.js"), &[("Cookie", &jar)]).await;
        assert_eq!(a.status, 200, "{}", a.head);
        assert_eq!(ose.dials(), i, "one stream per request");
    }
    for head in ose.heads() {
        assert_eq!(header(&head, "connection"), Some("close"), "{head}");
        assert!(!head.to_ascii_lowercase().contains("keep-alive"), "{head}");
    }

    let one = format!("GET /a HTTP/1.1\r\nHost: {addr}\r\nCookie: {jar}\r\n\r\n");
    let text = send_raw(addr, &format!("{one}{}", one.replace("/a", "/b"))).await;
    assert_eq!(text.matches("HTTP/1.1 200").count(), 1, "{text}");
    assert_eq!(ose.dials(), 4, "the pipelined request opened no stream");
    assert!(ose.heads().iter().all(|h| !h.starts_with("GET /b ")), "the pipelined request never reached the OSE");
}
