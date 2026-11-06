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

use ember_client::state::{Computer, Reachability};


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
    devices.sort_by(|a, b| {
        a.presence
            .rank()
            .cmp(&b.presence.rank())
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.id.cmp(&b.id))
    });
    devices
}

/// "근처", "직접 연결" or "릴레이", with " · 12ms" when the latency is known; `None` when the path is.
pub fn path_label(path: Option<PathKind>, latency_ms: Option<u32>) -> Option<String> {
    let base = match path? {
        PathKind::Nearby => "근처",
        PathKind::Direct => "직접 연결",
        PathKind::Relay => "릴레이",
    };
    Some(match latency_ms {
        Some(ms) => format!("{base} · {ms}ms"),
        None => base.to_string(),
    })
}

/// The user's registered computers as devices (the only devices the client knows today).
pub fn devices_from_computers(computers: &[Computer]) -> Vec<Device> {
    computers
        .iter()
        .map(|c| Device {
            id: c.id.clone(),
            name: c.name.clone(),
            kind: DeviceKind::Computer,
            presence: match c.reachability {
                Reachability::Online => Presence::Online,
                Reachability::Offline => Presence::Offline,
                Reachability::Unknown => Presence::Unknown,
            },
            path: None,
            latency_ms: None,
        })
        .collect()
}

/// How many devices are online.
pub fn connected_count(devices: &[Device]) -> usize {
    devices
        .iter()
        .filter(|d| d.presence == Presence::Online)
        .count()
}

/// The devices the send sheet offers: online ones only.
pub fn sendable_devices(devices: &[Device]) -> Vec<Device> {
    devices
        .iter()
        .filter(|d| d.presence == Presence::Online)
        .cloned()
        .collect()
}

/// Split into rows of `cols`, the last row padded with `None` so tiles keep an equal width.
pub fn grid<T: Clone>(items: &[T], cols: usize) -> Vec<Vec<Option<T>>> {
    let cols = cols.max(1);
    items
        .chunks(cols)
        .map(|c| (0..cols).map(|i| c.get(i).cloned()).collect())
        .collect()
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
    use TransferAction as A;
    use TransferState as S;
    match (state, action) {
        (S::Queued, A::Start) => Some(S::Sending),
        (S::Queued, A::Cancel) => Some(S::Cancelled),
        (S::Queued, A::Fail) => Some(S::Failed),
        (S::Sending, A::Pause) => Some(S::Paused),
        (S::Sending, A::Complete) => Some(S::Done),
        (S::Sending, A::Fail) => Some(S::Failed),
        (S::Sending, A::Cancel) => Some(S::Cancelled),
        (S::Paused, A::Resume) => Some(S::Sending),
        (S::Paused, A::Cancel) => Some(S::Cancelled),
        (S::Paused, A::Fail) => Some(S::Failed),
        (S::Failed, A::Retry) => Some(S::Queued),
        _ => None,
    }
}

/// The buttons a state offers the user, in order.
pub fn user_actions(state: TransferState) -> &'static [TransferAction] {
    use TransferAction as A;
    match state {
        TransferState::Queued => &[A::Cancel],
        TransferState::Sending => &[A::Pause, A::Cancel],
        TransferState::Paused => &[A::Resume, A::Cancel],
        TransferState::Failed => &[A::Retry],
        TransferState::Done | TransferState::Cancelled => &[],
    }
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
    match next_state(t.state, action) {
        Some(next) => {
            t.state = next;
            if action == TransferAction::Complete {
                t.done = t.total;
            }
            Ok(())
        }
        None => Err(IllegalTransition {
            from: t.state,
            action,
        }),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IllegalTransition {
    pub from: TransferState,
    pub action: TransferAction,
}

/// "0 B", "1.5 KB", "10 MB", "4.0 GB" (1024 steps, one decimal under 10).
pub fn format_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if v < 10.0 {
        format!("{:.1} {}", v, UNITS[i])
    } else {
        format!("{:.0} {}", v, UNITS[i])
    }
}

/// "12 MB/초", or "측정 중" while there is no rate.
pub fn format_speed(bytes_per_sec: Option<u64>) -> String {
    match bytes_per_sec {
        Some(b) => format!("{}/초", format_bytes(b)),
        None => "측정 중".to_string(),
    }
}

/// "8초", "2분 5초", "1시간 3분".
pub fn format_duration(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}초")
    } else if secs < 3600 {
        let (m, r) = (secs / 60, secs % 60);
        if r == 0 {
            format!("{m}분")
        } else {
            format!("{m}분 {r}초")
        }
    } else {
        let (h, m) = (secs / 3600, (secs % 3600) / 60);
        if m == 0 {
            format!("{h}시간")
        } else {
            format!("{h}시간 {m}분")
        }
    }
}

/// Whole seconds left, rounded up; `None` unless it is sending at a known, non-zero rate.
pub fn eta_secs(t: &Transfer) -> Option<u64> {
    if t.state != TransferState::Sending {
        return None;
    }
    let rate = t.bytes_per_sec.filter(|r| *r > 0)?;
    Some(t.total.saturating_sub(t.done).div_ceil(rate))
}

/// 0 to 100, rounded down so 100 means finished.
pub fn percent(t: &Transfer) -> u32 {
    if t.state == TransferState::Done {
        return 100;
    }
    if t.total == 0 {
        return 0;
    }
    ((t.done as u128 * 100 / t.total as u128).min(100)) as u32
}

/// The line under a transfer's bar.
pub fn progress_line(t: &Transfer) -> String {
    let p = percent(t);
    match t.state {
        TransferState::Queued => "대기 중입니다".to_string(),
        TransferState::Sending => {
            let mut s = format!("{p}% · {}", format_speed(t.bytes_per_sec));
            if let Some(eta) = eta_secs(t) {
                s.push_str(&format!(" · {} 남음", format_duration(eta)));
            }
            s
        }
        TransferState::Paused => format!("{p}% · 일시 정지됨"),
        TransferState::Done => "완료되었습니다".to_string(),
        TransferState::Failed => format!("{p}% · 연결이 끊겼습니다"),
        TransferState::Cancelled => "취소되었습니다".to_string(),
    }
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
    let names = match o.files.as_slice() {
        [] => "파일".to_string(),
        [one] => one.clone(),
        [first, rest @ ..] => format!("{first} 외 {}개", rest.len()),
    };
    format!(
        "{}에서 {names} ({})를 보내려고 합니다",
        o.from,
        format_bytes(o.total)
    )
}

/// The offers still waiting for an answer.
pub fn visible_offers(pending: &[Offer], answered: &[String]) -> Vec<Offer> {
    pending
        .iter()
        .filter(|o| !answered.contains(&o.id))
        .cloned()
        .collect()
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
    if answered.iter().any(|a| a == id) || !pending.iter().any(|o| o.id == id) {
        return false;
    }
    svc.answer(id, answer);
    answered.push(id.to_string());
    true
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
    let devices = svc
        .devices()
        .unwrap_or_else(|| devices_from_computers(computers));
    FilesModel {
        devices: order_devices(devices),
        transfers: svc.transfers(),
        received: svc.received(),
        history: svc.history(),
        ready: svc.ready(),
    }
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
    match section {
        Section::Devices => EMPTY_DEVICES,
        Section::Agents => EMPTY_AGENTS,
        _ if !ready => UNAVAILABLE,
        Section::Transfers => EMPTY_TRANSFERS,
        Section::Received => EMPTY_RECEIVED,
        Section::History => EMPTY_HISTORY,
    }
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
    let unavailable = |label| FilesKpi {
        label,
        value: UNAVAILABLE.to_string(),
        available: false,
    };
    let in_progress = if m.ready {
        FilesKpi {
            label: "전송 중",
            value: m
                .transfers
                .iter()
                .filter(|t| t.state.is_active())
                .count()
                .to_string(),
            available: true,
        }
    } else {
        unavailable("전송 중")
    };
    let known: Vec<&Device> = m.devices.iter().filter(|d| d.path.is_some()).collect();
    let direct = if m.ready && !known.is_empty() {
        let near = known
            .iter()
            .filter(|d| matches!(d.path, Some(PathKind::Nearby | PathKind::Direct)))
            .count();
        FilesKpi {
            label: "직접 연결 비율",
            value: format!("{}%", near * 100 / known.len()),
            available: true,
        }
    } else {
        unavailable("직접 연결 비율")
    };
    [
        FilesKpi {
            label: "연결된 장비",
            value: format!("{}/{}", connected_count(&m.devices), m.devices.len()),
            available: true,
        },
        in_progress,
        // TODO(FR-F8): sum the sent bytes of the last 7 days once the history has timestamps.
        unavailable("이번 주 보낸 용량"),
        direct,
    ]
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
    if !service_ready {
        return Some(UNAVAILABLE);
    }
    if d.target.is_none() {
        return Some(PICK_RECEIVER);
    }
    if d.files.is_empty() {
        return Some(if picker_available {
            PICK_FILES
        } else {
            FILE_PICK_UNAVAILABLE
        });
    }
    None
}

// ---------------------------------------------------------------------------------------------
// The share sheet (FR-L27.7)
// ---------------------------------------------------------------------------------------------

/// The share sheet's title text for the shared file when nothing can be shared yet.
pub const SHARE_NO_FILE: &str = "공유할 파일이 없습니다";

/// The explanation under the share sheet (the mockup's note).
pub const SHARE_NOTE: &str =
    "파일 전송 서비스(#487)가 준비되면 휴대폰에서 공유 버튼을 눌러 이 시트로 보낼 수 있습니다. 받는 장비에서 수락하면 전송이 시작됩니다.";

/// Under "에이전트에게": agents work in the folder of one of the user's own computers (D24).
pub const SHARE_AGENTS_NOTE: &str = "에이전트가 일하는 내 컴퓨터의 작업 폴더로 보냅니다";

/// A tile name wraps to this many lines before it is cut with an ellipsis.
pub const SHARE_NAME_LINES: u32 = 2;

/// The sheet's own actions (the mockup's list); none of them exists without the transfer
/// service, so each says [`UNAVAILABLE`].
pub const SHARE_ACTIONS: [&str; 3] = ["파일에 저장", "채널에 올리기", "프로젝트에 넣기"];

/// What the sheet says when the chosen device is not connected.
pub const SHARE_OFFLINE: &str = "이 장비는 지금 연결되어 있지 않습니다";

/// The mockup shows at most three agents under "에이전트에게".
pub const SHARE_AGENTS_MAX: usize = 3;

/// A device in the sheet's "Ember로 보내기" row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShareDevice {
    pub id: String,
    pub name: String,
    pub kind: DeviceKind,
    pub online: bool,
    /// The small line under the name: the path when known, else the kind; "꺼짐" when not online.
    pub caption: String,
}

/// Every device for the row, online ones first (then by name), so the reachable ones are
/// at the left. An offline device stays in the row, dimmed, as in the mockup.
pub fn share_devices(devices: &[Device]) -> Vec<ShareDevice> {
    let mut rows: Vec<&Device> = devices.iter().collect();
    rows.sort_by(|a, b| {
        (a.presence != Presence::Online)
            .cmp(&(b.presence != Presence::Online))
            .then_with(|| a.presence.rank().cmp(&b.presence.rank()))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.id.cmp(&b.id))
    });
    rows.into_iter()
        .map(|d| {
            let online = d.presence == Presence::Online;
            let caption = if online {
                path_label(d.path, None).unwrap_or_else(|| d.kind.label().to_string())
            } else {
                "꺼짐".to_string()
            };
            ShareDevice {
                id: d.id.clone(),
                name: d.name.clone(),
                kind: d.kind,
                online,
                caption,
            }
        })
        .collect()
}

/// The agents shown under "에이전트에게": the first [`SHARE_AGENTS_MAX`], in the order given.
pub fn share_agents<T: Clone>(agents: &[T]) -> Vec<T> {
    agents.iter().take(SHARE_AGENTS_MAX).cloned().collect()
}

/// What a tap on a receiver does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShareTap {
    /// Nothing is sent; the sheet says why.
    Blocked(&'static str),
    /// Hand the files to the service for this receiver.
    Send(SendTarget),
}

/// A tap on a device or an agent. An offline device and a missing service or file are
/// blocked with their reason; nothing reports success without the service.
pub fn share_tap(
    target: &SendTarget,
    devices: &[Device],
    files: &[String],
    picker_available: bool,
    service_ready: bool,
) -> ShareTap {
    if let SendTarget::Device(id) = target {
        let online = devices
            .iter()
            .find(|d| d.id == *id)
            .is_some_and(|d| d.presence == Presence::Online);
        if !online {
            return ShareTap::Blocked(SHARE_OFFLINE);
        }
    }
    let draft = SendDraft {
        target: Some(target.clone()),
        files: files.to_vec(),
    };
    match send_blocker(&draft, picker_available, service_ready) {
        Some(why) => ShareTap::Blocked(why),
        None => ShareTap::Send(target.clone()),
    }
}

/// What the file card of the sheet shows: the title, and the reason when it has no file.
pub fn share_file_card(files: &[String], picker_available: bool) -> (String, Option<&'static str>) {
    match files {
        [] => (
            SHARE_NO_FILE.to_string(),
            Some(if picker_available {
                PICK_FILES
            } else {
                FILE_PICK_UNAVAILABLE
            }),
        ),
        [one] => (one.clone(), None),
        [first, rest @ ..] => (format!("{first} 외 {}개", rest.len()), None),
    }
}

/// Shown when the chosen device is not online.
pub const TARGET_OFFLINE: &str = "이 장비는 지금 연결되어 있지 않습니다";

/// The files line of the send sheet: says what to do when nothing is chosen.
pub fn files_line(count: usize, picker_available: bool) -> String {
    match (count, picker_available) {
        (0, true) => "선택한 파일이 없습니다. 파일 고르기를 눌러 추가합니다".to_string(),
        (0, false) => "선택한 파일이 없습니다. 파일을 고르는 기능은 준비 중입니다".to_string(),
        (n, _) => format!("파일 {n}개 선택됨"),
    }
}

/// A target can be chosen when it is an agent or an online device (an unknown id cannot).
pub fn target_selectable(devices: &[Device], target: &SendTarget) -> bool {
    match target {
        SendTarget::Agent(_) => true,
        SendTarget::Device(id) => devices
            .iter()
            .any(|x| x.id == *id && x.presence == Presence::Online),
    }
}

/// The draft after choosing `target`, or the notice to show when it cannot be chosen.
pub fn choose_target(
    d: &SendDraft,
    devices: &[Device],
    target: SendTarget,
) -> Result<SendDraft, String> {
    if target_selectable(devices, &target) {
        let mut next = d.clone();
        next.target = Some(target);
        Ok(next)
    } else {
        let name = match &target {
            SendTarget::Device(id) => devices
                .iter()
                .find(|x| x.id == *id)
                .map(|x| x.name.clone())
                .unwrap_or_else(|| id.clone()),
            SendTarget::Agent(id) => id.clone(),
        };
        Err(format!("{name}은(는) 지금 연결되어 있지 않습니다"))
    }
}

/// The send state from the real draft: the reason it cannot send (`None` when it can) and
/// whether the button is enabled. A chosen device that went offline blocks, and so does an agent
/// target, because the service sends to a device id only.
pub fn send_state(
    d: &SendDraft,
    devices: &[Device],
    picker_available: bool,
    service_ready: bool,
) -> (Option<&'static str>, bool) {
    let mut blocker = send_blocker(d, picker_available, service_ready);
    if blocker.is_none() {
        if let Some(t) = &d.target {
            if !target_selectable(devices, t) {
                blocker = Some(TARGET_OFFLINE);
            }
        }
    }
    let can = blocker.is_none() && matches!(d.target, Some(SendTarget::Device(_)));
    (blocker, can)
}

/// The widest a chip's text may be so the whole chip stays within the mockup's 200 px
/// (`.fl-tg{max-width:200px}`): 12 padding, the glyph, 8, the text, 12 padding.
pub fn chip_text_max(glyph: f32) -> f32 {
    200.0 - 12.0 - glyph - 8.0 - 12.0
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

    // FR-L27.5
    #[test]
    fn fr_l27_5_an_offline_device_cannot_be_chosen() {
        let devs = vec![
            dev("on", "a", Presence::Online),
            dev("off", "스튜디오 PC", Presence::Offline),
            dev("zz", "b", Presence::Sleeping),
        ];
        let d = SendDraft::default();
        let ok = choose_target(&d, &devs, SendTarget::Device("on".into())).unwrap();
        assert_eq!(ok.target, Some(SendTarget::Device("on".into())));
        let err = choose_target(&d, &devs, SendTarget::Device("off".into())).unwrap_err();
        assert_eq!(err, "스튜디오 PC은(는) 지금 연결되어 있지 않습니다");
        assert!(choose_target(&d, &devs, SendTarget::Device("zz".into())).is_err());
        assert!(choose_target(&d, &devs, SendTarget::Device("missing".into())).is_err());
        assert!(choose_target(&d, &devs, SendTarget::Agent("s1".into())).is_ok());
    }

    // FR-L27.5
    #[test]
    fn fr_l27_5_the_send_button_follows_the_real_selection() {
        let devs = vec![
            dev("on", "a", Presence::Online),
            dev("off", "b", Presence::Offline),
        ];
        let mut d = SendDraft::default();
        assert_eq!(send_state(&d, &devs, true, true), (Some(PICK_RECEIVER), false));
        d.target = Some(SendTarget::Device("on".into()));
        assert_eq!(send_state(&d, &devs, true, true), (Some(PICK_FILES), false));
        d.files.push("/a.txt".into());
        assert_eq!(send_state(&d, &devs, true, true), (None, true));
        assert_eq!(send_state(&d, &devs, true, false), (Some(UNAVAILABLE), false));
        // The device went offline after it was chosen.
        d.target = Some(SendTarget::Device("off".into()));
        assert_eq!(send_state(&d, &devs, true, true), (Some(TARGET_OFFLINE), false));
        // An agent target has files but the service sends to a device id only.
        d.target = Some(SendTarget::Agent("s1".into()));
        assert_eq!(send_state(&d, &devs, true, true), (None, false));
        // Without a picker the draft never has files, so it never sends.
        let d = SendDraft { target: Some(SendTarget::Device("on".into())), files: vec![] };
        assert_eq!(send_state(&d, &devs, false, true), (Some(FILE_PICK_UNAVAILABLE), false));
    }

    // FR-L27.5
    #[test]
    fn fr_l27_5_zero_files_says_what_to_do() {
        assert_eq!(files_line(0, true), "선택한 파일이 없습니다. 파일 고르기를 눌러 추가합니다");
        assert!(files_line(0, false).contains("준비 중"));
        assert_eq!(files_line(3, true), "파일 3개 선택됨");
        for t in [files_line(0, true), files_line(0, false), files_line(2, false), TARGET_OFFLINE.to_string()] {
            assert!(t.ends_with("니다") || t.ends_with("됨"), "{t}");
            assert!(!t.contains('\u{2014}'));
        }
    }

    // FR-L27.5
    #[test]
    fn fr_l27_5_a_chip_never_exceeds_the_mockup_width() {
        assert_eq!(chip_text_max(16.0), 152.0);
        assert_eq!(chip_text_max(22.0), 146.0);
        assert_eq!(12.0 + 16.0 + 8.0 + chip_text_max(16.0) + 12.0, 200.0);
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

    #[test]
    fn fr_l27_7_share_devices_list_online_first_and_dim_the_rest() {
        let mut a = dev("a", "Zeta", Presence::Online);
        a.path = Some(PathKind::Direct);
        let b = dev("b", "alpha", Presence::Offline);
        let c = dev("c", "Beta", Presence::Online);
        let d = dev("d", "mid", Presence::Sleeping);
        let rows = share_devices(&[b, a, d, c]);
        let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["c", "a", "d", "b"]);
        assert_eq!(rows[0].caption, "컴퓨터");
        assert_eq!(rows[1].caption, "직접 연결");
        assert!(!rows[2].online && rows[2].caption == "꺼짐");
        assert!(share_devices(&[]).is_empty());
    }

    #[test]
    fn fr_l27_7_share_agents_are_capped_at_three_in_order() {
        let v: Vec<u32> = (1..=6).collect();
        assert_eq!(share_agents(&v), vec![1, 2, 3]);
        assert_eq!(share_agents(&v[..2]), vec![1, 2]);
    }

    #[test]
    fn fr_l27_7_a_tap_never_sends_without_the_service_or_a_file() {
        let on = dev("on", "맥", Presence::Online);
        let off = dev("off", "PC", Presence::Offline);
        let devs = [on, off];
        let t_on = SendTarget::Device("on".into());
        let t_off = SendTarget::Device("off".into());
        let file = vec!["a.png".to_string()];
        assert_eq!(share_tap(&t_off, &devs, &file, true, true), ShareTap::Blocked(SHARE_OFFLINE));
        assert_eq!(share_tap(&SendTarget::Device("none".into()), &devs, &file, true, true), ShareTap::Blocked(SHARE_OFFLINE));
        assert_eq!(share_tap(&t_on, &devs, &file, true, false), ShareTap::Blocked(UNAVAILABLE));
        assert_eq!(share_tap(&t_on, &devs, &[], false, true), ShareTap::Blocked(FILE_PICK_UNAVAILABLE));
        assert_eq!(share_tap(&t_on, &devs, &file, true, true), ShareTap::Send(t_on.clone()));
        // An agent is always online, the other blockers still apply.
        let ag = SendTarget::Agent("s1".into());
        assert_eq!(share_tap(&ag, &devs, &[], false, false), ShareTap::Blocked(UNAVAILABLE));
        assert_eq!(share_tap(&ag, &devs, &file, true, true), ShareTap::Send(ag.clone()));
    }

    #[test]
    fn fr_l27_7_the_file_card_says_why_it_is_empty() {
        let (t, why) = share_file_card(&[], false);
        assert_eq!(t, SHARE_NO_FILE);
        assert_eq!(why, Some(FILE_PICK_UNAVAILABLE));
        assert_eq!(share_file_card(&["a.png".into()], true), ("a.png".to_string(), None));
        assert_eq!(share_file_card(&["a.png".into(), "b".into(), "c".into()], true).0, "a.png 외 2개");
    }

    #[test]
    fn fr_l27_7_the_note_does_not_state_unbuilt_behaviour_as_fact() {
        assert!(SHARE_NOTE.contains("#487"), "names the service it depends on");
        assert!(SHARE_NOTE.contains("준비되면"), "says it is not ready");
        assert_eq!(SHARE_NAME_LINES, 2);
        assert!(SHARE_AGENTS_NOTE.contains("내 컴퓨터"));
    }

    #[test]
    fn fr_l27_7_the_share_texts_are_polite_and_have_no_em_dash() {
        let mut all: Vec<&str> = vec![SHARE_NO_FILE, SHARE_NOTE, SHARE_OFFLINE, SHARE_AGENTS_NOTE];
        all.extend(SHARE_ACTIONS);
        for t in all {
            assert!(!t.ends_with('다') || t.ends_with("니다"), "plain ending: {t}");
            assert!(!t.contains('\u{2014}'), "em-dash: {t}");
        }
    }
}
