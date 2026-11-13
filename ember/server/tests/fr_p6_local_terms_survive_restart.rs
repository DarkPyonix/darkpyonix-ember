//! FR-P6 (#26): the local computer's terminals survive an ember server restart. The first
//! server's local node starts each terminal with a PTY keeper (the `ember-server` binary itself,
//! `__keep-pty`) and a record in its state dir; a second server on the same state dir re-adopts
//! the live session, lists it again with its origin, restores its screen from the graceful stop,
//! accepts input, and kills it. Written test first; not compiled by the author.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ember_node::proto::*;
use ember_server::computers::local_node::LocalNodeOptions;
use ember_server::computers::{self, Computers, Registry};
use ember_server::store::Store;
use futures::{SinkExt, StreamExt};
use serde_json::json;
use tokio_tungstenite::tungstenite::Message;

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

fn computers(state: &std::path::Path) -> Arc<Computers> {
    let c = Computers::new(Registry::new(Arc::new(Store::open_in_memory().unwrap())), computers::default_connector());
    c.set_local_node_options(LocalNodeOptions {
        state_dir: Some(state.to_path_buf()),
        keeper: Some(PathBuf::from(env!("CARGO_BIN_EXE_ember-server"))),
    });
    c
}

async fn serve(c: Arc<Computers>) -> (String, tokio::task::JoinHandle<()>) {
    let app = computers::terms::router(c);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = listener.local_addr().unwrap().to_string();
    (base, tokio::spawn(async move { axum::serve(listener, app).await.unwrap() }))
}

async fn within<T>(f: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(90), f).await.expect("timed out")
}

async fn attach(base: &str, tid: &str) -> Ws {
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{base}/api/computers/local/terms/{tid}/attach")).await.unwrap();
    let hello = TermHello {
        device: "laptop".into(),
        kind: Some("test".into()),
        pid: None,
        size: None,
        active: true,
        read_only: false,
        snapshot: true,
    };
    ws.send(Message::text(serde_json::to_string(&hello).unwrap())).await.unwrap();
    ws
}

/// Type `line` every 3 s until `needle` shows up in the output (a starting shell can drop input).
async fn type_until(ws: &mut Ws, line: &str, needle: &str) {
    let mut out = String::new();
    loop {
        let input = TermInput::Input { data: line.as_bytes().to_vec() };
        ws.send(Message::text(serde_json::to_string(&input).unwrap())).await.unwrap();
        let round = tokio::time::Instant::now() + Duration::from_secs(3);
        while !out.contains(needle) {
            let ev = match tokio::time::timeout_at(round, ws.next()).await {
                Err(_) => break,
                Ok(Some(Ok(Message::Text(t)))) => serde_json::from_str::<TermEvent>(t.as_str()).unwrap(),
                Ok(Some(Ok(_))) => continue,
                Ok(_) => panic!("socket ended waiting for {needle:?}; output {out:?}"),
            };
            match ev {
                TermEvent::Output { data } | TermEvent::Snapshot { data, .. } => out.push_str(&String::from_utf8_lossy(&data)),
                TermEvent::Exit { .. } | TermEvent::Error { .. } => panic!("ended waiting for {needle:?}; output {out:?}"),
                _ => {}
            }
        }
        if out.contains(needle) {
            return;
        }
    }
}

#[tokio::test]
async fn fr_p6_local_terminal_is_listed_and_attachable_after_a_server_restart() {
    let state = tempfile::tempdir().unwrap();
    within(async {
        let home = std::env::var("HOME").expect("HOME is set");
        let http = |base: &str, p: &str| format!("http://{base}/api/computers/local/terms{p}");
        let c = reqwest::Client::new();

        // First server.
        let first = computers(state.path());
        let (base1, task1) = serve(first.clone()).await;
        let body = json!({
            "program": { "argv": ["/bin/sh"] }, "cwd": home, "env": { "PS1": "$ " },
            "size": { "rows": 24, "cols": 80 }, "origin": "ide-ember", "project": home, "key": "p6-restart",
        });
        let r = c.post(http(&base1, "")).json(&body).send().await.unwrap();
        assert_eq!(r.status(), 201, "{}", r.text().await.unwrap());
        let created: TermCreateResponse = r.json().await.unwrap();
        let tid = created.term.id.clone();
        assert!(created.term.survives_node_restart, "the local node gives every terminal a keeper");
        let mut ws = attach(&base1, &tid).await;
        type_until(&mut ws, "X=42; echo before-$X\n", "before-42").await;
        drop(ws);

        // Graceful stop of the first server: screens are saved, the listener goes away.
        first.shutdown().await;
        assert!(state.path().join("terms").join(format!("{tid}.snap")).exists(), "the screen is saved");
        task1.abort();

        // Second server on the same state dir: it resumes at start and lists the session again.
        let second = computers(state.path());
        assert!(second.resume_local_node().unwrap(), "records of the previous run start the local node");
        let (base2, task2) = serve(second.clone()).await;
        let list: Vec<TermInfo> = c.get(http(&base2, "?running=true")).send().await.unwrap().json().await.unwrap();
        let t = list.iter().find(|t| t.id == tid).expect("the terminal is listed after the restart");
        assert_eq!(t.state, TermState::Running);
        assert!(t.adopted, "re-adopted from its keeper");
        assert_eq!(t.origin, TermOrigin::IdeEmber, "the origin carries over");

        // Scrollback and screen from before the restart.
        let snap: TermSnapshot = c.get(http(&base2, &format!("/{tid}/snapshot"))).send().await.unwrap().json().await.unwrap();
        assert!(snap.text.contains("before-42"), "{}", snap.text);

        // The same shell takes input through the new server (its variable is still set). The
        // first server's node still runs in this test process and reads the same PTY, so the
        // answer is checked in a file, not in this socket's output.
        let file = state.path().join("adopted.txt");
        let mut ws = attach(&base2, &tid).await;
        let line = format!("echo after-$X > {}\n", file.display());
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            let input = TermInput::Input { data: line.as_bytes().to_vec() };
            ws.send(Message::text(serde_json::to_string(&input).unwrap())).await.unwrap();
            tokio::time::sleep(Duration::from_millis(500)).await;
            if std::fs::read_to_string(&file).map(|s| s.trim() == "after-42").unwrap_or(false) {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "the adopted shell never wrote {}", file.display());
        }
        drop(ws);

        // Kill from the list, then the record leaves.
        assert_eq!(c.post(http(&base2, &format!("/{tid}/kill"))).json(&json!({})).send().await.unwrap().status(), 204);
        let mut removed = false;
        for _ in 0..200 {
            if c.delete(http(&base2, &format!("/{tid}"))).send().await.unwrap().status() == 204 {
                removed = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(removed, "the killed session is removed");
        task2.abort();
    })
    .await;
}

#[test]
fn fr_p6_local_node_options_use_their_own_state_dir_and_honour_keep_pty_off() {
    let data = std::path::Path::new("/srv/ember");
    let exe = Some(PathBuf::from("/usr/bin/ember-server"));
    let o = LocalNodeOptions::choose(data, None, None, exe.clone());
    assert_eq!(o.state_dir.as_deref(), Some(std::path::Path::new("/srv/ember/local-node")));
    assert_eq!(o.keeper, exe);
    let o = LocalNodeOptions::choose(data, Some(PathBuf::from("/x/state")), Some("1"), exe.clone());
    assert_eq!(o.state_dir.as_deref(), Some(std::path::Path::new("/x/state")));
    for off in ["0", "false", "no"] {
        assert_eq!(LocalNodeOptions::choose(data, None, Some(off), exe.clone()), LocalNodeOptions::default());
    }
}

#[test]
fn fr_p6_has_records_only_with_term_json_files() {
    let d = tempfile::tempdir().unwrap();
    let o = LocalNodeOptions { state_dir: Some(d.path().to_path_buf()), keeper: None };
    assert!(!o.has_records());
    std::fs::create_dir_all(d.path().join("terms")).unwrap();
    std::fs::write(d.path().join("terms").join("a.snap"), b"x").unwrap();
    assert!(!o.has_records());
    std::fs::write(d.path().join("terms").join("a.json"), b"{}").unwrap();
    assert!(o.has_records());
    assert!(!LocalNodeOptions::default().has_records());
}
