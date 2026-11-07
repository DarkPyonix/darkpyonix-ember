//! SPEC FR-F3, FR-F4, FR-F9, FR-F10 over the in-memory fake transport. Nothing sleeps: the
//! clock is injected.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use ember_transfer::limits::Limits;
use ember_transfer::mode::ReceiveMode;
use ember_transfer::session::{
    IncomingOffer, Outgoing, PromptAnswer, Prompter, Reply, Service, DEFAULT_OFFER_TTL,
};
use ember_transfer::state::{ManualClock, State};
use ember_transfer::wire::{DeclineCode, FileEntry};
use ember_transfer::{PeerId, SERVICE};
use ember_transport::mem::MemNetwork;
use ember_transport::{Connection, Transport};
use tokio::sync::oneshot;

const GIB: u64 = 1024 * 1024 * 1024;

#[derive(Default)]
struct TestPrompter {
    answer: Mutex<Option<PromptAnswer>>,
    seen: Mutex<Vec<IncomingOffer>>,
    advance: Mutex<Option<(Arc<ManualClock>, Duration)>>,
    /// If set, the prompt waits for this signal and then returns Decline (the user never answers
    /// in time; used to test a cancel while the prompt is open).
    block: Mutex<Option<oneshot::Receiver<()>>>,
}

impl TestPrompter {
    fn answering(a: PromptAnswer) -> Arc<Self> {
        let p = Self::default();
        *p.answer.lock().unwrap() = Some(a);
        Arc::new(p)
    }
    fn prompts(&self) -> usize {
        self.seen.lock().unwrap().len()
    }
}

#[async_trait]
impl Prompter for TestPrompter {
    async fn prompt(&self, offer: &IncomingOffer) -> PromptAnswer {
        self.seen.lock().unwrap().push(offer.clone());
        if let Some((clock, by)) = self.advance.lock().unwrap().as_ref() {
            clock.advance(*by);
        }
        let blocked = self.block.lock().unwrap().take();
        if let Some(rx) = blocked {
            let _ = rx.await;
            return PromptAnswer::Decline;
        }
        self.answer.lock().unwrap().unwrap_or(PromptAnswer::Decline)
    }
}

struct Rig {
    a: Transport,
    b: Transport,
    a_to_b: Connection,
    at_b: Connection,
    clock: Arc<ManualClock>,
}

async fn rig() -> Rig {
    let net = MemNetwork::new();
    let (a, b) = (net.transport(), net.transport());
    let mut listener = b.listen(SERVICE).unwrap();
    let a_to_b = a.connect(b.peer_id(), SERVICE).await.unwrap();
    let at_b = listener.accept().await.unwrap();
    Rig { a, b, a_to_b, at_b, clock: Arc::new(ManualClock::new()) }
}

fn registered(ids: &[PeerId]) -> Arc<HashSet<PeerId>> {
    Arc::new(ids.iter().copied().collect())
}

fn file(name: &str, size: u64) -> FileEntry {
    FileEntry { name: name.into(), size }
}

fn service(rig: &Rig, policy: Arc<HashSet<PeerId>>, prompter: Arc<TestPrompter>, limits: Limits, mode: ReceiveMode) -> Service {
    Service::new(policy, prompter, rig.clock.clone(), limits, mode)
}

/// Runs one offer end to end and returns the sender, its first reply and the receiver's result.
async fn run(
    rig: &Rig,
    svc: &Service,
    files: Vec<FileEntry>,
) -> (Outgoing, Reply, ember_transfer::session::Handled) {
    let sender = async {
        let mut out = Outgoing::start(&rig.a_to_b, files, rig.clock.clone(), DEFAULT_OFFER_TTL)
            .await
            .unwrap();
        let reply = out.wait_answer().await.unwrap();
        (out, reply)
    };
    let receiver = async {
        let stream = rig.at_b.accept_bi().await.unwrap();
        svc.handle_stream(rig.at_b.peer(), stream).await.unwrap()
    };
    let ((out, reply), handled) = tokio::join!(sender, receiver);
    (out, reply, handled)
}

#[tokio::test]
async fn fr_f3_accepted_offer() {
    let rig = rig().await;
    let prompter = TestPrompter::answering(PromptAnswer::Accept);
    let svc = service(&rig, registered(&[rig.a.peer_id()]), prompter.clone(), Limits::default(), ReceiveMode::Ask);
    let (out, reply, handled) = run(&rig, &svc, vec![file("a.txt", 3), file("b.txt", 4)]).await;
    assert_eq!(reply, Reply::Accepted);
    assert_eq!(out.state(), &State::Accepted);
    assert_eq!(handled.state(), &State::Accepted);
    assert_eq!(handled.total, 7);
    assert_eq!(handled.from, rig.a.peer_id());
    assert_eq!(prompter.prompts(), 1);
    assert!(handled.halves.is_some());
}

#[tokio::test]
async fn fr_f3_declined_offer_tells_the_sender() {
    let rig = rig().await;
    let prompter = TestPrompter::answering(PromptAnswer::Decline);
    let svc = service(&rig, registered(&[rig.a.peer_id()]), prompter, Limits::default(), ReceiveMode::Ask);
    let (out, reply, handled) = run(&rig, &svc, vec![file("a.txt", 3)]).await;
    assert!(matches!(reply, Reply::Declined { code: DeclineCode::User, .. }));
    assert_eq!(out.state(), &State::Declined);
    assert_eq!(handled.state(), &State::Declined);
    assert!(handled.halves.is_none());
}

#[tokio::test]
async fn fr_f10_unregistered_peer_is_refused_before_any_prompt() {
    let rig = rig().await;
    let prompter = TestPrompter::answering(PromptAnswer::Accept);
    let stranger = MemNetwork::new().transport().peer_id();
    let svc = service(&rig, registered(&[stranger]), prompter.clone(), Limits::default(), ReceiveMode::Ask);
    let (out, reply, handled) = run(&rig, &svc, vec![file("a.txt", 3)]).await;
    assert!(matches!(reply, Reply::Declined { code: DeclineCode::NotRegistered, .. }));
    assert_eq!(out.state(), &State::Declined);
    assert_eq!(handled.refusal, Some(DeclineCode::NotRegistered));
    assert_eq!(prompter.prompts(), 0);
}

#[tokio::test]
async fn fr_f9_unregistered_peer_is_refused_even_in_auto_accept() {
    let rig = rig().await;
    let prompter = TestPrompter::answering(PromptAnswer::Accept);
    let svc = service(
        &rig,
        registered(&[]),
        prompter.clone(),
        Limits::default(),
        ReceiveMode::AutoAcceptFrom([rig.a.peer_id()].into_iter().collect()),
    );
    let (_, reply, _) = run(&rig, &svc, vec![file("a.txt", 3)]).await;
    assert!(matches!(reply, Reply::Declined { code: DeclineCode::NotRegistered, .. }));
}

#[tokio::test]
async fn fr_f10_over_limit_is_refused_with_no_prompt_and_the_limit_named() {
    let rig = rig().await;
    let prompter = TestPrompter::answering(PromptAnswer::Accept);
    let svc = service(&rig, registered(&[rig.a.peer_id()]), prompter.clone(), Limits::default(), ReceiveMode::Ask);
    let (_, reply, handled) = run(&rig, &svc, vec![file("a", 3 * GIB), file("b", 3 * GIB)]).await;
    match reply {
        Reply::Declined { code: DeclineCode::TooLarge, detail } => {
            assert!(detail.contains(&(4 * GIB).to_string()), "{detail}");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(handled.refusal, Some(DeclineCode::TooLarge));
    assert_eq!(prompter.prompts(), 0);
}

#[tokio::test]
async fn fr_f10_exactly_the_limit_is_accepted_and_changing_it_changes_the_next_decision() {
    let rig = rig().await;
    let prompter = TestPrompter::answering(PromptAnswer::Accept);
    let svc = service(&rig, registered(&[rig.a.peer_id()]), prompter, Limits::default(), ReceiveMode::Ask);
    let (_, reply, _) = run(&rig, &svc, vec![file("a", 4 * GIB)]).await;
    assert_eq!(reply, Reply::Accepted);
    svc.set_limits(Limits { max_bytes: GIB });
    let (_, reply, _) = run(&rig, &svc, vec![file("a", 2 * GIB)]).await;
    assert!(matches!(reply, Reply::Declined { code: DeclineCode::TooLarge, .. }));
}

#[tokio::test]
async fn fr_f9_receive_mode_off_refuses_every_offer() {
    let rig = rig().await;
    let prompter = TestPrompter::answering(PromptAnswer::Accept);
    let svc = service(&rig, registered(&[rig.a.peer_id()]), prompter.clone(), Limits::default(), ReceiveMode::Off);
    let (_, reply, _) = run(&rig, &svc, vec![file("a.txt", 3)]).await;
    assert!(matches!(reply, Reply::Declined { code: DeclineCode::ReceiveOff, .. }));
    assert_eq!(prompter.prompts(), 0);
}

#[tokio::test]
async fn fr_f9_auto_accept_skips_only_the_prompt_not_the_size_check() {
    let rig = rig().await;
    let prompter = TestPrompter::answering(PromptAnswer::Decline);
    let mode = ReceiveMode::AutoAcceptFrom([rig.a.peer_id()].into_iter().collect());
    let svc = service(&rig, registered(&[rig.a.peer_id()]), prompter.clone(), Limits::default(), mode);
    let (_, reply, _) = run(&rig, &svc, vec![file("a.txt", 3)]).await;
    assert_eq!(reply, Reply::Accepted);
    assert_eq!(prompter.prompts(), 0);
    let (_, reply, _) = run(&rig, &svc, vec![file("big", 5 * GIB)]).await;
    assert!(matches!(reply, Reply::Declined { code: DeclineCode::TooLarge, .. }));
}

#[tokio::test]
async fn fr_f9_the_prompt_sees_only_cleaned_names() {
    let rig = rig().await;
    let prompter = TestPrompter::answering(PromptAnswer::Accept);
    let svc = service(&rig, registered(&[rig.a.peer_id()]), prompter.clone(), Limits::default(), ReceiveMode::Ask);
    let (_, reply, handled) = run(
        &rig,
        &svc,
        vec![file("../x", 1), file(".bashrc", 1), file("d/../../y", 1), file("a/b\\c", 1)],
    )
    .await;
    assert_eq!(reply, Reply::Accepted);
    let seen = prompter.seen.lock().unwrap();
    let names: Vec<&str> = seen[0].files.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, vec!["x", "bashrc", "d/y", "a/b/c"]);
    let kept: Vec<&str> = handled.files.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(kept, names);
}

#[tokio::test]
async fn fr_f3_malformed_offers_are_refused() {
    let rig = rig().await;
    let prompter = TestPrompter::answering(PromptAnswer::Accept);
    let svc = service(&rig, registered(&[rig.a.peer_id()]), prompter.clone(), Limits::default(), ReceiveMode::Ask);
    let (_, reply, _) = run(&rig, &svc, vec![]).await;
    assert!(matches!(reply, Reply::Declined { code: DeclineCode::Invalid, .. }));
    assert_eq!(prompter.prompts(), 0);
}

#[tokio::test]
async fn fr_f3_an_answer_after_the_time_limit_expires_the_offer_on_both_sides() {
    let rig = rig().await;
    let prompter = TestPrompter::answering(PromptAnswer::Accept);
    *prompter.advance.lock().unwrap() = Some((rig.clock.clone(), DEFAULT_OFFER_TTL));
    let svc = service(&rig, registered(&[rig.a.peer_id()]), prompter, Limits::default(), ReceiveMode::Ask);
    let (out, reply, handled) = run(&rig, &svc, vec![file("a.txt", 3)]).await;
    assert_eq!(reply, Reply::Expired);
    assert_eq!(out.state(), &State::Expired);
    assert_eq!(handled.state(), &State::Expired);
    assert!(handled.halves.is_none());
}

#[tokio::test]
async fn fr_f4_sender_cancels_while_the_prompt_is_open() {
    let rig = rig().await;
    let (tx, rx) = oneshot::channel();
    let prompter = Arc::new(TestPrompter::default());
    *prompter.block.lock().unwrap() = Some(rx);
    let svc = service(&rig, registered(&[rig.a.peer_id()]), prompter.clone(), Limits::default(), ReceiveMode::Ask);

    let sender = async {
        let mut out = Outgoing::start(&rig.a_to_b, vec![file("a.txt", 3)], rig.clock.clone(), DEFAULT_OFFER_TTL)
            .await
            .unwrap();
        // Wait until the receiver is prompting, then cancel.
        while prompter.prompts() == 0 {
            tokio::task::yield_now().await;
        }
        out.cancel().await.unwrap();
        out
    };
    let receiver = async {
        let stream = rig.at_b.accept_bi().await.unwrap();
        svc.handle_stream(rig.at_b.peer(), stream).await.unwrap()
    };
    let (out, handled) = tokio::join!(sender, receiver);
    drop(tx);
    assert_eq!(out.state(), &State::Cancelled);
    assert_eq!(handled.state(), &State::Cancelled);
    assert!(handled.halves.is_none());
}

#[tokio::test]
async fn fr_f4_receiver_cancels_an_accepted_transfer() {
    let rig = rig().await;
    let prompter = TestPrompter::answering(PromptAnswer::Accept);
    let svc = service(&rig, registered(&[rig.a.peer_id()]), prompter, Limits::default(), ReceiveMode::Ask);
    let (mut out, reply, mut handled) = run(&rig, &svc, vec![file("a.txt", 3)]).await;
    assert_eq!(reply, Reply::Accepted);
    let (r1, r2) = tokio::join!(handled.cancel(), out.wait_cancel());
    r1.unwrap();
    r2.unwrap();
    assert_eq!(out.state(), &State::Cancelled);
    assert_eq!(handled.state(), &State::Cancelled);
    // A second cancel is an illegal transition.
    assert!(handled.cancel().await.is_err());
}

#[tokio::test]
async fn fr_f4_sender_cancels_after_the_answer() {
    let rig = rig().await;
    let prompter = TestPrompter::answering(PromptAnswer::Accept);
    let svc = service(&rig, registered(&[rig.a.peer_id()]), prompter, Limits::default(), ReceiveMode::Ask);
    let (mut out, _, handled) = run(&rig, &svc, vec![file("a.txt", 3)]).await;
    out.cancel().await.unwrap();
    assert_eq!(out.state(), &State::Cancelled);
    assert!(out.cancel().await.is_err());
    drop(handled);
    let _ = &rig.b;
}

#[tokio::test]
async fn fr_f5_streams_chunks_end_to_end_over_transport_and_saves_mode_0644() {
    let rig = rig().await;
    let prompter = TestPrompter::answering(PromptAnswer::Accept);
    let svc = service(&rig, registered(&[rig.a.peer_id()]), prompter, Limits::default(), ReceiveMode::Ask);
    let p1 = b"first file payload over ember-transfer";
    let p2 = b"#!/bin/sh\necho second file\n";
    let (mut out, reply, mut handled) = run(
        &rig,
        &svc,
        vec![file("../notes.txt", p1.len() as u64), file("sub/run.sh", p2.len() as u64)],
    )
    .await;
    assert_eq!(reply, Reply::Accepted);

    let dir = tempfile::tempdir().unwrap();
    let recv_dir = dir.path().to_path_buf();
    let payloads: [&[u8]; 2] = [p1.as_slice(), p2.as_slice()];
    let (sent, stored) = tokio::join!(
        out.send_bytes(&payloads, 8),
        handled.receive_into(&recv_dir, Limits::default()),
    );
    let digests = sent.unwrap();
    let stored = stored.unwrap();
    assert_eq!(out.state(), &State::Done);
    assert_eq!(handled.state(), &State::Done);
    assert_eq!(stored.len(), 2);
    assert_eq!(stored[0].name, "notes.txt");
    assert_eq!(stored[0].sha256, digests[0]);
    assert_eq!(std::fs::read(&stored[0].path).unwrap(), p1);
    assert_eq!(stored[1].name, "sub/run.sh");
    assert_eq!(stored[1].sha256, digests[1]);
    assert_eq!(std::fs::read(&stored[1].path).unwrap(), p2);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for s in &stored {
            let mode = std::fs::metadata(&s.path).unwrap().permissions().mode() & 0o7777;
            assert_eq!(mode, 0o644);
        }
    }
}
