//! The node's file receiver (SPEC `FR-F13`, `FR-F15`, `FR-F16`, #773).

use std::path::Path;
use std::sync::Arc;

use base64::Engine;
use ember_node::api::{self, Node};
use ember_node::config::NodeConfig;
use ember_node::transfers::{ChunkBody, OfferBody, ReceivePolicy, Receiver, RecvError, Status};
use ember_transfer::store::DeclaredFile;
use sha2::{Digest, Sha256};

const TOKEN: &str = "node-token";

fn hex256(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

fn payload(len: usize) -> Vec<u8> {
    (0..len).map(|i| ((i * 31 + i / 251) % 256) as u8).collect()
}

fn offer_body(name: &str, data: &[u8], owner: bool) -> OfferBody {
    OfferBody {
        from: "MacBook".into(),
        peer_id: None,
        files: vec![DeclaredFile { name: name.into(), size: data.len() as u64, sha256: Some(hex256(data)) }],
        owner,
    }
}

fn chunk(data: &[u8], offset: u64) -> ChunkBody {
    ChunkBody {
        file: 0,
        offset,
        data_base64: base64::engine::general_purpose::STANDARD.encode(data),
        sha256: Some(hex256(data)),
    }
}

fn open(state: &Path, policy: ReceivePolicy) -> Receiver {
    Receiver::open(Some(state), None, policy).unwrap()
}

fn id_of(v: &serde_json::Value) -> String {
    v["id"].as_str().unwrap().to_string()
}

#[test]
fn fr_f13_offer_chunks_and_complete_store_a_0644_file_with_its_hash() {
    let d = tempfile::tempdir().unwrap();
    let rx = open(d.path(), ReceivePolicy::Owner);
    let data = payload(100_000);
    let v = rx.offer(offer_body("report.pdf", &data, true)).unwrap();
    assert_eq!(v["status"], "accepted");
    let id = id_of(&v);
    assert!(id.starts_with("tr-") && id.len() == 35, "random id: {id}");
    let mut off = 0usize;
    for part in data.chunks(32 * 1024) {
        rx.chunk(&id, &chunk(part, off as u64)).unwrap();
        off += part.len();
    }
    let stored = rx.complete(&id).unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].sha256, hex256(&data));
    let path = d.path().canonicalize().unwrap().join("received").join("report.pdf");
    assert_eq!(std::fs::read(&path).unwrap(), data);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o7777, 0o644);
    }
    assert!(!d.path().join("received/.partial").join(&id).exists());
}

#[test]
fn fr_f13_a_chunk_with_a_wrong_hash_is_refused_and_nothing_is_written() {
    let d = tempfile::tempdir().unwrap();
    let rx = open(d.path(), ReceivePolicy::Owner);
    let data = payload(1000);
    let id = id_of(&rx.offer(offer_body("a.bin", &data, true)).unwrap());
    let mut bad = chunk(&data[..500], 0);
    bad.sha256 = Some(hex256(b"something else"));
    assert!(matches!(rx.chunk(&id, &bad), Err(RecvError::BadRequest(m)) if m.contains("SHA-256 mismatch")));
    assert_eq!(rx.resume_offset(&id, 0).unwrap(), 0);
}

#[test]
fn fr_f13_a_final_hash_mismatch_is_refused_and_not_stored() {
    let d = tempfile::tempdir().unwrap();
    let rx = open(d.path(), ReceivePolicy::Owner);
    let data = payload(1000);
    let mut body = offer_body("a.bin", &data, true);
    body.files[0].sha256 = Some(hex256(b"not the file"));
    let id = id_of(&rx.offer(body).unwrap());
    rx.chunk(&id, &chunk(&data, 0)).unwrap();
    assert!(matches!(rx.complete(&id), Err(RecvError::BadRequest(m)) if m.contains("SHA-256 mismatch")));
    assert!(!d.path().join("received/a.bin").exists());
}

#[test]
fn fr_f13_a_restarted_node_resumes_at_the_verified_offset() {
    let d = tempfile::tempdir().unwrap();
    let data = payload(200_000);
    let id = {
        let rx = open(d.path(), ReceivePolicy::Owner);
        let id = id_of(&rx.offer(offer_body("big.bin", &data, true)).unwrap());
        rx.chunk(&id, &chunk(&data[..80_000], 0)).unwrap();
        id
    };
    // A new receiver over the same state folder is the node after a restart.
    let rx = open(d.path(), ReceivePolicy::Owner);
    let snap = rx.snapshot().unwrap();
    assert_eq!(snap["transfers"][0]["id"], id.as_str());
    assert_eq!(snap["transfers"][0]["done"], 80_000);
    assert_eq!(rx.resume_offset(&id, 0).unwrap(), 80_000);
    rx.chunk(&id, &chunk(&data[80_000..], 80_000)).unwrap();
    let stored = rx.complete(&id).unwrap();
    assert_eq!(stored[0].sha256, hex256(&data));
}

#[test]
fn fr_f13_a_partial_folder_the_ledger_does_not_name_is_removed_at_start() {
    let d = tempfile::tempdir().unwrap();
    let stale = d.path().join("received/.partial/tr-stale");
    std::fs::create_dir_all(&stale).unwrap();
    std::fs::write(stale.join("0.part"), b"old").unwrap();
    let _rx = open(d.path(), ReceivePolicy::Owner);
    assert!(!stale.exists());
}

#[test]
fn fr_f13_a_received_dir_inside_the_state_folder_is_refused_at_start() {
    let d = tempfile::tempdir().unwrap();
    let state = d.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    let inside = state.join("transfers");
    assert!(Receiver::open(Some(&state), Some(&inside), ReceivePolicy::Owner).is_err());
    assert!(Receiver::open(Some(&state), Some(Path::new("relative/dir")), ReceivePolicy::Owner).is_err());
    assert!(Receiver::open(Some(&state), Some(d.path()), ReceivePolicy::Owner).is_err(), "a parent of the state folder");
    let outside = d.path().join("Ember Received");
    let rx = Receiver::open(Some(&state), Some(&outside), ReceivePolicy::Owner).unwrap();
    let snap = rx.snapshot().unwrap();
    assert_eq!(snap["settings"]["received_dir"], outside.canonicalize().unwrap().to_string_lossy().as_ref());
}

#[test]
fn fr_f13_without_a_state_folder_the_receiver_is_off() {
    let rx = Receiver::open(None, None, ReceivePolicy::Owner).unwrap();
    assert!(matches!(rx.snapshot(), Err(RecvError::Conflict(_))));
}

#[test]
fn fr_f15_an_offer_without_the_owner_mark_waits_for_an_accept() {
    let d = tempfile::tempdir().unwrap();
    let rx = open(d.path(), ReceivePolicy::Owner);
    let data = payload(4000);
    let v = rx.offer(offer_body("a.txt", &data, false)).unwrap();
    assert_eq!(v["status"], "offered");
    let id = id_of(&v);
    assert!(matches!(rx.chunk(&id, &chunk(&data, 0)), Err(RecvError::NotFound(_))));
    assert!(!d.path().join("received/.partial").join(&id).exists(), "nothing written before an accept");
    let t = rx.accept(&id).unwrap();
    assert_eq!(t.state, Status::Sending);
    rx.chunk(&id, &chunk(&data, 0)).unwrap();
    rx.complete(&id).unwrap();
}

#[test]
fn fr_f15_ask_holds_owner_offers_and_off_refuses() {
    let d = tempfile::tempdir().unwrap();
    let rx = open(d.path(), ReceivePolicy::Ask);
    assert_eq!(rx.offer(offer_body("a", b"x", true)).unwrap()["status"], "offered");
    let d2 = tempfile::tempdir().unwrap();
    let rx = open(d2.path(), ReceivePolicy::Off);
    assert!(matches!(rx.offer(offer_body("a", b"x", true)), Err(RecvError::Forbidden(m)) if m.contains("ReceiveOff")));
}

#[test]
fn fr_f16_an_offer_above_the_free_space_is_refused_before_any_byte() {
    let d = tempfile::tempdir().unwrap();
    let rx = open(d.path(), ReceivePolicy::Owner);
    rx.set_free_space(Arc::new(|_| Some(1024 * 1024)));
    let data = payload(10);
    assert!(matches!(rx.offer(offer_body("a", &data, true)), Err(RecvError::NoSpace(m)) if m.contains("NoSpace")));
    let partial = d.path().join("received/.partial");
    assert!(!partial.exists() || std::fs::read_dir(&partial).unwrap().next().is_none());
    // An unknown free space refuses nothing.
    rx.set_free_space(Arc::new(|_| None));
    assert_eq!(rx.offer(offer_body("a", &data, true)).unwrap()["status"], "accepted");
}

#[tokio::test]
async fn fr_f13_routes_are_behind_the_token_and_answer_507_for_no_space() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let cfg = NodeConfig { state_dir: Some(d.path().join("state")), ..NodeConfig::new(TOKEN, vec![root]) };
    let node = Node::new(cfg).unwrap();
    node.transfers().set_free_space(Arc::new(|_| Some(0)));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(api::serve(listener, node.clone()));
    let http = reqwest::Client::new();
    let r = http.get(format!("{base}/transfers")).send().await.unwrap();
    assert_eq!(r.status().as_u16(), 401);
    let r = http.get(format!("{base}/transfers")).bearer_auth(TOKEN).send().await.unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let body = serde_json::json!({ "from": "MacBook", "files": [{ "name": "a", "size": 5 }], "owner": true });
    let r = http.post(format!("{base}/transfers/offers")).bearer_auth(TOKEN).json(&body).send().await.unwrap();
    assert_eq!(r.status().as_u16(), 507);
    let v: serde_json::Value = r.json().await.unwrap();
    assert_eq!(v["code"], "no_space");
}
