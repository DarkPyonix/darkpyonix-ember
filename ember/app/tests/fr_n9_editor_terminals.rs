//! SPEC FR-N9 (#1050): the separate editor window's terminal panel and 실행 tasks over the
//! transport. The app's own code (`terminal_list::*_on`, `terminal_stream::start_on`,
//! `start_task_on`, `attach_existing_with` with a [`TermReach::Peer`]) against an in-process
//! server that serves the terminal relay on the in-memory transport, scoped by device (FR-N8):
//! an allowed device lists, creates, attaches to and kills the editor's terminals and runs a
//! task; a `chat_only` device gets the FR-N8 sentence on the list and on the shell, which does
//! not retry.

use std::sync::Arc;
use std::time::Duration;

use ember_app::device_link::DeviceLink;
use ember_app::terminal_list::{self, panel_error, TermAction};
use ember_app::terminal_stream::{self, TermLaunch, TermProgram, TermReach, TerminalHandle, TerminalStatus};
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

/// The server's API and terminal relay on `server_t`, scoped by device, as `main.rs` serves
/// the transport door.
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

fn shows(h: &TerminalHandle, text: &str) -> bool {
    h.snapshot().lines.iter().any(|l| l.contains(text))
}

fn failed_text(h: &TerminalHandle) -> Option<&'static str> {
    match view_from_state(&h.snapshot()) {
        TermView::Failed(t) => Some(t),
        _ => None,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn fr_n9_the_editor_terminals_and_tasks_work_over_the_transport() {
    let net = MemNetwork::new();
    let (server_t, device_t, narrow_t) = (net.transport(), net.transport(), net.transport());
    let devices = serve(&server_t);
    devices.add(device_t.peer_id(), "laptop").unwrap();
    devices.add(narrow_t.peer_id(), "tablet").unwrap();
    devices.set_scope(&narrow_t.peer_id(), Scope::ChatOnly).unwrap();
    let server = PeerAddr::new(server_t.peer_id());
    let api = DeviceLink::new(device_t.clone(), server.clone()).api();
    let reach = TermReach::select(api.base_url(), &api);
    assert!(reach.is_peer(), "{reach:?}");
    let (folder, n) = (home(), nonce());
    let project = format!("fr-n9-editor-{n}");

    // The tab strip: nothing yet, then one terminal made under the folder's key.
    let first = tokio::time::timeout(Duration::from_secs(30), terminal_list::list_on(&reach, "local", &project))
        .await
        .unwrap()
        .unwrap();
    assert!(first.is_empty(), "{first:?}");
    let key = terminal_list::tab_key(&project, &folder, 0);
    let info = terminal_list::create_on(&reach, "local", &folder, &project, key).await.unwrap();
    let listed = terminal_list::list_on(&reach, "local", &project).await.unwrap();
    assert!(listed.iter().any(|t| t.id == info.id), "{listed:?}");

    // The tab attaches and its shell answers.
    let tab = terminal_stream::attach_existing_with(reach.clone(), "local", &info.id, None);
    until("the tab attached", 30, || tab.snapshot().status == TerminalStatus::Connected).await;
    let line = format!("hi-{n}");
    let end = tokio::time::Instant::now() + Duration::from_secs(60);
    // A starting shell can drop early input (#345): type again every 3 s. The command holds
    // `hi-%s`, so only the output contains the line.
    while !shows(&tab, &line) {
        assert!(tokio::time::Instant::now() < end, "no {line:?}: {:?}", tab.snapshot().lines);
        tab.send_input(format!("printf 'hi-%s\\n' {n}\n").as_bytes());
        let round = tokio::time::Instant::now() + Duration::from_secs(3);
        while tokio::time::Instant::now() < round && !shows(&tab, &line) {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    // The keyed panel shell (`start_on`) attaches over the transport too.
    let shell = terminal_stream::start_on(reach.clone(), "local", &folder, &project, None);
    until("the panel shell attached", 30, || shell.snapshot().status == TerminalStatus::Connected).await;

    // A 실행 task runs its program in a new terminal over the transport.
    let task_line = format!("task-{n}");
    let launch = TermLaunch {
        program: TermProgram::Argv(vec!["/bin/sh".into(), "-c".into(), format!("printf 'task-%s\\n' {n}; sleep 5")]),
        cwd: folder.clone(),
        env: Default::default(),
        title: "fr-n9 task".into(),
    };
    let task = terminal_stream::start_task_on(reach.clone(), "local", &project, launch, None);
    until("the task printed", 30, || shows(&task, &task_line)).await;

    // The trash button: the terminal is killed and no longer listed as running.
    terminal_list::kill_on(&reach, "local", &info.id).await.unwrap();
    let mut gone = false;
    for _ in 0..100 {
        let now = terminal_list::list_on(&reach, "local", &project).await.unwrap();
        if !now.iter().any(|t| t.id == info.id) {
            gone = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(gone, "the killed terminal is still listed");

    // A device whose scope has no terminals: the FR-N8 sentence on the list, and the panel
    // shell ends in it without retrying.
    let narrow = DeviceLink::new(narrow_t.clone(), server.clone()).api();
    let narrow_reach = TermReach::select(narrow.base_url(), &narrow);
    let err = terminal_list::list_on(&narrow_reach, "local", &project).await.unwrap_err();
    assert_eq!(panel_error(TermAction::List, &err), SCOPE_DENIED);
    let denied = terminal_stream::start_on(narrow_reach, "local", &folder, &project, None);
    until("the narrow shell refused", 30, || failed_text(&denied).is_some()).await;
    assert_eq!(failed_text(&denied), Some(SCOPE_DENIED));
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(denied.snapshot().reconnect.is_none(), "a refusal is not retried");
    assert_eq!(failed_text(&denied), Some(SCOPE_DENIED));
    drop((tab, shell, task));
}

/// Without the app's services (or with a server URL) the editor's reach is HTTP to that URL.
#[test]
fn fr_n9_the_editor_reach_without_a_peer_is_the_url() {
    assert!(matches!(TermReach::for_base("http://127.0.0.1:8740"), TermReach::Http(u) if u == "http://127.0.0.1:8740"));
}
