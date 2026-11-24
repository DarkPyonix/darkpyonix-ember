//! SPEC FR-W5b (#1064): the loopback gateway against a fake relay WebSocket server.
//!
//! The fake relay plays the server's route (`/api/computers/{id}/ide/relay/{launch_id}`): it
//! records the path and `Authorization` of each upgrade, and echoes every binary message back, so
//! what a TCP client writes to the gateway comes back to it byte for byte.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use ember_gateway::{Gateway, RelayTarget, COOKIE_NAME};
use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::Message;

const RELAY: &str = "/api/computers/node-a/ide/relay/launch-1";

#[derive(Default, Clone)]
struct Seen {
    /// One entry per relay stream: (path, Authorization).
    upgrades: Arc<Mutex<Vec<(String, Option<String>)>>>,
    /// The bytes each stream received, in order.
    bytes: Arc<Mutex<Vec<Vec<u8>>>>,
}

async fn fake_relay() -> (String, Seen) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let seen = Seen::default();
    let s = seen.clone();
    tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = listener.accept().await else { return };
            let s = s.clone();
            tokio::spawn(async move {
                let up = s.upgrades.clone();
                let callback = move |req: &Request, resp: Response| -> Result<Response, ErrorResponse> {
                    let auth = req.headers().get("authorization").and_then(|v| v.to_str().ok()).map(str::to_owned);
                    up.lock().unwrap().push((req.uri().path().to_owned(), auth));
                    Ok(resp)
                };
                let Ok(mut ws) = tokio_tungstenite::accept_hdr_async(tcp, callback).await else { return };
                let index = {
                    let mut b = s.bytes.lock().unwrap();
                    b.push(Vec::new());
                    b.len() - 1
                };
                while let Some(Ok(msg)) = ws.next().await {
                    match msg {
                        Message::Binary(data) => {
                            s.bytes.lock().unwrap()[index].extend_from_slice(&data);
                            if ws.send(Message::Binary(data)).await.is_err() {
                                return;
                            }
                        }
                        Message::Close(_) => return,
                        _ => {}
                    }
                }
            });
        }
    });
    (base, seen)
}

async fn gateway(base: &str) -> Gateway {
    Gateway::start(RelayTarget { server_url: base.into(), relay_path: RELAY.into(), token: Some("srv-token".into()) })
        .await
        .unwrap()
}

async fn read_all(tcp: &mut TcpStream) -> String {
    let mut out = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), tcp.read_to_end(&mut out)).await.expect("the gateway closes").unwrap();
    String::from_utf8_lossy(&out).into_owned()
}

#[tokio::test]
async fn fr_w5b_a_request_without_the_token_is_403_and_opens_no_relay() {
    let (base, seen) = fake_relay().await;
    let gw = gateway(&base).await;
    let heads = [
        "GET /?folder=%2Fw HTTP/1.1\r\nHost: x\r\n\r\n".to_owned(),
        "GET /?folder=%2Fw&egw=wrong HTTP/1.1\r\nHost: x\r\n\r\n".to_owned(),
        format!("GET / HTTP/1.1\r\nHost: x\r\nCookie: {COOKIE_NAME}=wrong\r\n\r\n"),
    ];
    for head in heads {
        let mut tcp = TcpStream::connect(gw.addr()).await.unwrap();
        tcp.write_all(head.as_bytes()).await.unwrap();
        let answer = read_all(&mut tcp).await;
        assert!(answer.starts_with("HTTP/1.1 403 "), "{head:?} -> {answer:?}");
        assert!(!answer.contains("Set-Cookie"));
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(seen.upgrades.lock().unwrap().is_empty(), "a refused request opens no relay stream");
}

#[tokio::test]
async fn fr_w5b_the_query_token_is_answered_with_a_302_and_the_cookie() {
    let (base, seen) = fake_relay().await;
    let gw = gateway(&base).await;
    let mut tcp = TcpStream::connect(gw.addr()).await.unwrap();
    let head = format!("GET /?folder=%2Fwork%2Frepo&egw={} HTTP/1.1\r\nHost: {}\r\n\r\n", gw.token(), gw.addr());
    tcp.write_all(head.as_bytes()).await.unwrap();
    let answer = read_all(&mut tcp).await;
    assert!(answer.starts_with("HTTP/1.1 302 "), "{answer}");
    assert!(answer.contains("\r\nLocation: /?folder=%2Fwork%2Frepo\r\n"), "{answer}");
    assert!(
        answer.contains(&format!("\r\nSet-Cookie: ember-gw={}; HttpOnly; SameSite=Strict; Path=/\r\n", gw.token())),
        "{answer}"
    );
    assert!(seen.upgrades.lock().unwrap().is_empty(), "the redirect is the gateway's own answer");
    assert!(gw.page_url("/work/repo").ends_with(&format!("/?folder=%2Fwork%2Frepo&egw={}", gw.token())));
}

#[tokio::test]
async fn fr_w5b_with_the_cookie_head_and_body_reach_the_relay_and_the_answer_comes_back() {
    let (base, seen) = fake_relay().await;
    let gw = gateway(&base).await;
    let mut tcp = TcpStream::connect(gw.addr()).await.unwrap();
    let request = format!(
        "POST /upload HTTP/1.1\r\nHost: {}\r\nCookie: theme=dark; {COOKIE_NAME}={}\r\nContent-Length: 11\r\n\r\nhello world",
        gw.addr(),
        gw.token()
    );
    tcp.write_all(request.as_bytes()).await.unwrap();
    let mut back = vec![0u8; request.len()];
    tokio::time::timeout(Duration::from_secs(5), tcp.read_exact(&mut back)).await.expect("the echo comes back").unwrap();
    assert_eq!(back, request.as_bytes(), "the relay's answer reaches the TCP client unchanged");
    assert_eq!(seen.bytes.lock().unwrap()[0], request.as_bytes(), "head and body reach the relay unchanged");
    let upgrades = seen.upgrades.lock().unwrap().clone();
    assert_eq!(upgrades, vec![(RELAY.to_owned(), Some("Bearer srv-token".to_owned()))]);
}

/// SPEC FR-W5b, FR-W4c (1c), INTENT D55: the gateway rewrites nothing in a joined connection,
/// so the manager's ticket cookie reaches the relay and the manager's `Set-Cookie` (here, bytes
/// the fake relay echoes) reaches the window unchanged.
#[tokio::test]
async fn fr_w5b_the_managers_cookies_pass_through_unchanged() {
    let (base, seen) = fake_relay().await;
    let gw = gateway(&base).await;
    let mut tcp = TcpStream::connect(gw.addr()).await.unwrap();
    let body = "HTTP/1.1 302 Found\r\nLocation: /ide/ose/?folder=%2Fw\r\nSet-Cookie: dpx-editor=tk; HttpOnly; Path=/ide/ose/\r\n\r\n";
    let request = format!(
        "POST /ide/ose/?folder=%2Fw&dpx-ticket=tk HTTP/1.1\r\nHost: {}\r\nCookie: dpx-editor=tk; {COOKIE_NAME}={}\r\nContent-Length: {}\r\n\r\n{body}",
        gw.addr(),
        gw.token(),
        body.len()
    );
    tcp.write_all(request.as_bytes()).await.unwrap();
    let mut back = vec![0u8; request.len()];
    tokio::time::timeout(Duration::from_secs(5), tcp.read_exact(&mut back)).await.expect("the echo comes back").unwrap();
    assert_eq!(String::from_utf8_lossy(&back), request, "Set-Cookie and the ticket come back as they went");
    assert_eq!(seen.bytes.lock().unwrap()[0], request.as_bytes(), "the manager gets the ticket and its cookie unchanged");
}

#[tokio::test]
async fn fr_w5b_one_tcp_connection_is_one_relay_stream() {
    let (base, seen) = fake_relay().await;
    let gw = gateway(&base).await;
    let head = |path: &str| format!("GET {path} HTTP/1.1\r\nHost: x\r\nCookie: {COOKIE_NAME}={}\r\n\r\n", gw.token());

    // Two requests on one keep-alive connection share its one stream.
    let mut a = TcpStream::connect(gw.addr()).await.unwrap();
    let first = head("/one");
    a.write_all(first.as_bytes()).await.unwrap();
    let mut buf = vec![0u8; first.len()];
    a.read_exact(&mut buf).await.unwrap();
    let second = head("/two");
    a.write_all(second.as_bytes()).await.unwrap();
    let mut buf = vec![0u8; second.len()];
    a.read_exact(&mut buf).await.unwrap();
    assert_eq!(seen.upgrades.lock().unwrap().len(), 1);
    assert_eq!(seen.bytes.lock().unwrap()[0], format!("{first}{second}").into_bytes());

    // A second connection opens a second stream.
    let mut b = TcpStream::connect(gw.addr()).await.unwrap();
    let third = head("/three");
    b.write_all(third.as_bytes()).await.unwrap();
    let mut buf = vec![0u8; third.len()];
    b.read_exact(&mut buf).await.unwrap();
    assert_eq!(seen.upgrades.lock().unwrap().len(), 2);
    assert_eq!(seen.bytes.lock().unwrap()[1], third.into_bytes());
}

#[tokio::test]
async fn fr_w5b_a_head_over_the_limit_is_refused() {
    let (base, seen) = fake_relay().await;
    let gw = gateway(&base).await;
    let mut tcp = TcpStream::connect(gw.addr()).await.unwrap();
    // Exactly one byte over the limit and no blank line, so the gateway has read every byte
    // when it answers (no reset from unread data).
    let mut big = b"GET / HTTP/1.1\r\nX-Pad: ".to_vec();
    big.resize(ember_gateway::MAX_HEAD + 1, b'a');
    tcp.write_all(&big).await.unwrap();
    let answer = read_all(&mut tcp).await;
    assert!(answer.starts_with("HTTP/1.1 431 "), "{answer}");
    assert!(seen.upgrades.lock().unwrap().is_empty());
}
