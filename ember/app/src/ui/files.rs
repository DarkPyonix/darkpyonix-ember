//! The Files screen (FR-L27, FR-L27.1 to FR-L27.6, `docs/DESIGN.md` section 6.8): file transfer
//! between the user's devices (D24, FR-F1 to FR-F9, Draft).
//!
//! The decisions are plain types and functions in the first half of this file, unit-tested
//! without a renderer. The components in the second half only hand them to dioxus-compose.
//!
//! The transfer feature has no server or node API yet (FR-F is Draft), so the screen is built
//! against [`TransferService`]. The only implementation is [`NoTransferService`]: it holds no
//! device and no transfer, so the screen shows the user's real computers (from the computers API)
//! as devices and [`UNAVAILABLE`] for sending, receiving and history. Nothing is invented.
//!
//! #166 workarounds (revert when upstream lands):
//! - No drag and drop (dioxus-compose and compose-rust are frozen): a send sheet picks the
//!   receiver, then the files. The toolkit has no file picker, so the files step says
//!   [`FILE_PICK_UNAVAILABLE`] (see [`FILE_PICKER_AVAILABLE`]).
//! - No OS share sheet (FR-F7) and no live push of transfer progress: the screen re-reads the
//!   service whenever the launcher state changes.
//! - Tiles are not clickable containers (pG), so each tile carries a small button.
//! - Device kind icons: `IconRole` has no phone or laptop glyph, so the kind is text.

use dioxus_compose::prelude::*;

use ember_client::state::{Computer, Reachability};

use super::components::*;
use super::home::{CrewMember, crew};
use super::theme::*;
use crate::services::services;
use crate::ui::use_ui;

/// What a number or section shows while the transfer feature has no data.
pub const UNAVAILABLE: &str = "준비 중입니다";

pub const EMPTY_DEVICES: &str = "연결된 장비가 없습니다";
pub const EMPTY_AGENTS: &str = "지금 일하는 팀원이 없습니다";
pub const EMPTY_TRANSFERS: &str = "진행 중인 전송이 없습니다";
pub const EMPTY_RECEIVED: &str = "받은 파일이 없습니다";
pub const EMPTY_HISTORY: &str = "전송 기록이 없습니다";

/// The files step of the send sheet while the toolkit has no file picker.
pub const FILE_PICK_UNAVAILABLE: &str = "파일 선택은 준비 중입니다";
pub const PICK_RECEIVER: &str = "받는 곳을 선택합니다";
pub const PICK_FILES: &str = "보낼 파일을 선택합니다";
pub const LOCK_LINE: &str = "연결된 장비 사이에서 암호화되어 전송됩니다";

/// TODO(dioxus-compose): flip when the toolkit has a file picker (and drag and drop), then fill
/// [`SendDraft::files`] from it.
pub const FILE_PICKER_AVAILABLE: bool = false;

// ---------------------------------------------------------------------------------------------
// Devices (FR-F1)
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    Phone,
    Tablet,
    Laptop,
    Desktop,
    Server,
    /// A registered computer whose form factor the computers API does not say.
    Computer,
}

impl DeviceKind {
    pub fn label(self) -> &'static str {
        match self {
            DeviceKind::Phone => "휴대폰",
            DeviceKind::Tablet => "태블릿",
            DeviceKind::Laptop => "노트북",
            DeviceKind::Desktop => "데스크톱",
            DeviceKind::Server => "서버",
            DeviceKind::Computer => "컴퓨터",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presence {
    Online,
    Sleeping,
    Offline,
    /// Not checked yet.
    Unknown,
}

impl Presence {
    pub fn label(self) -> &'static str {
        match self {
            Presence::Online => "온라인",
            Presence::Sleeping => "잠자기",
            Presence::Offline => "오프라인",
            Presence::Unknown => "확인 중",
        }
    }

    /// Group order: online first.
    fn rank(self) -> u8 {
        match self {
            Presence::Online => 0,
            Presence::Sleeping => 1,
            Presence::Offline => 2,
            Presence::Unknown => 3,
        }
    }
}

/// How the bytes would travel (FR-F6, FR-N).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathKind {
    Nearby,
    Direct,
    Relay,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub kind: DeviceKind,
    pub presence: Presence,
    /// `None` while the path is not known.
    pub path: Option<PathKind>,
    pub latency_ms: Option<u32>,
}

/// Online first, then sleeping, offline, unknown; by name (case-insensitive), then id, in a group.
pub fn order_devices(mut devices: Vec<Device>) -> Vec<Device> {
    todo!()
}

/// "근처", "직접 연결" or "릴레이", with " · 12ms" when the latency is known; `None` when the path is.
pub fn path_label(path: Option<PathKind>, latency_ms: Option<u32>) -> Option<String> {
    todo!()
}

/// The user's registered computers as devices (the only devices the client knows today).
pub fn devices_from_computers(computers: &[Computer]) -> Vec<Device> {
    todo!()
}

/// How many devices are online.
pub fn connected_count(devices: &[Device]) -> usize {
    todo!()
}

/// The devices the send sheet offers: online ones only.
pub fn sendable_devices(devices: &[Device]) -> Vec<Device> {
    todo!()
}

/// Split into rows of `cols`, the last row padded with `None` so tiles keep an equal width.
pub fn grid<T: Clone>(items: &[T], cols: usize) -> Vec<Vec<Option<T>>> {
    todo!()
}

// ---------------------------------------------------------------------------------------------
// Transfers (FR-F4, FR-F5)
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Send,
    Receive,
}

impl Direction {
    pub fn label(self) -> &'static str {
        match self {
            Direction::Send => "보냄",
            Direction::Receive => "받음",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferState {
    Queued,
    Sending,
    Paused,
    Done,
    Failed,
    Cancelled,
}

impl TransferState {
    pub fn is_active(self) -> bool {
        matches!(
            self,
            TransferState::Queued | TransferState::Sending | TransferState::Paused
        )
    }
}

/// What can happen to a transfer. `Start`, `Complete` and `Fail` come from the service; the
/// others are the user's buttons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferAction {
    Start,
    Pause,
    Resume,
    Cancel,
    Retry,
    Complete,
    Fail,
}

impl TransferAction {
    pub fn label(self) -> &'static str {
        match self {
            TransferAction::Start => "시작",
            TransferAction::Pause => "일시 정지",
            TransferAction::Resume => "이어서",
            TransferAction::Cancel => "취소",
            TransferAction::Retry => "다시 시도",
            TransferAction::Complete => "완료",
            TransferAction::Fail => "실패",
        }
    }
}

/// The state machine: queued, sending, paused, sending again (resume), then done, failed or
/// cancelled. A failed transfer can be retried (it queues again; FR-F5 resumes from the last
/// verified chunk). `None` for an action the state does not allow.
pub fn next_state(state: TransferState, action: TransferAction) -> Option<TransferState> {
    todo!()
}

/// The buttons a state offers the user, in order.
pub fn user_actions(state: TransferState) -> &'static [TransferAction] {
    todo!()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transfer {
    pub id: String,
    pub name: String,
    /// The other device's name.
    pub device: String,
    pub direction: Direction,
    pub total: u64,
    pub done: u64,
    pub bytes_per_sec: Option<u64>,
    pub state: TransferState,
}

/// Apply an action to a transfer. `Err` leaves it unchanged.
pub fn apply(t: &mut Transfer, action: TransferAction) -> Result<(), IllegalTransition> {
    todo!()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IllegalTransition {
    pub from: TransferState,
    pub action: TransferAction,
}

/// "0 B", "1.5 KB", "10 MB", "4.0 GB" (1024 steps, one decimal under 10).
pub fn format_bytes(n: u64) -> String {
    todo!()
}

/// "12 MB/초", or "측정 중" while there is no rate.
pub fn format_speed(bytes_per_sec: Option<u64>) -> String {
    todo!()
}

/// "8초", "2분 5초", "1시간 3분".
pub fn format_duration(secs: u64) -> String {
    todo!()
}

/// Whole seconds left, rounded up; `None` unless it is sending at a known, non-zero rate.
pub fn eta_secs(t: &Transfer) -> Option<u64> {
    todo!()
}

/// 0 to 100, rounded down so 100 means finished.
pub fn percent(t: &Transfer) -> u32 {
    todo!()
}

/// The line under a transfer's bar.
pub fn progress_line(t: &Transfer) -> String {
    todo!()
}

// ---------------------------------------------------------------------------------------------
// Received files and history (FR-F8)
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceivedFile {
    pub name: String,
    pub size: u64,
    pub from: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryResult {
    Done,
    Failed,
    Declined,
    Cancelled,
}

impl HistoryResult {
    pub fn label(self) -> &'static str {
        match self {
            HistoryResult::Done => "완료",
            HistoryResult::Failed => "실패",
            HistoryResult::Declined => "거절됨",
            HistoryResult::Cancelled => "취소됨",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    pub name: String,
    pub device: String,
    pub direction: Direction,
    pub size: u64,
    pub result: HistoryResult,
    /// A ready-made time text from the service.
    pub when: String,
}

// ---------------------------------------------------------------------------------------------
// Incoming offers (FR-F3)
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offer {
    pub id: String,
    pub from: String,
    pub files: Vec<String>,
    pub total: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    Accept,
    Decline,
}

/// "iPhone에서 사진.jpg 외 2개 (3.2 MB)를 보내려고 합니다".
pub fn offer_line(o: &Offer) -> String {
    todo!()
}

/// The offers still waiting for an answer.
pub fn visible_offers(pending: &[Offer], answered: &[String]) -> Vec<Offer> {
    todo!()
}

/// Answer an offer: tell the service and remember it as answered. `false` (and nothing sent)
/// for an unknown or already answered offer.
pub fn answer_offer(
    svc: &dyn TransferService,
    pending: &[Offer],
    answered: &mut Vec<String>,
    id: &str,
    answer: Answer,
) -> bool {
    todo!()
}

// ---------------------------------------------------------------------------------------------
// The service (FR-L27.1)
// ---------------------------------------------------------------------------------------------

/// What the screen needs from the transfer feature. Implemented by the real transfer client when
/// FR-F lands; until then only [`NoTransferService`].
pub trait TransferService: Send + Sync {
    /// Whether transfers can happen at all.
    fn ready(&self) -> bool;
    /// Devices the transfer layer knows, with presence and path. `None`: it knows none.
    fn devices(&self) -> Option<Vec<Device>>;
    fn transfers(&self) -> Vec<Transfer>;
    fn received(&self) -> Vec<ReceivedFile>;
    fn history(&self) -> Vec<HistoryEntry>;
    /// Offers waiting for an answer.
    fn pending_offers(&self) -> Vec<Offer>;
    fn answer(&self, offer_id: &str, answer: Answer);
    /// A user action on a transfer (pause, resume, cancel, retry).
    fn control(&self, transfer_id: &str, action: TransferAction);
    /// Offer `files` (paths) to a device.
    fn send(&self, device_id: &str, files: Vec<String>) -> Result<(), String>;
}

/// The service until FR-F is built: nothing is ready and nothing is held.
pub struct NoTransferService;

impl TransferService for NoTransferService {
    fn ready(&self) -> bool {
        false
    }
    fn devices(&self) -> Option<Vec<Device>> {
        None
    }
    fn transfers(&self) -> Vec<Transfer> {
        Vec::new()
    }
    fn received(&self) -> Vec<ReceivedFile> {
        Vec::new()
    }
    fn history(&self) -> Vec<HistoryEntry> {
        Vec::new()
    }
    fn pending_offers(&self) -> Vec<Offer> {
        Vec::new()
    }
    fn answer(&self, _offer_id: &str, _answer: Answer) {}
    fn control(&self, _transfer_id: &str, _action: TransferAction) {}
    fn send(&self, _device_id: &str, _files: Vec<String>) -> Result<(), String> {
        Err(UNAVAILABLE.to_string())
    }
}

static NO_SERVICE: NoTransferService = NoTransferService;

/// TODO(FR-F): return the real service once the transfer client exists.
pub fn transfer_service() -> &'static dyn TransferService {
    &NO_SERVICE
}

/// Everything the screen shows, read in one pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilesModel {
    pub devices: Vec<Device>,
    pub transfers: Vec<Transfer>,
    pub received: Vec<ReceivedFile>,
    pub history: Vec<HistoryEntry>,
    pub ready: bool,
}

/// Devices are the service's when it has them, else the registered computers; always ordered.
pub fn build_model(computers: &[Computer], svc: &dyn TransferService) -> FilesModel {
    todo!()
}

/// Which section an empty state belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Devices,
    Agents,
    Transfers,
    Received,
    History,
}

/// The text of an empty section: [`UNAVAILABLE`] where the service is not ready (sending,
/// receiving and history cannot be known), else the section's own empty text.
pub fn empty_text(section: Section, ready: bool) -> &'static str {
    todo!()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilesKpi {
    pub label: &'static str,
    pub value: String,
    pub available: bool,
}

/// Connected devices, transfers in progress, sent this week, direct connection ratio.
/// Only the first is known without the service; the others say [`UNAVAILABLE`].
pub fn kpis(m: &FilesModel) -> [FilesKpi; 4] {
    todo!()
}

// ---------------------------------------------------------------------------------------------
// The send sheet (FR-L27.5)
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendTarget {
    Device(String),
    /// An agent's computer, from its session id (the file lands in the work folder).
    Agent(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SendDraft {
    pub target: Option<SendTarget>,
    pub files: Vec<String>,
}

/// Why the draft cannot be sent yet, in 합니다체; `None` when it can.
pub fn send_blocker(
    d: &SendDraft,
    picker_available: bool,
    service_ready: bool,
) -> Option<&'static str> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn dev(id: &str, name: &str, presence: Presence) -> Device {
        Device {
            id: id.into(),
            name: name.into(),
            kind: DeviceKind::Computer,
            presence,
            path: None,
            latency_ms: None,
        }
    }

    fn tr(state: TransferState, total: u64, done: u64, rate: Option<u64>) -> Transfer {
        Transfer {
            id: "t1".into(),
            name: "a.zip".into(),
            device: "맥미니".into(),
            direction: Direction::Send,
            total,
            done,
            bytes_per_sec: rate,
            state,
        }
    }

    fn computer(id: &str, name: &str, r: Reachability) -> Computer {
        Computer {
            id: id.into(),
            name: name.into(),
            reachability: r,
            projects: vec![],
        }
    }

    fn offer(id: &str) -> Offer {
        Offer {
            id: id.into(),
            from: "iPhone".into(),
            files: vec!["사진.jpg".into(), "b.png".into(), "c.png".into()],
            total: 3 * 1024 * 1024 + 200 * 1024,
        }
    }

    /// Records what the screen tells the service.
    #[derive(Default)]
    struct Spy {
        answers: Mutex<Vec<(String, Answer)>>,
    }

    impl TransferService for Spy {
        fn ready(&self) -> bool {
            true
        }
        fn devices(&self) -> Option<Vec<Device>> {
            None
        }
        fn transfers(&self) -> Vec<Transfer> {
            vec![]
        }
        fn received(&self) -> Vec<ReceivedFile> {
            vec![]
        }
        fn history(&self) -> Vec<HistoryEntry> {
            vec![]
        }
        fn pending_offers(&self) -> Vec<Offer> {
            vec![]
        }
        fn answer(&self, id: &str, a: Answer) {
            self.answers.lock().unwrap().push((id.into(), a));
        }
        fn control(&self, _: &str, _: TransferAction) {}
        fn send(&self, _: &str, _: Vec<String>) -> Result<(), String> {
            Ok(())
        }
    }

    // FR-L27.2
    #[test]
    fn devices_group_online_first_then_by_name() {
        let out = order_devices(vec![
            dev("1", "zeta", Presence::Offline),
            dev("2", "Beta", Presence::Online),
            dev("3", "alpha", Presence::Sleeping),
            dev("4", "alpha", Presence::Online),
            dev("5", "gamma", Presence::Unknown),
        ]);
        let ids: Vec<&str> = out.iter().map(|d| d.id.as_str()).collect();
        assert_eq!(ids, ["4", "2", "3", "1", "5"]);
    }

    #[test]
    fn ordering_is_stable_on_equal_names() {
        let out = order_devices(vec![
            dev("b", "same", Presence::Online),
            dev("a", "same", Presence::Online),
        ]);
        assert_eq!(out[0].id, "a");
    }

    #[test]
    fn path_labels() {
        assert_eq!(path_label(None, Some(5)), None);
        assert_eq!(path_label(Some(PathKind::Nearby), None).unwrap(), "근처");
        assert_eq!(
            path_label(Some(PathKind::Direct), Some(12)).unwrap(),
            "직접 연결 · 12ms"
        );
        assert_eq!(path_label(Some(PathKind::Relay), None).unwrap(), "릴레이");
    }

    // FR-L27.1
    #[test]
    fn computers_become_devices_without_inventing_a_path() {
        let d = devices_from_computers(&[
            computer("c1", "맥미니", Reachability::Online),
            computer("c2", "노트북", Reachability::Offline),
            computer("c3", "새 컴퓨터", Reachability::Unknown),
        ]);
        assert_eq!(d.len(), 3);
        assert_eq!(d[0].presence, Presence::Online);
        assert_eq!(d[1].presence, Presence::Offline);
        assert_eq!(d[2].presence, Presence::Unknown);
        assert!(
            d.iter()
                .all(|x| x.path.is_none() && x.kind == DeviceKind::Computer)
        );
        assert_eq!(connected_count(&d), 1);
    }

    #[test]
    fn only_online_devices_can_receive() {
        let d = vec![
            dev("1", "a", Presence::Online),
            dev("2", "b", Presence::Sleeping),
            dev("3", "c", Presence::Offline),
        ];
        let s = sendable_devices(&d);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].id, "1");
    }

    #[test]
    fn grid_pads_the_last_row() {
        let g = grid(&[1, 2, 3, 4, 5], 3);
        assert_eq!(
            g,
            vec![
                vec![Some(1), Some(2), Some(3)],
                vec![Some(4), Some(5), None]
            ]
        );
        assert!(grid::<u8>(&[], 3).is_empty());
        assert_eq!(grid(&[1, 2], 1).len(), 2);
    }

    // FR-L27.3
    #[test]
    fn bytes_format() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1023), "1023 B");
        assert_eq!(format_bytes(1536), "1.5 KB");
        assert_eq!(format_bytes(10 * 1024 * 1024), "10 MB");
        assert_eq!(format_bytes(4 * 1024 * 1024 * 1024), "4.0 GB");
    }

    #[test]
    fn speed_format() {
        assert_eq!(format_speed(Some(12 * 1024 * 1024)), "12 MB/초");
        assert_eq!(format_speed(None), "측정 중");
    }

    #[test]
    fn duration_format() {
        assert_eq!(format_duration(8), "8초");
        assert_eq!(format_duration(60), "1분");
        assert_eq!(format_duration(125), "2분 5초");
        assert_eq!(format_duration(3780), "1시간 3분");
    }

    #[test]
    fn eta_and_percent() {
        let t = tr(TransferState::Sending, 100, 50, Some(10));
        assert_eq!(eta_secs(&t), Some(5));
        let t = tr(TransferState::Sending, 101, 50, Some(10));
        assert_eq!(eta_secs(&t), Some(6));
        assert_eq!(eta_secs(&tr(TransferState::Sending, 100, 50, None)), None);
        assert_eq!(
            eta_secs(&tr(TransferState::Sending, 100, 50, Some(0))),
            None
        );
        assert_eq!(
            eta_secs(&tr(TransferState::Paused, 100, 50, Some(10))),
            None
        );
        assert_eq!(percent(&tr(TransferState::Sending, 200, 99, None)), 49);
        assert_eq!(percent(&tr(TransferState::Sending, 200, 199, None)), 99);
        assert_eq!(percent(&tr(TransferState::Done, 200, 200, None)), 100);
        assert_eq!(percent(&tr(TransferState::Queued, 0, 0, None)), 0);
    }

    #[test]
    fn progress_lines() {
        let t = tr(
            TransferState::Sending,
            100 * 1024 * 1024,
            50 * 1024 * 1024,
            Some(10 * 1024 * 1024),
        );
        assert_eq!(progress_line(&t), "50% · 10 MB/초 · 5초 남음");
        assert_eq!(
            progress_line(&tr(TransferState::Paused, 100, 45, Some(1))),
            "45% · 일시 정지됨"
        );
        assert_eq!(
            progress_line(&tr(TransferState::Queued, 100, 0, None)),
            "대기 중입니다"
        );
        assert_eq!(
            progress_line(&tr(TransferState::Done, 100, 100, None)),
            "완료되었습니다"
        );
        assert_eq!(
            progress_line(&tr(TransferState::Failed, 100, 30, None)),
            "30% · 연결이 끊겼습니다"
        );
        assert_eq!(
            progress_line(&tr(TransferState::Cancelled, 100, 30, None)),
            "취소되었습니다"
        );
    }

    #[test]
    fn transfer_walks_queued_sending_paused_resumed_done() {
        use TransferAction::*;
        use TransferState::*;
        let mut t = tr(Queued, 100, 0, None);
        for (a, want) in [
            (Start, Sending),
            (Pause, Paused),
            (Resume, Sending),
            (Complete, Done),
        ] {
            apply(&mut t, a).unwrap();
            assert_eq!(t.state, want);
        }
    }

    #[test]
    fn transfer_can_fail_and_retry() {
        use TransferAction::*;
        use TransferState::*;
        let mut t = tr(Sending, 100, 40, Some(1));
        apply(&mut t, Fail).unwrap();
        assert_eq!(t.state, Failed);
        apply(&mut t, Retry).unwrap();
        assert_eq!(t.state, Queued);
        assert_eq!(t.done, 40, "a retry keeps the verified bytes (FR-F5)");
    }

    #[test]
    fn illegal_transitions_leave_the_transfer_alone() {
        use TransferAction::*;
        use TransferState::*;
        let mut t = tr(Done, 100, 100, None);
        let e = apply(&mut t, Pause).unwrap_err();
        assert_eq!(
            e,
            IllegalTransition {
                from: Done,
                action: Pause
            }
        );
        assert_eq!(t.state, Done);
        assert_eq!(next_state(Queued, Pause), None);
        assert_eq!(next_state(Sending, Resume), None);
        assert_eq!(next_state(Paused, Pause), None);
        assert_eq!(next_state(Cancelled, Retry), None);
        assert_eq!(next_state(Paused, Cancel), Some(Cancelled));
    }

    #[test]
    fn buttons_follow_the_state() {
        use TransferAction::*;
        use TransferState::*;
        assert_eq!(user_actions(Queued), [Cancel]);
        assert_eq!(user_actions(Sending), [Pause, Cancel]);
        assert_eq!(user_actions(Paused), [Resume, Cancel]);
        assert_eq!(user_actions(Failed), [Retry]);
        assert!(user_actions(Done).is_empty());
        assert!(user_actions(Cancelled).is_empty());
        for s in [Queued, Sending, Paused, Failed, Done, Cancelled] {
            for a in user_actions(s) {
                assert!(
                    next_state(s, *a).is_some(),
                    "{s:?} offers {a:?} but rejects it"
                );
            }
        }
    }

    // FR-L27.4
    #[test]
    fn accept_and_decline_reach_the_service_once() {
        let spy = Spy::default();
        let pending = vec![offer("o1"), offer("o2")];
        let mut answered = Vec::new();
        assert!(answer_offer(
            &spy,
            &pending,
            &mut answered,
            "o1",
            Answer::Accept
        ));
        assert!(answer_offer(
            &spy,
            &pending,
            &mut answered,
            "o2",
            Answer::Decline
        ));
        assert!(!answer_offer(
            &spy,
            &pending,
            &mut answered,
            "o1",
            Answer::Decline
        ));
        assert!(!answer_offer(
            &spy,
            &pending,
            &mut answered,
            "nope",
            Answer::Accept
        ));
        assert_eq!(
            *spy.answers.lock().unwrap(),
            vec![
                ("o1".to_string(), Answer::Accept),
                ("o2".to_string(), Answer::Decline)
            ]
        );
        assert!(visible_offers(&pending, &answered).is_empty());
    }

    #[test]
    fn unanswered_offers_stay_visible() {
        let pending = vec![offer("o1"), offer("o2")];
        let v = visible_offers(&pending, &["o1".to_string()]);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].id, "o2");
    }

    #[test]
    fn offer_line_names_sender_files_and_size() {
        assert_eq!(
            offer_line(&offer("o")),
            "iPhone에서 사진.jpg 외 2개 (3.2 MB)를 보내려고 합니다"
        );
        let one = Offer {
            id: "x".into(),
            from: "맥미니".into(),
            files: vec!["a.txt".into()],
            total: 10,
        };
        assert_eq!(
            offer_line(&one),
            "맥미니에서 a.txt (10 B)를 보내려고 합니다"
        );
    }

    // FR-L27.1, FR-L27.6
    #[test]
    fn without_a_service_the_model_has_only_real_computers() {
        let m = build_model(
            &[computer("c1", "맥미니", Reachability::Online)],
            &NoTransferService,
        );
        assert_eq!(m.devices.len(), 1);
        assert!(m.transfers.is_empty() && m.received.is_empty() && m.history.is_empty());
        assert!(!m.ready);
        assert!(NoTransferService.send("c1", vec![]).is_err());
    }

    #[test]
    fn unknown_numbers_say_unavailable() {
        let m = build_model(
            &[
                computer("c1", "a", Reachability::Online),
                computer("c2", "b", Reachability::Offline),
            ],
            &NoTransferService,
        );
        let k = kpis(&m);
        assert_eq!(k[0].label, "연결된 장비");
        assert_eq!(k[0].value, "1/2");
        assert!(k[0].available);
        for other in &k[1..] {
            assert_eq!(other.value, UNAVAILABLE);
            assert!(!other.available);
        }
    }

    #[test]
    fn empty_states() {
        assert_eq!(empty_text(Section::Devices, false), EMPTY_DEVICES);
        assert_eq!(empty_text(Section::Agents, false), EMPTY_AGENTS);
        for s in [Section::Transfers, Section::Received, Section::History] {
            assert_eq!(empty_text(s, false), UNAVAILABLE);
        }
        assert_eq!(empty_text(Section::Transfers, true), EMPTY_TRANSFERS);
        assert_eq!(empty_text(Section::Received, true), EMPTY_RECEIVED);
        assert_eq!(empty_text(Section::History, true), EMPTY_HISTORY);
    }

    // FR-L27.5
    #[test]
    fn send_sheet_blocks_until_it_can_send() {
        let mut d = SendDraft::default();
        assert_eq!(send_blocker(&d, true, false), Some(UNAVAILABLE));
        assert_eq!(send_blocker(&d, true, true), Some(PICK_RECEIVER));
        d.target = Some(SendTarget::Device("c1".into()));
        assert_eq!(send_blocker(&d, false, true), Some(FILE_PICK_UNAVAILABLE));
        assert_eq!(send_blocker(&d, true, true), Some(PICK_FILES));
        d.files.push("/a.txt".into());
        assert_eq!(send_blocker(&d, true, true), None);
    }

    #[test]
    fn the_toolkit_has_no_file_picker_yet() {
        assert!(!FILE_PICKER_AVAILABLE);
    }

    // NFR-L4 / FR-L27.6
    #[test]
    fn korean_ui_text_is_hamnida_style() {
        use TransferAction::*;
        use TransferState::*;
        let mut s: Vec<String> = vec![
            UNAVAILABLE,
            EMPTY_DEVICES,
            EMPTY_AGENTS,
            EMPTY_TRANSFERS,
            EMPTY_RECEIVED,
            EMPTY_HISTORY,
            FILE_PICK_UNAVAILABLE,
            PICK_RECEIVER,
            PICK_FILES,
            LOCK_LINE,
        ]
        .into_iter()
        .map(String::from)
        .collect();
        for st in [Queued, Sending, Paused, Done, Failed, Cancelled] {
            s.push(progress_line(&tr(st, 100, 10, Some(1))));
        }
        s.push(offer_line(&offer("o")));
        for a in [Pause, Resume, Cancel, Retry] {
            s.push(a.label().into());
        }
        for sec in [Section::Devices, Section::Agents, Section::Transfers] {
            s.push(empty_text(sec, true).into());
        }
        for text in s {
            assert!(
                !text.ends_with('다') || text.ends_with("니다"),
                "plain ending: {text}"
            );
            assert!(!text.contains('\u{2014}'), "em-dash: {text}");
        }
    }
}
