//! SPEC FR-N9.1 (#879): the editor window's byte stream to the OSE through the server's relay
//! route (`GET /api/computers/{computer}/ide/relay/{launch_id}`, `FR-W4`). The app's own dial
//! (`ember_app::editor_reach`) against an in-process server whose OSE launch is a fake OSE that
//! records what it receives and echoes it: over the transport (an allowed device's
//! [`TermReach::Peer`]) and over plain HTTP with the server token. The editor server sits behind
//! the DarkPyonix manager with no connection token (`FR-W4c` (1), INTENT D55): the relay carries
//! the bytes unchanged, the upgrade goes below the manager's path with the ticket cookie, and the
//! `auth` message carries the "no token" value.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ember_app::device_link::{DeviceLink, NOT_ALLOWED, UNREACHABLE};
use ember_app::editor_reach::{self, RelayStream};
use ember_app::terminal_stream::TermReach;
use ember_app::ui::device_scope::SCOPE_DENIED;
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::agents::AgentAdapter;
use ember_server::api::ide::{self, IdeConfig};
use ember_server::auth::{self, ServerAuth};
use ember_server::devices::{Devices, Scope};
use ember_node::editor_manager::{EditorRelays, ManagerEndpoint, ManagerSource};
use ember_server::session::Sessions;
use ember_server::store::Store;
use ember_transport::mem::MemNetwork;
use ember_transport::{PeerAddr, Transport};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// The manager's master token the launch is registered with. It stays on the server: the editor
/// never sends it, so it must not appear in what reaches the manager.
const REAL_TOKEN: &str = "1111111111111111222222222222222233333333333333334444444444444444";

/// The manager's ticket the editor sends as its cookie.
const TICKET: &str = "tk-fr-n9";

const WAIT: Duration = Duration::from_secs(30);

/// A fake OSE: every connection's bytes are appended to the returned log and echoed back.
async fn start_recording_ose() -> (SocketAddr, Arc<Mutex<Vec<u8>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let log = Arc::new(Mutex::new(Vec::new()));
    let log_bg = log.clone();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = listener.accept().await {
            let log = log_bg.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 32 * 1024];
                loop {
                    match s.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            log.lock().unwrap().extend_from_slice(&buf[..n]);
                            if s.write_all(&buf[..n]).await.is_err() {
                                break;
                            }
                        }
                    }
                }
            });
        }
    });
    (addr, log)
}

/// A launch on the local computer whose manager is a fresh fake (it records and echoes): the IDE
/// router, the launch's relay path, and the fake's log.
struct Launch {
    app: axum::Router,
    relay: String,
    log: Arc<Mutex<Vec<u8>>>,
    store: Arc<Store>,
    dir: tempfile::TempDir,
}

async fn launch() -> Launch {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open_in_memory().unwrap());
    let editor = Arc::new(EditorRelays::new(ManagerSource::none()));
    let (manager_addr, log) = start_recording_ose().await;
    let launch_id = editor
        .register_launch_for_test(ManagerEndpoint { url: format!("http://{manager_addr}"), token: REAL_TOKEN.into() });
    let relay = format!("/api/computers/local/ide/relay/{launch_id}");
    let sessions = || Sessions::new(store.clone(), vec![Arc::new(ScriptedAdapter) as Arc<dyn AgentAdapter>]);
    let app = ember_server::api::router(sessions()).merge(ide::router_with_editor(
        sessions(),
        Arc::new(IdeConfig::default()),
        editor,
    ));
    Launch { app, relay, log, store, dir }
}

/// What the editor's first socket writes: its upgrade request below the manager's path with the
/// ticket cookie (as `ember-editor-conn` builds it), then an `auth` message with no token.
fn first_writes() -> (Vec<u8>, Vec<u8>) {
    let mut opts = ember_editor_conn::handshake::ConnectOptions::new("localhost");
    opts.path = "/ide/ose/".into();
    opts.ticket = ember_editor_conn::handshake::Ticket::new(TICKET);
    let upgrade = ember_editor_conn::handshake::upgrade_request(&opts, false);
    assert!(upgrade.starts_with("GET /ide/ose/?reconnectionToken="), "{upgrade}");
    let no_token = ember_editor_conn::handshake::NO_CONNECTION_TOKEN;
    (upgrade.into_bytes(), format!("{{\"type\":\"auth\",\"auth\":\"{no_token}\",\"data\":\"d\"}}").into_bytes())
}

/// Writes the two buffers (each one `write_all`, then a flush), reads the echo of both back and
/// checks the fake OSE saw exactly those bytes: the relay rewrites nothing, and no token appears.
async fn round_trip(stream: &mut RelayStream, l: &Launch) {
    let (a, b) = first_writes();
    stream.write_all(&a).await.unwrap();
    stream.flush().await.unwrap();
    stream.write_all(&b).await.unwrap();
    stream.flush().await.unwrap();

    let mut expected = a.clone();
    expected.extend_from_slice(&b);
    let mut echo = vec![0u8; expected.len()];
    tokio::time::timeout(WAIT, stream.read_exact(&mut echo)).await.expect("the echo came back in time").unwrap();
    assert_eq!(String::from_utf8_lossy(&echo), String::from_utf8_lossy(&expected));

    let seen = String::from_utf8(l.log.lock().unwrap().clone()).unwrap();
    assert_eq!(seen.as_bytes(), expected.as_slice(), "the bytes reach the OSE unchanged");
    assert!(seen.contains(&format!("\r\nCookie: dpx-editor={TICKET}\r\n")), "{seen}");
    assert!(!seen.contains(REAL_TOKEN), "no connection token is ever sent: {seen}");
}

/// Shutting the stream down closes the relay; reading then ends without an error.
async fn ends_cleanly(mut stream: RelayStream) {
    tokio::time::timeout(WAIT, stream.shutdown()).await.expect("shutdown in time").unwrap();
    let mut rest = Vec::new();
    let r = tokio::time::timeout(WAIT, stream.read_to_end(&mut rest)).await.expect("the stream ended in time");
    assert!(r.is_ok(), "the end of the relay is EOF, not an error: {r:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn fr_n9_a_device_editor_stream_reaches_the_ose_through_the_relay_over_the_transport() {
    let l = launch().await;
    let net = MemNetwork::new();
    let (server_t, device_t, stranger_t, narrow_t): (Transport, Transport, Transport, Transport) =
        (net.transport(), net.transport(), net.transport(), net.transport());
    let devices = Devices::open(l.store.clone()).unwrap();
    tokio::spawn(ember_server::transport::serve_scoped(&server_t, l.app.clone(), devices.clone()).unwrap());
    devices.add(device_t.peer_id(), "phone").unwrap();
    devices.add(narrow_t.peer_id(), "tablet").unwrap();
    devices.set_scope(&narrow_t.peer_id(), Scope::ChatOnly).unwrap();
    let server = PeerAddr::new(server_t.peer_id());

    // The app's reach for a device: the `Api` over the transport.
    let api = DeviceLink::new(device_t.clone(), server.clone()).api();
    let reach = TermReach::select(api.base_url(), &api);
    assert!(reach.is_peer(), "{reach:?}");

    let mut stream = tokio::time::timeout(WAIT, editor_reach::dial(&reach, &l.relay)).await.unwrap().unwrap();
    round_trip(&mut stream, &l).await;
    ends_cleanly(stream).await;

    // `dial` is the closure `EditorSession::connect` calls three times: each call is a new relay.
    let again = tokio::time::timeout(WAIT, editor_reach::dial(&reach, &l.relay)).await.unwrap();
    assert!(again.is_ok(), "a second dial opens a second relay: {:?}", again.err());

    // A device that is not on the allow-list is refused with the device sentence.
    let stranger = DeviceLink::new(stranger_t.clone(), server.clone()).api();
    let refused = tokio::time::timeout(WAIT, editor_reach::dial(&TermReach::select(stranger.base_url(), &stranger), &l.relay))
        .await
        .unwrap()
        .err()
        .expect("an unregistered device is refused");
    let text = refused.to_string();
    assert!(text == NOT_ALLOWED || text == UNREACHABLE, "{text}");

    // A device whose scope has no IDE gets the FR-N8 sentence.
    let narrow = DeviceLink::new(narrow_t.clone(), server.clone()).api();
    let denied = tokio::time::timeout(WAIT, editor_reach::dial(&TermReach::select(narrow.base_url(), &narrow), &l.relay))
        .await
        .unwrap()
        .err()
        .expect("a chat_only device is refused");
    assert_eq!(denied.to_string(), SCOPE_DENIED);
}

#[tokio::test(flavor = "multi_thread")]
async fn fr_n9_the_editor_stream_reaches_the_ose_through_the_relay_over_http_with_the_server_token() {
    let l = launch().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let token = auth::load_or_create_token(l.dir.path()).unwrap();
    let app = auth::protect(l.app.clone(), ServerAuth::new(token.clone(), addr, []));
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    let base = format!("http://{addr}");
    assert_eq!(editor_reach::relay_url(&base, &l.relay).unwrap(), format!("ws://{addr}{}", l.relay));

    // Without the token the protected listener refuses the upgrade.
    let refused = tokio::time::timeout(WAIT, editor_reach::dial_http(&base, &l.relay, None)).await.unwrap();
    assert!(refused.is_err(), "no token, no relay");
    assert!(l.log.lock().unwrap().is_empty(), "a refused upgrade dials nothing");

    let mut stream =
        tokio::time::timeout(WAIT, editor_reach::dial_http(&base, &l.relay, Some(&token))).await.unwrap().unwrap();
    round_trip(&mut stream, &l).await;
    ends_cleanly(stream).await;

    // A `peer:<id>` base without a transport is never dialed as a URL.
    let peer = tokio::time::timeout(WAIT, editor_reach::dial(&TermReach::Http("peer:abc".into()), &l.relay)).await.unwrap();
    assert_eq!(peer.err().map(|e| e.to_string()).as_deref(), Some(UNREACHABLE));
}

/// SPEC FR-L7, FR-W5b, INTENT D55 (#1064): the `ember-app --vscode-window` helper's gateway works
/// on a server reached over the transport (`peer:`). Its dial is the app's own reach
/// ([`editor_reach::ReachDial`] on [`TermReach::Peer`]), so a connection the window opens to the
/// gateway's loopback port, with the per-launch cookie, reaches the OSE through the relay with the
/// device key and no server token; the first-load URL answers 302 with the cookie, and a request
/// without the token is refused before any relay is opened.
#[tokio::test(flavor = "multi_thread")]
async fn fr_l7_vscode_window_works_on_a_peer_server() {
    let l = launch().await;
    let net = MemNetwork::new();
    let (server_t, device_t): (Transport, Transport) = (net.transport(), net.transport());
    let devices = Devices::open(l.store.clone()).unwrap();
    tokio::spawn(ember_server::transport::serve_scoped(&server_t, l.app.clone(), devices.clone()).unwrap());
    devices.add(device_t.peer_id(), "laptop").unwrap();
    let api = DeviceLink::new(device_t.clone(), PeerAddr::new(server_t.peer_id())).api();

    // The helper's reach for a `peer:` server is the app's `Api` over the transport.
    assert!(api.base_url().starts_with("peer:"), "{}", api.base_url());
    let reach = TermReach::select(api.base_url(), &api);
    assert!(reach.is_peer(), "{reach:?}");
    let gateway = ember_gateway::Gateway::start_with(editor_reach::ReachDial::new(reach, l.relay.clone())).await.unwrap();
    let token = gateway.token().to_owned();

    // No token: 403, and the OSE sees nothing.
    let mut tcp = tokio::net::TcpStream::connect(gateway.addr()).await.unwrap();
    tcp.write_all(b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").await.unwrap();
    let mut answer = Vec::new();
    tokio::time::timeout(WAIT, tcp.read_to_end(&mut answer)).await.unwrap().unwrap();
    assert!(answer.starts_with(b"HTTP/1.1 403"), "{}", String::from_utf8_lossy(&answer));
    assert!(l.log.lock().unwrap().is_empty());

    // The first-load URL under the manager's path: 302 with the cookie, keeping the ticket.
    let page = ember_gateway::page_url_at(gateway.addr(), &token, "/ide/ose/", &[("folder", "/work/repo"), ("dpx-ticket", TICKET)]);
    let path = page.strip_prefix(&gateway.origin()).unwrap().to_owned();
    let mut tcp = tokio::net::TcpStream::connect(gateway.addr()).await.unwrap();
    tcp.write_all(format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").as_bytes()).await.unwrap();
    let mut answer = Vec::new();
    tokio::time::timeout(WAIT, tcp.read_to_end(&mut answer)).await.unwrap().unwrap();
    let answer = String::from_utf8_lossy(&answer).into_owned();
    assert!(answer.starts_with("HTTP/1.1 302"), "{answer}");
    assert!(answer.contains(&ember_gateway::set_cookie(&token)), "{answer}");
    assert!(answer.contains(&format!("\r\nLocation: /ide/ose/?folder=%2Fwork%2Frepo&dpx-ticket={TICKET}\r\n")), "{answer}");

    // With the cookie the request goes through the relay to the manager (the fake echoes it),
    // the manager's ticket query included.
    let head = format!(
        "GET /ide/ose/?folder=%2Fwork%2Frepo&dpx-ticket={TICKET} HTTP/1.1\r\nHost: 127.0.0.1\r\nCookie: ember-gw={token}\r\n\r\n"
    );
    let mut tcp = tokio::net::TcpStream::connect(gateway.addr()).await.unwrap();
    tcp.write_all(head.as_bytes()).await.unwrap();
    let mut echo = vec![0u8; head.len()];
    tokio::time::timeout(WAIT, tcp.read_exact(&mut echo)).await.expect("the echo came back in time").unwrap();
    assert_eq!(String::from_utf8_lossy(&echo), head);
    let seen = String::from_utf8(l.log.lock().unwrap().clone()).unwrap();
    assert_eq!(seen, head, "the OSE got the request as the window sent it");
}
