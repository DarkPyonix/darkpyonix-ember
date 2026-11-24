//! Exercise the server's background status and transfer pushes over the public WebSocket.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::{Json, Router, routing::get};
use futures::StreamExt;
use reqwest::{Client, RequestBuilder, StatusCode};
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio::process::{Child, Command};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

type PushSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

struct Fixture {
    http: Client,
    base: String,
    token: String,
    _server: Child,
    _dir: tempfile::TempDir,
}

impl Fixture {
    async fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let base = format!("http://127.0.0.1:{port}");
        let server = Command::new(env!("CARGO_BIN_EXE_ember-server"))
            .env("EMBER_DATA_DIR", dir.path())
            .env("EMBER_LISTEN", format!("127.0.0.1:{port}"))
            .env("EMBER_TRANSPORT", "0")
            .env("EMBER_OWNER_PRESENCE", "warn")
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let http = Client::new();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if http
                    .get(format!("{base}/api/health"))
                    .send()
                    .await
                    .is_ok_and(|r| r.status().is_success())
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("server did not become ready");
        let token = std::fs::read_to_string(dir.path().join("server-token"))
            .unwrap()
            .trim()
            .to_owned();
        Self {
            http,
            base,
            token,
            _server: server,
            _dir: dir,
        }
    }

    fn post(&self, path: &str) -> RequestBuilder {
        self.http
            .post(format!("{}{path}", self.base))
            .bearer_auth(&self.token)
    }

    async fn push(&self) -> PushSocket {
        let mut request = format!("{}/api/push", self.base.replacen("http://", "ws://", 1))
            .into_client_request()
            .unwrap();
        request.headers_mut().insert(
            "Authorization",
            format!("Bearer {}", self.token).parse().unwrap(),
        );
        connect_async(request).await.unwrap().0
    }
}

async fn next_push(socket: &mut PushSocket, kind: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(3), async {
        while let Some(message) = socket.next().await {
            if let Ok(text) = message.unwrap().into_text() {
                let value: Value = serde_json::from_str(&text).unwrap();
                if value["type"] == kind {
                    return value;
                }
            }
        }
        panic!("socket closed before {kind}");
    })
    .await
    .unwrap_or_else(|_| panic!("no {kind} push"))
}

#[tokio::test]
async fn pr_1_server_probes_registered_computers_when_a_push_client_connects() {
    let f = Fixture::start().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let node = Router::new().route(
        "/health",
        get(move || {
            let counter = counter.clone();
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Json(json!({"ok":true,"version":"test"}))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, node).await.unwrap() });
    let added = f
        .post("/api/computers")
        .json(&json!({"name":"worker","url":url,"token":"node"}))
        .send()
        .await
        .unwrap();
    assert_eq!(added.status(), StatusCode::CREATED);
    let record: Value = added.json().await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let mut push = f.push().await;
    assert_eq!(
        next_push(&mut push, "computer_status").await,
        json!({
            "type":"computer_status", "computer_id":record["id"], "reachable":true, "error":null
        })
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    task.abort();
}

#[tokio::test]
async fn pr_1_server_pushes_offer_progress_and_completion_records() {
    let f = Fixture::start().await;
    let mut push = f.push().await;
    let created = f
        .post("/api/transfers/offers")
        .json(&json!({
            "from":"sender","files":[{"name":"one.txt","size":1}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let offer: Value = created.json().await.unwrap();
    let tid = offer["id"].as_str().unwrap();
    assert_eq!(
        next_push(&mut push, "transfer_updated").await["transfer"],
        offer["offer"]
    );
    assert_eq!(
        f.post(&format!("/api/transfers/offers/{tid}/accept"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let chunk = f
        .post(&format!("/api/transfers/{tid}/chunks"))
        .json(&json!({"offset":0,"data_base64":"YQ=="}))
        .send()
        .await
        .unwrap();
    assert_eq!(chunk.status(), StatusCode::OK);
    let progress: Value = chunk.json().await.unwrap();
    let pushed = next_push(&mut push, "transfer_updated").await;
    let pushed = if pushed["transfer"]["done"] == 0 {
        next_push(&mut push, "transfer_updated").await
    } else {
        pushed
    };
    assert_eq!(pushed["transfer"], progress);

    assert_eq!(
        f.post(&format!("/api/transfers/{tid}/complete"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let ended = next_push(&mut push, "transfer_updated").await;
    assert_eq!(ended["transfer"]["id"], tid);
    assert_eq!(ended["transfer"]["result"], "done");
}
