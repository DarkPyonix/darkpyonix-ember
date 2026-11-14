//! SPEC FR-N9 (#867): a session pane's terminal over the transport. The app's own terminal code
//! (`ember_app::terminal_stream` with a [`TermReach::Peer`]) against an in-process server that
//! serves the terminal relay on the in-memory transport, scoped by device (`FR-N8`): the pane
//! (`start_view_with`) and a writer (`attach_existing_with`) of an allowed device attach to a
//! shell on the server's local node, the writer's `echo hi-<nonce>` comes back on the pane, a
//! `chat_only` device gets the FR-N8 sentence, and the revoked device's pane ends with the
//! device sentence. Written test first; not compiled by the author (sub-agents do not build).

use std::sync::Arc;
use std::time::Duration;

use ember_app::device_link::{DeviceLink, NOT_ALLOWED, UNREACHABLE};
use ember_app::terminal_stream::{self, TermReach, TerminalHandle, TerminalStatus};
use ember_app::ui::device_scope::SCOPE_DENIED;
use ember_app::ui::term_pane::{view_from_state, TermView};
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::agents::AgentAdapter;
use ember_server::computers::{self, Computers, Registry};
use ember_server::devices::{Devices, Scope};
use ember_server::session::Sessions;
use ember_server::store::Store;
use ember_transport::mem::MemNetwork;
use ember_transport::{PeerAddr, Transport};

/// The server's API and terminal relay on `server_t`, each call checked against the caller's
/// scope, as `main.rs` serves the transport door.
fn serve(server_t: &Transport) -> Arc<Devices> {
    let devices = Devices::open(Arc::new(Store::open_in_memory().unwrap())).unwrap();
    let adapters: Vec<Arc<dyn AgentAdapter>> = vec![Arc::new(ScriptedAdapter)];
    let sessions = Sessions::new(Arc::new(Store::open_in_memory().unwrap()), adapters);
    let c = Computers::new(Registry::new(Arc::new(Store::open_in_memory().unwrap())), computers::default_connector());
    let app = ember_server::api::router(sessions).merge(computers::terms::router(c));
    tokio::spawn(ember_server::transport::serve_scoped(server_t, app, devices.clone()).unwrap());
    devices
}

fn home() -> String {
    std::env::var("HOME").expect("HOME is set")
}

fn nonce() -> String {
    let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    format!("{:x}", n & 0xffff_ffff)
}

/// Polls `f` every 50 ms for up to `secs` seconds.
async fn until(what: &str, secs: u64, mut f: impl FnMut() -> bool) {
    let end = tokio::time::Instant::now() + Duration::from_secs(secs);
    while tokio::time::Instant::now() < end {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("timed out waiting for {what}");
}

fn shows_line(h: &TerminalHandle, line: &str) -> bool {
    h.snapshot().lines.iter().any(|l| l.trim() == line)
}

/// The sentence the pane shows for `h`, when it shows one for a failure.
fn failed_text(h: &TerminalHandle) -> Option<&'static str> {
    match view_from_state(&h.snapshot()) {
        TermView::Failed(t) => Some(t),
        _ => None,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn fr_n9_a_device_pane_attaches_to_a_terminal_over_the_transport() {
    let net = MemNetwork::new();
    let (server_t, device_t, narrow_t) = (net.transport(), net.transport(), net.transport());
    let devices = serve(&server_t);
    devices.add(device_t.peer_id(), "phone").unwrap();
    devices.add(narrow_t.peer_id(), "tablet").unwrap();
    devices.set_scope(&narrow_t.peer_id(), Scope::ChatOnly).unwrap();
    let server = PeerAddr::new(server_t.peer_id());

    let api = DeviceLink::new(device_t.clone(), server.clone()).api();
    // The app's reach: the `Api` over the transport, never the `peer:<id>` string as a URL.
    let reach = TermReach::select(api.base_url(), &api);
    assert!(matches!(reach, TermReach::Peer(_)), "{reach:?}");

    // An agent's shell on the server's own computer, made over the transport.
    let project = home();
    let n = nonce();
    let body = serde_json::json!({
        "program": { "argv": ["/bin/sh"] },
        "cwd": project,
        "env": { "PS1": "$ " },
        "size": { "rows": 24, "cols": 80 },
        "origin": "agent",
        "project": project,
        "key": format!("fr-n9-{n}"),
    });
    let (status, created) = tokio::time::timeout(
        Duration::from_secs(30),
        api.raw_request(reqwest::Method::POST, "/computers/local/terms", Some(body)),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(status, 201, "{}", String::from_utf8_lossy(&created));
    let created: serde_json::Value = serde_json::from_slice(&created).unwrap();
    let tid = created["term"]["id"].as_str().unwrap().to_string();

    // The session pane's path: list the project's agent terminal and watch it.
    let pane = terminal_stream::start_view_with(reach.clone(), "local", &project, None);
    until("the pane attached", 30, || pane.snapshot().status == TerminalStatus::Connected).await;
    assert_eq!(pane.pick(), None, "one agent terminal, picked");

    // A writer on the same terminal through the same reach types; the pane shows the output.
    let writer = terminal_stream::attach_existing_with(reach.clone(), "local", &tid, None);
    until("the writer attached", 30, || writer.snapshot().status == TerminalStatus::Connected).await;
    let line = format!("hi-{n}");
    let end = tokio::time::Instant::now() + Duration::from_secs(60);
    // A starting shell can drop early input (#345): type again every 3 s.
    while !shows_line(&pane, &line) {
        assert!(tokio::time::Instant::now() < end, "no {line:?} on the pane: {:?}", pane.snapshot().lines);
        writer.send_input(format!("echo {line}\n").as_bytes());
        let round = tokio::time::Instant::now() + Duration::from_secs(3);
        while tokio::time::Instant::now() < round && !shows_line(&pane, &line) {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
    assert!(shows_line(&writer, &line));

    // A device whose scope has no terminals gets the FR-N8 sentence, on the list and on the attach.
    let narrow = DeviceLink::new(narrow_t.clone(), server.clone()).api();
    let narrow_reach = TermReach::select(narrow.base_url(), &narrow);
    let denied_pane = terminal_stream::start_view_with(narrow_reach.clone(), "local", &project, None);
    until("the narrow pane refused", 30, || failed_text(&denied_pane).is_some()).await;
    assert_eq!(failed_text(&denied_pane), Some(SCOPE_DENIED));
    let denied_attach = terminal_stream::attach_existing_with(narrow_reach, "local", &tid, None);
    until("the narrow attach refused", 30, || failed_text(&denied_attach).is_some()).await;
    assert_eq!(failed_text(&denied_attach), Some(SCOPE_DENIED));

    // Revoked: the stream is cut and the pane says why, once, without reconnecting.
    assert!(devices.remove(&device_t.peer_id()).unwrap() >= 1);
    until("the revoked pane refused", 30, || failed_text(&pane).is_some()).await;
    let text = failed_text(&pane).unwrap();
    assert!(text == NOT_ALLOWED || text == UNREACHABLE, "{text}");
    assert!(pane.snapshot().reconnect.is_none(), "the pane does not retry");
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(failed_text(&pane), Some(text), "the refusal stays");

    // The pane over plain HTTP is the same entry with the URL (unchanged path); not attached here.
    assert!(matches!(
        TermReach::select("http://127.0.0.1:8740", &ember_client::api::Api::new("http://127.0.0.1:8740").unwrap()),
        TermReach::Http(_)
    ));
    drop((writer, denied_pane));
}
