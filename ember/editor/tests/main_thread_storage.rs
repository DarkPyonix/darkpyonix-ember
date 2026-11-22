//! Fake extension-host integration tests for `MainThreadStorage` persistence.
//!
//! Each test runs a real [`EditorSession`] against an in-process scripted Code-OSS server whose
//! extension host sends `MainThreadStorage.$initializeExtensionStorage`, `$setValue`, and
//! `$registerExtensionStorageKeysToSync` requests over the real RPC framing.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ember_editor::{EditorSession, SessionConfig, StorageHub};
use ember_editor_conn::exthost::{proxy, MSG_INITIALIZED, MSG_READY, MSG_TERMINATE};
use ember_editor_conn::frame::{Frame, FrameDecoder, MessageType};
use ember_editor_conn::ipc::{decode_message, encode_message, request_type, response_type, IpcValue};
use ember_editor_conn::rpc::{Arg, Reply, RpcMessage};
use ember_editor_conn::rpc_ids;
use ember_editor_conn::uri::UriComponents;
use ember_editor_conn::PINNED_COMMIT;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio::sync::{mpsc, oneshot, Notify};

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

/// Controllable out-of-order rename gate: delays the first rename of a tmp file whose bytes match
/// `slow_marker` until either a second rename finishes or `timeout` elapses.
#[derive(Clone)]
struct ReorderGate {
    slow_marker: Vec<u8>,
    timeout: Duration,
    notify: Arc<Notify>,
    triggered: Arc<AtomicUsize>,
}

impl ReorderGate {
    fn new(slow_marker: impl Into<Vec<u8>>, timeout: Duration) -> Self {
        Self {
            slow_marker: slow_marker.into(),
            timeout,
            notify: Arc::new(Notify::new()),
            triggered: Arc::new(AtomicUsize::new(0)),
        }
    }
}

/// Virtual filesystem backing the scripted OSE server's `remoteFilesystem` channel across sessions.
#[derive(Default, Clone)]
struct FakeRemoteDisk {
    files: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    read_failures: Arc<Mutex<VecDeque<String>>>,
    write_failures: Arc<Mutex<VecDeque<String>>>,
    write_delay: Arc<Mutex<Option<Duration>>>,
    reorder_gate: Arc<Mutex<Option<ReorderGate>>>,
    write_count: Arc<AtomicUsize>,
    rename_count: Arc<AtomicUsize>,
}

impl FakeRemoteDisk {
    fn snapshot(&self) -> HashMap<String, Vec<u8>> {
        self.files.lock().unwrap().clone()
    }

    fn insert(&self, path: impl Into<String>, bytes: Vec<u8>) {
        self.files.lock().unwrap().insert(path.into(), bytes);
    }

    fn push_read_failure(&self, msg: impl Into<String>) {
        self.read_failures.lock().unwrap().push_back(msg.into());
    }

    fn push_write_failure(&self, msg: impl Into<String>) {
        self.write_failures.lock().unwrap().push_back(msg.into());
    }

    fn set_write_delay(&self, delay: Option<Duration>) {
        *self.write_delay.lock().unwrap() = delay;
    }

    fn set_reorder_gate(&self, gate: Option<ReorderGate>) {
        *self.reorder_gate.lock().unwrap() = gate;
    }

    fn write_count(&self) -> usize {
        self.write_count.load(Ordering::SeqCst)
    }

    fn rename_count(&self) -> usize {
        self.rename_count.load(Ordering::SeqCst)
    }
}

async fn run_fake_management(mut server: DuplexStream, disk: FakeRemoteDisk, authority: String) {
    let (mut dec, ct) = accept_upgraded_handshake(&mut server).await;
    assert_eq!(ct["desiredConnectionType"], 1);

    let (mut reader, mut writer) = tokio::io::split(server);
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<(Vec<u8>, u32)>();

    let writer_task = tokio::spawn(async move {
        let mut srv_id = 0u32;
        while let Some((payload, ack)) = out_rx.recv().await {
            srv_id += 1;
            let frame = Frame::new(MessageType::Regular, srv_id, ack, payload).encode();
            if writer.write_all(&frame).await.is_err() {
                break;
            }
        }
    });

    let init_header = IpcValue::Array(vec![IpcValue::Int(response_type::INITIALIZE)]);
    let _ = out_tx.send((encode_message(&init_header, &IpcValue::Undefined), 0));

    while let Some(frame) = recv_frame(&mut reader, &mut dec).await {
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
            IpcValue::String(s) => s.clone(),
            _ => String::new(),
        };
        let cmd = match &h[3] {
            IpcValue::String(s) => s.clone(),
            _ => String::new(),
        };

        let disk = disk.clone();
        let authority = authority.clone();
        let out_tx = out_tx.clone();
        tokio::spawn(async move {
            let result: Result<IpcValue, Value> = match (channel.as_str(), cmd.as_str()) {
                ("remoteextensionsenvironment", "getEnvironmentData") => {
                    let u = |p: &str| UriComponents::remote(&authority, p);
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
                    let injected = disk.read_failures.lock().unwrap().pop_front();
                    if let Some(msg) = injected {
                        Err(json!({ "name": "IOError", "message": msg }))
                    } else {
                        match disk.files.lock().unwrap().get(&path).cloned() {
                            Some(bytes) => Ok(IpcValue::VsBuffer(bytes)),
                            None => Err(json!({ "name": "EntryNotFound", "message": format!("ENOENT: {path}") })),
                        }
                    }
                }
                ("remoteFilesystem", "writeFile") => {
                    let delay = *disk.write_delay.lock().unwrap();
                    if let Some(d) = delay {
                        tokio::time::sleep(d).await;
                    }
                    let injected = disk.write_failures.lock().unwrap().pop_front();
                    if let Some(msg) = injected {
                        Err(json!({ "name": "IOError", "message": msg }))
                    } else {
                        disk.write_count.fetch_add(1, Ordering::SeqCst);
                        if let IpcValue::Array(args) = &body {
                            let path = args.first().and_then(|a| a.to_json()["path"].as_str().map(str::to_owned)).unwrap_or_default();
                            let bytes = args.get(1).and_then(IpcValue::as_bytes).map(<[u8]>::to_vec).unwrap_or_default();
                            disk.files.lock().unwrap().insert(path, bytes);
                        }
                        Ok(IpcValue::Undefined)
                    }
                }
                ("remoteFilesystem", "rename") => {
                    let (from, to) = if let IpcValue::Array(args) = &body {
                        let from = args.first().and_then(|a| a.to_json()["path"].as_str().map(str::to_owned)).unwrap_or_default();
                        let to = args.get(1).and_then(|a| a.to_json()["path"].as_str().map(str::to_owned)).unwrap_or_default();
                        (from, to)
                    } else {
                        (String::new(), String::new())
                    };
                    let gate = disk.reorder_gate.lock().unwrap().clone();
                    let bytes_preview = disk.files.lock().unwrap().get(&from).cloned();
                    if let Some(gate) = gate {
                        let is_slow = bytes_preview.as_deref() == Some(gate.slow_marker.as_slice());
                        if is_slow && gate.triggered.fetch_add(1, Ordering::SeqCst) == 0 {
                            let _ = tokio::time::timeout(gate.timeout, gate.notify.notified()).await;
                        } else {
                            // Perform the fast rename first, then release the delayed rename.
                            let mut map = disk.files.lock().unwrap();
                            if let Some(bytes) = map.remove(&from) {
                                map.insert(to.clone(), bytes);
                            }
                            drop(map);
                            disk.rename_count.fetch_add(1, Ordering::SeqCst);
                            gate.notify.notify_waiters();
                            let resp_header = IpcValue::Array(vec![
                                IpcValue::Int(response_type::PROMISE_SUCCESS),
                                IpcValue::Int(id),
                            ]);
                            let _ = out_tx.send((encode_message(&resp_header, &IpcValue::Undefined), last_client_id));
                            return;
                        }
                    }
                    let mut map = disk.files.lock().unwrap();
                    if let Some(bytes) = map.remove(&from) {
                        map.insert(to, bytes);
                    }
                    drop(map);
                    disk.rename_count.fetch_add(1, Ordering::SeqCst);
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
            let _ = out_tx.send((encode_message(&resp_header, &resp_body), last_client_id));
        });
    }
    drop(out_tx);
    let _ = writer_task.await;
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
    run_session_script_with(disk, "127.0.0.1:8000", workspace_path, storage_dir, None, ops, false).await
}

async fn run_session_script_with(
    disk: FakeRemoteDisk,
    authority: &str,
    workspace_path: &str,
    storage_dir: Option<PathBuf>,
    storage_hub: Option<StorageHub>,
    ops: Vec<StorageOp>,
    pipelined: bool,
) -> Vec<ObservedReply> {
    let dials = Arc::new(AtomicUsize::new(0));
    let (done_tx, done_rx) = oneshot::channel::<Vec<ObservedReply>>();
    let done_tx = Arc::new(Mutex::new(Some(done_tx)));
    let ops = Arc::new(ops);

    let mut config = SessionConfig::new(authority, workspace_path);
    config.storage_dir = storage_dir;
    config.storage_hub = storage_hub;

    let dials_cb = Arc::clone(&dials);
    let disk_cb = disk.clone();
    let ops_cb = Arc::clone(&ops);
    let done_cb = Arc::clone(&done_tx);
    let auth_cb = authority.to_owned();

    let (session, _updates) = EditorSession::connect(config, move || {
        let n = dials_cb.fetch_add(1, Ordering::SeqCst);
        let disk = disk_cb.clone();
        let ops = Arc::clone(&ops_cb);
        let done = Arc::clone(&done_cb);
        let auth = auth_cb.clone();
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
                    tokio::spawn(run_fake_management(server, disk, auth));
                }
                2 => {
                    tokio::spawn(async move {
                        run_fake_exthost_script(server, &ops, pipelined, done).await;
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
    pipelined: bool,
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
    let mut replies = vec![ObservedReply::Ok(Reply::Empty); ops.len()];
    if pipelined {
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
        }
        let mut remaining = ops.len();
        while remaining > 0 {
            let frame = recv_frame(&mut server, &mut dec).await.expect("storage reply frame");
            if frame.ty == MessageType::Disconnect || frame.data == [MSG_TERMINATE] {
                panic!("extension host terminated with {remaining} unanswered storage reqs");
            }
            if frame.ty != MessageType::Regular {
                continue;
            }
            last_client_id = frame.id;
            match RpcMessage::decode(&frame.data).expect("decode RPC reply") {
                RpcMessage::Acknowledged { .. } => {}
                RpcMessage::ReplyOk { req, reply } if req >= 1 && (req as usize) <= ops.len() => {
                    replies[(req - 1) as usize] = ObservedReply::Ok(reply);
                    remaining -= 1;
                }
                RpcMessage::ReplyErr { req, error } if req >= 1 && (req as usize) <= ops.len() => {
                    replies[(req - 1) as usize] = ObservedReply::Err(error);
                    remaining -= 1;
                }
                RpcMessage::Request { req, .. } => {
                    srv_id += 1;
                    let ok = RpcMessage::ReplyOk { req, reply: Reply::Empty }.encode().unwrap();
                    server.write_all(&Frame::new(MessageType::Regular, srv_id, last_client_id, ok).encode()).await.unwrap();
                }
                _ => {}
            }
        }
    } else {
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
                        replies[idx] = ObservedReply::Ok(reply);
                        break;
                    }
                    RpcMessage::ReplyErr { req, error } if req == req_id => {
                        replies[idx] = ObservedReply::Err(error);
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

    // Inject a corrupt file directly at the persisted path and verify it is moved aside to
    // `*.corrupt` before recovery and subsequent writes proceed.
    let roots = ember_editor::StorageRoots::from_env(
        "127.0.0.1:8000",
        &UriComponents::remote("127.0.0.1:8000", "/ose-data/User/globalStorage"),
        &UriComponents::remote("127.0.0.1:8000", "/ose-data/User/workspaceStorage"),
        "/workspace/proj",
    );
    let corrupt_path = roots.file_uri(true, "pub.ext").path;
    disk.insert(corrupt_path.clone(), b"{corrupt json".to_vec());

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
    assert_eq!(
        disk.snapshot().get(&format!("{corrupt_path}.corrupt")),
        Some(&b"{corrupt json".to_vec()),
        "corrupt file must be preserved at *.corrupt"
    );

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

// ---- Follow-up tests for PR #1026 review items 1..6 -------------------------------------------

#[tokio::test]
async fn set_value_writes_are_serialized_and_coalesced_per_key() {
    let disk = FakeRemoteDisk::default();
    let v1 = serde_json::to_vec(&json!({ "seq": 1 })).unwrap();
    // Delay the first write's rename so an unserialized second write would finish earlier and
    // get overwritten by the stale first write.
    disk.set_reorder_gate(Some(ReorderGate::new(v1, Duration::from_millis(150))));

    let replies = run_session_script_with(
        disk.clone(),
        "127.0.0.1:8000",
        "/workspace/order",
        None,
        None,
        vec![
            set_op(true, "pub.order", json!({ "seq": 1 })),
            set_op(true, "pub.order", json!({ "seq": 2 })),
            set_op(true, "pub.order", json!({ "seq": 3 })),
        ],
        true,
    )
    .await;
    assert_eq!(
        replies,
        vec![
            ObservedReply::Ok(Reply::Empty),
            ObservedReply::Ok(Reply::Empty),
            ObservedReply::Ok(Reply::Empty),
        ]
    );

    let roots = ember_editor::StorageRoots::from_env(
        "127.0.0.1:8000",
        &UriComponents::remote("127.0.0.1:8000", "/ose-data/User/globalStorage"),
        &UriComponents::remote("127.0.0.1:8000", "/ose-data/User/workspaceStorage"),
        "/workspace/order",
    );
    let path = roots.file_uri(true, "pub.order").path;
    let on_disk = disk.snapshot().get(&path).cloned().expect("persisted file should exist");
    let parsed: Value = serde_json::from_slice(&on_disk).expect("valid JSON on disk");
    assert_eq!(parsed, json!({ "seq": 3 }), "latest write must win on disk");
    // Write 2 arrived while write 1 was in flight and was superseded by write 3, so at most 2
    // writes (seq=1 and seq=3) should hit disk.
    assert!(
        disk.write_count() <= 2 && disk.rename_count() <= 2,
        "intermediate write should coalesce (writes={}, renames={})",
        disk.write_count(),
        disk.rename_count()
    );
}

#[tokio::test]
async fn transient_read_error_is_not_cached_and_does_not_allow_overwrite() {
    let disk = FakeRemoteDisk::default();
    let roots = ember_editor::StorageRoots::from_env(
        "127.0.0.1:8000",
        &UriComponents::remote("127.0.0.1:8000", "/ose-data/User/globalStorage"),
        &UriComponents::remote("127.0.0.1:8000", "/ose-data/User/workspaceStorage"),
        "/workspace/err",
    );
    let path = roots.file_uri(true, "pub.err").path;
    let original = br#"{"precious":"data"}"#.to_vec();
    disk.insert(path.clone(), original.clone());

    // First read fails transiently; a subsequent setValue must NOT overwrite the unreadable file;
    // a retry of initialize after the fs recovers must return the original intact bytes.
    disk.push_read_failure("EIO: transient ssh hiccup");
    let replies = run_session_script(
        disk.clone(),
        "/workspace/err",
        None,
        vec![
            init_op(true, "pub.err"),
            set_op(true, "pub.err", json!({ "clobbered": true })),
            init_op(true, "pub.err"),
        ],
    )
    .await;

    assert!(
        matches!(replies[0], ObservedReply::Err(_)),
        "transient read error must reject initialize instead of caching absent: {:?}",
        replies[0]
    );
    assert_eq!(parse_reply_json(&replies[2]), json!({ "precious": "data" }));
    assert_eq!(disk.snapshot().get(&path), Some(&original), "unreadable file must not be overwritten");
}

#[tokio::test]
async fn corrupt_read_moves_file_aside_before_recovery_and_writes() {
    let disk = FakeRemoteDisk::default();
    let roots = ember_editor::StorageRoots::from_env(
        "127.0.0.1:8000",
        &UriComponents::remote("127.0.0.1:8000", "/ose-data/User/globalStorage"),
        &UriComponents::remote("127.0.0.1:8000", "/ose-data/User/workspaceStorage"),
        "/workspace/corrupt",
    );
    let path = roots.file_uri(true, "pub.corrupt").path;
    let corrupt_bytes = b"{not valid json!!!".to_vec();
    disk.insert(path.clone(), corrupt_bytes.clone());

    let replies = run_session_script(
        disk.clone(),
        "/workspace/corrupt",
        None,
        vec![
            init_op(true, "pub.corrupt"),
            set_op(true, "pub.corrupt", json!({ "fresh": 1 })),
        ],
    )
    .await;

    assert_eq!(replies[0], ObservedReply::Ok(Reply::Empty));
    assert_eq!(replies[1], ObservedReply::Ok(Reply::Empty));

    let snap = disk.snapshot();
    let backup_path = format!("{path}.corrupt");
    assert_eq!(
        snap.get(&backup_path),
        Some(&corrupt_bytes),
        "corrupt file must be moved aside to *.corrupt with original bytes intact (keys: {:?})",
        snap.keys().collect::<Vec<_>>()
    );
    let current: Value = serde_json::from_slice(snap.get(&path).expect("recovered file")).unwrap();
    assert_eq!(current, json!({ "fresh": 1 }));
}

/// Interactive fake extension host that records incoming `ExtHostStorage.$acceptValue` calls.
async fn connect_interactive_session(
    disk: FakeRemoteDisk,
    authority: &str,
    workspace_path: &str,
    storage_hub: StorageHub,
) -> (
    EditorSession,
    mpsc::UnboundedSender<(StorageOp, oneshot::Sender<ObservedReply>)>,
    mpsc::UnboundedReceiver<(bool, String, String)>,
) {
    let dials = Arc::new(AtomicUsize::new(0));
    let (op_tx, op_rx) = mpsc::unbounded_channel::<(StorageOp, oneshot::Sender<ObservedReply>)>();
    let op_rx = Arc::new(Mutex::new(Some(op_rx)));
    let (accept_tx, accept_rx) = mpsc::unbounded_channel::<(bool, String, String)>();

    let mut config = SessionConfig::new(authority, workspace_path);
    config.storage_hub = Some(storage_hub);

    let dials_cb = Arc::clone(&dials);
    let disk_cb = disk.clone();
    let auth_cb = authority.to_owned();
    let op_rx_cb = Arc::clone(&op_rx);
    let accept_tx_cb = accept_tx.clone();

    let (session, _updates) = EditorSession::connect(config, move || {
        let n = dials_cb.fetch_add(1, Ordering::SeqCst);
        let disk = disk_cb.clone();
        let auth = auth_cb.clone();
        let op_rx = Arc::clone(&op_rx_cb);
        let accept_tx = accept_tx_cb.clone();
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
                    tokio::spawn(run_fake_management(server, disk, auth));
                }
                2 => {
                    let mut op_rx = op_rx.lock().unwrap().take().unwrap();
                    tokio::spawn(async move {
                        let (mut dec, ct) = accept_upgraded_handshake(&mut server).await;
                        assert_eq!(ct["desiredConnectionType"], 2);

                        let mut srv_id = 0u32;
                        let mut last_client_id = 0u32;
                        srv_id += 1;
                        server
                            .write_all(&Frame::new(MessageType::Regular, srv_id, last_client_id, vec![MSG_READY]).encode())
                            .await
                            .unwrap();
                        let init_frame = recv_frame(&mut server, &mut dec).await.expect("initData");
                        last_client_id = init_frame.id;
                        srv_id += 1;
                        server
                            .write_all(
                                &Frame::new(MessageType::Regular, srv_id, last_client_id, vec![MSG_INITIALIZED]).encode(),
                            )
                            .await
                            .unwrap();

                        let mut bootstrap_seen = 0usize;
                        while bootstrap_seen < 2 {
                            let frame = recv_frame(&mut server, &mut dec).await.expect("bootstrap");
                            if frame.ty != MessageType::Regular {
                                continue;
                            }
                            last_client_id = frame.id;
                            if let RpcMessage::Request { req, .. } = RpcMessage::decode(&frame.data).unwrap() {
                                bootstrap_seen += 1;
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
                            }
                        }

                        let main_storage_id = rpc_ids::id_of(proxy::MAIN_THREAD_STORAGE).unwrap();
                        let ext_storage_id = rpc_ids::id_of("ExtHostStorage").unwrap();
                        let mut next_req = 1u32;
                        let mut pending: HashMap<u32, oneshot::Sender<ObservedReply>> = HashMap::new();
                        let mut chunk = [0u8; 4096];

                        loop {
                            while let Some(frame) = dec.next_frame().expect("decode frame") {
                                if frame.ty == MessageType::Disconnect || frame.data == [MSG_TERMINATE] {
                                    return;
                                }
                                if frame.ty != MessageType::Regular {
                                    continue;
                                }
                                last_client_id = frame.id;
                                match RpcMessage::decode(&frame.data).expect("decode RPC") {
                                    RpcMessage::Acknowledged { .. } => {}
                                    RpcMessage::ReplyOk { req, reply } => {
                                        if let Some(tx) = pending.remove(&req) {
                                            let _ = tx.send(ObservedReply::Ok(reply));
                                        }
                                    }
                                    RpcMessage::ReplyErr { req, error } => {
                                        if let Some(tx) = pending.remove(&req) {
                                            let _ = tx.send(ObservedReply::Err(error));
                                        }
                                    }
                                    RpcMessage::Request { req, rpc_id, method, args, .. } => {
                                        if rpc_id == ext_storage_id && method == "$acceptValue" {
                                            let shared = args.first().and_then(Arg::as_json).and_then(Value::as_bool).unwrap_or(false);
                                            let key = args.get(1).and_then(Arg::as_json).and_then(Value::as_str).unwrap_or("").to_owned();
                                            let val = args.get(2).and_then(Arg::as_json).and_then(Value::as_str).unwrap_or("").to_owned();
                                            let _ = accept_tx.send((shared, key, val));
                                        }
                                        srv_id += 1;
                                        let ok = RpcMessage::ReplyOk { req, reply: Reply::Empty }.encode().unwrap();
                                        let _ = server
                                            .write_all(&Frame::new(MessageType::Regular, srv_id, last_client_id, ok).encode())
                                            .await;
                                    }
                                    _ => {}
                                }
                            }

                            tokio::select! {
                                Some((op, reply_tx)) = op_rx.recv() => {
                                    let req_id = next_req;
                                    next_req += 1;
                                    pending.insert(req_id, reply_tx);
                                    let payload = RpcMessage::Request {
                                        req: req_id,
                                        rpc_id: main_storage_id,
                                        method: op.method.to_owned(),
                                        args: op.args,
                                        cancellable: false,
                                    }
                                    .encode()
                                    .unwrap();
                                    srv_id += 1;
                                    if server
                                        .write_all(&Frame::new(MessageType::Regular, srv_id, last_client_id, payload).encode())
                                        .await
                                        .is_err()
                                    {
                                        return;
                                    }
                                }
                                read = server.read(&mut chunk) => {
                                    match read {
                                        Ok(0) | Err(_) => return,
                                        Ok(n) => dec.push(&chunk[..n]),
                                    }
                                }
                            }
                        }
                    });
                }
                _ => panic!("unexpected dial {n}"),
            }
            Ok(client)
        }
    })
    .await
    .expect("EditorSession::connect");

    (session, op_tx, accept_rx)
}

#[tokio::test]
async fn shared_set_value_broadcasts_accept_value_to_other_sessions() {
    let disk = FakeRemoteDisk::default();
    let hub = StorageHub::new();

    let (sess_a, ops_a, mut accept_a) =
        connect_interactive_session(disk.clone(), "127.0.0.1:8000", "/workspace/a", hub.clone()).await;
    let (sess_b, ops_b, mut accept_b) =
        connect_interactive_session(disk.clone(), "127.0.0.1:8000", "/workspace/b", hub.clone()).await;

    // Session A writes a shared=true key.
    let (tx, rx) = oneshot::channel();
    ops_a.send((set_op(true, "pub.shared", json!({ "from": "session-a" })), tx)).unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), rx).await.unwrap().unwrap(),
        ObservedReply::Ok(Reply::Empty)
    );

    // Session B must receive ExtHostStorage.$acceptValue(true, "pub.shared", ...).
    let (shared, key, val) = tokio::time::timeout(Duration::from_secs(2), accept_b.recv())
        .await
        .expect("timed out waiting for $acceptValue on session B")
        .expect("session B accept channel closed");
    assert!(shared);
    assert_eq!(key, "pub.shared");
    assert_eq!(serde_json::from_str::<Value>(&val).unwrap(), json!({ "from": "session-a" }));

    // Session B's cache must also reflect the updated value immediately.
    let (tx, rx) = oneshot::channel();
    ops_b.send((init_op(true, "pub.shared"), tx)).unwrap();
    let reply_b = tokio::time::timeout(Duration::from_secs(5), rx).await.unwrap().unwrap();
    assert_eq!(parse_reply_json(&reply_b), json!({ "from": "session-a" }));

    // Session A must NOT receive an echo of its own write.
    assert!(accept_a.try_recv().is_err(), "originating session must not receive $acceptValue echo");

    sess_a.shutdown().await;
    sess_b.shutdown().await;
}

#[tokio::test]
async fn shutdown_drains_delayed_storage_writes_before_exiting() {
    let disk = FakeRemoteDisk::default();
    disk.set_write_delay(Some(Duration::from_millis(150)));
    let hub = StorageHub::new();

    let (session, ops, _accept) =
        connect_interactive_session(disk.clone(), "127.0.0.1:8000", "/workspace/drain", hub).await;

    // Send $setValue and immediately shut down without sleeping.
    let t0 = std::time::Instant::now();
    let (tx, rx) = oneshot::channel();
    ops.send((set_op(true, "pub.drain", json!({ "flushed": true })), tx)).unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), rx).await.unwrap().unwrap(),
        ObservedReply::Ok(Reply::Empty)
    );
    assert!(
        t0.elapsed() < Duration::from_millis(120),
        "$setValue should resolve immediately before the 150ms disk write finishes (took {:?})",
        t0.elapsed()
    );
    session.shutdown().await;

    let roots = ember_editor::StorageRoots::from_env(
        "127.0.0.1:8000",
        &UriComponents::remote("127.0.0.1:8000", "/ose-data/User/globalStorage"),
        &UriComponents::remote("127.0.0.1:8000", "/ose-data/User/workspaceStorage"),
        "/workspace/drain",
    );
    let path = roots.file_uri(true, "pub.drain").path;
    let on_disk = disk
        .snapshot()
        .get(&path)
        .cloned()
        .expect("delayed storage write must land on disk before shutdown returns");
    assert_eq!(serde_json::from_slice::<Value>(&on_disk).unwrap(), json!({ "flushed": true }));
}

#[tokio::test]
async fn set_value_resolves_immediately_and_retries_on_transient_fs_error() {
    let disk = FakeRemoteDisk::default();
    disk.push_write_failure("EIO: transient disk error 1");
    disk.push_write_failure("EIO: transient disk error 2");

    let replies = run_session_script(
        disk.clone(),
        "/workspace/retry",
        None,
        vec![set_op(true, "pub.retry", json!({ "retried": 42 }))],
    )
    .await;

    assert_eq!(
        replies[0],
        ObservedReply::Ok(Reply::Empty),
        "$setValue must resolve immediately without rejecting on fs errors"
    );

    let roots = ember_editor::StorageRoots::from_env(
        "127.0.0.1:8000",
        &UriComponents::remote("127.0.0.1:8000", "/ose-data/User/globalStorage"),
        &UriComponents::remote("127.0.0.1:8000", "/ose-data/User/workspaceStorage"),
        "/workspace/retry",
    );
    let path = roots.file_uri(true, "pub.retry").path;
    let on_disk = disk
        .snapshot()
        .get(&path)
        .cloned()
        .expect("retried write must persist once fs recovers");
    assert_eq!(serde_json::from_slice::<Value>(&on_disk).unwrap(), json!({ "retried": 42 }));
}

#[tokio::test]
async fn local_mode_workspace_state_isolates_same_path_across_authorities() {
    let dir = std::env::temp_dir().join(format!("ember-storage-auth-{}", uuid::Uuid::new_v4().simple()));
    let disk = FakeRemoteDisk::default();

    // Server A writes workspaceState for /workspace/shared-path.
    let r_a = run_session_script_with(
        disk.clone(),
        "server-a:8000",
        "/workspace/shared-path",
        Some(dir.clone()),
        None,
        vec![set_op(false, "pub.auth", json!({ "server": "A" }))],
        false,
    )
    .await;
    assert_eq!(r_a[0], ObservedReply::Ok(Reply::Empty));

    // Server B has the exact same workspace_path and local storage_dir, but a different authority:
    // it must NOT see server A's workspaceState.
    let r_b = run_session_script_with(
        disk.clone(),
        "server-b:8000",
        "/workspace/shared-path",
        Some(dir.clone()),
        None,
        vec![
            init_op(false, "pub.auth"),
            set_op(false, "pub.auth", json!({ "server": "B" })),
        ],
        false,
    )
    .await;
    assert_eq!(
        r_b[0],
        ObservedReply::Ok(Reply::Empty),
        "different authorities with same workspace_path must not collide in local workspaceState"
    );
    assert_eq!(r_b[1], ObservedReply::Ok(Reply::Empty));

    // Reconnecting to Server A still sees Server A's workspaceState intact.
    let r_a2 = run_session_script_with(
        disk,
        "server-a:8000",
        "/workspace/shared-path",
        Some(dir.clone()),
        None,
        vec![init_op(false, "pub.auth")],
        false,
    )
    .await;
    assert_eq!(parse_reply_json(&r_a2[0]), json!({ "server": "A" }));

    let _ = std::fs::remove_dir_all(&dir);
}
