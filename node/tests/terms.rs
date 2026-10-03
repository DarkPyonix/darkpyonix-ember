//! Persistent terminal sessions (SPEC §P): the daemon on an ephemeral port, driven through the
//! client. Covers create / attach / detach / re-attach with a snapshot (FR-P1, FR-P2), several
//! clients typing (FR-P3, FR-P4), take/release control, the resize policy, kill, a session
//! outliving every client, finished sessions, idempotent create, and re-adoption by a new node
//! through the PTY keeper (FR-P6).

use std::path::{Path, PathBuf};
use std::time::Duration;

use ember_node::api::{self, Node};
use ember_node::client::{NodeClient, TermAttachment};
use ember_node::config::NodeConfig;
use ember_node::proto::*;

const TOKEN: &str = "test-token";

struct Fixture {
    client: NodeClient,
    node: Node,
    root: PathBuf,
    state: PathBuf,
    _dir: tempfile::TempDir,
}

fn config(root: &Path, state: &Path) -> NodeConfig {
    NodeConfig {
        state_dir: Some(state.to_path_buf()),
        pty_keeper: Some(PathBuf::from(env!("CARGO_BIN_EXE_ember-node"))),
        ..NodeConfig::new(TOKEN, vec![root.to_path_buf()])
    }
}

async fn serve(cfg: NodeConfig) -> (NodeClient, Node) {
    let node = Node::new(cfg).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(api::serve(listener, node.clone()));
    (NodeClient::new(&base, TOKEN).unwrap(), node)
}

async fn start() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().canonicalize().unwrap();
    let root = base.join("root");
    // Short: keeper sockets live here and unix socket paths are limited to ~104 bytes.
    let state = base.join("s");
    std::fs::create_dir_all(&root).unwrap();
    let (client, node) = serve(config(&root, &state)).await;
    Fixture { client, node, root, state, _dir: dir }
}

async fn within<T>(fut: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(30), fut).await.expect("timed out")
}

fn req(root: &Path, argv: &[&str]) -> TermCreateRequest {
    let mut env = std::collections::BTreeMap::new();
    env.insert("PS1".to_string(), "$ ".to_string());
    TermCreateRequest {
        program: Some(Program::Argv(argv.iter().map(|s| s.to_string()).collect())),
        cwd: root.to_path_buf(),
        env,
        env_clear: false,
        size: Some(PtySize { rows: 24, cols: 80 }),
        origin: TermOrigin::IdeVscode,
        project: Some(root.display().to_string()),
        title: None,
        key: None,
        tags: Default::default(),
    }
}

fn hello(device: &str, size: Option<PtySize>, active: bool) -> TermHello {
    TermHello { device: device.into(), kind: Some("test".into()), pid: None, size, active, read_only: false, snapshot: true }
}

/// Append output (and snapshot) bytes to `out` until it contains `needle`; other events are
/// returned in `seen`.
async fn until(a: &mut TermAttachment, out: &mut String, needle: &str, seen: &mut Vec<TermEvent>) {
    while !out.contains(needle) {
        match a.recv().await.unwrap() {
            Some(TermEvent::Output { data }) | Some(TermEvent::Snapshot { data, .. }) => {
                out.push_str(&String::from_utf8_lossy(&data))
            }
            Some(TermEvent::Exit { code, signal }) => {
                panic!("exited ({code:?}, {signal:?}) waiting for {needle:?}; output {out:?}")
            }
            Some(ev) => seen.push(ev),
            None => panic!("closed waiting for {needle:?}; output {out:?}"),
        }
    }
}

/// The next event that is not output.
async fn next_control_event(a: &mut TermAttachment) -> TermEvent {
    loop {
        match a.recv().await.unwrap() {
            Some(TermEvent::Output { .. }) | Some(TermEvent::Clients { .. }) => continue,
            Some(ev) => return ev,
            None => panic!("closed"),
        }
    }
}

async fn wait_state(c: &NodeClient, id: &str, state: TermState) -> TermInfo {
    loop {
        let t = c.term(id).await.unwrap();
        if t.state == state {
            return t;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn create_attach_detach_and_reattach_with_snapshot() {
    let f = start().await;
    within(async {
        let created = f.client.term_create(&req(&f.root, &["/bin/sh"])).await.unwrap();
        assert!(created.created);
        let t = created.term;
        assert_eq!(t.state, TermState::Running);
        assert_eq!(t.origin, TermOrigin::IdeVscode);
        assert!(t.survives_node_restart, "a keeper should hold the PTY");

        let mut a = f.client.term_attach(&t.id, &hello("laptop", Some(PtySize { rows: 24, cols: 80 }), true)).await.unwrap();
        assert_eq!(a.term.clients.len(), 1);
        let (mut out, mut seen) = (String::new(), Vec::new());
        a.tx.input("echo hello-$((40+2))\n").await.unwrap();
        until(&mut a, &mut out, "hello-42", &mut seen).await;
        a.tx.detach().await.unwrap();

        // Nobody attached; still running.
        tokio::time::sleep(Duration::from_millis(300)).await;
        let t2 = f.client.term(&t.id).await.unwrap();
        assert_eq!(t2.state, TermState::Running);
        assert!(t2.clients.is_empty());

        // Re-attach from another device: the snapshot redraws what was there.
        let mut b = f.client.term_attach(&t.id, &hello("phone", None, false)).await.unwrap();
        match b.recv().await.unwrap() {
            Some(TermEvent::Snapshot { data, size }) => {
                let s = String::from_utf8_lossy(&data);
                assert!(s.starts_with("\x1bc"), "snapshot starts with a reset");
                assert!(s.contains("hello-42"), "{s:?}");
                assert_eq!(size, PtySize { rows: 24, cols: 80 });
            }
            other => panic!("expected snapshot first, got {other:?}"),
        }
        let (mut out, mut seen) = (String::new(), Vec::new());
        b.tx.input("echo again-$((1+1))\n").await.unwrap();
        until(&mut b, &mut out, "again-2", &mut seen).await;

        // The plain-text snapshot shows the same screen.
        let snap = f.client.term_snapshot(&t.id).await.unwrap();
        assert!(snap.text.contains("hello-42") && snap.text.contains("again-2"), "{}", snap.text);

        f.client.term_kill(&t.id, None).await.unwrap();
        wait_state(&f.client, &t.id, TermState::Exited).await;
    })
    .await;
}

#[tokio::test]
async fn session_outlives_every_client() {
    let f = start().await;
    within(async {
        let marker = f.root.join("survived");
        let cmd = format!("sleep 1; touch {}; echo done-marker; sleep 30", marker.display());
        let t = f.client.term_create(&req(&f.root, &["/bin/sh", "-c", &cmd])).await.unwrap().term;
        // Attach and drop without a clean detach (a window that just vanished).
        let a = f.client.term_attach(&t.id, &hello("laptop", None, true)).await.unwrap();
        drop(a);
        tokio::time::sleep(Duration::from_millis(1600)).await;
        assert!(marker.exists(), "the session's process did not outlive its clients");

        let mut b = f.client.term_attach(&t.id, &hello("laptop", None, true)).await.unwrap();
        let (mut out, mut seen) = (String::new(), Vec::new());
        until(&mut b, &mut out, "done-marker", &mut seen).await;
        f.client.term_kill(&t.id, None).await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn two_clients_type_and_both_see_everything_in_order() {
    let f = start().await;
    within(async {
        let t = f.client.term_create(&req(&f.root, &["cat"])).await.unwrap().term;
        let mut a = f.client.term_attach(&t.id, &hello("A", None, true)).await.unwrap();
        let mut b = f.client.term_attach(&t.id, &hello("B", None, false)).await.unwrap();
        let (mut out_a, mut out_b, mut seen) = (String::new(), String::new(), Vec::new());
        for i in 0..4 {
            a.tx.input(format!("from-a-{i}\n")).await.unwrap();
            until(&mut b, &mut out_b, &format!("from-a-{i}"), &mut seen).await;
            b.tx.input(format!("from-b-{i}\n")).await.unwrap();
            until(&mut a, &mut out_a, &format!("from-b-{i}"), &mut seen).await;
        }
        until(&mut a, &mut out_a, "from-a-3", &mut seen).await;
        until(&mut b, &mut out_b, "from-b-3", &mut seen).await;
        for out in [&out_a, &out_b] {
            let mut at = 0;
            for i in 0..4 {
                for who in ["a", "b"] {
                    let needle = format!("from-{who}-{i}");
                    let pos = out[at..].find(&needle).unwrap_or_else(|| panic!("{needle} missing or out of order in {out:?}"));
                    at += pos + needle.len();
                }
            }
        }
        f.client.term_kill(&t.id, None).await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn take_and_release_control() {
    let f = start().await;
    within(async {
        let t = f.client.term_create(&req(&f.root, &["cat"])).await.unwrap().term;
        let mut a = f.client.term_attach(&t.id, &hello("device-A", None, true)).await.unwrap();
        let mut b = f.client.term_attach(&t.id, &hello("device-B", None, false)).await.unwrap();
        let (a_id, b_id) = (a.client, b.client);

        a.send(TermInput::TakeControl).await.unwrap();
        for x in [&mut a, &mut b] {
            match next_control_event(x).await {
                TermEvent::Control { controller: Some(c) } => assert_eq!((c.client, c.device.as_str()), (a_id, "device-A")),
                other => panic!("expected control by A, got {other:?}"),
            }
        }
        assert_eq!(f.client.term(&t.id).await.unwrap().controller.map(|c| c.client), Some(a_id));

        // B is read-only now: refused, with who controls.
        b.tx.input("refused-text\n").await.unwrap();
        match next_control_event(&mut b).await {
            TermEvent::Refused { reason: TermRefusal::Controlled, controller: Some(c) } => assert_eq!(c.device, "device-A"),
            other => panic!("expected refusal, got {other:?}"),
        }
        a.tx.input("from-controller\n").await.unwrap();
        let (mut out, mut seen) = (String::new(), Vec::new());
        until(&mut a, &mut out, "from-controller", &mut seen).await;
        assert!(!out.contains("refused-text"));

        // Release over HTTP (as a UI would), then B types again.
        f.client.term_control(&t.id, a_id, false).await.unwrap();
        match next_control_event(&mut b).await {
            TermEvent::Control { controller: None } => {}
            other => panic!("expected release, got {other:?}"),
        }
        b.tx.input("accepted-again\n").await.unwrap();
        until(&mut a, &mut out, "accepted-again", &mut seen).await;

        // B takes control, then detaches: control is released for everyone.
        f.client.term_control(&t.id, b_id, true).await.unwrap();
        match next_control_event(&mut a).await {
            TermEvent::Control { controller: Some(c) } => assert_eq!(c.client, b_id),
            other => panic!("expected control by B, got {other:?}"),
        }
        b.tx.detach().await.unwrap();
        match next_control_event(&mut a).await {
            TermEvent::Control { controller: None } => {}
            other => panic!("expected release on detach, got {other:?}"),
        }
        assert!(f.client.term(&t.id).await.unwrap().controller.is_none());
        f.client.term_kill(&t.id, None).await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn size_follows_controller_else_most_recently_active_client() {
    let f = start().await;
    within(async {
        let small = PtySize { rows: 30, cols: 100 };
        let big = PtySize { rows: 40, cols: 120 };
        let t = f.client.term_create(&req(&f.root, &["/bin/sh"])).await.unwrap().term;
        assert_eq!(t.size, PtySize { rows: 24, cols: 80 });

        // An active attach takes the PTY to its size before the snapshot.
        let mut a = f.client.term_attach(&t.id, &hello("A", Some(small), true)).await.unwrap();
        assert_eq!(a.term.size, small);
        let (mut out, mut seen) = (String::new(), Vec::new());
        a.tx.input("stty size\n").await.unwrap();
        until(&mut a, &mut out, "30 100", &mut seen).await;

        // A passive attach (restoring a terminal list) does not.
        let mut b = f.client.term_attach(&t.id, &hello("B", Some(big), false)).await.unwrap();
        assert_eq!(b.term.size, small);
        // B types: it is now the most recently active client.
        b.tx.input("stty size\n").await.unwrap();
        let mut out_b = String::new();
        until(&mut b, &mut out_b, "40 120", &mut seen).await;
        assert_eq!(f.client.term(&t.id).await.unwrap().size, big);

        // A takes control: the size follows the controller, also when it resizes.
        a.send(TermInput::TakeControl).await.unwrap();
        loop {
            if f.client.term(&t.id).await.unwrap().size == small {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let narrow = PtySize { rows: 20, cols: 70 };
        a.send(TermInput::Resize { size: narrow }).await.unwrap();
        out.clear();
        a.tx.input("stty size\n").await.unwrap();
        until(&mut a, &mut out, "20 70", &mut seen).await;
        // B's own resize does not move the PTY while A controls it.
        b.send(TermInput::Resize { size: big }).await.unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(f.client.term(&t.id).await.unwrap().size, narrow);
        f.client.term_kill(&t.id, None).await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn kill_ends_the_session_and_finished_sessions_replay_their_output() {
    let f = start().await;
    within(async {
        let t = f.client.term_create(&req(&f.root, &["sleep", "30"])).await.unwrap().term;
        let mut a = f.client.term_attach(&t.id, &hello("A", None, true)).await.unwrap();
        f.client.term_kill(&t.id, None).await.unwrap();
        loop {
            match a.recv().await.unwrap() {
                Some(TermEvent::Exit { code, signal }) => {
                    assert_eq!((code, signal), (None, Some(1)), "SIGHUP first");
                    break;
                }
                Some(_) => continue,
                None => panic!("closed without exit"),
            }
        }
        assert_eq!(a.recv().await.unwrap(), None, "exit is the last event");
        let done = f.client.term(&t.id).await.unwrap();
        assert_eq!(done.state, TermState::Exited);
        assert!(done.finished_ms.is_some() && done.pid.is_none());

        // A task that finished while nobody watched: attach shows its output, then its exit.
        let t = f.client.term_create(&req(&f.root, &["/bin/sh", "-c", "echo built-ok; exit 3"])).await.unwrap().term;
        wait_state(&f.client, &t.id, TermState::Exited).await;
        let mut b = f.client.term_attach(&t.id, &hello("B", None, true)).await.unwrap();
        match b.recv().await.unwrap() {
            Some(TermEvent::Snapshot { data, .. }) => assert!(String::from_utf8_lossy(&data).contains("built-ok")),
            other => panic!("expected snapshot, got {other:?}"),
        }
        assert_eq!(b.recv().await.unwrap(), Some(TermEvent::Exit { code: Some(3), signal: None }));

        // Finished sessions can be forgotten; running ones cannot.
        let running = f.client.term_create(&req(&f.root, &["sleep", "30"])).await.unwrap().term;
        let e = f.client.term_remove(&running.id).await.unwrap_err();
        assert!(matches!(e, ember_node::client::ClientError::Api { status: 409, .. }), "{e:?}");
        f.client.term_remove(&t.id).await.unwrap();
        assert_eq!(f.client.term(&t.id).await.unwrap_err().code(), Some(ErrorCode::NotFound));
        f.client.term_kill(&running.id, Some(9)).await.unwrap();
        let k = wait_state(&f.client, &running.id, TermState::Exited).await;
        assert_eq!(k.signal, Some(9));
    })
    .await;
}

#[tokio::test]
async fn key_makes_create_idempotent_and_listing_filters() {
    let f = start().await;
    within(async {
        let mut r = req(&f.root, &["sleep", "30"]);
        r.key = Some("vscode:term-1".into());
        r.tags.insert("ember-term.pid".into(), "1234".into());
        let first = f.client.term_create(&r).await.unwrap();
        let second = f.client.term_create(&r).await.unwrap();
        assert!(first.created && !second.created);
        assert_eq!(first.term.id, second.term.id);
        assert_eq!(second.term.tags.get("ember-term.pid").map(String::as_str), Some("1234"));

        let mut other = req(&f.root, &["sleep", "30"]);
        other.origin = TermOrigin::Agent;
        other.project = Some("/elsewhere".into());
        let o = f.client.term_create(&other).await.unwrap().term;

        let project = f.root.display().to_string();
        let mine = f.client.terms(&TermListQuery { project: Some(project), ..Default::default() }).await.unwrap();
        assert_eq!(mine.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), vec![first.term.id.as_str()]);
        let agents = f.client.terms(&TermListQuery { origin: Some(TermOrigin::Agent), ..Default::default() }).await.unwrap();
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].id, o.id);

        // Outside the roots: refused like exec.
        let mut bad = req(&f.root, &["true"]);
        bad.cwd = PathBuf::from("/");
        assert_eq!(f.client.term_create(&bad).await.unwrap_err().code(), Some(ErrorCode::ForbiddenPath));

        for id in [first.term.id, o.id] {
            f.client.term_kill(&id, None).await.unwrap();
        }
        let running = f.client.terms(&TermListQuery { running: Some(true), ..Default::default() }).await;
        let _ = running;
    })
    .await;
}

#[tokio::test]
async fn a_new_node_readopts_running_sessions_and_reports_lost_ones() {
    let f = start().await;
    within(async {
        let t = f.client.term_create(&req(&f.root, &["/bin/sh"])).await.unwrap().term;
        let mut a = f.client.term_attach(&t.id, &hello("A", None, true)).await.unwrap();
        let (mut out, mut seen) = (String::new(), Vec::new());
        a.tx.input("X=42; echo before-$X\n").await.unwrap();
        until(&mut a, &mut out, "before-42", &mut seen).await;
        drop(a);

        // A graceful stop saves the screen next to the record.
        f.node.terms().shutdown();
        assert!(f.state.join("terms").join(format!("{}.snap", t.id)).exists());

        // A record of a previous run whose process is gone.
        let lost_id = "0123456789abcdef0123456789abcdef";
        let mut lost = serde_json::to_value(f.client.term(&t.id).await.unwrap()).unwrap();
        let o = lost.as_object_mut().unwrap();
        o.insert("id".into(), lost_id.into());
        o.insert("pid".into(), 999_999.into());
        o.insert("keeper_socket".into(), serde_json::Value::Null);
        std::fs::write(f.state.join("terms").join(format!("{lost_id}.json")), serde_json::to_vec(&lost).unwrap()).unwrap();

        // "Restart": a second node on the same state dir. (The first one still runs in this
        // process; only the second is used from here on.)
        let (c2, _node2) = serve(config(&f.root, &f.state)).await;
        let t2 = c2.term(&t.id).await.unwrap();
        assert_eq!(t2.state, TermState::Running);
        assert!(t2.adopted);
        let l = c2.term(lost_id).await.unwrap();
        assert_eq!(l.state, TermState::Lost);
        assert!(c2.term_attach(lost_id, &hello("A", None, true)).await.is_err());

        // The restored model has the scrollback from before the restart.
        let snap = c2.term_snapshot(&t.id).await.unwrap();
        assert!(String::from_utf8_lossy(&snap.data).contains("before-42"));

        // Input through the new node reaches the same shell (its variable is still set).
        let mut b = c2.term_attach(&t.id, &hello("B", None, true)).await.unwrap();
        let file = f.root.join("adopted.txt");
        b.tx.input(format!("echo $X-adopted > {}\n", file.display())).await.unwrap();
        loop {
            if std::fs::read_to_string(&file).map(|s| s.trim() == "42-adopted").unwrap_or(false) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        c2.term_kill(&t.id, Some(9)).await.unwrap();
        wait_state(&c2, &t.id, TermState::Exited).await;
    })
    .await;
}
