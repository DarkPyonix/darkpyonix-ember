//! SPEC FR-N9.1 (#879): the editor window's byte stream to the OSE through the server's relay
//! route (`GET /api/computers/{computer}/ide/relay/{launch_id}`, `FR-W4`). The app's own dial
//! (`ember_app::editor_reach`) against an in-process server whose OSE launch is a fake OSE that
//! records what it receives and echoes it: over the transport (an allowed device's
//! [`TermReach::Peer`]) and over plain HTTP with the server token. The launch hands out a
//! placeholder token; the relay replaces it with the real one only inside one client WebSocket
//! message, so each write of the stream must go out as one binary message.

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
use ember_server::ose::OseManager;
use ember_server::session::Sessions;
use ember_server::store::Store;
use ember_transport::mem::MemNetwork;
use ember_transport::{PeerAddr, Transport};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// The real OSE token: 64 characters, as `OseManager` makes them, so it has the placeholder's length.
const REAL_TOKEN: &str = "1111111111111111222222222222222233333333333333334444444444444444";

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

/// An OSE launch on the local computer pointing at a fresh fake OSE: the IDE router, the launch's
/// relay path and placeholder, and the fake's log.
struct Launch {
    app: axum::Router,
    relay: String,
    placeholder: String,
    log: Arc<Mutex<Vec<u8>>>,
    store: Arc<Store>,
    dir: tempfile::TempDir,
}

async fn launch() -> Launch {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open_in_memory().unwrap());
    let ose = Arc::new(OseManager::with_data_root(None, dir.path().join("ose-data")));
    let (ose_addr, log) = start_recording_ose().await;
    let conn = ose.register_launch_for_test("local", dir.path(), ose_addr, REAL_TOKEN);
    let relay = conn.relay.clone().expect("a launch has a relay path");
    assert_ne!(conn.connection_token, REAL_TOKEN, "the launch carries a placeholder");
    let sessions = || Sessions::new(store.clone(), vec![Arc::new(ScriptedAdapter) as Arc<dyn AgentAdapter>]);
    let app = ember_server::api::router(sessions()).merge(ide::router_with_ose(
        sessions(),
        Arc::new(IdeConfig::default()),
        ose,
    ));
    Launch { app, relay, placeholder: conn.connection_token, log, store, dir }
}

/// What the editor's first socket writes: an upgrade-like line, then an `auth`-like message,
/// each holding the placeholder, each one write.
fn first_writes(placeholder: &str) -> (Vec<u8>, Vec<u8>) {
    (
        format!("GET /?reconnectionToken=r&tkn={placeholder} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").into_bytes(),
        format!("{{\"type\":\"auth\",\"auth\":\"{placeholder}\",\"data\":\"d\"}}").into_bytes(),
    )
}

/// Writes the two buffers (each one `write_all`, then a flush), reads the echo of both back and
/// checks the fake OSE saw the real token in place of the placeholder, never the placeholder.
async fn round_trip(stream: &mut RelayStream, l: &Launch) {
    let (a, b) = first_writes(&l.placeholder);
    stream.write_all(&a).await.unwrap();
    stream.flush().await.unwrap();
    stream.write_all(&b).await.unwrap();
    stream.flush().await.unwrap();

    let swap = |v: &[u8]| String::from_utf8(v.to_vec()).unwrap().replace(&l.placeholder, REAL_TOKEN).into_bytes();
    let mut expected = swap(&a);
    expected.extend_from_slice(&swap(&b));
    let mut echo = vec![0u8; expected.len()];
    tokio::time::timeout(WAIT, stream.read_exact(&mut echo)).await.expect("the echo came back in time").unwrap();
    assert_eq!(String::from_utf8_lossy(&echo), String::from_utf8_lossy(&expected));

    let seen = String::from_utf8(l.log.lock().unwrap().clone()).unwrap();
    assert_eq!(seen.matches(REAL_TOKEN).count(), 2, "both writes reached the OSE with the real token: {seen}");
    assert!(!seen.contains(&l.placeholder), "the placeholder never reaches the OSE: {seen}");
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
