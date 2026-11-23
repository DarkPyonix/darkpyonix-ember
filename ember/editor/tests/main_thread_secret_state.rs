//! Fake extension-host integration tests for `MainThreadSecretState`:
//! `$setPassword`, `$getPassword`, `$deletePassword`, missing-key `None` return,
//! isolation between different `extensionId`s, and backend error propagation.
//!
//! Every test injects an in-memory [`SecretStore`] so it never touches the host OS keychain.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use ember_editor::{EditorSession, InMemorySecretStore, SecretStore, SessionConfig};
use ember_editor_conn::actors::generated::main_thread_secret_state;
use ember_editor_conn::exthost::{MSG_INITIALIZED, MSG_READY, MSG_TERMINATE};
use ember_editor_conn::frame::{Frame, FrameDecoder, MessageType};
use ember_editor_conn::ipc::{decode_message, deserialize, encode_message, request_type, response_type, IpcValue};
use ember_editor_conn::rpc::{Arg, Reply, RpcMessage};
use ember_editor_conn::rpc_ids;
use ember_editor_conn::uri::UriComponents;
use ember_editor_conn::PINNED_COMMIT;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio::sync::{mpsc, oneshot};

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

async fn read_http_head<S: AsyncReadExt + Unpin>(stream: &mut S) -> (String, Vec<u8>) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        if let Some(end) = find_header_end(&buf) {
            let head = String::from_utf8_lossy(&buf[..end]).into_owned();
            let rest = buf[end + 4..].to_vec();
            return (head, rest);
        }
        let n = stream.read(&mut chunk).await.expect("read HTTP request");
        assert!(n > 0, "unexpected EOF while reading HTTP headers");
        buf.extend_from_slice(&chunk[..n]);
    }
}

async fn recv_frame<S: AsyncReadExt + Unpin>(stream: &mut S, dec: &mut FrameDecoder) -> Option<Frame> {
    let mut chunk = [0u8; 4096];
    loop {
        if let Some(f) = dec.next_frame().expect("decode frame") {
            return Some(f);
        }
        let n = match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => return None,
            Ok(n) => n,
        };
        dec.push(&chunk[..n]);
    }
}

async fn accept_upgraded_handshake<S: AsyncReadExt + AsyncWriteExt + Unpin>(
    server: &mut S,
) -> (FrameDecoder, Value) {
    let (head, leftover) = read_http_head(server).await;
    assert!(head.contains("Upgrade: websocket"), "expected WebSocket upgrade: {head}");
    let mut dec = FrameDecoder::new();
    dec.push(&leftover);
    server
        .write_all(b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n")
        .await
        .unwrap();

    let auth_frame = recv_frame(server, &mut dec).await.expect("auth frame");
    assert_eq!(auth_frame.ty, MessageType::Control);

    let sign = br#"{"type":"sign","data":"challenge","signedData":"sig"}"#.to_vec();
    server
        .write_all(&Frame::new(MessageType::Control, 0, 0, sign).encode())
        .await
        .unwrap();

    let ct_frame = recv_frame(server, &mut dec).await.expect("connectionType frame");
    assert_eq!(ct_frame.ty, MessageType::Control);
    let ct: Value = serde_json::from_slice(&ct_frame.data).unwrap();
    assert_eq!(ct["commit"].as_str(), Some(PINNED_COMMIT));

    let reply = if ct["desiredConnectionType"] == 1 {
        br#"{"type":"ok"}"#.to_vec()
    } else {
        br#"{"debugPort":null}"#.to_vec()
    };
    server
        .write_all(&Frame::new(MessageType::Control, 0, 0, reply).encode())
        .await
        .unwrap();

    (dec, ct)
}

async fn run_fake_management(mut server: DuplexStream) {
    let (mut dec, ct) = accept_upgraded_handshake(&mut server).await;
    assert_eq!(ct["desiredConnectionType"], 1);

    let mut srv_id = 0u32;
    let ctx_frame = recv_frame(&mut server, &mut dec).await.expect("ipc context frame");
    let mut last_client_id = ctx_frame.id;
    let mut pos = 0;
    let _ = deserialize(&ctx_frame.data, &mut pos).unwrap();

    let init_bytes = encode_message(
        &IpcValue::Array(vec![IpcValue::Int(response_type::INITIALIZE)]),
        &IpcValue::Undefined,
    );
    srv_id += 1;
    server
        .write_all(&Frame::new(MessageType::Regular, srv_id, last_client_id, init_bytes).encode())
        .await
        .unwrap();

    while let Some(frame) = recv_frame(&mut server, &mut dec).await {
        if frame.ty == MessageType::Disconnect {
            break;
        }
        if frame.ty != MessageType::Regular {
            continue;
        }
        last_client_id = frame.id;
        let (header, _body) = decode_message(&frame.data).unwrap();
        let IpcValue::Array(h) = header else { continue };
        let Some(req_ty) = h.first().and_then(IpcValue::as_int).map(|n| n as i32) else {
            continue;
        };
        if req_ty != request_type::PROMISE {
            continue;
        }
        let id = h[1].as_int().unwrap() as i32;
        let channel = match &h[2] {
            IpcValue::String(s) => s.as_str(),
            _ => "",
        };
        let cmd = match &h[3] {
            IpcValue::String(s) => s.as_str(),
            _ => "",
        };

        let reply_body = match (channel, cmd) {
            ("remoteextensionsenvironment", "getEnvironmentData") => {
                let u = |p: &str| UriComponents::file(p);
                IpcValue::json(&json!({
                    "pid": 4242,
                    "connectionToken": "none",
                    "appRoot": u("/ose"),
                    "extensionHostLogsPath": u("/ose/logs"),
                    "globalStorageHome": u("/ose/global"),
                    "workspaceStorageHome": u("/ose/ws"),
                    "userHome": u("/home/ember"),
                    "os": 3,
                    "arch": "x64",
                }))
                .unwrap()
            }
            ("remoteExtensionsScanner", "scanExtensions") => IpcValue::json(&json!([])).unwrap(),
            ("remoteFilesystem", "watch") | ("remoteFilesystem", "unwatch") => IpcValue::Undefined,
            other => panic!("unexpected IPC call: {other:?}"),
        };

        let resp_header = IpcValue::Array(vec![
            IpcValue::Int(response_type::PROMISE_SUCCESS),
            IpcValue::Int(id),
        ]);
        srv_id += 1;
        let payload = encode_message(&resp_header, &reply_body);
        if server
            .write_all(&Frame::new(MessageType::Regular, srv_id, last_client_id, payload).encode())
            .await
            .is_err()
        {
            break;
        }
    }
}

#[derive(Debug, PartialEq)]
enum ExtHostReply {
    Ok(Reply),
    Err(Option<Value>),
}

struct OutboundCall {
    method: String,
    args: Vec<Arg>,
    reply: oneshot::Sender<ExtHostReply>,
}

/// What the fake extension host saw on the wire, in arrival order.
#[derive(Debug, PartialEq)]
enum Seen {
    /// The reply to a call the fake sent to Ember (`method` is the call's method).
    Reply(String),
    /// A request Ember sent to the extension host.
    Request { rpc_id: u8, method: String, args: Vec<Arg> },
}

struct FakeExtHostHandle {
    tx: mpsc::UnboundedSender<OutboundCall>,
    events: tokio::sync::Mutex<mpsc::UnboundedReceiver<Seen>>,
}

impl FakeExtHostHandle {
    async fn next_seen(&self) -> Seen {
        let mut events = self.events.lock().await;
        tokio::time::timeout(Duration::from_secs(5), events.recv())
            .await
            .expect("timed out waiting for the fake ext host to see a message")
            .expect("fake ext host stopped")
    }

    /// Whether the fake saw nothing more within a short grace period.
    async fn nothing_more_seen(&self) -> bool {
        let mut events = self.events.lock().await;
        tokio::time::timeout(Duration::from_millis(300), events.recv()).await.is_err()
    }

    async fn call(&self, method: &str, args: Vec<Arg>) -> ExtHostReply {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(OutboundCall {
                method: method.to_owned(),
                args,
                reply: reply_tx,
            })
            .expect("fake ext host still running");
        tokio::time::timeout(Duration::from_secs(5), reply_rx)
            .await
            .expect("timed out waiting for MainThreadSecretState reply")
            .expect("fake ext host dropped reply channel")
    }

    async fn get_password(&self, extension_id: &str, key: &str) -> ExtHostReply {
        self.call("$getPassword", vec![Arg::Json(json!(extension_id)), Arg::Json(json!(key))])
            .await
    }

    async fn set_password(&self, extension_id: &str, key: &str, value: &str) -> ExtHostReply {
        self.call(
            "$setPassword",
            vec![Arg::Json(json!(extension_id)), Arg::Json(json!(key)), Arg::Json(json!(value))],
        )
        .await
    }

    async fn delete_password(&self, extension_id: &str, key: &str) -> ExtHostReply {
        self.call("$deletePassword", vec![Arg::Json(json!(extension_id)), Arg::Json(json!(key))])
            .await
    }
}

async fn run_fake_exthost(
    mut server: DuplexStream,
    mut outbound_rx: mpsc::UnboundedReceiver<OutboundCall>,
    events: mpsc::UnboundedSender<Seen>,
) {
    let (mut dec, ct) = accept_upgraded_handshake(&mut server).await;
    assert_eq!(ct["desiredConnectionType"], 2);

    let mut srv_id = 0u32;
    let mut last_client_id = 0u32;
    let mut exthost_req_seq = 0u32;
    let mut pending: HashMap<u32, (String, oneshot::Sender<ExtHostReply>)> = HashMap::new();

    // 1. Send Ready
    srv_id += 1;
    server
        .write_all(&Frame::new(MessageType::Regular, srv_id, last_client_id, vec![MSG_READY]).encode())
        .await
        .unwrap();

    // 2. Receive IExtensionHostInitData
    let init_frame = recv_frame(&mut server, &mut dec).await.expect("initData frame");
    last_client_id = init_frame.id;

    // 3. Send Initialized
    srv_id += 1;
    server
        .write_all(&Frame::new(MessageType::Regular, srv_id, last_client_id, vec![MSG_INITIALIZED]).encode())
        .await
        .unwrap();

    let rpc_id = rpc_ids::id_of(main_thread_secret_state::PROXY).unwrap();
    let mut chunk = [0u8; 4096];

    loop {
        while let Some(frame) = dec.next_frame().expect("decode frame") {
            if frame.ty == MessageType::Disconnect {
                return;
            }
            if frame.ty != MessageType::Regular {
                continue;
            }
            last_client_id = frame.id;
            if frame.data == [MSG_TERMINATE] {
                return;
            }
            let msg = RpcMessage::decode(&frame.data).expect("decode RPC message");
            match msg {
                RpcMessage::Request { req, rpc_id: incoming_id, method, args, .. } => {
                    srv_id += 1;
                    let ack = RpcMessage::Acknowledged { req }.encode().unwrap();
                    server
                        .write_all(&Frame::new(MessageType::Regular, srv_id, last_client_id, ack).encode())
                        .await
                        .unwrap();

                    srv_id += 1;
                    let ok = RpcMessage::ReplyOk { req, reply: Reply::Empty }.encode().unwrap();
                    server
                        .write_all(&Frame::new(MessageType::Regular, srv_id, last_client_id, ok).encode())
                        .await
                        .unwrap();
                    let _ = events.send(Seen::Request { rpc_id: incoming_id, method, args });
                }
                RpcMessage::Acknowledged { .. } => {}
                RpcMessage::ReplyOk { req, reply } => {
                    if let Some((method, tx)) = pending.remove(&req) {
                        let _ = events.send(Seen::Reply(method));
                        let _ = tx.send(ExtHostReply::Ok(reply));
                    }
                }
                RpcMessage::ReplyErr { req, error } => {
                    if let Some((method, tx)) = pending.remove(&req) {
                        let _ = events.send(Seen::Reply(method));
                        let _ = tx.send(ExtHostReply::Err(error));
                    }
                }
                RpcMessage::Cancel { .. } => {}
            }
        }

        tokio::select! {
            call = outbound_rx.recv() => {
                let Some(call) = call else { break };
                exthost_req_seq += 1;
                let req = exthost_req_seq;
                pending.insert(req, (call.method.clone(), call.reply));
                let payload = RpcMessage::Request {
                    req,
                    rpc_id,
                    method: call.method,
                    args: call.args,
                    cancellable: false,
                }
                .encode()
                .unwrap();
                srv_id += 1;
                server
                    .write_all(&Frame::new(MessageType::Regular, srv_id, last_client_id, payload).encode())
                    .await
                    .unwrap();
            }
            read = server.read(&mut chunk) => {
                match read {
                    Ok(0) | Err(_) => break,
                    Ok(n) => dec.push(&chunk[..n]),
                }
            }
        }
    }
}

async fn connect_with_store(store: Arc<dyn SecretStore>) -> (EditorSession, FakeExtHostHandle) {
    let (outbound_tx, outbound_rx) = mpsc::unbounded_channel();
    let (events_tx, events_rx) = mpsc::unbounded_channel();
    let outbound_rx = Arc::new(tokio::sync::Mutex::new(Some(outbound_rx)));
    let dials = Arc::new(AtomicUsize::new(0));

    let config = SessionConfig::new("127.0.0.1:8000", "/workspace/project").with_secret_store(store);
    let dials_cb = Arc::clone(&dials);
    let rx_cb = Arc::clone(&outbound_rx);
    let (session, _updates) = EditorSession::connect(config, move || {
        let step = dials_cb.fetch_add(1, Ordering::SeqCst);
        let rx_holder = Arc::clone(&rx_cb);
        let events_tx = events_tx.clone();
        async move {
            let (client, mut server) = tokio::io::duplex(128 * 1024);
            match step {
                0 => {
                    tokio::spawn(async move {
                        let (head, _) = read_http_head(&mut server).await;
                        assert!(head.starts_with("GET /version HTTP/1.0"));
                        let resp = format!("HTTP/1.0 200 OK\r\nContent-Type: text/plain\r\n\r\n{PINNED_COMMIT}");
                        server.write_all(resp.as_bytes()).await.unwrap();
                    });
                }
                1 => {
                    tokio::spawn(run_fake_management(server));
                }
                2 => {
                    let rx = rx_holder.lock().await.take().expect("exthost dial once");
                    tokio::spawn(run_fake_exthost(server, rx, events_tx));
                }
                other => panic!("unexpected dial #{other}"),
            }
            Ok(client)
        }
    })
    .await
    .expect("EditorSession::connect should succeed");

    (session, FakeExtHostHandle { tx: outbound_tx, events: tokio::sync::Mutex::new(events_rx) })
}

#[tokio::test]
async fn secret_state_set_get_overwrite_delete_and_missing_key() {
    let store = Arc::new(InMemorySecretStore::new());
    let (session, exthost) = connect_with_store(store.clone()).await;

    // Missing key returns undefined (Reply::Empty).
    assert_eq!(exthost.get_password("ext.alpha", "token").await, ExtHostReply::Ok(Reply::Empty));

    // Set then get returns the stored string.
    assert_eq!(
        exthost.set_password("ext.alpha", "token", "s3cr3t-v1").await,
        ExtHostReply::Ok(Reply::Empty)
    );
    assert_eq!(
        exthost.get_password("ext.alpha", "token").await,
        ExtHostReply::Ok(Reply::Json(json!("s3cr3t-v1")))
    );

    // Overwriting replaces the stored string.
    assert_eq!(
        exthost.set_password("ext.alpha", "token", "s3cr3t-v2").await,
        ExtHostReply::Ok(Reply::Empty)
    );
    assert_eq!(
        exthost.get_password("ext.alpha", "token").await,
        ExtHostReply::Ok(Reply::Json(json!("s3cr3t-v2")))
    );

    // Delete removes the entry so subsequent get returns Reply::Empty.
    assert_eq!(exthost.delete_password("ext.alpha", "token").await, ExtHostReply::Ok(Reply::Empty));
    assert_eq!(exthost.get_password("ext.alpha", "token").await, ExtHostReply::Ok(Reply::Empty));

    // Deleting an already-missing key succeeds.
    assert_eq!(exthost.delete_password("ext.alpha", "token").await, ExtHostReply::Ok(Reply::Empty));
    assert!(store.snapshot().is_empty());

    session.shutdown().await;
}

#[tokio::test]
async fn secret_state_isolates_different_extension_ids() {
    let store = Arc::new(InMemorySecretStore::new());
    let (session, exthost) = connect_with_store(store.clone()).await;

    assert_eq!(
        exthost.set_password("ext.alpha", "api_key", "alpha-key").await,
        ExtHostReply::Ok(Reply::Empty)
    );
    assert_eq!(
        exthost.set_password("ext.beta", "api_key", "beta-key").await,
        ExtHostReply::Ok(Reply::Empty)
    );
    assert_eq!(
        exthost.set_password("a:b", "c", "val-ab-c").await,
        ExtHostReply::Ok(Reply::Empty)
    );
    assert_eq!(
        exthost.set_password("a", "b:c", "val-a-bc").await,
        ExtHostReply::Ok(Reply::Empty)
    );

    assert_eq!(
        exthost.get_password("ext.alpha", "api_key").await,
        ExtHostReply::Ok(Reply::Json(json!("alpha-key")))
    );
    assert_eq!(
        exthost.get_password("ext.beta", "api_key").await,
        ExtHostReply::Ok(Reply::Json(json!("beta-key")))
    );
    assert_eq!(
        exthost.get_password("ext.gamma", "api_key").await,
        ExtHostReply::Ok(Reply::Empty)
    );
    assert_eq!(
        exthost.get_password("a:b", "c").await,
        ExtHostReply::Ok(Reply::Json(json!("val-ab-c")))
    );
    assert_eq!(
        exthost.get_password("a", "b:c").await,
        ExtHostReply::Ok(Reply::Json(json!("val-a-bc")))
    );

    // Deleting ext.alpha's key leaves ext.beta's key untouched.
    assert_eq!(exthost.delete_password("ext.alpha", "api_key").await, ExtHostReply::Ok(Reply::Empty));
    assert_eq!(exthost.get_password("ext.alpha", "api_key").await, ExtHostReply::Ok(Reply::Empty));
    assert_eq!(
        exthost.get_password("ext.beta", "api_key").await,
        ExtHostReply::Ok(Reply::Json(json!("beta-key")))
    );

    session.shutdown().await;
}

#[derive(Debug)]
struct FailingSecretStore;

impl SecretStore for FailingSecretStore {
    fn get(&self, _extension_id: &str, _key: &str) -> Result<Option<String>, String> {
        Err("keychain locked".to_owned())
    }

    fn set(&self, _extension_id: &str, _key: &str, _value: &str) -> Result<(), String> {
        Err("keychain read-only".to_owned())
    }

    fn delete(&self, _extension_id: &str, _key: &str) -> Result<(), String> {
        Err("keychain unavailable".to_owned())
    }
}

#[tokio::test]
async fn secret_state_backend_error_returns_rpc_error() {
    let (session, exthost) = connect_with_store(Arc::new(FailingSecretStore)).await;

    match exthost.get_password("ext.alpha", "k").await {
        ExtHostReply::Err(Some(err)) => {
            assert_eq!(err["message"], "keychain locked");
        }
        other => panic!("expected ReplyErr for get, got {other:?}"),
    }
    match exthost.set_password("ext.alpha", "k", "v").await {
        ExtHostReply::Err(Some(err)) => {
            assert_eq!(err["message"], "keychain read-only");
        }
        other => panic!("expected ReplyErr for set, got {other:?}"),
    }
    match exthost.delete_password("ext.alpha", "k").await {
        ExtHostReply::Err(Some(err)) => {
            assert_eq!(err["message"], "keychain unavailable");
        }
        other => panic!("expected ReplyErr for delete, got {other:?}"),
    }

    session.shutdown().await;
}

fn change_request(extension_id: &str, key: &str) -> Seen {
    Seen::Request {
        rpc_id: rpc_ids::id_of("ExtHostSecretState").unwrap(),
        method: "$onDidChangePassword".to_owned(),
        args: vec![Arg::Json(json!({ "extensionId": extension_id, "key": key }))],
    }
}

#[tokio::test]
async fn fr_e1_secret_set_and_delete_notify_the_extension_host_of_the_change() {
    let (session, exthost) = connect_with_store(Arc::new(InMemorySecretStore::new())).await;

    assert_eq!(exthost.set_password("ext.alpha", "token", "s3cr3t").await, ExtHostReply::Ok(Reply::Empty));
    assert_eq!(exthost.next_seen().await, Seen::Reply("$setPassword".to_owned()));
    assert_eq!(exthost.next_seen().await, change_request("ext.alpha", "token"));

    assert_eq!(exthost.delete_password("ext.alpha", "token").await, ExtHostReply::Ok(Reply::Empty));
    assert_eq!(exthost.next_seen().await, Seen::Reply("$deletePassword".to_owned()));
    assert_eq!(exthost.next_seen().await, change_request("ext.alpha", "token"));

    // Reads never notify, and deleting what is not stored changes nothing, so it stays silent.
    assert_eq!(exthost.get_password("ext.alpha", "token").await, ExtHostReply::Ok(Reply::Empty));
    assert_eq!(exthost.next_seen().await, Seen::Reply("$getPassword".to_owned()));
    assert_eq!(exthost.delete_password("ext.alpha", "token").await, ExtHostReply::Ok(Reply::Empty));
    assert_eq!(exthost.next_seen().await, Seen::Reply("$deletePassword".to_owned()));
    assert!(exthost.nothing_more_seen().await);

    session.shutdown().await;
}

#[tokio::test]
async fn fr_e1_failed_secret_write_does_not_notify() {
    let (session, exthost) = connect_with_store(Arc::new(FailingSecretStore)).await;

    assert!(matches!(exthost.set_password("ext.alpha", "k", "v").await, ExtHostReply::Err(Some(_))));
    assert_eq!(exthost.next_seen().await, Seen::Reply("$setPassword".to_owned()));
    assert!(matches!(exthost.delete_password("ext.alpha", "k").await, ExtHostReply::Err(Some(_))));
    assert_eq!(exthost.next_seen().await, Seen::Reply("$deletePassword".to_owned()));
    assert!(exthost.nothing_more_seen().await);

    session.shutdown().await;
}
