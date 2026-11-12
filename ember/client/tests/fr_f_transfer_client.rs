//! The transfer client against the real ember-server transfer router in-process
//! (SPEC `FR-F3`, `FR-F4`, `FR-F5`, `FR-F10`, `FR-L27.8`, #487).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use ember_client::api::Api;
use ember_client::transfers::{
    locate, offer, prepare, sha256_hex, upload, LocalFile, TransferError, TransferResult, TransferStatus,
    UploadEnd, UploadOptions, Whereabouts,
};
use ember_server::transfers::{self, TransferManager};
use tokio::sync::watch;

struct Server {
    api: Api,
    mgr: Arc<TransferManager>,
    addr: std::net::SocketAddr,
    dir: tempfile::TempDir,
}

impl Server {
    fn received(&self, name: &str) -> PathBuf {
        self.dir.path().join("received").join(name)
    }
}

async fn start() -> Server {
    let dir = tempfile::tempdir().unwrap();
    let mgr = TransferManager::new(dir.path(), None, None);
    let app = transfers::router(mgr.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Server { api: Api::with_token(&format!("http://{addr}"), None).unwrap(), mgr, addr, dir }
}

/// Deterministic bytes that differ along the file, so a chunk written at the wrong offset fails
/// the final hash.
fn payload(len: usize) -> Vec<u8> {
    (0..len).map(|i| ((i * 31 + i / 251) % 256) as u8).collect()
}

async fn local_file(dir: &Path, name: &str, data: &[u8]) -> Vec<LocalFile> {
    let path = dir.join(name);
    tokio::fs::write(&path, data).await.unwrap();
    prepare(&[path]).await.unwrap()
}

fn fast(chunk_bytes: usize) -> UploadOptions {
    UploadOptions {
        chunk_bytes,
        poll: Duration::from_millis(20),
        answer_timeout: Duration::from_secs(10),
        max_retries: 40,
        retry_delay: Duration::from_millis(40),
    }
}

async fn wait_until(what: &str, mut ready: impl FnMut() -> bool) {
    for _ in 0..500 {
        if ready() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {what}");
}

#[tokio::test]
async fn fr_f3_f5_an_offer_waits_for_the_answer_then_streams_and_verifies() {
    let s = start().await;
    let src = tempfile::tempdir().unwrap();
    let data = payload(10_000);
    let files = local_file(src.path(), "notes.bin", &data).await;
    assert_eq!(files[0].declared.size, 10_000);
    assert_eq!(files[0].declared.sha256.as_deref(), Some(sha256_hex(&data).as_str()));

    let created = offer(&s.api, "laptop", None, &files).await.unwrap();
    assert_eq!(created.status, "offered");
    let snapshot = s.api.transfers().await.unwrap();
    assert_eq!(locate(&snapshot, &created.id), Whereabouts::Pending);
    assert_eq!(snapshot.pending_offers[0].cleaned_names, vec!["notes.bin".to_string()]);
    assert_eq!(snapshot.pending_offers[0].from, "laptop");

    // The upload sends nothing before the receiver accepts (FR-F3).
    let api = s.api.clone();
    let id = created.id.clone();
    let seen = Arc::new(AtomicU64::new(0));
    let seen_in = seen.clone();
    let task = tokio::spawn(async move {
        let mut progress = move |done: u64| seen_in.store(done, Ordering::SeqCst);
        upload(&api, &id, &files, &fast(1024), &mut progress).await
    });
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(seen.load(Ordering::SeqCst), 0);
    assert!(!s.dir.path().join("received/.partial").exists(), "nothing is written before the answer");

    s.api.accept_transfer_offer(&created.id).await.unwrap();
    let end = task.await.unwrap().unwrap();
    let UploadEnd::Done(stored) = end else { panic!("expected done, got {end:?}") };
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].name, "notes.bin");
    assert_eq!(stored[0].sha256, sha256_hex(&data));
    assert_eq!(seen.load(Ordering::SeqCst), 10_000);
    assert_eq!(std::fs::read(s.received("notes.bin")).unwrap(), data);

    let snapshot = s.api.transfers().await.unwrap();
    assert!(snapshot.transfers.is_empty());
    assert_eq!(snapshot.received[0].name, "notes.bin");
    assert_eq!(locate(&snapshot, &created.id), Whereabouts::Ended(Some(TransferResult::Done)));
    assert_eq!(s.api.transfer_history().await.unwrap().len(), 1);
    s.api.clear_transfer_history().await.unwrap();
    assert!(s.api.transfer_history().await.unwrap().is_empty());
    assert!(s.received("notes.bin").exists(), "clearing the history keeps the file (FR-F8)");
}

#[tokio::test]
async fn fr_f2_two_files_travel_as_one_offer() {
    let s = start().await;
    let src = tempfile::tempdir().unwrap();
    let a = payload(3000);
    let b = payload(5);
    tokio::fs::write(src.path().join("a.bin"), &a).await.unwrap();
    tokio::fs::write(src.path().join("b.bin"), &b).await.unwrap();
    tokio::fs::write(src.path().join("empty.txt"), b"").await.unwrap();
    let files =
        prepare(&[src.path().join("a.bin"), src.path().join("b.bin"), src.path().join("empty.txt")]).await.unwrap();

    let created = offer(&s.api, "laptop", None, &files).await.unwrap();
    s.api.accept_transfer_offer(&created.id).await.unwrap();
    let end = upload(&s.api, &created.id, &files, &fast(1000), &mut |_| {}).await.unwrap();
    assert!(matches!(end, UploadEnd::Done(ref stored) if stored.len() == 3), "{end:?}");
    assert_eq!(std::fs::read(s.received("a.bin")).unwrap(), a);
    assert_eq!(std::fs::read(s.received("b.bin")).unwrap(), b);
    assert_eq!(std::fs::read(s.received("empty.txt")).unwrap(), b"");
}

/// A TCP relay in front of the server that can be cut: while `down`, it closes every open
/// connection and drops new ones at once.
async fn cuttable_proxy(target: std::net::SocketAddr, down: watch::Receiver<bool>) -> std::net::SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut inbound, _)) = listener.accept().await else { return };
            let mut down = down.clone();
            tokio::spawn(async move {
                if *down.borrow() {
                    return;
                }
                let Ok(mut outbound) = tokio::net::TcpStream::connect(target).await else { return };
                tokio::select! {
                    _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound) => {}
                    _ = async { let _ = down.wait_for(|d| *d).await; } => {}
                }
            });
        }
    });
    addr
}

#[tokio::test]
async fn fr_f5_a_dropped_connection_resumes_from_the_verified_offset_not_from_zero() {
    let s = start().await;
    let (down_tx, down_rx) = watch::channel(false);
    let proxy = cuttable_proxy(s.addr, down_rx).await;
    let api = Api::with_token(&format!("http://{proxy}"), None).unwrap();

    let src = tempfile::tempdir().unwrap();
    let data = payload(64 * 1024);
    let files = local_file(src.path(), "big.bin", &data).await;
    let created = offer(&api, "laptop", None, &files).await.unwrap();
    s.api.accept_transfer_offer(&created.id).await.unwrap();

    const CUT_AT: u64 = 32 * 1024;
    let cut = Arc::new(AtomicBool::new(false));
    let chunks_after_cut = Arc::new(AtomicU64::new(0));
    let lowest_after_cut = Arc::new(AtomicU64::new(u64::MAX));
    let (cut_in, chunks_in, lowest_in) = (cut.clone(), chunks_after_cut.clone(), lowest_after_cut.clone());
    let down_tx = Arc::new(down_tx);
    let mut progress = move |done: u64| {
        if cut_in.load(Ordering::SeqCst) {
            chunks_in.fetch_add(1, Ordering::SeqCst);
            lowest_in.fetch_min(done, Ordering::SeqCst);
        } else if done >= CUT_AT {
            // Half way: cut the connection, and bring it back a little later.
            cut_in.store(true, Ordering::SeqCst);
            let _ = down_tx.send(true);
            let tx = down_tx.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(200)).await;
                let _ = tx.send(false);
            });
        }
    };

    let end = upload(&api, &created.id, &files, &fast(1024), &mut progress).await.unwrap();
    assert!(matches!(end, UploadEnd::Done(_)), "{end:?}");
    assert!(cut.load(Ordering::SeqCst), "the connection was cut during the transfer");
    assert_eq!(std::fs::read(s.received("big.bin")).unwrap(), data, "the final hash and bytes match");
    // After the cut the stream continued past the cut point: nothing below it was sent again.
    assert!(lowest_after_cut.load(Ordering::SeqCst) > CUT_AT, "resumed at {}", lowest_after_cut.load(Ordering::SeqCst));
    assert!(
        chunks_after_cut.load(Ordering::SeqCst) <= 32,
        "only the remaining half was sent after the cut, got {} chunks",
        chunks_after_cut.load(Ordering::SeqCst)
    );
}

#[tokio::test]
async fn fr_f5_an_upload_that_gave_up_continues_when_called_again() {
    let s = start().await;
    let (down_tx, down_rx) = watch::channel(false);
    let proxy = cuttable_proxy(s.addr, down_rx).await;
    let api = Api::with_token(&format!("http://{proxy}"), None).unwrap();

    let src = tempfile::tempdir().unwrap();
    let data = payload(20_000);
    let files = local_file(src.path(), "again.bin", &data).await;
    let created = offer(&api, "laptop", None, &files).await.unwrap();
    s.api.accept_transfer_offer(&created.id).await.unwrap();

    // The connection goes down for good after the first chunks: the upload gives up.
    let down_tx = Arc::new(down_tx);
    let tx = down_tx.clone();
    let mut cut_once = move |done: u64| {
        if done >= 4000 {
            let _ = tx.send(true);
        }
    };
    let opts = UploadOptions { max_retries: 2, ..fast(1000) };
    let err = upload(&api, &created.id, &files, &opts, &mut cut_once).await.unwrap_err();
    assert_eq!(err, TransferError::Connection);
    assert!(err.can_resume());

    // The receiver kept what it verified.
    let kept = s.api.transfer_resume_offset(&created.id, 0).await.unwrap();
    assert!(kept >= 4000, "kept {kept}");

    // Back online: the same call continues from there.
    down_tx.send(false).unwrap();
    let first = Arc::new(AtomicU64::new(0));
    let first_in = first.clone();
    let mut progress = move |done: u64| {
        let _ = first_in.compare_exchange(0, done, Ordering::SeqCst, Ordering::SeqCst);
    };
    let end = upload(&api, &created.id, &files, &fast(1000), &mut progress).await.unwrap();
    assert!(matches!(end, UploadEnd::Done(_)), "{end:?}");
    assert!(first.load(Ordering::SeqCst) > kept, "continued after {kept}, first progress {}", first.load(Ordering::SeqCst));
    assert_eq!(std::fs::read(s.received("again.bin")).unwrap(), data);
}

#[tokio::test]
async fn fr_f4_a_cancel_stops_the_upload_and_leaves_no_partial_file() {
    let s = start().await;
    let src = tempfile::tempdir().unwrap();
    let files = local_file(src.path(), "long.bin", &payload(1024 * 1024)).await;
    let created = offer(&s.api, "laptop", None, &files).await.unwrap();
    s.api.accept_transfer_offer(&created.id).await.unwrap();

    let started = Arc::new(AtomicBool::new(false));
    let started_in = started.clone();
    let (api, id) = (s.api.clone(), created.id.clone());
    let task = tokio::spawn(async move {
        let mut progress = move |_done: u64| started_in.store(true, Ordering::SeqCst);
        upload(&api, &id, &files, &fast(512), &mut progress).await
    });
    wait_until("the first chunk", || started.load(Ordering::SeqCst)).await;
    assert!(s.dir.path().join("received/.partial").exists());

    let record = s.api.cancel_transfer(&created.id).await.unwrap();
    assert_eq!(record.result, TransferResult::Cancelled);
    let end = tokio::time::timeout(Duration::from_secs(2), task).await.expect("stops within 2 s").unwrap();
    assert_eq!(end, Ok(UploadEnd::Cancelled));
    assert!(!s.dir.path().join("received/.partial").exists(), "no partial file is left (FR-F4)");
    assert!(!s.received("long.bin").exists());
    assert!(s.api.transfers().await.unwrap().transfers.is_empty());
}

#[tokio::test]
async fn fr_f4_a_pause_holds_the_stream_until_it_is_resumed() {
    let s = start().await;
    let src = tempfile::tempdir().unwrap();
    let data = payload(1024 * 1024);
    let files = local_file(src.path(), "held.bin", &data).await;
    let created = offer(&s.api, "laptop", None, &files).await.unwrap();
    s.api.accept_transfer_offer(&created.id).await.unwrap();

    let seen = Arc::new(AtomicU64::new(0));
    let seen_in = seen.clone();
    let (api, id) = (s.api.clone(), created.id.clone());
    let task = tokio::spawn(async move {
        let mut progress = move |done: u64| seen_in.store(done, Ordering::SeqCst);
        upload(&api, &id, &files, &fast(512), &mut progress).await
    });
    wait_until("the first chunk", || seen.load(Ordering::SeqCst) > 0).await;

    let paused = s.api.pause_transfer(&created.id).await.unwrap();
    assert_eq!(paused.state, TransferStatus::Paused);
    // A chunk already on its way may still land; after that the count stands still.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let held = s.api.transfer_resume_offset(&created.id, 0).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(s.api.transfer_resume_offset(&created.id, 0).await.unwrap(), held, "no byte moves while paused");
    assert!(held < data.len() as u64);
    assert!(!task.is_finished(), "a pause is not a failure");

    s.api.resume_transfer(&created.id).await.unwrap();
    let end = task.await.unwrap().unwrap();
    assert!(matches!(end, UploadEnd::Done(_)), "{end:?}");
    assert_eq!(std::fs::read(s.received("held.bin")).unwrap(), data);
}

#[tokio::test]
async fn fr_f3_a_decline_ends_the_offer_for_the_sender() {
    let s = start().await;
    let src = tempfile::tempdir().unwrap();
    let files = local_file(src.path(), "no.bin", &payload(100)).await;
    let created = offer(&s.api, "laptop", None, &files).await.unwrap();
    s.api.decline_transfer_offer(&created.id).await.unwrap();

    let end = upload(&s.api, &created.id, &files, &fast(64), &mut |_| panic!("no byte moves")).await;
    assert_eq!(end, Ok(UploadEnd::Declined));
    assert!(!s.received("no.bin").exists());
    assert!(!s.dir.path().join("received/.partial").exists());
}

#[tokio::test]
async fn fr_f3_an_unanswered_offer_expires_and_is_withdrawn() {
    let s = start().await;
    let src = tempfile::tempdir().unwrap();
    let files = local_file(src.path(), "late.bin", &payload(100)).await;
    let created = offer(&s.api, "laptop", None, &files).await.unwrap();

    let opts = UploadOptions { answer_timeout: Duration::from_millis(120), ..fast(64) };
    let err = upload(&s.api, &created.id, &files, &opts, &mut |_| panic!("no byte moves")).await.unwrap_err();
    assert_eq!(err, TransferError::Expired);
    let snapshot = s.api.transfers().await.unwrap();
    assert!(snapshot.pending_offers.is_empty(), "the receiver's prompt is gone too");
    assert_eq!(locate(&snapshot, &created.id), Whereabouts::Ended(Some(TransferResult::Cancelled)));
}

#[tokio::test]
async fn fr_f10_an_offer_above_the_limit_is_refused_before_any_byte() {
    let s = start().await;
    s.mgr.update_settings(None, None, Some(1000), None).unwrap();
    let src = tempfile::tempdir().unwrap();

    let big = local_file(src.path(), "big.bin", &payload(1001)).await;
    let err = offer(&s.api, "laptop", None, &big).await.unwrap_err();
    assert_eq!(err, TransferError::TooLarge);
    assert!(s.api.transfers().await.unwrap().pending_offers.is_empty());

    // Exactly the limit is offered.
    let exact = local_file(src.path(), "exact.bin", &payload(1000)).await;
    assert_eq!(offer(&s.api, "laptop", None, &exact).await.unwrap().status, "offered");
}

#[tokio::test]
async fn fr_f9_receiving_off_refuses_the_offer_with_its_reason() {
    let s = start().await;
    s.mgr.update_settings(Some("off"), None, None, None).unwrap();
    let src = tempfile::tempdir().unwrap();
    let files = local_file(src.path(), "a.bin", &payload(10)).await;
    let err = offer(&s.api, "laptop", None, &files).await.unwrap_err();
    assert_eq!(err, TransferError::ReceiveOff);
    assert_eq!(err.text_ko(), "받는 쪽이 파일 받기를 꺼 두었습니다.");
}

#[tokio::test]
async fn fr_f5_a_file_that_does_not_match_its_hash_is_not_stored() {
    let s = start().await;
    let src = tempfile::tempdir().unwrap();
    let files = local_file(src.path(), "bad.bin", &payload(2000)).await;
    // The file changes after it was hashed (same size, other bytes).
    tokio::fs::write(src.path().join("bad.bin"), vec![7u8; 2000]).await.unwrap();
    assert_eq!(files[0].declared.sha256, Some(sha256_hex(&payload(2000))));

    let created = offer(&s.api, "laptop", None, &files).await.unwrap();
    s.api.accept_transfer_offer(&created.id).await.unwrap();
    let err = upload(&s.api, &created.id, &files, &fast(500), &mut |_| {}).await.unwrap_err();
    assert_eq!(err, TransferError::HashMismatch);
    assert!(!s.received("bad.bin").exists());
}

#[tokio::test]
async fn fr_f2_a_missing_path_or_a_folder_is_refused_locally() {
    let src = tempfile::tempdir().unwrap();
    assert_eq!(prepare(&[]).await, Err(TransferError::NoFiles));
    assert_eq!(
        prepare(&[src.path().join("nope.txt")]).await,
        Err(TransferError::NotAFile("nope.txt".into()))
    );
    let folder = src.path().join("folder");
    tokio::fs::create_dir(&folder).await.unwrap();
    assert_eq!(prepare(&[folder]).await, Err(TransferError::NotAFile("folder".into())));
}

#[tokio::test]
async fn fr_l27_8_a_server_without_the_transfer_api_says_unsupported() {
    // A server whose router has no /api/transfers.
    let app = axum::Router::new();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let api = Api::with_token(&format!("http://{addr}"), None).unwrap();

    let err = api.transfers().await.unwrap_err();
    assert_eq!(TransferError::from_api(&err), TransferError::Unsupported);
}
