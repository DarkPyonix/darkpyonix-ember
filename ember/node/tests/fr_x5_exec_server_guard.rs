//! FR-X5 / FR-X1 (#1047): the node checks every request of Codex's exec-server protocol against
//! its allowed folders before it reaches `codex exec-server`.
//!
//! The guard is a pure function over one newline-delimited JSON-RPC line (codex-rs/exec-server at
//! rust-v0.167.0): it answers `Forward` (the line codex receives), `Refuse` (a JSON-RPC error
//! line for the client; codex never sees the request) or `Drop`. Paths travel as `file:` URIs.
#![cfg(unix)]

use std::path::{Path, PathBuf};

use ember_node::exec_server_guard::{check_line, Verdict, FORBIDDEN_CODE, METHOD_NOT_ALLOWED_CODE};
use ember_node::policy::PathPolicy;
use serde_json::{json, Value};

struct Fx {
    policy: PathPolicy,
    root: PathBuf,
    outside: PathBuf,
    _dir: tempfile::TempDir,
}

fn fx() -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().canonicalize().unwrap();
    let root = base.join("root");
    let outside = base.join("outside");
    std::fs::create_dir_all(root.join("work")).unwrap();
    std::fs::create_dir_all(root.join(".ember")).unwrap();
    std::fs::create_dir_all(root.join("a/b/c")).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(root.join("work/ok"), "inside").unwrap();
    std::fs::write(root.join(".ember/server-token"), "s3cret").unwrap();
    std::fs::write(outside.join("secret"), "s3cret").unwrap();
    // A link inside the root that leaves it.
    std::os::unix::fs::symlink(&outside, root.join("out")).unwrap();
    // A link inside the root that stays inside but deeper (for the lexical `..` case).
    std::os::unix::fs::symlink(root.join("a/b/c"), root.join("deep")).unwrap();
    let policy = PathPolicy::with_denied(vec![root.clone()], vec![root.join(".ember")]).unwrap();
    Fx { policy, root, outside, _dir: dir }
}

fn uri(p: &Path) -> String {
    format!("file://{}", p.display())
}

fn req(id: i64, method: &str, params: Value) -> Vec<u8> {
    serde_json::to_vec(&json!({"id": id, "method": method, "params": params})).unwrap()
}

fn line_json(bytes: &[u8]) -> Value {
    assert_eq!(bytes.last(), Some(&b'\n'), "a verdict line ends with a newline");
    let body = &bytes[..bytes.len() - 1];
    assert!(!body.contains(&b'\n'), "one line only");
    serde_json::from_slice(body).unwrap()
}

#[track_caller]
fn refused(v: Verdict) -> Value {
    match v {
        Verdict::Refuse(line) => line_json(&line),
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[track_caller]
fn forwarded(v: Verdict) -> Value {
    match v {
        Verdict::Forward(line) => line_json(&line),
        other => panic!("expected the request to be forwarded, got {other:?}"),
    }
}

#[track_caller]
fn assert_forbidden(v: Verdict, id: i64) {
    let e = refused(v);
    assert_eq!(e["id"], json!(id), "{e}");
    assert_eq!(e["error"]["code"], json!(FORBIDDEN_CODE), "{e}");
    assert!(e.get("result").is_none(), "{e}");
}

#[test]
fn fr_x5_read_outside_the_allowed_folders_is_refused() {
    let f = fx();
    let line = req(1, "fs/readFile", json!({"path": uri(&f.outside.join("secret"))}));
    let e = refused(check_line(&f.policy, &line));
    assert_eq!(e["id"], json!(1));
    assert_eq!(e["error"]["code"], json!(FORBIDDEN_CODE));
    assert!(e["error"]["message"].as_str().unwrap().contains("allowed folders"), "{e}");
}

#[test]
fn fr_x5_dotdot_out_of_the_allowed_folders_is_refused() {
    let f = fx();
    let plain = format!("{}/work/../../outside/secret", uri(&f.root));
    assert_forbidden(check_line(&f.policy, &req(1, "fs/readFile", json!({"path": plain}))), 1);
    // Percent-encoded dots are dot segments in a `file:` URL too.
    let encoded = format!("{}/work/%2e%2e/%2E%2E/outside/secret", uri(&f.root));
    assert_forbidden(check_line(&f.policy, &req(2, "fs/readFile", json!({"path": encoded}))), 2);
    let write = format!("{}/../outside/new", uri(&f.root));
    assert_forbidden(
        check_line(&f.policy, &req(3, "fs/writeFile", json!({"path": write, "dataBase64": "eA=="}))),
        3,
    );
}

#[test]
fn fr_x5_dotdot_is_judged_lexically_like_codex_not_through_the_symlink() {
    // `deep` -> root/a/b/c. Codex (WHATWG URL parsing) drops `deep/..` lexically, so
    // root/deep/../../x means base/x, outside. Resolving through the link would give root/a/x,
    // inside: the guard must judge the path codex will use.
    let f = fx();
    let p = format!("{}/deep/../../x", uri(&f.root));
    assert_forbidden(
        check_line(&f.policy, &req(1, "fs/writeFile", json!({"path": p, "dataBase64": "eA=="}))),
        1,
    );
}

#[test]
fn fr_x5_symlink_out_of_the_allowed_folders_is_refused() {
    let f = fx();
    let through = f.root.join("out/secret");
    assert_forbidden(check_line(&f.policy, &req(1, "fs/readFile", json!({"path": uri(&through)}))), 1);
    assert_forbidden(check_line(&f.policy, &req(2, "fs/open", json!({"handleId": "h", "path": uri(&through), "mode": "read"}))), 2);
    assert_forbidden(check_line(&f.policy, &req(3, "fs/getMetadata", json!({"path": uri(&f.root.join("out"))}))), 3);
    assert_forbidden(check_line(&f.policy, &req(4, "fs/readDirectory", json!({"path": uri(&f.root.join("out"))}))), 4);
    assert_forbidden(check_line(&f.policy, &req(5, "fs/canonicalize", json!({"path": uri(&f.root.join("out"))}))), 5);
    // A write target below the link does not exist yet; its parent resolves outside.
    let new = f.root.join("out/new");
    assert_forbidden(
        check_line(&f.policy, &req(6, "fs/writeFile", json!({"path": uri(&new), "dataBase64": "eA=="}))),
        6,
    );
    assert_forbidden(
        check_line(&f.policy, &req(7, "fs/createDirectory", json!({"path": uri(&f.root.join("out/d/e")), "recursive": true}))),
        7,
    );
    // Following the link explicitly is refused as well.
    assert_forbidden(
        check_line(&f.policy, &req(8, "fs/readFile", json!({"path": uri(&through), "followSymlinks": true}))),
        8,
    );
}

#[test]
fn fr_x5_denied_entry_is_refused_like_outside() {
    let f = fx();
    let token = f.root.join(".ember/server-token");
    assert_forbidden(check_line(&f.policy, &req(1, "fs/readFile", json!({"path": uri(&token)}))), 1);
    assert_forbidden(
        check_line(&f.policy, &req(2, "process/start", json!({"processId": "p", "argv": ["cat", "server-token"], "cwd": uri(&f.root.join(".ember")), "env": {}, "tty": false}))),
        2,
    );
}

#[test]
fn fr_x5_process_start_cwd_must_be_inside() {
    let f = fx();
    let start = |id: i64, cwd: &Path| {
        req(id, "process/start", json!({"processId": format!("p{id}"), "argv": ["ls"], "cwd": uri(cwd), "env": {}, "tty": false}))
    };
    assert_forbidden(check_line(&f.policy, &start(1, &f.outside)), 1);
    assert_forbidden(check_line(&f.policy, &start(2, &f.root.join("out"))), 2);
    assert_forbidden(check_line(&f.policy, &start(3, Path::new("/"))), 3);
    let ok = forwarded(check_line(&f.policy, &start(4, &f.root.join("work"))));
    assert_eq!(ok["method"], json!("process/start"));
    assert_eq!(ok["params"]["cwd"], json!(uri(&f.root.join("work"))));
}

#[test]
fn fr_x5_every_path_of_a_request_is_checked() {
    let f = fx();
    let inside = uri(&f.root.join("work/ok"));
    let outside = uri(&f.outside.join("secret"));
    // fs/copy: both ends.
    assert_forbidden(
        check_line(&f.policy, &req(1, "fs/copy", json!({"sourcePath": inside, "destinationPath": outside, "recursive": false}))),
        1,
    );
    assert_forbidden(
        check_line(&f.policy, &req(2, "fs/copy", json!({"sourcePath": outside, "destinationPath": uri(&f.root.join("work/copy")), "recursive": false}))),
        2,
    );
    // fs/watchMissing: every missing path and the present one.
    assert_forbidden(
        check_line(&f.policy, &req(3, "fs/watchMissing", json!({"watchId": 1, "missing": [uri(&f.root.join("work/x")), uri(&f.outside.join("y"))]}))),
        3,
    );
    assert_forbidden(
        check_line(&f.policy, &req(4, "fs/watchMissing", json!({"watchId": 2, "missing": [], "present": outside}))),
        4,
    );
    // Capability discovery and config reads walk from a cwd or root on the node.
    assert_forbidden(
        check_line(&f.policy, &req(5, "capabilityRoots/discoverV1", json!({"roots": [{"id": "a", "path": uri(&f.root)}, {"id": "b", "path": uri(&f.outside)}]}))),
        5,
    );
    assert_forbidden(check_line(&f.policy, &req(6, "capabilities/discoverV2", json!({"cwd": uri(&f.outside)}))), 6);
    assert_forbidden(
        check_line(&f.policy, &req(7, "environmentConfig/read", json!({"cwd": uri(&f.outside), "configPaths": [], "requirementsPaths": []}))),
        7,
    );
    assert_forbidden(check_line(&f.policy, &req(8, "fs/walk", json!({"path": uri(&f.outside), "options": {"maxDepth": 1, "maxDirectories": 1, "maxEntries": 1, "followDirectorySymlinks": false}}))), 8);
    assert_forbidden(check_line(&f.policy, &req(9, "fs/remove", json!({"path": outside, "recursive": true}))), 9);
}

#[test]
fn fr_x5_paths_inside_the_allowed_folders_are_forwarded() {
    let f = fx();
    let ok = uri(&f.root.join("work/ok"));
    let v = forwarded(check_line(&f.policy, &req(1, "fs/readFile", json!({"path": ok}))));
    assert_eq!(v, json!({"id": 1, "method": "fs/readFile", "params": {"path": ok}}));
    // A new file and a new directory chain inside.
    let new = uri(&f.root.join("work/new/deeper/file"));
    forwarded(check_line(&f.policy, &req(2, "fs/writeFile", json!({"path": new, "dataBase64": "eA=="}))));
    forwarded(check_line(&f.policy, &req(3, "fs/createDirectory", json!({"path": uri(&f.root.join("work/n1/n2")), "recursive": true}))));
    forwarded(check_line(&f.policy, &req(4, "fs/copy", json!({"sourcePath": ok, "destinationPath": uri(&f.root.join("work/copy")), "recursive": false}))));
    forwarded(check_line(&f.policy, &req(5, "capabilities/discoverV2", json!({"cwd": uri(&f.root)}))));
    // `..` that stays inside is fine; `localhost` is the empty authority.
    forwarded(check_line(&f.policy, &req(6, "fs/getMetadata", json!({"path": format!("{}/work/../work/ok", uri(&f.root))}))));
    forwarded(check_line(&f.policy, &req(7, "fs/getMetadata", json!({"path": format!("file://localhost{}", f.root.join("work/ok").display())}))));
    // Removing a link itself (not following it) only touches the directory it is in.
    forwarded(check_line(&f.policy, &req(8, "fs/remove", json!({"path": uri(&f.root.join("out")), "followSymlinks": false}))));
}

#[test]
fn fr_x5_walk_never_follows_directory_symlinks() {
    let f = fx();
    let line = req(1, "fs/walk", json!({"path": uri(&f.root), "options": {"maxDepth": 4, "maxDirectories": 100, "maxEntries": 100, "followDirectorySymlinks": true}}));
    let v = forwarded(check_line(&f.policy, &line));
    assert_eq!(v["params"]["options"]["followDirectorySymlinks"], json!(false), "{v}");
    assert_eq!(v["params"]["options"]["maxDepth"], json!(4));
}

#[test]
fn fr_x5_removing_an_allowed_folder_itself_is_refused() {
    let f = fx();
    assert_forbidden(check_line(&f.policy, &req(1, "fs/remove", json!({"path": uri(&f.root), "recursive": true, "force": true}))), 1);
}

#[test]
fn fr_x5_paths_that_are_not_local_file_uris_are_refused() {
    let f = fx();
    for (id, p) in [
        (1, "/etc/passwd".to_string()),
        (2, "work/ok".to_string()),
        (3, "http://example.com/etc/passwd".to_string()),
        (4, format!("file://otherhost{}", f.root.join("work/ok").display())),
        (5, format!("{}?x=1", uri(&f.root.join("work/ok")))),
        (6, format!("{}#frag", uri(&f.root.join("work/ok")))),
        // Codex's opaque encoding of a path it could not spell as a URI.
        (7, "file:///%00/bad/path/L2V0Yy9wYXNzd2Q".to_string()),
    ] {
        let e = refused(check_line(&f.policy, &req(id, "fs/readFile", json!({"path": p}))));
        assert_eq!(e["id"], json!(id), "{p}: {e}");
        assert!(e["error"]["code"].as_i64().is_some(), "{p}: {e}");
    }
    // A missing or non-string path is refused too.
    refused(check_line(&f.policy, &req(8, "fs/readFile", json!({}))));
    refused(check_line(&f.policy, &req(9, "fs/readFile", json!({"path": 7}))));
}

#[test]
fn fr_x5_unknown_methods_are_refused() {
    let f = fx();
    for (id, m) in [(1, "fs/rename"), (2, "fs/symlink"), (3, "process/spawnAnything"), (4, "")] {
        let e = refused(check_line(&f.policy, &req(id, m, json!({"path": uri(&f.root)}))));
        assert_eq!(e["error"]["code"], json!(METHOD_NOT_ALLOWED_CODE), "{m}: {e}");
        assert_eq!(e["id"], json!(id));
    }
}

#[test]
fn fr_x5_codex_receives_exactly_what_was_checked() {
    let f = fx();
    let inside = uri(&f.root.join("work/ok"));
    let outside = uri(&f.outside.join("secret"));
    // Duplicate keys: JSON parsers keep the last one. The guard must judge, and forward, that one.
    let dup = format!(r#"{{"id":1,"method":"fs/readFile","params":{{"path":"{inside}","path":"{outside}"}}}}"#);
    assert_forbidden(check_line(&f.policy, dup.as_bytes()), 1);
    let dup = format!(r#"{{"id":2,"method":"fs/readFile","params":{{"path":"{outside}","path":"{inside}"}}}}"#);
    let Verdict::Forward(line) = check_line(&f.policy, dup.as_bytes()) else { panic!("forward") };
    let text = String::from_utf8(line).unwrap();
    assert!(!text.contains("outside"), "the forwarded line carries only the checked path: {text}");
    // A duplicate method key cannot switch the request to an unchecked method.
    let dup = format!(r#"{{"id":3,"method":"fs/getMetadata","method":"fs/readFile","params":{{"path":"{outside}"}}}}"#);
    assert_forbidden(check_line(&f.policy, dup.as_bytes()), 3);
}

#[test]
fn fr_x5_requests_without_paths_and_replies_pass_unchanged() {
    let f = fx();
    for (m, p) in [
        ("initialize", json!({"clientName": "codex"})),
        ("environment/info", json!({})),
        ("process/read", json!({"processId": "p", "afterSeq": 0})),
        ("process/write", json!({"processId": "p", "chunk": "eA==", "writeId": "w"})),
        ("process/terminate", json!({"processId": "p"})),
        ("fs/readBlock", json!({"handleId": "h", "offset": 0, "len": 10})),
        ("fs/close", json!({"handleId": "h"})),
    ] {
        let v = forwarded(check_line(&f.policy, &req(1, m, p.clone())));
        assert_eq!(v, json!({"id": 1, "method": m, "params": p}));
    }
    let initialized = forwarded(check_line(&f.policy, br#"{"method":"initialized"}"#));
    assert_eq!(initialized, json!({"method": "initialized"}));
    // The client's answer to a request codex sent (no method) goes through.
    let reply = forwarded(check_line(&f.policy, br#"{"id":"s-1","result":{"decision":"allow"}}"#));
    assert_eq!(reply["id"], json!("s-1"));
    // String ids are echoed back as strings in a refusal.
    let e = refused(check_line(&f.policy, &serde_json::to_vec(&json!({"id": "x", "method": "fs/readFile", "params": {"path": uri(&f.outside)}})).unwrap()));
    assert_eq!(e["id"], json!("x"));
}

#[test]
fn fr_x5_unanswerable_lines_are_dropped() {
    let f = fx();
    for line in [&b""[..], b"   ", b"not json", b"[1,2]", br#"{"method":"fs/readFile","params":{"path":"file:///etc/passwd"}}"#] {
        assert!(matches!(check_line(&f.policy, line), Verdict::Drop), "{}", String::from_utf8_lossy(line));
    }
}
