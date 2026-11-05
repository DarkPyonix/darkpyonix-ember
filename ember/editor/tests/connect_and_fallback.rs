//! Scripted end-to-end integration tests for `EditorSession::connect_or_fallback` and
//! `EditorSession::connect_tcp_or_fallback` (M8 / Issue #32):
//!
//! 1. Connect-time version check (`GET /version` vs `PINNED_COMMIT`) and fallback to the VS Code
//!    target (`<ide_url>/?folder=<workspace_path>`) when the commit differs, is missing/404, is an
//!    empty body, is non-version HTML, or when the server replies `"Client refused: version
//!    mismatch"` during the handshake.
//! 2. Non-version errors (such as auth refusal) surface as `Err` without switching targets.
//! 3. Full "Open IDE -> Ember" flow against an in-process scripted Code-OSS server at
//!    `PINNED_COMMIT`: connect, list workspace files (`read_dir` / `stat`), open a project file,
//!    edit it, receive linter diagnostics (`MainThreadDiagnostics.$changeMany` -> Error underline
//!    decoration), and save (`writeFile` + `$acceptModelSaved`).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ember_editor::{
    ConnectOutcome, DecorationKind, EditorSession, Error, Fallback, FallbackTarget, SessionConfig,
    SessionUpdate, Severity, WidgetEvent, WidgetPos, WidgetRange,
};
use ember_editor_conn::exthost::{proxy, MSG_INITIALIZED, MSG_READY, MSG_TERMINATE};
use ember_editor_conn::frame::{Frame, FrameDecoder, MessageType};
use ember_editor_conn::ipc::{decode_message, deserialize, encode_message, request_type, response_type, IpcValue};
use ember_editor_conn::remote_fs::file_type;
use ember_editor_conn::rpc::{Arg, Reply, RpcMessage};
use ember_editor_conn::rpc_ids;
use ember_editor_conn::uri::UriComponents;
use ember_editor_conn::PINNED_COMMIT;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio::sync::mpsc;

const INITIAL_JSON: &str = "{\n  \"name\": \"ember\",\n  \"ok\": true\n}\n";
const EDITED_JSON: &str = "{\n  \"name\": \"ember\",\n  \"ok\": true,,\n}\n";

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

/// Complete the WebSocket upgrade + `auth -> sign -> connectionType -> ok` handshake on `server`,
/// returning a `FrameDecoder` primed with any trailing bytes and the parsed `connectionType` JSON.
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

    // 1. Auth
    let auth_frame = recv_frame(server, &mut dec).await.expect("auth frame");
    assert_eq!(auth_frame.ty, MessageType::Control);
    let auth: Value = serde_json::from_slice(&auth_frame.data).unwrap();
    assert_eq!(auth["type"], "auth");

    // 2. Sign
    let sign = br#"{"type":"sign","data":"challenge","signedData":"sig"}"#.to_vec();
    server
        .write_all(&Frame::new(MessageType::Control, 0, 0, sign).encode())
        .await
        .unwrap();

    // 3. ConnectionType
    let ct_frame = recv_frame(server, &mut dec).await.expect("connectionType frame");
    assert_eq!(ct_frame.ty, MessageType::Control);
    let ct: Value = serde_json::from_slice(&ct_frame.data).unwrap();
    assert_eq!(ct["type"], "connectionType");
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

#[tokio::test]
async fn version_mismatch_commit_falls_back_to_vscode_target_before_upgrade() {
    let other_commit = "0036dcb6c18a603d2168c00fa578be600987ac7b";
    let dials = Arc::new(AtomicUsize::new(0));
    let dials_for_cb = Arc::clone(&dials);

    let config = SessionConfig::new("127.0.0.1:8000", "/workspace/project")
        .with_vscode_url("http://127.0.0.1:8890/");

    let outcome = EditorSession::connect_or_fallback(config, move || {
        let dials = Arc::clone(&dials_for_cb);
        async move {
            let n = dials.fetch_add(1, Ordering::SeqCst);
            assert_eq!(n, 0, "version mismatch at GET /version must not dial management or exthost");
            let (client, mut server) = tokio::io::duplex(4096);
            tokio::spawn(async move {
                let (head, _) = read_http_head(&mut server).await;
                assert!(head.starts_with("GET /version HTTP/1.0"));
                let resp = format!("HTTP/1.0 200 OK\r\nContent-Type: text/plain\r\n\r\n{other_commit}\n");
                server.write_all(resp.as_bytes()).await.unwrap();
            });
            Ok(client)
        }
    })
    .await
    .expect("connect_or_fallback should succeed with Fallback");

    assert_eq!(dials.load(Ordering::SeqCst), 1);
    let ConnectOutcome::Fallback(target) = outcome else {
        panic!("expected Fallback, got {outcome:?}");
    };
    assert_eq!(
        target,
        FallbackTarget {
            fallback: Fallback::OpenInVsCode { server_commit: Some(other_commit.into()) },
            vscode_url: Some("http://127.0.0.1:8890/?folder=/workspace/project".into()),
        }
    );
    assert!(target.fallback.message().contains(other_commit));
}

#[tokio::test]
async fn unknown_version_404_or_empty_or_html_falls_back_to_vscode_target() {
    let responses = [
        "HTTP/1.0 404 Not Found\r\nContent-Length: 9\r\n\r\nNot Found",
        "HTTP/1.0 200 OK\r\nContent-Type: text/plain\r\n\r\n",
        "HTTP/1.0 200 OK\r\nContent-Type: text/html\r\n\r\n<!DOCTYPE html><html><body>proxy</body></html>",
    ];

    for raw_resp in responses {
        let dials = Arc::new(AtomicUsize::new(0));
        let dials_for_cb = Arc::clone(&dials);
        let config = SessionConfig::new("127.0.0.1:8000", "/workspace/my project")
            .with_vscode_url("http://127.0.0.1:8890");

        let outcome = EditorSession::connect_or_fallback(config, move || {
            let dials = Arc::clone(&dials_for_cb);
            let resp = raw_resp.to_owned();
            async move {
                dials.fetch_add(1, Ordering::SeqCst);
                let (client, mut server) = tokio::io::duplex(4096);
                tokio::spawn(async move {
                    let (head, _) = read_http_head(&mut server).await;
                    assert!(head.starts_with("GET /version HTTP/1.0"));
                    server.write_all(resp.as_bytes()).await.unwrap();
                });
                Ok(client)
            }
        })
        .await
        .expect("unknown version response should fall back cleanly");

        assert_eq!(dials.load(Ordering::SeqCst), 1);
        let ConnectOutcome::Fallback(target) = outcome else {
            panic!("expected Fallback for response {raw_resp:?}, got {outcome:?}");
        };
        assert_eq!(
            target,
            FallbackTarget {
                fallback: Fallback::OpenInVsCode { server_commit: None },
                vscode_url: Some("http://127.0.0.1:8890/?folder=/workspace/my%20project".into()),
            }
        );
        assert!(target.fallback.message().contains("version is unknown"));
    }
}

#[tokio::test]
async fn handshake_version_mismatch_falls_back_to_vscode_target() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        // 1. GET /version returns PINNED_COMMIT
        let (mut s1, _) = listener.accept().await.unwrap();
        let (head, _) = read_http_head(&mut s1).await;
        assert!(head.starts_with("GET /version HTTP/1.0"));
        let resp = format!("HTTP/1.0 200 OK\r\nContent-Type: text/plain\r\n\r\n{PINNED_COMMIT}");
        s1.write_all(resp.as_bytes()).await.unwrap();
        drop(s1);

        // 2. Management upgrade: server refuses with "Client refused: version mismatch"
        let (mut s2, _) = listener.accept().await.unwrap();
        let (_head, leftover) = read_http_head(&mut s2).await;
        let mut dec = FrameDecoder::new();
        dec.push(&leftover);
        s2.write_all(b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n")
            .await
            .unwrap();
        let _auth = recv_frame(&mut s2, &mut dec).await.unwrap();
        let err = br#"{"type":"error","reason":"Client refused: version mismatch"}"#.to_vec();
        s2.write_all(&Frame::new(MessageType::Control, 0, 0, err).encode()).await.unwrap();
        let mut buf = [0u8; 64];
        let _ = s2.read(&mut buf).await;
    });

    let config = SessionConfig::new(addr.to_string(), "/workspace/project")
        .with_vscode_url("http://127.0.0.1:8890/");
    let outcome = EditorSession::connect_tcp_or_fallback(config).await.unwrap();
    let ConnectOutcome::Fallback(target) = outcome else {
        panic!("expected Fallback, got {outcome:?}");
    };
    assert_eq!(
        target,
        FallbackTarget {
            fallback: Fallback::OpenInVsCode { server_commit: None },
            vscode_url: Some("http://127.0.0.1:8890/?folder=/workspace/project".into()),
        }
    );
}

#[tokio::test]
async fn network_or_auth_errors_do_not_fall_back() {
    let dials = Arc::new(AtomicUsize::new(0));
    let dials_for_cb = Arc::clone(&dials);
    let config = SessionConfig::new("127.0.0.1:8000", "/workspace/project")
        .with_vscode_url("http://127.0.0.1:8890/");

    let err = EditorSession::connect_or_fallback(config, move || {
        let dials = Arc::clone(&dials_for_cb);
        async move {
            let step = dials.fetch_add(1, Ordering::SeqCst);
            let (client, mut server) = tokio::io::duplex(16 * 1024);
            tokio::spawn(async move {
                if step == 0 {
                    let _ = read_http_head(&mut server).await;
                    let resp = format!("HTTP/1.0 200 OK\r\nContent-Type: text/plain\r\n\r\n{PINNED_COMMIT}");
                    server.write_all(resp.as_bytes()).await.unwrap();
                } else {
                    let (_, leftover) = read_http_head(&mut server).await;
                    let mut dec = FrameDecoder::new();
                    dec.push(&leftover);
                    server
                        .write_all(b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n")
                        .await
                        .unwrap();
                    let _auth = recv_frame(&mut server, &mut dec).await.unwrap();
                    let refused = br#"{"type":"error","reason":"Unauthorized client refused: auth mismatch"}"#.to_vec();
                    server
                        .write_all(&Frame::new(MessageType::Control, 0, 0, refused).encode())
                        .await
                        .unwrap();
                    let mut buf = [0u8; 64];
                    let _ = server.read(&mut buf).await;
                }
            });
            Ok(client)
        }
    })
    .await
    .expect_err("auth failure must not fall back to VS Code");

    assert!(matches!(err, Error::Conn(ember_editor_conn::Error::Refused(ref r)) if r.contains("auth mismatch")));
}

/// Drive the fake management connection (`remoteextensionsenvironment`, `remoteExtensionsScanner`,
/// `remoteFilesystem`).
async fn run_fake_management(mut server: DuplexStream, disk: Arc<Mutex<Vec<u8>>>) {
    let (mut dec, ct) = accept_upgraded_handshake(&mut server).await;
    assert_eq!(ct["desiredConnectionType"], 1);

    let mut srv_id = 0u32;
    let mut last_client_id = 0u32;
    let mut send_regular = |server: &mut DuplexStream, payload: Vec<u8>, ack: u32| {
        srv_id += 1;
        let frame = Frame::new(MessageType::Regular, srv_id, ack, payload).encode();
        let _ = server;
        frame
    };

    // 1. Read client's IPC context message and client's Initialize ([200]).
    let ctx_frame = recv_frame(&mut server, &mut dec).await.expect("ipc context frame");
    assert_eq!(ctx_frame.ty, MessageType::Regular);
    last_client_id = ctx_frame.id;
    let mut pos = 0;
    let ctx = deserialize(&ctx_frame.data, &mut pos).unwrap().to_json();
    assert_eq!(ctx["remoteAuthority"], "127.0.0.1:8000");

    // Send server's Initialize ([200]).
    let init_bytes = encode_message(
        &IpcValue::Array(vec![IpcValue::Int(response_type::INITIALIZE)]),
        &IpcValue::Undefined,
    );
    let init_frame = send_regular(&mut server, init_bytes, last_client_id);
    server.write_all(&init_frame).await.unwrap();

    while let Some(frame) = recv_frame(&mut server, &mut dec).await {
        if frame.ty == MessageType::Disconnect {
            break;
        }
        if frame.ty != MessageType::Regular {
            continue;
        }
        last_client_id = frame.id;
        let (header, body) = decode_message(&frame.data).unwrap();
        let IpcValue::Array(h) = header else { continue };
        let Some(req_ty) = h.first().and_then(IpcValue::as_int).map(|n| n as i32) else {
            continue;
        };
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
            ("remoteExtensionsScanner", "scanExtensions") => IpcValue::json(&json!([
                {
                    "identifier": { "value": "vscode.json-language-features", "_lower": "vscode.json-language-features" },
                    "main": "./client/dist/node/jsonClientMain",
                    "activationEvents": ["onLanguage:json"],
                    "contributes": {
                        "languages": [
                            { "id": "json", "extensions": [".json"] }
                        ],
                        "configuration": {
                            "properties": {
                                "json.validate.enable": { "type": "boolean", "default": true, "scope": "resource" }
                            }
                        }
                    }
                }
            ]))
            .unwrap(),
            ("remoteFilesystem", "readdir") => IpcValue::Array(vec![IpcValue::Array(vec![
                IpcValue::str("data.json"),
                IpcValue::Int(file_type::FILE as i32),
            ])]),
            ("remoteFilesystem", "stat") => {
                let len = disk.lock().unwrap().len() as u64;
                IpcValue::json(&json!({
                    "type": file_type::FILE,
                    "mtime": 1700000000000.0,
                    "ctime": 1700000000000.0,
                    "size": len,
                }))
                .unwrap()
            }
            ("remoteFilesystem", "readFile") => {
                let bytes = disk.lock().unwrap().clone();
                IpcValue::VsBuffer(bytes)
            }
            ("remoteFilesystem", "writeFile") => {
                if let IpcValue::Array(args) = &body {
                    if let Some(bytes) = args.get(1).and_then(IpcValue::as_bytes) {
                        *disk.lock().unwrap() = bytes.to_vec();
                    }
                }
                IpcValue::Undefined
            }
            ("remoteFilesystem", "watch") | ("remoteFilesystem", "unwatch") => IpcValue::Undefined,
            other => panic!("unexpected IPC call: {other:?}"),
        };

        let resp_header = IpcValue::Array(vec![
            IpcValue::Int(response_type::PROMISE_SUCCESS),
            IpcValue::Int(id),
        ]);
        let f = send_regular(&mut server, encode_message(&resp_header, &reply_body), last_client_id);
        if server.write_all(&f).await.is_err() {
            break;
        }
    }
}

/// Drive the fake extension-host connection:
/// Ready -> initData -> Initialized -> RPC calls (`$initializeConfiguration`,
/// `$initializeWorkspace`, `$activateByEvent`, `$acceptDocumentsAndEditorsDelta`,
/// `$acceptModelChanged` -> emit `MainThreadDiagnostics.$changeMany`, `$acceptModelSaved`).
async fn run_fake_exthost(
    mut server: DuplexStream,
    events_tx: mpsc::UnboundedSender<(String, Value)>,
) {
    let (mut dec, ct) = accept_upgraded_handshake(&mut server).await;
    assert_eq!(ct["desiredConnectionType"], 2);

    let mut srv_id = 0u32;
    let mut last_client_id = 0u32;
    let mut exthost_req_seq = 0u32;

    // 1. Send Ready (1 byte `[2]`)
    srv_id += 1;
    server
        .write_all(&Frame::new(MessageType::Regular, srv_id, last_client_id, vec![MSG_READY]).encode())
        .await
        .unwrap();

    // 2. Receive IExtensionHostInitData JSON
    let init_frame = recv_frame(&mut server, &mut dec).await.expect("initData frame");
    assert_eq!(init_frame.ty, MessageType::Regular);
    last_client_id = init_frame.id;
    let init_json: Value = serde_json::from_slice(&init_frame.data).expect("valid initData JSON");
    assert_eq!(init_json["commit"].as_str(), Some(PINNED_COMMIT));
    assert_eq!(init_json["workspace"]["name"].as_str(), Some("project"));

    // 3. Send Initialized (1 byte `[1]`)
    srv_id += 1;
    server
        .write_all(&Frame::new(MessageType::Regular, srv_id, last_client_id, vec![MSG_INITIALIZED]).encode())
        .await
        .unwrap();

    // 4. Process RPC traffic
    while let Some(frame) = recv_frame(&mut server, &mut dec).await {
        if frame.ty == MessageType::Disconnect {
            break;
        }
        if frame.ty != MessageType::Regular {
            continue;
        }
        last_client_id = frame.id;
        if frame.data == [MSG_TERMINATE] {
            break;
        }
        let msg = RpcMessage::decode(&frame.data).expect("decode RPC message");
        if let RpcMessage::Request { req, rpc_id, method, args, .. } = msg {
            let proxy_name = rpc_ids::name_of(rpc_id).unwrap_or("unknown");
            let json_args: Vec<Value> = args.iter().map(|a| a.as_json().cloned().unwrap_or(Value::Null)).collect();
            let _ = events_tx.send((format!("{proxy_name}.{method}"), Value::Array(json_args.clone())));

            // Acknowledge and reply OK Empty to every request from Ember.
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

            // When the user edits `data.json` (`ExtHostDocuments.$acceptModelChanged`), act like
            // the unmodified JSON linter extension and push an Error diagnostic on line 3 (`,,`).
            if proxy_name == proxy::EXT_HOST_DOCUMENTS && method == "$acceptModelChanged" {
                let doc_uri = json_args.first().cloned().unwrap_or(Value::Null);
                exthost_req_seq += 1;
                let diag_rpc_id = rpc_ids::id_of(proxy::MAIN_THREAD_DIAGNOSTICS).unwrap();
                let change_many = RpcMessage::Request {
                    req: exthost_req_seq,
                    rpc_id: diag_rpc_id,
                    method: "$changeMany".into(),
                    args: vec![
                        Arg::Json(json!("json")),
                        Arg::Json(json!([
                            [
                                doc_uri,
                                [
                                    {
                                        "severity": 8,
                                        "message": "Expected property name or '}' in JSON",
                                        "source": "json",
                                        "startLineNumber": 3,
                                        "startColumn": 13,
                                        "endLineNumber": 3,
                                        "endColumn": 15
                                    }
                                ]
                            ]
                        ])),
                    ],
                    cancellable: false,
                }
                .encode()
                .unwrap();
                srv_id += 1;
                server
                    .write_all(&Frame::new(MessageType::Regular, srv_id, last_client_id, change_many).encode())
                    .await
                    .unwrap();
            }
        }
    }
}

async fn wait_for<T>(
    what: &str,
    rx: &mut mpsc::UnboundedReceiver<SessionUpdate>,
    mut pick: impl FnMut(&SessionUpdate) -> Option<T>,
) -> T {
    let fut = async {
        loop {
            match rx.recv().await {
                Some(u) => {
                    if let SessionUpdate::ConnectionLost { reason } = &u {
                        panic!("connection lost while waiting for {what}: {reason}");
                    }
                    if let Some(v) = pick(&u) {
                        return v;
                    }
                }
                None => panic!("session ended while waiting for {what}"),
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(10), fut)
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
}

#[tokio::test]
async fn pinned_commit_connects_lists_workspace_opens_edits_shows_diagnostics_and_saves() {
    let disk = Arc::new(Mutex::new(INITIAL_JSON.as_bytes().to_vec()));
    let disk_for_srv = Arc::clone(&disk);
    let (rpc_log_tx, mut rpc_log_rx) = mpsc::unbounded_channel();
    let dials = Arc::new(AtomicUsize::new(0));
    let dials_for_cb = Arc::clone(&dials);

    let config = SessionConfig::new("127.0.0.1:8000", "/workspace/project")
        .with_vscode_url("http://127.0.0.1:8890/");

    let outcome = EditorSession::connect_or_fallback(config, move || {
        let step = dials_for_cb.fetch_add(1, Ordering::SeqCst);
        let disk = Arc::clone(&disk_for_srv);
        let rpc_log_tx = rpc_log_tx.clone();
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
                    tokio::spawn(run_fake_management(server, disk));
                }
                2 => {
                    tokio::spawn(run_fake_exthost(server, rpc_log_tx));
                }
                other => panic!("unexpected dial #{other}"),
            }
            Ok(client)
        }
    })
    .await
    .expect("connect_or_fallback at PINNED_COMMIT should connect");

    let ConnectOutcome::Connected { session, mut updates } = outcome else {
        panic!("expected Connected, got {outcome:?}");
    };
    assert_eq!(dials.load(Ordering::SeqCst), 3);

    // 1. Browse the project workspace via RemoteFs (`workspace_uri`, `read_dir`, `stat`).
    let ws_uri = session.workspace_uri();
    assert_eq!(ws_uri.path, "/workspace/project");
    let entries = session.read_dir(&ws_uri).await.expect("read_dir workspace");
    assert_eq!(entries, vec![("data.json".to_string(), file_type::FILE)]);

    let doc_uri = session.uri("/workspace/project/data.json");
    let st = session.stat(&doc_uri).await.expect("stat data.json");
    assert!(st.is_file());
    assert_eq!(st.size, INITIAL_JSON.len() as u64);

    // 2. Open `data.json` in the Ember editor.
    session.open(doc_uri.clone()).unwrap();
    let generation = wait_for("Opened", &mut updates, |u| match u {
        SessionUpdate::Opened { uri, generation, text, language_id, .. } if uri.path == doc_uri.path => {
            assert_eq!(text, INITIAL_JSON);
            assert_eq!(language_id, "json");
            Some(*generation)
        }
        SessionUpdate::OpenFailed { error, .. } => panic!("open failed: {error}"),
        _ => None,
    })
    .await;

    // 3. Edit `data.json`: insert `,,` after `true` on 0-based line 2, UTF-16 col 12.
    session
        .widget_event(
            doc_uri.clone(),
            generation,
            WidgetEvent::CodeChanged {
                version: 1,
                range: WidgetRange::empty(WidgetPos::new(2, 12)),
                text: ",,".into(),
            },
        )
        .unwrap();

    // 4. Expect the Error underline decoration from the extension host's `$changeMany`.
    let diags = wait_for("Error underline decoration", &mut updates, |u| match u {
        SessionUpdate::Decorations { uri, generation: g, decorations }
            if uri.path == doc_uri.path && *g == generation =>
        {
            let errs: Vec<_> = decorations
                .iter()
                .filter(|d| d.kind == DecorationKind::Underline && d.severity == Some(Severity::Error))
                .cloned()
                .collect();
            (!errs.is_empty()).then_some(errs)
        }
        SessionUpdate::Desync { error, .. } => panic!("desync: {error}"),
        _ => None,
    })
    .await;
    assert_eq!(diags.len(), 1);
    assert_eq!(diags[0].version, 1);
    assert_eq!(diags[0].range, WidgetRange::new(WidgetPos::new(2, 12), WidgetPos::new(2, 14)));

    // 5. Save `data.json` and verify both `writeFile` and `$acceptModelSaved`.
    session
        .widget_event(doc_uri.clone(), generation, WidgetEvent::CodeSaveRequested { version: 1 })
        .unwrap();
    wait_for("Saved", &mut updates, |u| match u {
        SessionUpdate::Saved { uri } if uri.path == doc_uri.path => Some(()),
        SessionUpdate::SaveFailed { error, .. } => panic!("save failed: {error}"),
        _ => None,
    })
    .await;
    assert_eq!(String::from_utf8(disk.lock().unwrap().clone()).unwrap(), EDITED_JSON);

    // Wait until the extension host receives `$acceptModelSaved`.
    let mut saw_saved = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        if let Ok(Some((call_name, _))) = tokio::time::timeout(Duration::from_millis(200), rpc_log_rx.recv()).await {
            if call_name == "ExtHostDocuments.$acceptModelSaved" {
                saw_saved = true;
                break;
            }
        }
    }
    assert!(saw_saved, "extension host did not receive ExtHostDocuments.$acceptModelSaved");

    session.shutdown().await;
}
