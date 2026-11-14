//! The app's owner-presence claim (SPEC `FR-S12`, INTENT D37 "Owner presence", #784, #842).
//!
//! At start the app claims an owner key over the server's claim door (`<data dir>/owner.sock`,
//! on Windows the named pipe; [`OwnerEndpoint`]) and keeps the link open: the key lives exactly
//! as long as the link. The first claim is confirmed by the code the server prints on its
//! console; the app asks for it ([`Prompt::Code`]). While the app holds a key, another claimant
//! waits for this app's yes or no ([`Prompt::Pending`]).
//!
//! This module has no UI. [`on_message`], [`on_command`] and [`after_close`] are the pure
//! transitions; [`drive`] owns the [`OwnerLink`] and runs them; [`ClaimHooks`] is how the
//! driver hands the key to the [`ember_client::api::Api`] and the view to the screens.
//!
//! The key is handed to [`ClaimHooks::set_key`] and to nothing else: it is never logged, never
//! written to a file, and never part of [`ClaimState`], [`ClaimView`] or any `Debug` output.
//! When the link closes or is refused the key is cleared at once.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use ember_client::owner::{OwnerEndpoint, OwnerKey, OwnerMessage};
use tokio::sync::mpsc;

/// The name the server shows for this claimant.
pub const CLAIMANT_NAME: &str = "Ember";

/// One claim waiting for this app's decision (this app holds a key).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingClaim {
    pub claim: u64,
    pub pid: u64,
    pub exe: String,
    /// What the claimant calls itself. Chosen by the claimant: shown beside `exe` and `pid`.
    pub name: String,
}

/// Where the claim stands. Holds no key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimState {
    /// Never claimed: demo mode, the transport, a server that is not on this computer, or no
    /// claim door known.
    Off,
    /// Connecting to the claim door.
    Connecting,
    /// The server waits for the console code. With `holders > 0` a window that holds a key may
    /// approve instead.
    NeedCode { holders: u64 },
    /// The code was wrong; `left` tries remain.
    WrongCode { left: u64, holders: u64 },
    /// A code was sent; the answer has not come.
    Checking { holders: u64 },
    /// The key is held and sent on owner routes.
    Held,
    /// The key is held and other claims wait for this app's decision (the first is shown).
    PendingOther { claims: Vec<PendingClaim> },
    /// The server refused; no automatic retry. `reason` is the server's word
    /// ([`OwnerMessage::Refused`]).
    Refused { reason: String },
    /// The link closed or could not be opened; the key is cleared and the next claim starts in
    /// `retry_in_ms`.
    Lost { retry_in_ms: u64 },
}

/// What the screens read: the state and which claim attempt it belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimView {
    pub state: ClaimState,
    /// Counts claim attempts, so a prompt dismissed for one attempt opens again for the next.
    pub generation: u64,
}

impl Default for ClaimView {
    fn default() -> Self {
        ClaimView { state: ClaimState::Off, generation: 0 }
    }
}

/// The result of one server line. No `Debug`: it may carry the key.
pub struct Step {
    pub state: ClaimState,
    /// `Some` on a grant: hand it to [`ClaimHooks::set_key`], keep it nowhere else.
    pub key: Option<OwnerKey>,
    /// A line for the user (another claimant got a key).
    pub notice: Option<String>,
    /// The server ends the link after this line (a refusal).
    pub end: bool,
}

/// What the user asked for.
#[derive(Clone, PartialEq, Eq)]
pub enum Command {
    /// The console code as typed.
    Submit(String),
    Approve(u64),
    Deny(u64),
    /// Claim again now (after a refusal, or while waiting for the next try).
    Retry,
}

/// The code is a one-time secret: `Debug` hides it.
impl std::fmt::Debug for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Command::Submit(_) => f.write_str("Submit(hidden)"),
            Command::Approve(c) => write!(f, "Approve({c})"),
            Command::Deny(c) => write!(f, "Deny({c})"),
            Command::Retry => f.write_str("Retry"),
        }
    }
}

/// What the driver sends on the link for a command.
#[derive(Clone, PartialEq, Eq)]
pub enum LinkOp {
    Confirm(String),
    Approve(u64),
    Deny(u64),
}

impl std::fmt::Debug for LinkOp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LinkOp::Confirm(_) => f.write_str("Confirm(hidden)"),
            LinkOp::Approve(c) => write!(f, "Approve({c})"),
            LinkOp::Deny(c) => write!(f, "Deny({c})"),
        }
    }
}

/// How long the driver waits before the next claim after a lost link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Backoff {
    pub first_ms: u64,
    pub max_ms: u64,
}

impl Default for Backoff {
    /// 1 s, doubling, at most a minute: a restarted server is found quickly, a missing door is
    /// asked about once a minute.
    fn default() -> Self {
        Backoff { first_ms: 1_000, max_ms: 60_000 }
    }
}

impl Backoff {
    /// The wait before try `attempt` (0 is the first retry).
    pub fn delay_ms(&self, attempt: u32) -> u64 {
        todo!("FR-S12 #842: backoff {attempt}")
    }
}

/// Whether the app claims at all: a real server (not the demo), over plain HTTP (not the
/// transport), on this computer (a loopback host), with a claim door known.
pub fn applicable(demo: bool, base_url: &str, over_transport: bool, endpoint: Option<&OwnerEndpoint>) -> bool {
    todo!("FR-S12 #842: applicable {demo} {base_url} {over_transport} {endpoint:?}")
}

/// A refusal after which the driver claims again by itself (with the backoff).
pub fn retryable(reason: &str) -> bool {
    todo!("FR-S12 #842: retryable {reason}")
}

/// One server line in `state`.
pub fn on_message(state: &ClaimState, msg: OwnerMessage) -> Step {
    todo!("FR-S12 #842: on_message {state:?} {msg:?}")
}

/// A user command in `state` while the link is open: what to send, and the state after it.
/// `None` for the op ignores the command.
pub fn on_command(state: &ClaimState, cmd: &Command) -> (Option<LinkOp>, ClaimState) {
    todo!("FR-S12 #842: on_command {state:?} {cmd:?}")
}

/// The state after the link closed in `last`, with `attempt` failed tries before it and
/// `held` whether a key was granted on it: the next state, the wait before the next claim
/// (`None`: wait for [`Command::Retry`]) and the next attempt count.
pub fn after_close(last: &ClaimState, held: bool, attempt: u32, backoff: &Backoff) -> (ClaimState, Option<Duration>, u32) {
    todo!("FR-S12 #842: after_close {last:?} {held} {attempt} {backoff:?}")
}

// ---- texts and the prompt the screens show ------------------------------------------------

pub const SHEET_TITLE: &str = "소유자 확인";
pub const PENDING_TITLE: &str = "소유자 키 요청";
pub const CODE_LINE: &str =
    "서버를 실행한 터미널에 확인 코드가 표시되었습니다. 그 코드를 입력하면 이 앱에서 승인, 기기와 계정 변경 같은 소유자 작업을 할 수 있습니다.";
pub const HOLDERS_LINE: &str = "소유자 키를 가진 다른 Ember 창에서 이 요청을 허용해도 됩니다.";
pub const CODE_PLACEHOLDER: &str = "확인 코드";
pub const CHECKING_LINE: &str = "코드를 확인하는 중입니다";
pub const PENDING_LINE: &str = "다른 프로그램이 이 서버의 소유자 키를 요청합니다. 직접 실행한 프로그램이 아니면 거부하세요.";
pub const SUBMIT: &str = "확인";
pub const LATER: &str = "나중에";
pub const APPROVE: &str = "허용";
pub const DENY: &str = "거부";
pub const RETRY: &str = "다시 시도";
pub const CLOSE: &str = "닫기";
/// The notice for a 403 `owner_required` while the app claims: the code sheet opens with it.
pub const OWNER_NEEDED: &str = "이 작업에는 소유자 확인이 필요합니다. 서버 콘솔에 표시된 코드를 입력하세요.";
/// The notice for a 403 `owner_required` when the app does not claim (a server elsewhere).
pub const OWNER_NEEDED_ELSEWHERE: &str =
    "이 작업에는 소유자 확인이 필요합니다. 서버 컴퓨터의 터미널에서 ember-server owner 명령으로 실행하세요.";

/// The line for wrong code with `left` tries remaining.
pub fn wrong_code_text(left: u64) -> String {
    todo!("FR-S12 #842: wrong_code_text {left}")
}

/// The fixed sentence for a refusal reason. Never the server's own words.
pub fn refused_text(reason: &str) -> &'static str {
    todo!("FR-S12 #842: refused_text {reason}")
}

/// What the claim sheet shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Prompt {
    /// The code field. `left` after a wrong code; `checking` while a code is on its way.
    Code { holders: u64, left: Option<u64>, checking: bool },
    /// A refusal, with 다시 시도.
    Refused { text: &'static str },
    /// Another claim to allow or refuse; `more` waiting behind it.
    Pending { claim: PendingClaim, more: usize },
}

/// The prompt for `state`; `None` shows no sheet.
pub fn prompt(state: &ClaimState) -> Option<Prompt> {
    todo!("FR-S12 #842: prompt {state:?}")
}

/// What dismissing the sheet hides: the attempt for a code or a refusal, the claim for a
/// pending request. A new attempt or a new claim opens the sheet again.
pub fn prompt_key(view: &ClaimView) -> Option<String> {
    todo!("FR-S12 #842: prompt_key {view:?}")
}

/// Whether the sheet is open for `view` when the user dismissed `dismissed`.
pub fn sheet_open(view: &ClaimView, dismissed: Option<&str>) -> bool {
    todo!("FR-S12 #842: sheet_open {view:?} {dismissed:?}")
}

/// True for error text that carries a 403 `owner_required`: the code itself (as
/// [`crate::server::ServerError`] prints it), or the server's message behind a 403 (as
/// [`ember_client::api::ApiError`] prints it).
pub fn text_is_owner_required(text: &str) -> bool {
    todo!("FR-S12 #842: text_is_owner_required {text}")
}

// ---- the driver ----------------------------------------------------------------------------

/// Where the driver puts what it learns. No `Debug` anywhere on this path.
pub trait ClaimHooks: Send + Sync + 'static {
    /// Sets (`Some`) or clears (`None`) the key on every client that sends it. False when a
    /// client would not keep it.
    fn set_key(&self, key: Option<&OwnerKey>) -> bool;
    /// The new state, for the screens.
    fn view(&self, view: ClaimView);
    /// A line for the user.
    fn notice(&self, text: String);
}

/// Sends commands to a running [`drive`].
#[derive(Clone)]
pub struct ClaimHandle {
    tx: mpsc::UnboundedSender<Command>,
}

impl ClaimHandle {
    pub fn submit(&self, code: &str) {
        let _ = self.tx.send(Command::Submit(code.to_string()));
    }
    pub fn approve(&self, claim: u64) {
        let _ = self.tx.send(Command::Approve(claim));
    }
    pub fn deny(&self, claim: u64) {
        let _ = self.tx.send(Command::Deny(claim));
    }
    pub fn retry(&self) {
        let _ = self.tx.send(Command::Retry);
    }
}

/// Starts [`drive`] on `rt` and returns its handle. Dropping every handle ends the driver,
/// which clears the key.
#[cfg(any(unix, windows))]
pub fn spawn(rt: &tokio::runtime::Handle, endpoint: OwnerEndpoint, hooks: Arc<dyn ClaimHooks>, backoff: Backoff) -> ClaimHandle {
    let (tx, rx) = mpsc::unbounded_channel();
    rt.spawn(drive(endpoint, CLAIMANT_NAME.to_string(), hooks, backoff, rx));
    ClaimHandle { tx }
}

/// Claims, holds and claims again until `cmds` closes.
#[cfg(any(unix, windows))]
pub async fn drive(
    endpoint: OwnerEndpoint,
    name: String,
    hooks: Arc<dyn ClaimHooks>,
    backoff: Backoff,
    mut cmds: mpsc::UnboundedReceiver<Command>,
) {
    todo!("FR-S12 #842: drive {endpoint} {name} {backoff:?} {}", {
        let _ = (&hooks, &mut cmds);
        ""
    })
}

static HANDLE: OnceLock<ClaimHandle> = OnceLock::new();

/// Installs the app's claim handle (once, at start).
pub fn install(handle: ClaimHandle) {
    let _ = HANDLE.set(handle);
}

/// The app's claim, when it claims.
pub fn handle() -> Option<&'static ClaimHandle> {
    HANDLE.get()
}

/// A call came back 403 `owner_required`: claim again when nothing is in progress. True when
/// the app claims at all (the sheet then opens); false when it does not.
pub fn request_claim() -> bool {
    match handle() {
        Some(h) => {
            h.retry();
            true
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending(claim: u64) -> PendingClaim {
        PendingClaim { claim, pid: 41, exe: "/bin/x".into(), name: "cli".into() }
    }

    #[test]
    fn fr_s12_claim_only_for_a_real_local_server_over_http() {
        let ep = OwnerEndpoint::Socket("/d/owner.sock".into());
        assert!(applicable(false, "http://127.0.0.1:8740", false, Some(&ep)));
        assert!(applicable(false, "http://localhost:8740", false, Some(&ep)));
        assert!(applicable(false, "http://[::1]:8740", false, Some(&ep)));
        // Demo, transport, a remote host, no door, or not a URL: never claim.
        assert!(!applicable(true, "http://127.0.0.1:8740", false, Some(&ep)));
        assert!(!applicable(false, "http://127.0.0.1:8740", true, Some(&ep)));
        assert!(!applicable(false, "peer:abc", true, Some(&ep)));
        assert!(!applicable(false, "http://192.0.2.1:8740", false, Some(&ep)));
        assert!(!applicable(false, "http://studio.local:8740", false, Some(&ep)));
        assert!(!applicable(false, "http://127.0.0.1:8740", false, None));
        assert!(!applicable(false, "not a url", false, Some(&ep)));
    }

    #[test]
    fn fr_s12_messages_drive_the_claim_to_a_held_key() {
        let s = on_message(&ClaimState::Connecting, OwnerMessage::Confirm { claim: 3, holders: 1 });
        assert_eq!(s.state, ClaimState::NeedCode { holders: 1 });
        assert!(s.key.is_none() && !s.end);

        let s = on_message(&ClaimState::Checking { holders: 1 }, OwnerMessage::WrongCode { left: 2 });
        assert_eq!(s.state, ClaimState::WrongCode { left: 2, holders: 1 });

        let s = on_message(
            &ClaimState::Checking { holders: 0 },
            OwnerMessage::Granted { claim: 3, key: OwnerKey::new("eok_secret") },
        );
        assert_eq!(s.state, ClaimState::Held);
        assert_eq!(s.key.as_ref().map(|k| k.expose()), Some("eok_secret"));
        assert!(!format!("{:?}", s.state).contains("secret"));

        // Refusals end the link.
        let s = on_message(&ClaimState::NeedCode { holders: 0 }, OwnerMessage::Refused { reason: "agent_tree".into() });
        assert_eq!(s.state, ClaimState::Refused { reason: "agent_tree".into() });
        assert!(s.end && s.key.is_none());

        // Unknown lines change nothing.
        let s = on_message(&ClaimState::Held, OwnerMessage::Other);
        assert_eq!(s.state, ClaimState::Held);
        assert!(!s.end);
    }

    #[test]
    fn fr_s12_a_holder_sees_pending_claims_and_their_end() {
        let ask = |c: u64| OwnerMessage::ClaimPending { claim: c, pid: 41, exe: "/bin/x".into(), name: "cli".into() };
        let s = on_message(&ClaimState::Held, ask(8));
        assert_eq!(s.state, ClaimState::PendingOther { claims: vec![pending(8)] });
        let s = on_message(&s.state, ask(9));
        assert_eq!(s.state, ClaimState::PendingOther { claims: vec![pending(8), pending(9)] });
        // The same claim twice is listed once.
        let s = on_message(&s.state, ask(9));
        assert_eq!(s.state, ClaimState::PendingOther { claims: vec![pending(8), pending(9)] });
        let s = on_message(&s.state, OwnerMessage::ClaimClosed { claim: 8 });
        assert_eq!(s.state, ClaimState::PendingOther { claims: vec![pending(9)] });
        let s = on_message(&s.state, OwnerMessage::ClaimClosed { claim: 9 });
        assert_eq!(s.state, ClaimState::Held);
        // A claimant that holds nothing ignores pending claims.
        let s = on_message(&ClaimState::NeedCode { holders: 1 }, ask(8));
        assert_eq!(s.state, ClaimState::NeedCode { holders: 1 });
        // Another claimant got a key: told, state unchanged.
        let s = on_message(
            &ClaimState::Held,
            OwnerMessage::OwnerClaimed { claim: 9, pid: 41, exe: "/bin/x".into(), name: "cli".into() },
        );
        assert_eq!(s.state, ClaimState::Held);
        assert!(s.notice.is_some_and(|t| t.contains("cli")));
    }

    #[test]
    fn fr_s12_commands_send_the_code_and_the_decisions() {
        let (op, st) = on_command(&ClaimState::NeedCode { holders: 0 }, &Command::Submit(" ab-cd ".into()));
        assert_eq!(op, Some(LinkOp::Confirm("ab-cd".into())));
        assert_eq!(st, ClaimState::Checking { holders: 0 });
        let (op, st) = on_command(&ClaimState::WrongCode { left: 1, holders: 2 }, &Command::Submit("x".into()));
        assert_eq!(op, Some(LinkOp::Confirm("x".into())));
        assert_eq!(st, ClaimState::Checking { holders: 2 });
        // An empty code, or a code while nothing asks for one, is not sent.
        let (op, st) = on_command(&ClaimState::NeedCode { holders: 0 }, &Command::Submit("  ".into()));
        assert_eq!((op, st), (None, ClaimState::NeedCode { holders: 0 }));
        let (op, _) = on_command(&ClaimState::Checking { holders: 0 }, &Command::Submit("x".into()));
        assert_eq!(op, None);
        let (op, _) = on_command(&ClaimState::Held, &Command::Submit("x".into()));
        assert_eq!(op, None);

        let two = ClaimState::PendingOther { claims: vec![pending(8), pending(9)] };
        let (op, st) = on_command(&two, &Command::Approve(8));
        assert_eq!(op, Some(LinkOp::Approve(8)));
        assert_eq!(st, ClaimState::PendingOther { claims: vec![pending(9)] });
        let (op, st) = on_command(&st, &Command::Deny(9));
        assert_eq!(op, Some(LinkOp::Deny(9)));
        assert_eq!(st, ClaimState::Held);
        // A claim that is not pending is not decided.
        let (op, st) = on_command(&two, &Command::Approve(5));
        assert_eq!((op, st), (None, two.clone()));
        // Retry while the link is open does nothing.
        let (op, st) = on_command(&ClaimState::Held, &Command::Retry);
        assert_eq!((op, st), (None, ClaimState::Held));
        // The code never shows in Debug output.
        assert!(!format!("{:?} {:?}", Command::Submit("ab-cd".into()), LinkOp::Confirm("ab-cd".into())).contains("ab-cd"));
    }

    #[test]
    fn fr_s12_backoff_doubles_and_is_capped() {
        let b = Backoff::default();
        assert_eq!(b.delay_ms(0), 1_000);
        assert_eq!(b.delay_ms(1), 2_000);
        assert_eq!(b.delay_ms(5), 32_000);
        assert_eq!(b.delay_ms(6), 60_000);
        assert_eq!(b.delay_ms(200), 60_000);
    }

    #[test]
    fn fr_s12_a_closed_link_backs_off_and_a_refusal_waits_for_retry() {
        let b = Backoff::default();
        // The link closed while holding: start again quickly.
        let (st, wait, next) = after_close(&ClaimState::Held, true, 4, &b);
        assert_eq!((st, wait, next), (ClaimState::Lost { retry_in_ms: 1_000 }, Some(Duration::from_millis(1_000)), 1));
        // No door: each failure doubles the wait.
        let (st, wait, next) = after_close(&ClaimState::Connecting, false, 3, &b);
        assert_eq!((st, wait, next), (ClaimState::Lost { retry_in_ms: 8_000 }, Some(Duration::from_millis(8_000)), 4));
        // A refusal the server may lift soon: retried.
        let (st, wait, _) = after_close(&ClaimState::Refused { reason: "busy".into() }, false, 0, &b);
        assert_eq!((st, wait), (ClaimState::Lost { retry_in_ms: 1_000 }, Some(Duration::from_millis(1_000))));
        assert!(retryable("server_stopping"));
        // A refusal that stands: shown, and only Retry claims again.
        for reason in ["agent_tree", "other_user", "unknown_process", "no_peer_pid", "wrong_code", "denied", "timeout", "bad_request"] {
            assert!(!retryable(reason), "{reason}");
            let (st, wait, next) = after_close(&ClaimState::Refused { reason: reason.into() }, false, 2, &b);
            assert_eq!((st, wait, next), (ClaimState::Refused { reason: reason.into() }, None, 0), "{reason}");
        }
    }

    #[test]
    fn fr_s12_the_sheet_shows_the_code_field_refusals_and_pending_claims() {
        assert_eq!(prompt(&ClaimState::Off), None);
        assert_eq!(prompt(&ClaimState::Connecting), None);
        assert_eq!(prompt(&ClaimState::Held), None);
        assert_eq!(prompt(&ClaimState::Lost { retry_in_ms: 1 }), None);
        assert_eq!(prompt(&ClaimState::NeedCode { holders: 1 }), Some(Prompt::Code { holders: 1, left: None, checking: false }));
        assert_eq!(
            prompt(&ClaimState::WrongCode { left: 2, holders: 0 }),
            Some(Prompt::Code { holders: 0, left: Some(2), checking: false })
        );
        assert_eq!(prompt(&ClaimState::Checking { holders: 0 }), Some(Prompt::Code { holders: 0, left: None, checking: true }));
        assert_eq!(
            prompt(&ClaimState::Refused { reason: "agent_tree".into() }),
            Some(Prompt::Refused { text: refused_text("agent_tree") })
        );
        assert_eq!(
            prompt(&ClaimState::PendingOther { claims: vec![pending(8), pending(9)] }),
            Some(Prompt::Pending { claim: pending(8), more: 1 })
        );

        // Dismissing hides this attempt only; the next attempt opens the sheet again.
        let v = ClaimView { state: ClaimState::NeedCode { holders: 0 }, generation: 3 };
        let key = prompt_key(&v).unwrap();
        assert!(sheet_open(&v, None));
        assert!(!sheet_open(&v, Some(&key)));
        let refused = ClaimView { state: ClaimState::Refused { reason: "timeout".into() }, generation: 3 };
        assert!(!sheet_open(&refused, Some(&key)), "a refusal of a dismissed attempt stays hidden");
        let next = ClaimView { state: ClaimState::NeedCode { holders: 0 }, generation: 4 };
        assert!(sheet_open(&next, Some(&key)));
        // A pending claim opens the sheet even after a code prompt was dismissed.
        let p = ClaimView { state: ClaimState::PendingOther { claims: vec![pending(8)] }, generation: 3 };
        assert!(sheet_open(&p, Some(&key)));
        assert!(!sheet_open(&ClaimView::default(), None));
    }

    #[test]
    fn fr_s12_texts_are_fixed_korean_sentences() {
        assert!(wrong_code_text(2).contains('2'));
        for reason in ["agent_tree", "other_user", "unknown_process", "no_peer_pid", "wrong_code", "denied", "timeout", "busy", "server_stopping", "bad_request", "later_reason"] {
            let t = refused_text(reason);
            assert!(!t.is_empty() && !t.contains(reason), "{reason}: {t}");
        }
        for t in [CODE_LINE, HOLDERS_LINE, PENDING_LINE, OWNER_NEEDED, OWNER_NEEDED_ELSEWHERE] {
            assert!(!t.contains('\u{2014}'));
        }
    }

    #[test]
    fn fr_s12_owner_required_text_is_recognised() {
        assert!(text_is_owner_required("server answered 403: owner_required: this route needs owner presence"));
        assert!(text_is_owner_required(
            "응답하지 못했습니다: server answered 403: this route needs owner presence: the Ember app, or `ember-server owner` in a terminal"
        ));
        assert!(!text_is_owner_required("server answered 403: this device's scope (chat) does not allow this route"));
        assert!(!text_is_owner_required("server answered 500: boom"));
    }

    // ---- the driver against a fake door ----------------------------------------------------

    #[derive(Default)]
    struct Seen {
        views: std::sync::Mutex<Vec<ClaimView>>,
        notices: std::sync::Mutex<Vec<String>>,
    }

    struct TestHooks {
        api: ember_client::api::Api,
        seen: Arc<Seen>,
    }

    impl ClaimHooks for TestHooks {
        fn set_key(&self, key: Option<&OwnerKey>) -> bool {
            self.api.set_owner_key(key)
        }
        fn view(&self, view: ClaimView) {
            self.seen.views.lock().unwrap().push(view);
        }
        fn notice(&self, text: String) {
            self.seen.notices.lock().unwrap().push(text);
        }
    }

    #[cfg(unix)]
    async fn wait_for(seen: &Seen, what: &str, f: impl Fn(&ClaimState) -> bool) {
        for _ in 0..500 {
            if seen.views.lock().unwrap().last().is_some_and(|v| f(&v.state)) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("never reached {what}: {:?}", seen.views.lock().unwrap());
    }

    /// The driver against a door that speaks the server's line protocol: the console code
    /// (one wrong try), the grant, a pending claim it approves, and the link closing, which
    /// clears the key and schedules the next claim.
    #[cfg(unix)]
    #[tokio::test]
    async fn fr_s12_driver_claims_holds_and_loses_the_key_with_a_fake_door() {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        // A relative path keeps the socket inside the repository and under the length limit
        // of a unix socket path (the tests run in `ember/app`).
        let dir = std::path::Path::new("../../.scratch/owner-claim-test");
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(format!("{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let listener = tokio::net::UnixListener::bind(&path).unwrap();

        let (seen_tx, mut seen_rx) = mpsc::unbounded_channel::<serde_json::Value>();
        let (close_tx, close_rx) = tokio::sync::oneshot::channel::<()>();
        let door = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (r, mut w) = stream.into_split();
            let mut lines = BufReader::new(r).lines();
            async fn read(lines: &mut tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>) -> serde_json::Value {
                let line = lines.next_line().await.unwrap().unwrap();
                serde_json::from_str::<serde_json::Value>(&line).unwrap()
            }
            let hello = read(&mut lines).await;
            seen_tx.send(hello).unwrap();
            w.write_all(b"{\"state\":\"confirm\",\"claim\":1,\"holders\":0}\n").await.unwrap();
            loop {
                let v = read(&mut lines).await;
                seen_tx.send(v.clone()).unwrap();
                if v["code"] == "ABCD-EFGH" {
                    break;
                }
                w.write_all(b"{\"state\":\"wrong_code\",\"left\":2}\n").await.unwrap();
            }
            w.write_all(b"{\"state\":\"granted\",\"claim\":1,\"key\":\"eok_fake_door_key\"}\n").await.unwrap();
            w.write_all(b"{\"event\":\"claim_pending\",\"claim\":2,\"pid\":77,\"exe\":\"/bin/cli\",\"name\":\"cli\"}\n")
                .await
                .unwrap();
            let v = read(&mut lines).await;
            seen_tx.send(v).unwrap();
            w.write_all(b"{\"event\":\"claim_closed\",\"claim\":2}\n").await.unwrap();
            let _ = close_rx.await;
            // Closing the link voids the key.
            drop(w);
            drop(lines);
        });

        let api = ember_client::api::Api::with_token("http://127.0.0.1:1", None).unwrap();
        let seen = Arc::new(Seen::default());
        let hooks = Arc::new(TestHooks { api: api.clone(), seen: seen.clone() });
        let (tx, rx) = mpsc::unbounded_channel();
        let handle = ClaimHandle { tx };
        let backoff = Backoff { first_ms: 60_000, max_ms: 60_000 };
        let driver = tokio::spawn(drive(OwnerEndpoint::Socket(path.clone()), CLAIMANT_NAME.into(), hooks, backoff, rx));

        let hello = seen_rx.recv().await.unwrap();
        assert_eq!((hello["op"].as_str(), hello["name"].as_str()), (Some("claim"), Some(CLAIMANT_NAME)));
        wait_for(&seen, "NeedCode", |s| *s == ClaimState::NeedCode { holders: 0 }).await;
        assert!(!api.has_owner_key());

        handle.submit("WRONG");
        assert_eq!(seen_rx.recv().await.unwrap(), serde_json::json!({ "op": "confirm", "code": "WRONG" }));
        wait_for(&seen, "WrongCode", |s| *s == ClaimState::WrongCode { left: 2, holders: 0 }).await;

        handle.submit("ABCD-EFGH");
        assert_eq!(seen_rx.recv().await.unwrap(), serde_json::json!({ "op": "confirm", "code": "ABCD-EFGH" }));
        wait_for(&seen, "PendingOther", |s| matches!(s, ClaimState::PendingOther { .. })).await;
        assert!(api.has_owner_key(), "the granted key reaches the Api");

        handle.approve(2);
        assert_eq!(seen_rx.recv().await.unwrap(), serde_json::json!({ "op": "approve", "claim": 2 }));
        wait_for(&seen, "Held", |s| *s == ClaimState::Held).await;
        assert!(api.has_owner_key());

        close_tx.send(()).unwrap();
        wait_for(&seen, "Lost", |s| matches!(s, ClaimState::Lost { .. })).await;
        assert!(!api.has_owner_key(), "a closed link clears the key");

        // The key is in no view and no notice.
        let views = format!("{:?}", seen.views.lock().unwrap());
        assert!(!views.contains("eok_fake_door_key"), "{views}");
        assert!(!seen.notices.lock().unwrap().iter().any(|n| n.contains("eok_")));
        // Every view of this link belongs to one attempt.
        assert!(seen.views.lock().unwrap().iter().all(|v| v.generation == 1));

        // Dropping the handle ends the driver, which leaves the key cleared.
        drop(handle);
        tokio::time::timeout(Duration::from_secs(5), driver).await.expect("the driver ends").unwrap();
        door.await.unwrap();
        assert!(!api.has_owner_key());
        let _ = std::fs::remove_file(&path);
    }

    /// No door at all (an older server): the driver backs off quietly and never prompts.
    #[cfg(unix)]
    #[tokio::test]
    async fn fr_s12_driver_without_a_door_backs_off_without_a_prompt() {
        let api = ember_client::api::Api::with_token("http://127.0.0.1:1", None).unwrap();
        let seen = Arc::new(Seen::default());
        let hooks = Arc::new(TestHooks { api: api.clone(), seen: seen.clone() });
        let (tx, rx) = mpsc::unbounded_channel();
        let handle = ClaimHandle { tx };
        let backoff = Backoff { first_ms: 20, max_ms: 40 };
        let missing = std::path::PathBuf::from("../../.scratch/owner-claim-test/missing.sock");
        let driver = tokio::spawn(drive(OwnerEndpoint::Socket(missing), CLAIMANT_NAME.into(), hooks, backoff, rx));
        let lost = || seen.views.lock().unwrap().iter().filter(|x| matches!(x.state, ClaimState::Lost { .. })).count();
        for _ in 0..500 {
            if lost() >= 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(lost() >= 2, "{:?}", seen.views.lock().unwrap());
        assert!(seen.views.lock().unwrap().iter().all(|v| prompt(&v.state).is_none()));
        assert!(!api.has_owner_key());
        drop(handle);
        tokio::time::timeout(Duration::from_secs(5), driver).await.expect("the driver ends").unwrap();
    }
}
