//! FR-W4 (#455): the local computer's terminal relay `/api/computers/local/terms` is served by an
//! in-process node on a loopback listener, started on first use, with a random in-memory token.
//! Written test first; not compiled by the author (sub-agents do not build).

use std::sync::Arc;
use std::time::Duration;

use ember_node::proto::*;
use ember_server::computers::{self, Computers, Registry};
use ember_server::store::Store;
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

fn computers() -> Arc<Computers> {
    Computers::new(Registry::new(Arc::new(Store::open_in_memory().unwrap())), computers::default_connector())
}

async fn serve(c: Arc<Computers>) -> (String, tokio::task::JoinHandle<()>) {
    let app = computers::terms::router(c);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = listener.local_addr().unwrap().to_string();
    (base, tokio::spawn(async move { axum::serve(listener, app).await.unwrap() }))
}

fn home() -> String {
    std::env::var("HOME").expect("HOME is set")
}

fn create_body(key: &str) -> Value {
    json!({
        "program": { "argv": ["/bin/sh"] },
        "cwd": home(),
        "env": { "PS1": "$ " },
        "size": { "rows": 24, "cols": 80 },
        "origin": "ide-ember",
        "project": home(),
        "key": key,
    })
}

async fn within<T>(f: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(60), f).await.expect("timed out")
}

async fn next_event(ws: &mut Ws) -> Option<TermEvent> {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            match ws.next().await? {
                Ok(Message::Text(t)) => return Some(serde_json::from_str(t.as_str()).unwrap()),
                Ok(Message::Close(_)) | Err(_) => return None,
                Ok(_) => continue,
            }
        }
    })
    .await
    .expect("terminal event within 20 s")
}

/// Type `line` every 3 s until `needle` shows up (a starting shell can drop early input, #345).
async fn type_until(ws: &mut Ws, line: &str, needle: &str) {
    let mut out = String::new();
    loop {
        let input = TermInput::Input { data: line.as_bytes().to_vec() };
        ws.send(Message::text(serde_json::to_string(&input).unwrap())).await.unwrap();
        let round = tokio::time::Instant::now() + Duration::from_secs(3);
        while !out.contains(needle) {
            match tokio::time::timeout_at(round, next_event(ws)).await {
                Err(_) => break,
                Ok(Some(TermEvent::Output { data })) | Ok(Some(TermEvent::Snapshot { data, .. })) => {
                    out.push_str(&String::from_utf8_lossy(&data))
                }
                Ok(Some(TermEvent::Exit { .. })) | Ok(Some(TermEvent::Error { .. })) | Ok(None) => {
                    panic!("ended waiting for {needle:?}; output {out:?}")
                }
                Ok(Some(_)) => {}
            }
        }
        if out.contains(needle) {
            return;
        }
    }
}

#[tokio::test]
async fn fr_w4_local_terms_create_list_attach_kill_and_delete() {
    within(async {
        let (base, task) = serve(computers()).await;
        let http = |p: &str| format!("http://{base}/api/computers/local/terms{p}");
        let c = reqwest::Client::new();

        let r = c.post(http("")).json(&create_body("lk1")).send().await.unwrap();
        assert_eq!(r.status(), 201, "{}", r.text().await.unwrap());
        let created: TermCreateResponse = c.post(http("")).json(&create_body("lk1")).send().await.unwrap().json().await.unwrap();
        assert!(!created.created, "the same key answers the running session");
        let tid = created.term.id;

        let list: Vec<TermInfo> = c.get(http("?origin=ide-ember&running=true")).send().await.unwrap().json().await.unwrap();
        assert!(list.iter().any(|t| t.id == tid));

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
        match next_event(&mut ws).await {
            Some(TermEvent::Attached { term, .. }) => assert_eq!(term.id, tid),
            other => panic!("expected attached, got {other:?}"),
        }
        type_until(&mut ws, "echo local-$((40+2))\n", "local-42").await;

        let snap: TermSnapshot = c.get(http(&format!("/{tid}/snapshot"))).send().await.unwrap().json().await.unwrap();
        assert!(snap.text.contains("local-42"), "{}", snap.text);
        let one: TermInfo = c.get(http(&format!("/{tid}"))).send().await.unwrap().json().await.unwrap();
        assert_eq!(one.id, tid);

        let r = c.post(http(&format!("/{tid}/kill"))).json(&json!({})).send().await.unwrap();
        assert_eq!(r.status(), 204);
        // A killed session can be removed once it has exited.
        let mut removed = false;
        for _ in 0..100 {
            if c.delete(http(&format!("/{tid}"))).send().await.unwrap().status() == 204 {
                removed = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(removed, "the killed session is removed");
        assert_eq!(c.get(http(&format!("/{tid}"))).send().await.unwrap().status(), 404);
        task.abort();
    })
    .await;
}

#[tokio::test]
async fn fr_w4_local_terms_listener_is_bound_lazily_on_loopback_and_needs_the_token() {
    within(async {
        let c = computers();
        assert!(c.local_node_addr().is_none(), "nothing is bound before the first terminal use");
        // Other computers' routes and a listing of computers do not start it.
        let _ = c.client("nope");
        assert!(c.local_node_addr().is_none());

        let client = c.local_node_client().unwrap();
        let addr = c.local_node_addr().expect("started on first use");
        assert!(addr.ip().is_loopback(), "{addr}");
        // The client carries the token and works.
        assert!(client.terms(&TermListQuery::default()).await.unwrap().is_empty());

        // Without the token the node refuses.
        let r = reqwest::get(format!("http://{addr}/terms")).await.unwrap();
        assert_eq!(r.status(), 401);
        let r = reqwest::Client::new().get(format!("http://{addr}/terms")).bearer_auth("local").send().await.unwrap();
        assert_eq!(r.status(), 401, "a guessable token is refused");
    })
    .await;
}

#[tokio::test]
async fn fr_w4_local_terms_two_calls_share_one_node() {
    within(async {
        let c = computers();
        c.local_node_client().unwrap();
        let first = c.local_node_addr().unwrap();
        c.local_node_client().unwrap();
        assert_eq!(c.local_node_addr().unwrap(), first);

        let a = c.local_node_client().unwrap();
        let b = c.local_node_client().unwrap();
        let made = a.term_create(&serde_json::from_value(create_body("shared")).unwrap()).await.unwrap();
        assert!(b.terms(&TermListQuery::default()).await.unwrap().iter().any(|t| t.id == made.term.id));
        a.term_kill(&made.term.id, None).await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn fr_w4_local_terms_dropping_computers_closes_the_port() {
    within(async {
        let c = computers();
        c.local_node_client().unwrap();
        let addr = c.local_node_addr().unwrap();
        assert!(tokio::net::TcpStream::connect(addr).await.is_ok());
        drop(c);
        let mut closed = false;
        for _ in 0..200 {
            if tokio::net::TcpStream::connect(addr).await.is_err() {
                closed = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert!(closed, "the loopback port is closed after Computers is dropped");
    })
    .await;
}
