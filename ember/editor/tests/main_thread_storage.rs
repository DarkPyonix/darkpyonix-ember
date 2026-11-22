//! Fake extension-host integration tests for `MainThreadStorage` persistence.
//!
//! Each test runs a real [`EditorSession`] against an in-process scripted Code-OSS server whose
//! extension host sends `MainThreadStorage.$initializeExtensionStorage`, `$setValue`, and
//! `$registerExtensionStorageKeysToSync` requests over the real RPC framing.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ember_editor::{EditorSession, SessionConfig};
use ember_editor_conn::exthost::{proxy, MSG_INITIALIZED, MSG_READY, MSG_TERMINATE};
use ember_editor_conn::frame::{Frame, FrameDecoder, MessageType};
use ember_editor_conn::ipc::{decode_message, encode_message, request_type, response_type, IpcValue};
use ember_editor_conn::rpc::{Arg, Reply, RpcMessage};
use ember_editor_conn::rpc_ids;
use ember_editor_conn::uri::UriComponents;
use ember_editor_conn::PINNED_COMMIT;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio::sync::oneshot;

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

async fn accept_upgraded_handshake<S: AsyncReadExt + AsyncWriteExt + Unpin>(server: &mut S) -> (FrameDecoder, Value) {
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
    server.write_all(&Frame::new(MessageType::Control, 0, 0, sign).encode()).await.unwrap();

    let ct_frame = recv_frame(server, &mut dec).await.expect("connectionType frame");
    let ct: Value = serde_json::from_slice(&ct_frame.data).unwrap();
    let reply = if ct["desiredConnectionType"] == 1 {
        br#"{"type":"ok"}"#.to_vec()
    } else {
        br#"{"debugPort":null}"#.to_vec()
    };
    server.write_all(&Frame::new(MessageType::Control, 0, 0, reply).encode()).await.unwrap();
    (dec, ct)
}

/// Virtual filesystem backing the scripted OSE server's `remoteFilesystem` channel across sessions.
#[derive(Default, Clone)]
struct FakeRemoteDisk {
    files: Arc<Mutex<HashMap<String, Vec<u8>>>>,
}

impl FakeRemoteDisk {
    fn snapshot(&self) -> HashMap<String, Vec<u8>> {
        self.files.lock().unwrap().clone()
    }

    fn insert(&self, path: impl Into<String>, bytes: Vec<u8>) {
        self.files.lock().unwrap().insert(path.into(), bytes);
    }
}

async fn run_fake_management(mut server: DuplexStream, disk: FakeRemoteDisk) {
    let (mut dec, ct) = accept_upgraded_handshake(&mut server).await;
    assert_eq!(ct["desiredConnectionType"], 1);

    let mut srv_id = 0u32;

    let mut send_regular = |payload: Vec<u8>, ack: u32| {
        srv_id += 1;
        Frame::new(MessageType::Regular, srv_id, ack, payload).encode()
    };

    let init_header = IpcValue::Array(vec![IpcValue::Int(response_type::INITIALIZE)]);
    let init_frame = send_regular(encode_message(&init_header, &IpcValue::Undefined), 0);
    server.write_all(&init_frame).await.unwrap();

    while let Some(frame) = recv_frame(&mut server, &mut dec).await {
        if frame.ty == MessageType::Disconnect {
            break;
        }
        if frame.ty != MessageType::Regular {
            continue;
        }
        let last_client_id = frame.id;
        let (header, body) = decode_message(&frame.data).unwrap();
        let IpcValue::Array(h) = header else { continue };
        let Some(req_ty) = h.first().and_then(IpcValue::as_int).map(|n| n as i32) else { continue };
        if req_ty == response_type::INITIALIZE || req_ty == request_type::EVENT_LISTEN {
            continue;
        }
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

        let result: Result<IpcValue, Value> = match (channel, cmd) {
            ("remoteextensionsenvironment", "getEnvironmentData") => {
                let u = |p: &str| UriComponents::remote("127.0.0.1:8000", p);
                Ok(IpcValue::json(&json!({
                    "pid": 4242,
                    "connectionToken": "none",
                    "appRoot": u("/ose"),
                    "extensionHostLogsPath": u("/ose/logs"),
                    "globalStorageHome": u("/ose-data/User/globalStorage"),
                    "workspaceStorageHome": u("/ose-data/User/workspaceStorage"),
                    "userHome": u("/home/ember"),
                    "os": 3,
                    "arch": "x64",
                }))
                .unwrap())
            }
            ("remoteExtensionsScanner", "scanExtensions") => Ok(IpcValue::json(&json!([])).unwrap()),
            ("remoteFilesystem", "watch") | ("remoteFilesystem", "unwatch") => Ok(IpcValue::Undefined),
            ("remoteFilesystem", "mkdir") => Ok(IpcValue::Undefined),
            ("remoteFilesystem", "readFile") => {
                let path = match &body {
                    IpcValue::Array(args) => args.first().and_then(|a| a.to_json()["path"].as_str().map(str::to_owned)),
                    _ => None,
                }
                .unwrap_or_default();
                match disk.files.lock().unwrap().get(&path).cloned() {
                    Some(bytes) => Ok(IpcValue::VsBuffer(bytes)),
                    None => Err(json!({ "name": "EntryNotFound", "message": format!("ENOENT: {path}") })),
                }
            }
            ("remoteFilesystem", "writeFile") => {
                if let IpcValue::Array(args) = &body {
                    let path = args.first().and_then(|a| a.to_json()["path"].as_str().map(str::to_owned)).unwrap_or_default();
                    let bytes = args.get(1).and_then(IpcValue::as_bytes).map(<[u8]>::to_vec).unwrap_or_default();
                    disk.files.lock().unwrap().insert(path, bytes);
                }
                Ok(IpcValue::Undefined)
            }
            ("remoteFilesystem", "rename") => {
                if let IpcValue::Array(args) = &body {
                    let from = args.first().and_then(|a| a.to_json()["path"].as_str().map(str::to_owned)).unwrap_or_default();
                    let to = args.get(1).and_then(|a| a.to_json()["path"].as_str().map(str::to_owned)).unwrap_or_default();
                    let mut map = disk.files.lock().unwrap();
                    if let Some(bytes) = map.remove(&from) {
                        map.insert(to, bytes);
                    }
                }
                Ok(IpcValue::Undefined)
            }
            ("remoteFilesystem", "delete") => {
                if let IpcValue::Array(args) = &body {
                    let path = args.first().and_then(|a| a.to_json()["path"].as_str().map(str::to_owned)).unwrap_or_default();
                    disk.files.lock().unwrap().remove(&path);
                }
                Ok(IpcValue::Undefined)
            }
            other => panic!("unexpected IPC call: {other:?}"),
        };

        let (resp_header, resp_body) = match result {
            Ok(v) => (
                IpcValue::Array(vec![IpcValue::Int(response_type::PROMISE_SUCCESS), IpcValue::Int(id)]),
                v,
            ),
            Err(err) => (
                IpcValue::Array(vec![IpcValue::Int(response_type::PROMISE_ERROR_OBJ), IpcValue::Int(id)]),
                IpcValue::Object(err),
            ),
        };
        let f = send_regular(encode_message(&resp_header, &resp_body), last_client_id);
        if server.write_all(&f).await.is_err() {
            break;
        }
    }
}

/// One RPC request the scripted extension host will send to `MainThreadStorage`.
#[derive(Clone)]
struct StorageOp {
    method: &'static str,
    args: Vec<Arg>,
}

/// Reply observed by the scripted extension host for one `MainThreadStorage` request.
#[derive(Debug, Clone, PartialEq)]
enum ObservedReply {
    Ok(Reply),
    Err(Option<Value>),
}

/// Connect one `EditorSession` against the scripted server, run `ops` sequentially from the fake
/// extension host once bootstrap finishes, return all replies, and shut the session down cleanly.
async fn run_session_script(
    disk: FakeRemoteDisk,
    workspace_path: &str,
    storage_dir: Option<PathBuf>,
    ops: Vec<StorageOp>,
) -> Vec<ObservedReply> {
    let dials = Arc::new(AtomicUsize::new(0));
    let (done_tx, done_rx) = oneshot::channel::<Vec<ObservedReply>>();
    let done_tx = Arc::new(Mutex::new(Some(done_tx)));
    let ops = Arc::new(ops);

    let mut config = SessionConfig::new("127.0.0.1:8000", workspace_path);
    config.storage_dir = storage_dir;

    let dials_cb = Arc::clone(&dials);
    let disk_cb = disk.clone();
    let ops_cb = Arc::clone(&ops);
    let done_cb = Arc::clone(&done_tx);

    let (session, _updates) = EditorSession::connect(config, move || {
        let n = dials_cb.fetch_add(1, Ordering::SeqCst);
        let disk = disk_cb.clone();
        let ops = Arc::clone(&ops_cb);
        let done = Arc::clone(&done_cb);
        async move {
            let (client, mut server) = tokio::io::duplex(64 * 1024);
            match n {
                0 => {
                    tokio::spawn(async move {
                        let _ = read_http_head(&mut server).await;
                        let body = PINNED_COMMIT;
                        let resp = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        );
                        let _ = server.write_all(resp.as_bytes()).await;
                    });
                }
                1 => {
                    tokio::spawn(run_fake_management(server, disk));
                }
                2 => {
                    tokio::spawn(async move {
                        run_fake_exthost_script(server, &ops, done).await;
                    });
                }
                _ => panic!("unexpected dial {n}"),
            }
            Ok(client)
        }
    })
    .await
    .expect("EditorSession::connect");

    let replies = tokio::time::timeout(Duration::from_secs(10), done_rx)
        .await
        .expect("timed out waiting for scripted storage replies")
        .expect("fake exthost dropped");
    session.shutdown().await;
    replies
}

async fn run_fake_exthost_script(
    mut server: DuplexStream,
    ops: &[StorageOp],
    done: Arc<Mutex<Option<oneshot::Sender<Vec<ObservedReply>>>>>,
) {
    let (mut dec, ct) = accept_upgraded_handshake(&mut server).await;
    assert_eq!(ct["desiredConnectionType"], 2);

    let mut srv_id = 0u32;
    let mut last_client_id = 0u32;

    srv_id += 1;
    server.write_all(&Frame::new(MessageType::Regular, srv_id, last_client_id, vec![MSG_READY]).encode()).await.unwrap();

    let init_frame = recv_frame(&mut server, &mut dec).await.expect("initData frame");
    last_client_id = init_frame.id;

    srv_id += 1;
    server
        .write_all(&Frame::new(MessageType::Regular, srv_id, last_client_id, vec![MSG_INITIALIZED]).encode())
        .await
        .unwrap();

    // Answer bootstrap calls (`$initializeConfiguration`, `$initializeWorkspace`) first.
    let mut bootstrap_seen = 0usize;
    while bootstrap_seen < 2 {
        let frame = recv_frame(&mut server, &mut dec).await.expect("bootstrap frame");
        if frame.ty != MessageType::Regular {
            continue;
        }
        last_client_id = frame.id;
        let msg = RpcMessage::decode(&frame.data).expect("decode bootstrap RPC");
        if let RpcMessage::Request { req, .. } = msg {
            bootstrap_seen += 1;
            srv_id += 1;
            let ack = RpcMessage::Acknowledged { req }.encode().unwrap();
            server.write_all(&Frame::new(MessageType::Regular, srv_id, last_client_id, ack).encode()).await.unwrap();
            srv_id += 1;
            let ok = RpcMessage::ReplyOk { req, reply: Reply::Empty }.encode().unwrap();
            server.write_all(&Frame::new(MessageType::Regular, srv_id, last_client_id, ok).encode()).await.unwrap();
        }
    }

    let storage_rpc_id = rpc_ids::id_of(proxy::MAIN_THREAD_STORAGE).unwrap();
    let mut replies = Vec::with_capacity(ops.len());
    for (idx, op) in ops.iter().enumerate() {
        let req_id = (idx as u32) + 1;
        let payload = RpcMessage::Request {
            req: req_id,
            rpc_id: storage_rpc_id,
            method: op.method.to_owned(),
            args: op.args.clone(),
            cancellable: false,
        }
        .encode()
        .unwrap();
        srv_id += 1;
        server.write_all(&Frame::new(MessageType::Regular, srv_id, last_client_id, payload).encode()).await.unwrap();

        loop {
            let frame = recv_frame(&mut server, &mut dec).await.expect("storage reply frame");
            if frame.ty == MessageType::Disconnect || frame.data == [MSG_TERMINATE] {
                panic!("extension host terminated before answering storage req {req_id}");
            }
            if frame.ty != MessageType::Regular {
                continue;
            }
            last_client_id = frame.id;
            match RpcMessage::decode(&frame.data).expect("decode RPC reply") {
                RpcMessage::Acknowledged { .. } => {}
                RpcMessage::ReplyOk { req, reply } if req == req_id => {
                    replies.push(ObservedReply::Ok(reply));
                    break;
                }
                RpcMessage::ReplyErr { req, error } if req == req_id => {
                    replies.push(ObservedReply::Err(error));
                    break;
                }
                RpcMessage::Request { req, .. } => {
                    srv_id += 1;
                    let ok = RpcMessage::ReplyOk { req, reply: Reply::Empty }.encode().unwrap();
                    server.write_all(&Frame::new(MessageType::Regular, srv_id, last_client_id, ok).encode()).await.unwrap();
                }
                _ => {}
            }
        }
    }

    if let Some(tx) = done.lock().unwrap().take() {
        let _ = tx.send(replies);
    }

    // Drain until terminate so session shutdown closes cleanly.
    while let Some(frame) = recv_frame(&mut server, &mut dec).await {
        if frame.ty == MessageType::Disconnect || frame.data == [MSG_TERMINATE] {
            break;
        }
    }
}

fn init_op(shared: bool, ext_id: &str) -> StorageOp {
    StorageOp {
        method: "$initializeExtensionStorage",
        args: vec![Arg::Json(json!(shared)), Arg::Json(json!(ext_id))],
    }
}

fn set_op(shared: bool, ext_id: &str, value: Value) -> StorageOp {
    StorageOp {
        method: "$setValue",
        args: vec![Arg::Json(json!(shared)), Arg::Json(json!(ext_id)), Arg::Json(value)],
    }
}

fn sync_keys_op(ext_id: &str, keys: &[&str]) -> StorageOp {
    StorageOp {
        method: "$registerExtensionStorageKeysToSync",
        args: vec![
            Arg::Json(json!({ "id": ext_id, "version": "1.0.0" })),
            Arg::Json(json!(keys)),
        ],
    }
}

fn parse_reply_json(reply: &ObservedReply) -> Value {
    match reply {
        ObservedReply::Ok(Reply::Json(Value::String(s))) => serde_json::from_str(s).expect("valid JSON string"),
        other => panic!("expected Reply::Json(String), got {other:?}"),
    }
}

#[tokio::test]
async fn global_state_persists_across_sessions_and_workspaces() {
    let disk = FakeRemoteDisk::default();

    // Session 1 on workspace alpha: absent initially, then set, then readable in-session.
    let r1 = run_session_script(
        disk.clone(),
        "/workspace/alpha",
        None,
        vec![
            init_op(true, "pub.ext"),
            set_op(true, "pub.ext", json!({ "token": "abc", "count": 1 })),
            init_op(true, "pub.ext"),
            sync_keys_op("pub.ext", &["token"]),
        ],
    )
    .await;
    assert_eq!(r1[0], ObservedReply::Ok(Reply::Empty));
    assert_eq!(r1[1], ObservedReply::Ok(Reply::Empty));
    assert_eq!(parse_reply_json(&r1[2]), json!({ "token": "abc", "count": 1 }));
    assert_eq!(r1[3], ObservedReply::Ok(Reply::Empty));

    // Session 2 on workspace alpha (different casing of extension id): persisted value returns.
    let r2 = run_session_script(disk.clone(), "/workspace/alpha", None, vec![init_op(true, "Pub.Ext")]).await;
    assert_eq!(parse_reply_json(&r2[0]), json!({ "token": "abc", "count": 1 }));

    // Session 3 on workspace beta: globalState is shared across workspaces.
    let r3 = run_session_script(disk.clone(), "/workspace/beta", None, vec![init_op(true, "pub.ext")]).await;
    assert_eq!(parse_reply_json(&r3[0]), json!({ "token": "abc", "count": 1 }));
}

#[tokio::test]
async fn workspace_state_isolated_per_workspace_and_persists_across_restarts() {
    let disk = FakeRemoteDisk::default();

    let r1 = run_session_script(
        disk.clone(),
        "/workspace/alpha",
        None,
        vec![
            init_op(false, "pub.ext"),
            set_op(false, "pub.ext", json!({ "branch": "main", "openCount": 3 })),
        ],
    )
    .await;
    assert_eq!(r1[0], ObservedReply::Ok(Reply::Empty));
    assert_eq!(r1[1], ObservedReply::Ok(Reply::Empty));

    // Workspace beta does not see workspace alpha's state.
    let r2 = run_session_script(
        disk.clone(),
        "/workspace/beta",
        None,
        vec![
            init_op(false, "pub.ext"),
            set_op(false, "pub.ext", json!({ "branch": "dev" })),
        ],
    )
    .await;
    assert_eq!(r2[0], ObservedReply::Ok(Reply::Empty));
    assert_eq!(r2[1], ObservedReply::Ok(Reply::Empty));

    // Restarting on alpha still sees alpha's state; restarting on beta still sees beta's state.
    let r3 = run_session_script(disk.clone(), "/workspace/alpha", None, vec![init_op(false, "pub.ext")]).await;
    assert_eq!(parse_reply_json(&r3[0]), json!({ "branch": "main", "openCount": 3 }));

    let r4 = run_session_script(disk.clone(), "/workspace/beta", None, vec![init_op(false, "pub.ext")]).await;
    assert_eq!(parse_reply_json(&r4[0]), json!({ "branch": "dev" }));
}

#[tokio::test]
async fn overwrite_clear_and_corrupt_recovery_persist_accurately() {
    let disk = FakeRemoteDisk::default();

    // Write initial state, then overwrite with a smaller object.
    let r1 = run_session_script(
        disk.clone(),
        "/workspace/proj",
        None,
        vec![
            set_op(true, "pub.ext", json!({ "a": 1, "b": 2 })),
            set_op(true, "pub.ext", json!({ "a": 9 })),
        ],
    )
    .await;
    assert_eq!(r1, vec![ObservedReply::Ok(Reply::Empty), ObservedReply::Ok(Reply::Empty)]);

    // Restart: overwritten value persists; then clear with empty object `{}`.
    let r2 = run_session_script(
        disk.clone(),
        "/workspace/proj",
        None,
        vec![
            init_op(true, "pub.ext"),
            set_op(true, "pub.ext", json!({})),
        ],
    )
    .await;
    assert_eq!(parse_reply_json(&r2[0]), json!({ "a": 9 }));
    assert_eq!(r2[1], ObservedReply::Ok(Reply::Empty));

    // Restart: `{}` persists; then clear with `null` (deletes the file).
    let r3 = run_session_script(
        disk.clone(),
        "/workspace/proj",
        None,
        vec![
            init_op(true, "pub.ext"),
            set_op(true, "pub.ext", Value::Null),
        ],
    )
    .await;
    assert_eq!(parse_reply_json(&r3[0]), json!({}));
    assert_eq!(r3[1], ObservedReply::Ok(Reply::Empty));
    assert!(disk.snapshot().is_empty(), "null clear should delete persisted file");

    // Restart after null clear: returns Empty (`undefined`).
    let r4 = run_session_script(disk.clone(), "/workspace/proj", None, vec![init_op(true, "pub.ext")]).await;
    assert_eq!(r4[0], ObservedReply::Ok(Reply::Empty));

    // Inject a corrupt file directly at the persisted path and verify graceful recovery.
    let roots = ember_editor::StorageRoots::from_env(
        "127.0.0.1:8000",
        &UriComponents::remote("127.0.0.1:8000", "/ose-data/User/globalStorage"),
        &UriComponents::remote("127.0.0.1:8000", "/ose-data/User/workspaceStorage"),
        "/workspace/proj",
    );
    let corrupt_path = roots.file_uri(true, "pub.ext").path;
    disk.insert(corrupt_path, b"{corrupt json".to_vec());

    let r5 = run_session_script(
        disk.clone(),
        "/workspace/proj",
        None,
        vec![
            init_op(true, "pub.ext"),
            set_op(true, "pub.ext", json!({ "recovered": true })),
        ],
    )
    .await;
    assert_eq!(r5[0], ObservedReply::Ok(Reply::Empty));
    assert_eq!(r5[1], ObservedReply::Ok(Reply::Empty));

    let r6 = run_session_script(disk.clone(), "/workspace/proj", None, vec![init_op(true, "pub.ext")]).await;
    assert_eq!(parse_reply_json(&r6[0]), json!({ "recovered": true }));
}

#[tokio::test]
async fn local_storage_dir_persists_atomically_across_restarts() {
    let dir = std::env::temp_dir().join(format!("ember-storage-e2e-{}", uuid::Uuid::new_v4().simple()));
    let disk = FakeRemoteDisk::default();

    let r1 = run_session_script(
        disk.clone(),
        "/workspace/one",
        Some(dir.clone()),
        vec![
            set_op(true, "pub.local", json!({ "g": 42 })),
            set_op(false, "pub.local", json!({ "w": "one" })),
        ],
    )
    .await;
    assert_eq!(r1, vec![ObservedReply::Ok(Reply::Empty), ObservedReply::Ok(Reply::Empty)]);
    assert!(disk.snapshot().is_empty(), "local storage_dir should not write to remote disk");

    let r2 = run_session_script(
        disk.clone(),
        "/workspace/one",
        Some(dir.clone()),
        vec![init_op(true, "pub.local"), init_op(false, "pub.local")],
    )
    .await;
    assert_eq!(parse_reply_json(&r2[0]), json!({ "g": 42 }));
    assert_eq!(parse_reply_json(&r2[1]), json!({ "w": "one" }));

    let r3 = run_session_script(
        disk,
        "/workspace/two",
        Some(dir.clone()),
        vec![init_op(true, "pub.local"), init_op(false, "pub.local")],
    )
    .await;
    assert_eq!(parse_reply_json(&r3[0]), json!({ "g": 42 }));
    assert_eq!(r3[1], ObservedReply::Ok(Reply::Empty));

    let _ = std::fs::remove_dir_all(&dir);
}
