//! The pure model of adding a computer with a one-time code (FR-C1, FR-C2, FR-C2c, FR-C2g, D39):
//! which view the sheet shows for a status the server answered, how fast it polls, what the
//! owner sees at the confirm (the node's reported name and the observed address as plain
//! text, with warnings that inform and never block), and the fixed Korean sentences. No
//! widgets here. The code is a secret: `Debug` of the views never prints it.
//!
//! The command to run on the other machine is NOT built here. The server gives it, and the
//! sheet shows it exactly (the server knows the door address and pins its own peer id). A
//! closed door or an unknown door URL mints no code (409), which the sheet explains.

use std::fmt;
use std::net::IpAddr;

use ember_client::api::{EnrolCallError, EnrolMinted, EnrolNodeInfo, EnrolState, EnrolStatus};

use crate::ui::add_computer::{sanitize_device_text, MAX_ADDRESS_CHARS, MAX_NAME_CHARS};
use crate::ui::channel_owner::confirm_tap;

/// The status poll cadence: the node polls at the same pace and the server limits polls.
pub const POLL_EVERY_MS: u64 = 2_000;

pub const NEW_COMPUTER: &str = "새 컴퓨터 추가";
pub const NAME_LABEL: &str = "컴퓨터 이름";
pub const NAME_HELP: &str = "프로젝트 화면에 이 이름으로 표시됩니다. 확인할 때 바꿀 수 있습니다";
pub const NAME_EMPTY: &str = "이름을 입력하세요";
pub const NAME_TOO_LONG: &str = "이름은 64자 이하여야 합니다";
pub const MAKE_CODE: &str = "코드 만들기";
pub const WAITING_LINE: &str = "다른 컴퓨터에서 아래 명령을 실행하면 이 화면에 요청이 표시됩니다";
pub const CODE_ONCE: &str = "코드는 한 번만 표시되며 한 번 쓰면 무효가 됩니다";
pub const DOOR_OFF: &str = "노드 가입 문이 꺼져 있어 코드를 만들 수 없습니다. 서버 설정(EMBER_ENROL_LISTEN)을 확인하세요";
pub const DOOR_URL_UNKNOWN: &str = "노드가 접속할 서버 주소를 알 수 없어 코드를 만들 수 없습니다. 서버 설정(EMBER_ENROL_PUBLIC_URL)을 확인하세요";
pub const NO_COMMAND: &str = "서버가 실행할 명령을 알려주지 않았습니다";
pub const HTTP_WARNING: &str = "이 입구는 암호화되지 않은 HTTP입니다. 같은 네트워크의 누구나 코드를 볼 수 있으므로 믿을 수 있는 네트워크에서만 쓰세요";
/// Under the command: the code can be given on standard input. Explained in words only; the
/// app builds no command (the server writes the command with its full peer id pin).
pub const STDIN_NOTE: &str = "코드 자리에 -를 쓰면 코드를 표준 입력으로 줄 수 있어 프로세스 목록에 남지 않습니다";
pub const SERVER_FINGERPRINT_LABEL: &str = "서버 지문 (노드 화면과 대조)";
pub const CONFIRM_QUESTION: &str = "이 기기를 컴퓨터로 추가합니다. 추가한 컴퓨터는 서버 사용자와 같은 권한으로 쓰입니다";
pub const CONFIRM_CHECK: &str = "아래 내용이 방금 명령을 실행한 컴퓨터가 맞는지 확인합니다";
pub const REPORTED_NAME: &str = "기기가 보고한 이름";
pub const OBSERVED_ADDR: &str = "관측 주소";
pub const REPORTED_ADDR: &str = "기기가 알린 주소";
pub const PROPOSED_ADDR: &str = "제안 주소";
pub const NODE_FINGERPRINT: &str = "기기 지문 (노드 화면과 대조)";
pub const CONFIRMED_WAIT: &str = "확인했습니다. 기기가 응답하기를 기다리고 있습니다";
pub const JOINED_LINE: &str = "컴퓨터가 추가되었습니다";
pub const JOINED_NEXT: &str = "이제 프로젝트의 컴퓨터 목록에서 고를 수 있습니다";
pub const NODE_CMD_NOTE: &str = "이 명령은 추가할 다른 컴퓨터에서 실행합니다. --server-fingerprint 핀이 없으면 노드가 지문을 보여 주고 확인을 묻습니다";
pub const ZSH_QUOTE_NOTE: &str = "주소에 [ ]가 있으면 따옴표로 감싸 실행하세요";
pub const DENY_LABEL: &str = "거부";
pub const DENIED_LINE: &str = "요청을 거부했습니다. 컴퓨터는 추가되지 않았습니다";
pub const EXPIRED_LINE: &str = "코드가 만료되었습니다. 새 코드를 만드세요";
pub const GONE_LINE: &str = "이 요청을 서버에서 찾을 수 없습니다. 새 코드를 만드세요";
pub const DEMO_LINE: &str = "예시입니다. 데모에서는 서버를 쓰지 않습니다";
pub const WARN_ADDR_DIFFERS: &str = "기기가 알린 주소와 서버가 관측한 주소가 서로 다릅니다 (확인 필요합니다)";
pub const WARN_NAME_DIFFERS: &str = "기기가 보고한 이름이 입력한 컴퓨터 이름과 서로 다릅니다 (확인 필요합니다)";
pub const WARN_PUBLIC_ADDR: &str = "관측 주소가 사설 네트워크 밖입니다";
pub const NOTE_PROPOSED: &str = "기기가 주소를 알리지 않아 서버가 관측 주소로 제안합니다. 자동으로 믿지 않습니다";
pub const PEER_ID_UNUSED: &str = "노드가 주소와 peer id를 함께 알렸습니다. 이 컴퓨터는 주소로 등록되며 peer id는 쓰이지 않습니다";
pub const PEER_ONLY_NOTE: &str = "노드가 주소 없이 peer id만 알렸습니다. 아래 지문을 노드 화면의 지문과 대조해 직접 입력해야 추가됩니다";
pub const TYPED_FINGERPRINT_LABEL: &str = "노드 화면에 표시된 지문";
pub const TYPED_FINGERPRINT_HELP: &str = "노드 화면의 지문을 직접 읽어 입력합니다. 이 카드의 값은 복사하지 않습니다";
pub const NODE_FINGERPRINT_PEER: &str = "노드가 보고한 지문";
pub const FINGERPRINT_MISMATCH: &str = "입력한 지문이 노드가 보고한 지문과 다릅니다. 아무것도 추가되지 않았습니다. 노드 화면의 지문을 다시 확인하세요";
pub const FINGERPRINT_REQUIRED: &str = "이 노드는 지문 확인이 필요합니다. 노드 화면의 지문을 입력하세요";
/// Under the command in its standard-input form: the owner pastes the code when the node asks.
pub const STDIN_FORM_NOTE: &str = "위에 표시된 코드를 노드가 물을 때 붙여 넣습니다. 프로세스 목록에 남지 않습니다";
/// Label of the second, muted line: the command with the code as an argument.
pub const ARG_LABEL: &str = "코드를 인자로 줄 때";
/// A cancel on leave failed: the request stays on the server until it expires.
pub const CANCEL_FAILED: &str = "요청을 취소하지 못했습니다. 서버에서 최대 10분 뒤 만료됩니다";
/// Under the argument-form command: the code lands in shell history and the process list.
pub const ARG_WARNING: &str = "코드를 인자로 주면 셸 기록과 프로세스 목록에 남습니다";
/// Notice when the computer was added.
pub const ADDED_NOTICE: &str = "컴퓨터를 추가했습니다";
/// Notice when a confirm that outlived the closed sheet failed.
pub const CONFIRM_FAILED_LATE: &str = "컴퓨터를 추가하지 못했습니다. 컴퓨터 목록을 확인하세요";
/// A confirm whose answer never came (transport error): the server may have accepted it.
pub const CONFIRM_UNKNOWN: &str = "연결이 끊겨 결과를 알 수 없습니다. 컴퓨터 목록을 확인하세요";
pub const FINGERPRINT_PENDING_HINT: &str = "노드 화면의 지문과 같아질 때까지 추가할 수 없습니다";
pub const WARN_INFORMS: &str = "경고는 알려 줄 뿐 추가를 막지 않습니다";

/// Every sentence of the code flow, for the NFR-L4 ending check in `ui::session`.
pub const FLOW_STRINGS: &[&str] = &[
    NAME_HELP, STDIN_NOTE, STDIN_FORM_NOTE, ARG_LABEL, ARG_WARNING, ADDED_NOTICE, CONFIRM_FAILED_LATE, CONFIRM_UNKNOWN, CANCEL_FAILED, WAITING_LINE, CODE_ONCE, DOOR_OFF, DOOR_URL_UNKNOWN, NO_COMMAND, HTTP_WARNING, CONFIRM_QUESTION, CONFIRM_CHECK,
    CONFIRMED_WAIT, JOINED_LINE, JOINED_NEXT, NODE_CMD_NOTE, ZSH_QUOTE_NOTE, DENIED_LINE, EXPIRED_LINE, GONE_LINE, DEMO_LINE, WARN_ADDR_DIFFERS, WARN_NAME_DIFFERS,
    WARN_PUBLIC_ADDR, NOTE_PROPOSED, WARN_INFORMS, PEER_ID_UNUSED, PEER_ONLY_NOTE, TYPED_FINGERPRINT_HELP, FINGERPRINT_MISMATCH,
    FINGERPRINT_REQUIRED, FINGERPRINT_PENDING_HINT,
];

/// What the owner decides on: the node's own words next to the server's observation. Every
/// text is already sanitised (control and bidi characters dropped, capped).
#[derive(Debug, Clone, PartialEq)]
pub struct ConfirmRows {
    pub reported_name: String,
    /// `os arch node-version`, only what was reported.
    pub system: String,
    pub observed_addr: String,
    /// The address the node reported.
    pub reported_addr: Option<String>,
    /// The server's proposal when the node reported none.
    pub proposed_addr: Option<String>,
    /// The short form of the node's peer id, for comparison on the node's screen.
    pub fingerprint: Option<String>,
    /// The name the owner chose at mint.
    pub chosen_name: String,
    pub deadline_ms: Option<i64>,
    /// The node reported a peer id.
    pub peer_reported: bool,
    /// How the server registers the computer: `url`, `peer` or `observed`.
    pub registered_by: Option<String>,
}

/// The card asks for a typed fingerprint. The server's `registered_by` decides: only `peer`
/// does. An older server sends none, and then a node with a peer id and no address is read as
/// peer-only (the safe side: the owner types the fingerprint).
pub fn peer_only(rows: &ConfirmRows) -> bool {
    match rows.registered_by.as_deref() {
        Some(by) => by == "peer",
        None => rows.peer_reported && rows.reported_addr.is_none(),
    }
}

/// The proposed address row is hidden only when the server registers by peer id.
pub fn hide_proposed(rows: &ConfirmRows) -> bool {
    rows.registered_by.as_deref() == Some("peer")
}

/// The hint under the disabled confirm button: shown while the card wants a fingerprint and the
/// typed text does not match yet.
pub fn fingerprint_hint(rows: &ConfirmRows, typed: &str) -> Option<&'static str> {
    (!fingerprint_ok(rows, typed)).then_some(FINGERPRINT_PENDING_HINT)
}

/// Whether the line "the peer id is not used" shows: the node reported both and the server
/// registers by address.
pub fn peer_id_unused(rows: &ConfirmRows) -> bool {
    rows.peer_reported && rows.reported_addr.is_some() && rows.registered_by.as_deref() == Some("url")
}

fn fingerprint_norm(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).flat_map(|c| c.to_lowercase()).collect()
}

/// Whether what the owner typed equals the node's reported fingerprint: whitespace and case are
/// ignored and both must be 16 hex digits.
pub fn fingerprint_matches(typed: &str, reported: &str) -> bool {
    let (t, r) = (fingerprint_norm(typed), fingerprint_norm(reported));
    let hex16 = |s: &str| s.len() == 16 && s.chars().all(|c| c.is_ascii_hexdigit());
    hex16(&t) && hex16(&r) && t == r
}

/// The `expect_fingerprint` to send: the owner's typed text for a peer-only node, else nothing.
pub fn expect_fingerprint(rows: &ConfirmRows, typed: &str) -> Option<String> {
    peer_only(rows).then(|| typed.trim().to_string())
}

/// Whether 확인하고 추가 may be tapped as far as the fingerprint goes.
pub fn fingerprint_ok(rows: &ConfirmRows, typed: &str) -> bool {
    !peer_only(rows) || rows.fingerprint.as_deref().is_some_and(|r| fingerprint_matches(typed, r))
}

/// What the sheet shows. `Debug` never prints the code.
#[derive(Clone, PartialEq)]
pub enum EnrolView {
    Naming,
    WaitingForNode { code: String, expires_ms: i64 },
    AwaitingConfirm { rows: ConfirmRows },
    Joined,
    Denied,
    Expired,
    Failed(String),
}

impl fmt::Debug for EnrolView {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EnrolView::Naming => f.write_str("Naming"),
            EnrolView::WaitingForNode { expires_ms, .. } => {
                f.debug_struct("WaitingForNode").field("code", &"<redacted>").field("expires_ms", expires_ms).finish()
            }
            EnrolView::AwaitingConfirm { rows } => f.debug_struct("AwaitingConfirm").field("rows", rows).finish(),
            EnrolView::Joined => f.write_str("Joined"),
            EnrolView::Denied => f.write_str("Denied"),
            EnrolView::Expired => f.write_str("Expired"),
            EnrolView::Failed(m) => f.debug_tuple("Failed").field(m).finish(),
        }
    }
}

/// The view for a status the server answered. `code` and `expires_ms` come from the mint (the
/// status never carries the code).
pub fn view_from_status(st: &EnrolStatus, code: &str, expires_ms: i64) -> EnrolView {
    match st.status {
        EnrolState::Waiting => EnrolView::WaitingForNode { code: code.to_string(), expires_ms },
        EnrolState::AwaitingConfirmation => match confirm_rows(st) {
            Some(rows) => EnrolView::AwaitingConfirm { rows },
            // Awaiting with no node information: the node's side is not shown yet, or the
            // owner already confirmed and the node has not taken its credential.
            None => EnrolView::WaitingForNode { code: code.to_string(), expires_ms },
        },
        EnrolState::Joined => EnrolView::Joined,
        EnrolState::Denied => EnrolView::Denied,
        EnrolState::Expired => EnrolView::Expired,
        EnrolState::Unknown => EnrolView::Failed(UNKNOWN_STATE.to_string()),
    }
}

pub const UNKNOWN_STATE: &str = "서버가 알 수 없는 상태를 알려 왔습니다. 앱을 업데이트하세요";

/// The rows of the confirm card from a status, `None` while the node reported nothing.
pub fn confirm_rows(st: &EnrolStatus) -> Option<ConfirmRows> {
    let info = st.node_info.as_ref()?;
    let clean = |s: &str, max| sanitize_device_text(s, max);
    let system = [info.os.as_str(), info.arch.as_str(), info.node_version.as_str()]
        .iter()
        .map(|s| clean(s, MAX_NAME_CHARS))
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let opt = |s: &Option<String>| s.as_deref().map(|s| clean(s, MAX_ADDRESS_CHARS)).filter(|s| !s.is_empty());
    let peer_reported = info.peer_id.as_deref().is_some_and(|p| !p.trim().is_empty());
    let reported_addr = opt(&info.url);
    Some(ConfirmRows {
        peer_reported,
        registered_by: st.registered_by.clone(),
        reported_name: clean(&info.hostname, MAX_NAME_CHARS),
        system,
        observed_addr: opt(&st.observed_addr).unwrap_or_default(),
        reported_addr,
        proposed_addr: opt(&st.proposed_url),
        fingerprint: opt(&st.fingerprint),
        chosen_name: clean(&st.name, MAX_NAME_CHARS),
        deadline_ms: st.confirm_deadline_ms,
    })
}

/// Whether the sheet keeps polling in this view. The final views and a failure stop it, and so
/// does the form, which has no enrolment yet.
pub fn should_poll(v: &EnrolView) -> bool {
    matches!(v, EnrolView::WaitingForNode { .. } | EnrolView::AwaitingConfirm { .. })
}

/// What a failed poll does: a missing enrolment ends the flow, anything else is tried again on
/// the next beat and the view stays.
pub fn view_after_poll_error(current: &EnrolView, e: &EnrolCallError) -> EnrolView {
    match e {
        EnrolCallError::NotFound => EnrolView::Failed(GONE_LINE.to_string()),
        _ => current.clone(),
    }
}

/// Seconds left until `expires_ms`, 0 when past.
pub fn remaining_secs(expires_ms: i64, now_ms: i64) -> u64 {
    (expires_ms.saturating_sub(now_ms).max(0) / 1000) as u64
}

/// `9:41 남음`, or `만료됨` at zero.
pub fn countdown_text(secs: u64) -> String {
    if secs == 0 {
        "만료됨".to_string()
    } else {
        format!("{}:{:02} 남음", secs / 60, secs % 60)
    }
}

/// How the command area reads for a mint answer.
#[derive(Debug, Clone, PartialEq)]
pub enum CommandView {
    /// The command exactly as the server gave it.
    Command { text: String, plain_http: bool },
    Missing,
}

pub fn command_view(m: &EnrolMinted) -> CommandView {
    if m.command.trim().is_empty() {
        return CommandView::Missing;
    }
    CommandView::Command { plain_http: m.command.contains("http://"), text: m.command.clone() }
}

/// The comparison line for the server's short fingerprint, when the server gave one.
pub fn server_fingerprint_line(m: &EnrolMinted) -> Option<String> {
    let fp = sanitize_device_text(m.server_fingerprint.as_deref()?, MAX_ADDRESS_CHARS);
    (!fp.is_empty()).then_some(fp)
}

/// A warning on the confirm card. It informs and never blocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Warning {
    AddressDiffers,
    NameDiffers,
    PublicAddress,
}

impl Warning {
    pub fn text(self) -> &'static str {
        match self {
            Warning::AddressDiffers => WARN_ADDR_DIFFERS,
            Warning::NameDiffers => WARN_NAME_DIFFERS,
            Warning::PublicAddress => WARN_PUBLIC_ADDR,
        }
    }
}

/// The host part of `http://host:port/...`, `host:port`, `[v6]:port` or a bare address.
fn host_of(s: &str) -> String {
    let s = s.trim();
    let s = s.split_once("://").map(|(_, r)| r).unwrap_or(s);
    let s = s.split(['/', '?', '#']).next().unwrap_or("");
    let s = s.rsplit_once('@').map(|(_, h)| h).unwrap_or(s);
    if let Some(rest) = s.strip_prefix('[') {
        return rest.split(']').next().unwrap_or("").to_ascii_lowercase();
    }
    if s.matches(':').count() == 1 {
        return s.split(':').next().unwrap_or("").to_ascii_lowercase();
    }
    s.to_ascii_lowercase()
}

fn private_or_local(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(a) => a.is_private() || a.is_loopback() || a.is_link_local(),
        IpAddr::V6(a) => a.is_loopback() || (a.segments()[0] & 0xfe00) == 0xfc00 || (a.segments()[0] & 0xffc0) == 0xfe80,
    }
}

fn squash(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(|c| c.to_lowercase()).collect()
}

/// The warnings for a confirm card. `chosen` is the name the owner will give the computer. An
/// address the node reported that names another host than the observed one, a reported host
/// name that shares nothing with the chosen name, and an observed address outside the private
/// ranges are listed. All of them inform; none blocks the confirm.
pub fn warning_rows(rows: &ConfirmRows, chosen: &str) -> Vec<Warning> {
    let mut out = Vec::new();
    let observed_host = host_of(&rows.observed_addr);
    if let Some(reported) = &rows.reported_addr {
        let rh = host_of(reported);
        let both_ips = rh.parse::<IpAddr>().is_ok() && observed_host.parse::<IpAddr>().is_ok();
        // A host name against an address cannot be compared here, so only two addresses are.
        if both_ips && rh != observed_host {
            out.push(Warning::AddressDiffers);
        }
    }
    let (a, b) = (squash(&rows.reported_name), squash(chosen));
    if !a.is_empty() && !b.is_empty() && !a.contains(&b) && !b.contains(&a) {
        out.push(Warning::NameDiffers);
    }
    if let Ok(ip) = observed_host.parse::<IpAddr>() {
        if !private_or_local(ip) {
            out.push(Warning::PublicAddress);
        }
    }
    out
}

/// The name sent at confirm: the owner's text, else the name chosen at mint.
pub fn confirm_name(input: &str, chosen: &str) -> String {
    let t = input.trim();
    if t.is_empty() { chosen.to_string() } else { t.to_string() }
}

/// Checks the name field before the mint: not blank, at most 64 characters.
pub fn validate_name(input: &str) -> Result<String, &'static str> {
    let t = input.trim();
    if t.is_empty() {
        Err(NAME_EMPTY)
    } else if t.chars().count() > MAX_NAME_CHARS {
        Err(NAME_TOO_LONG)
    } else {
        Ok(t.to_string())
    }
}

/// A second tap on 확인하고 추가 counts only 600 ms after the card appeared.
pub fn confirm_counts(armed_at_ms: i64, now_ms: i64) -> bool {
    confirm_tap(armed_at_ms, now_ms)
}

/// The line for a failed mint.
pub fn mint_error_text(e: &EnrolCallError) -> &'static str {
    match e {
        EnrolCallError::DoorOff => DOOR_OFF,
        EnrolCallError::DoorUrlUnknown => DOOR_URL_UNKNOWN,
        EnrolCallError::Off => "서버의 등록 기능이 꺼져 있어 코드를 만들 수 없습니다",
        _ if matches!(e.status(), Some(s) if s >= 500) => "서버가 코드를 만들지 못했습니다. 잠시 뒤 다시 시도하세요",
        _ => "코드를 만들지 못했습니다. 이름과 연결을 확인하고 다시 시도하세요",
    }
}

/// The line for a failed confirm or deny.
pub fn confirm_error_text(e: &EnrolCallError) -> &'static str {
    match e {
        EnrolCallError::NameTaken => "같은 이름의 컴퓨터가 이미 있습니다. 다른 이름을 입력하세요",
        EnrolCallError::FingerprintMismatch => FINGERPRINT_MISMATCH,
        EnrolCallError::FingerprintRequired => FINGERPRINT_REQUIRED,
        EnrolCallError::WrongState => "이 요청은 더 이상 확인을 기다리지 않습니다",
        EnrolCallError::Expired => EXPIRED_LINE,
        EnrolCallError::NotFound => GONE_LINE,
        EnrolCallError::Off | EnrolCallError::DoorOff | EnrolCallError::DoorUrlUnknown => "서버의 등록 기능이 꺼져 있습니다",
        EnrolCallError::Api(_) => "요청을 처리하지 못했습니다. 연결을 확인하고 다시 시도하세요",
    }
}

/// The stage a debug capture opens (`EMBER_CR_PROJECT_SHEET=enrol-waiting|enrol-confirm|enrol-joined`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Name,
    Waiting,
    Confirm,
    /// A node that reported a peer id and no address.
    ConfirmPeer,
    Joined,
}

/// Decides the capture stage; a release build never opens one.
pub fn review_stage(sheet: Option<&str>, debug: bool) -> Option<Stage> {
    if !debug {
        return None;
    }
    match sheet? {
        "enrol-name" => Some(Stage::Name),
        "enrol-waiting" => Some(Stage::Waiting),
        "enrol-confirm" => Some(Stage::Confirm),
        "enrol-confirm-peer" => Some(Stage::ConfirmPeer),
        "enrol-joined" => Some(Stage::Joined),
        _ => None,
    }
}

pub const DEMO_CODE: &str = "EXAMPLE-0000-0000";
pub const DEMO_COMMAND: &str = "ember-node enrol EXAMPLE-0000-0000 --server http://192.168.0.10:8742";

/// The demo view of a stage: sample values, marked 예시 by the sheet, no server call.
pub fn demo_view(stage: Stage, now_ms: i64) -> EnrolView {
    match stage {
        Stage::Waiting => EnrolView::WaitingForNode { code: DEMO_CODE.to_string(), expires_ms: now_ms + 9 * 60_000 + 41_000 },
        Stage::Confirm => EnrolView::AwaitingConfirm {
            rows: ConfirmRows {
                reported_name: "lab-pc".to_string(),
                system: "linux x86_64 0.1.0".to_string(),
                observed_addr: "192.168.0.23:51742".to_string(),
                reported_addr: Some("http://192.168.0.23:8741".to_string()),
                proposed_addr: None,
                fingerprint: Some("ab12 cd34 ef56 7890".to_string()),
                chosen_name: "연구실 컴퓨터".to_string(),
                deadline_ms: Some(now_ms + 9 * 60_000),
                peer_reported: true,
                registered_by: Some("url".to_string()),
            },
        },
        Stage::ConfirmPeer => EnrolView::AwaitingConfirm {
            rows: ConfirmRows {
                reported_name: "lab-pc".to_string(),
                system: "linux x86_64 0.1.0".to_string(),
                observed_addr: "192.168.0.23:51742".to_string(),
                reported_addr: None,
                proposed_addr: None,
                fingerprint: Some("ab12 cd34 ef56 7890".to_string()),
                chosen_name: "연구실 컴퓨터".to_string(),
                deadline_ms: Some(now_ms + 9 * 60_000),
                peer_reported: true,
                registered_by: Some("peer".to_string()),
            },
        },
        Stage::Name => EnrolView::Naming,
        Stage::Joined => EnrolView::Joined,
    }
}

/// The largest code size (Xl3, Xl2, Xl, Lg) whose whole code fits one line in `avail` dp. A mono
/// glyph is about 0.62 of the size wide. The code never breaks inside a group.
pub fn code_font_size(len: usize, avail: f32) -> crate::ui::cr::theme::Fs {
    use crate::ui::cr::theme::Fs;
    for fs in [Fs::Xl3, Fs::Xl2, Fs::Xl] {
        if len as f32 * fs.size() * 0.62 <= avail {
            return fs;
        }
    }
    Fs::Lg
}

/// Which run the sheet may still act on. A mint answer, a poll, a confirm or a deny belongs to
/// the sheet only while its generation and id still match; closing or starting over bumps the
/// generation, so a late answer changes nothing and a late code is never stored.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RunGuard {
    generation: u64,
    open: bool,
    run_id: Option<String>,
}

impl RunGuard {
    /// The sheet opens.
    pub fn open(&mut self) {
        self.open = true;
    }

    /// A mint starts: the number its answer must still match.
    pub fn begin_mint(&mut self) -> u64 {
        self.generation += 1;
        self.run_id = None;
        self.generation
    }

    /// Whether the mint of `generation` is still the one the sheet waits for.
    pub fn mint_current(&self, generation: u64) -> bool {
        self.open && self.generation == generation
    }

    /// Takes the mint answer when the sheet is open and no newer mint or reset happened.
    pub fn accept_mint(&mut self, generation: u64, id: &str) -> bool {
        if !self.mint_current(generation) {
            return false;
        }
        self.run_id = Some(id.to_string());
        true
    }

    pub fn run_id(&self) -> Option<&str> {
        self.run_id.as_deref()
    }

    pub fn is_current(&self, id: &str) -> bool {
        self.open && self.run_id.as_deref() == Some(id)
    }

    /// The run ended or the form restarted.
    pub fn clear(&mut self) {
        self.generation += 1;
        self.run_id = None;
    }

    /// The sheet closes.
    pub fn close(&mut self) {
        self.open = false;
        self.clear();
    }
}

/// Whether the view is an end state: the code and command are dropped there.
pub fn is_end_state(v: &EnrolView) -> bool {
    matches!(v, EnrolView::Joined | EnrolView::Denied | EnrolView::Expired | EnrolView::Failed(_))
}

/// The end state a failed confirm or deny leads to, `None` when the request stays open (a taken
/// name, a connection problem).
pub fn view_after_confirm_error(e: &EnrolCallError) -> Option<EnrolView> {
    match e {
        EnrolCallError::Expired => Some(EnrolView::Expired),
        EnrolCallError::NotFound => Some(EnrolView::Failed(GONE_LINE.to_string())),
        EnrolCallError::WrongState => Some(EnrolView::Failed(confirm_error_text(e).to_string())),
        _ => None,
    }
}

/// A confirm that failed with no HTTP status (the connection broke): the server may have
/// accepted it, so the outcome is unknown and the request must not be cancelled.
pub fn confirm_is_unknown(e: &EnrolCallError) -> bool {
    matches!(e, EnrolCallError::Api(_)) && e.status().is_none()
}

/// The view after the clock passed the code's time: a waiting code ends as expired at once.
pub fn view_after_tick(v: &EnrolView, now_ms: i64) -> Option<EnrolView> {
    match v {
        EnrolView::WaitingForNode { expires_ms, .. } if remaining_secs(*expires_ms, now_ms) == 0 => Some(EnrolView::Expired),
        _ => None,
    }
}

/// Whether leaving the sheet cancels the enrolment on the server: only a code nobody redeemed or
/// a request not yet confirmed. A confirmed one is left to finish, a confirm still in flight
/// counts as confirmed (the owner just pressed it), and a sample calls nothing.
pub fn should_cancel_on_leave(view: Option<&EnrolView>, confirmed: bool, confirm_in_flight: bool, sample: bool) -> bool {
    if sample {
        return false;
    }
    match view {
        Some(EnrolView::WaitingForNode { .. }) => true,
        Some(EnrolView::AwaitingConfirm { .. }) => !confirmed && !confirm_in_flight,
        _ => false,
    }
}

/// The command with the code argument replaced by `-` (the code on standard input), for the
/// display only. `None` when the code is not a whole argument of the command (a quoted code
/// counts), so nothing is guessed.
pub fn stdin_form(command: &str, code: &str) -> Option<String> {
    if code.is_empty() {
        return None;
    }
    let (single, double) = (format!("'{code}'"), format!("\"{code}\""));
    let mut found = false;
    let parts: Vec<&str> = command
        .split(' ')
        .map(|t| {
            if t == code || t == single || t == double {
                found = true;
                "-"
            } else {
                t
            }
        })
        .collect();
    found.then(|| parts.join(" "))
}

/// What the sheet knows for the async-order decisions. The widget glue copies it from its
/// states, calls `reduce` and writes it back.
#[derive(Debug, Clone, Default)]
pub struct SheetState {
    pub guard: RunGuard,
    pub view: Option<EnrolView>,
    pub confirmed: bool,
    pub confirm_in_flight: bool,
    /// A confirm failed without an answer: it may have been accepted, so Leave does not cancel.
    pub confirm_unknown: bool,
    /// The run whose confirm was in flight when the sheet closed: its answer is still reported,
    /// once.
    pub detached_confirm: Option<String>,
    pub sample: bool,
}

/// Something that happened to the sheet. An answer names the run (or mint generation) it is for.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    MintAnswered { generation: u64, id: String },
    /// A status answer mapped to a view; also used for the end state a failed confirm or deny
    /// leads to.
    PollAnswered { id: String, view: EnrolView },
    ConfirmAnswered { id: String, ok: bool },
    /// The confirm call failed without an HTTP answer (transport error).
    ConfirmUnknown { id: String },
    DenyAnswered { id: String, ok: bool },
    Tick { now_ms: i64 },
    Leave,
}

/// What the glue does for the reducer.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// Cancel this enrolment on the server.
    Cancel(String),
    /// A new confirm card is on screen: start the 600 ms delay.
    ArmConfirm,
    RefreshComputers,
    /// Tell the owner something in a notice.
    Notice(String),
    /// Drop the typed fingerprint (the confirm was accepted).
    ClearTypedFingerprint,
    /// Drop the code, the command and the typed fingerprint.
    ClearSecrets,
    SetView(EnrolView),
}

fn end(s: &mut SheetState, v: EnrolView, out: &mut Vec<Effect>) {
    s.guard.clear();
    s.confirm_in_flight = false;
    s.view = Some(v.clone());
    out.push(Effect::ClearSecrets);
    out.push(Effect::SetView(v));
}

/// The single place that orders the sheet's async answers. A late or stale answer returns no
/// effect (a late mint returns a cancel), every end state clears the secrets.
pub fn reduce(s: &mut SheetState, ev: Event) -> Vec<Effect> {
    let mut out = Vec::new();
    match ev {
        Event::MintAnswered { generation, id } => {
            if !s.guard.accept_mint(generation, &id) {
                out.push(Effect::Cancel(id));
            }
        }
        Event::PollAnswered { id, view: next } => {
            if !s.guard.is_current(&id) {
                return out;
            }
            let Some(cur) = s.view.clone() else { return out };
            if matches!(next, EnrolView::AwaitingConfirm { .. }) && !matches!(cur, EnrolView::AwaitingConfirm { .. }) {
                out.push(Effect::ArmConfirm);
            }
            if next == EnrolView::Joined && cur != EnrolView::Joined {
                out.push(Effect::RefreshComputers);
                out.push(Effect::Notice(ADDED_NOTICE.to_string()));
            }
            if is_end_state(&next) {
                end(s, next, &mut out);
            } else if next != cur {
                s.view = Some(next.clone());
                out.push(Effect::SetView(next));
            }
        }
        Event::ConfirmAnswered { id, ok } => {
            if s.guard.is_current(&id) {
                s.confirm_in_flight = false;
                s.confirm_unknown = false;
                if ok {
                    s.confirmed = true;
                    out.push(Effect::ClearTypedFingerprint);
                }
            } else if s.detached_confirm.as_deref() == Some(id.as_str()) {
                s.detached_confirm = None;
                if ok {
                    out.push(Effect::RefreshComputers);
                    out.push(Effect::Notice(ADDED_NOTICE.to_string()));
                } else {
                    out.push(Effect::Notice(CONFIRM_FAILED_LATE.to_string()));
                }
            }
        }
        Event::ConfirmUnknown { id } => {
            if s.guard.is_current(&id) {
                s.confirm_in_flight = false;
                s.confirm_unknown = true;
            } else if s.detached_confirm.as_deref() == Some(id.as_str()) {
                s.detached_confirm = None;
                out.push(Effect::RefreshComputers);
                out.push(Effect::Notice(CONFIRM_UNKNOWN.to_string()));
            }
        }
        Event::DenyAnswered { id, ok } => {
            if ok && s.guard.is_current(&id) {
                end(s, EnrolView::Denied, &mut out);
            }
        }
        Event::Tick { now_ms } => {
            if let Some(next) = s.view.as_ref().and_then(|v| view_after_tick(v, now_ms)) {
                end(s, next, &mut out);
            }
        }
        Event::Leave => {
            let cancel = should_cancel_on_leave(s.view.as_ref(), s.confirmed, s.confirm_in_flight, s.sample) && !s.confirm_unknown;
            let run = s.guard.run_id().map(str::to_string);
            if s.confirm_in_flight && !s.sample {
                s.detached_confirm = run.clone();
            }
            if let (true, Some(id)) = (cancel, run) {
                out.push(Effect::Cancel(id));
            }
            s.guard.close();
            s.view = None;
            s.confirmed = false;
            s.confirm_in_flight = false;
            s.confirm_unknown = false;
            out.push(Effect::ClearSecrets);
        }
    }
    out
}

/// Whether a node info carries anything worth showing.
pub fn info_is_empty(i: &EnrolNodeInfo) -> bool {
    i.hostname.trim().is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(json: serde_json::Value) -> EnrolStatus {
        serde_json::from_value(json).unwrap()
    }

    fn awaiting() -> EnrolStatus {
        st(serde_json::json!({
            "status": "awaiting_confirmation", "name": "lab",
            "node_info": {"hostname": "lab-pc", "os": "linux", "arch": "x86_64", "node_version": "0.1", "url": "http://10.0.0.5:8741"},
            "observed_addr": "10.0.0.5:4000", "fingerprint": "ab12 cd34", "confirm_deadline_ms": 99
        }))
    }

    #[test]
    fn fr_c2_enrol_state_mapping() {
        let w = view_from_status(&st(serde_json::json!({"status": "waiting", "name": "lab"})), "CODE", 7);
        assert_eq!(w, EnrolView::WaitingForNode { code: "CODE".into(), expires_ms: 7 });
        assert!(matches!(view_from_status(&awaiting(), "C", 7), EnrolView::AwaitingConfirm { .. }));
        // Awaiting without node information (confirmed, credential not yet taken) keeps waiting.
        let a = st(serde_json::json!({"status": "awaiting_confirmation", "name": "lab"}));
        assert!(matches!(view_from_status(&a, "C", 7), EnrolView::WaitingForNode { .. }));
        for (s, want) in [("joined", EnrolView::Joined), ("denied", EnrolView::Denied), ("expired", EnrolView::Expired)] {
            assert_eq!(view_from_status(&st(serde_json::json!({"status": s})), "C", 7), want);
        }
        assert!(matches!(view_from_status(&st(serde_json::json!({"status": "zzz"})), "C", 7), EnrolView::Failed(_)));
    }

    #[test]
    fn fr_c2_enrol_poll_cadence_and_stop_conditions() {
        assert_eq!(POLL_EVERY_MS, 2_000);
        assert!(should_poll(&EnrolView::WaitingForNode { code: "c".into(), expires_ms: 1 }));
        assert!(should_poll(&view_from_status(&awaiting(), "c", 1)));
        for v in [EnrolView::Naming, EnrolView::Joined, EnrolView::Denied, EnrolView::Expired, EnrolView::Failed("x".into())] {
            assert!(!should_poll(&v), "{v:?}");
        }
        let cur = EnrolView::WaitingForNode { code: "c".into(), expires_ms: 1 };
        assert_eq!(view_after_poll_error(&cur, &EnrolCallError::Off), cur);
        assert!(matches!(view_after_poll_error(&cur, &EnrolCallError::NotFound), EnrolView::Failed(_)));
    }

    #[test]
    fn fr_c2_enrol_countdown() {
        assert_eq!(remaining_secs(10_000, 4_500), 5);
        assert_eq!(remaining_secs(1_000, 5_000), 0);
        assert_eq!(countdown_text(581), "9:41 남음");
        assert_eq!(countdown_text(0), "만료됨");
    }

    fn minted(v: serde_json::Value) -> EnrolMinted {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn fr_c2_enrol_command_is_shown_exactly_as_the_server_gave_it() {
        let m = minted(serde_json::json!({"id":"e","code":"c","expires_ms":1,"command":"ember-node enrol c --server http://h:1","door":"on"}));
        assert_eq!(command_view(&m), CommandView::Command { text: "ember-node enrol c --server http://h:1".into(), plain_http: true });
        // No door key (an older server) reads as on.
        let m = minted(serde_json::json!({"id":"e","code":"c","expires_ms":1,"command":"x https://h"}));
        assert_eq!(command_view(&m), CommandView::Command { text: "x https://h".into(), plain_http: false });
    }

    #[test]
    fn fr_c2_enrol_door_errors_explain_and_show_no_code() {
        assert_eq!(mint_error_text(&EnrolCallError::DoorOff), DOOR_OFF);
        assert_eq!(mint_error_text(&EnrolCallError::DoorUrlUnknown), DOOR_URL_UNKNOWN);
        assert!(DOOR_OFF.contains("EMBER_ENROL_LISTEN") && DOOR_URL_UNKNOWN.contains("EMBER_ENROL_PUBLIC_URL"));
        let m = minted(serde_json::json!({"id":"e","code":"c","expires_ms":1,"command":"  "}));
        assert_eq!(command_view(&m), CommandView::Missing);
    }

    #[test]
    fn fr_c2_enrol_server_fingerprint_is_a_comparison_line_only_when_given() {
        let m = minted(serde_json::json!({"id":"e","code":"c","expires_ms":1,"server_fingerprint":"ab12\u{202E} cd34"}));
        assert_eq!(server_fingerprint_line(&m).as_deref(), Some("ab12 cd34"));
        let m = minted(serde_json::json!({"id":"e","code":"c","expires_ms":1}));
        assert_eq!(server_fingerprint_line(&m), None);
    }

    #[test]
    fn fr_c2_enrol_node_text_is_sanitised_and_capped() {
        let s = st(serde_json::json!({
            "status": "awaiting_confirmation", "name": "n",
            "node_info": {"hostname": format!("evil\u{202E}{}", "h".repeat(100)), "os": "li\u{7}nux"},
            "observed_addr": "1.2.3.4:5\n6"
        }));
        let rows = confirm_rows(&s).unwrap();
        assert_eq!(rows.reported_name.chars().count(), 64);
        assert!(!rows.reported_name.contains('\u{202E}'));
        assert_eq!(rows.system, "linux");
        assert_eq!(rows.observed_addr, "1.2.3.4:5 6");
        assert!(confirm_rows(&st(serde_json::json!({"status": "waiting"}))).is_none());
    }

    #[test]
    fn fr_c2_enrol_warning_rows_inform() {
        let rows = confirm_rows(&awaiting()).unwrap();
        assert_eq!(warning_rows(&rows, "lab"), vec![]);
        let mut r = rows.clone();
        r.reported_addr = Some("http://10.0.0.99:8741".into());
        assert_eq!(warning_rows(&r, "lab"), vec![Warning::AddressDiffers]);
        // A host name cannot be compared with an address, so it is no warning.
        r.reported_addr = Some("http://lab.local:8741".into());
        assert_eq!(warning_rows(&r, "lab"), vec![]);
        assert_eq!(warning_rows(&rows, "완전히 다른 이름"), vec![Warning::NameDiffers]);
        assert_eq!(warning_rows(&rows, "LAB-pc"), vec![]);
        let mut p = rows.clone();
        p.observed_addr = "8.8.8.8:1".into();
        p.reported_addr = None;
        assert_eq!(warning_rows(&p, "lab"), vec![Warning::PublicAddress]);
        p.observed_addr = "[fe80::1]:9".into();
        assert_eq!(warning_rows(&p, "lab"), vec![]);
        assert!(Warning::AddressDiffers.text().contains("주소"));
    }

    #[test]
    fn fr_c2_enrol_confirm_waits_600_ms_and_names_default_to_the_chosen_one() {
        assert!(!confirm_counts(1_000, 1_599));
        assert!(confirm_counts(1_000, 1_600));
        assert_eq!(confirm_name("  ", "lab"), "lab");
        assert_eq!(confirm_name(" new ", "lab"), "new");
        assert_eq!(validate_name(" a "), Ok("a".to_string()));
        assert_eq!(validate_name(" "), Err(NAME_EMPTY));
        assert_eq!(validate_name(&"a".repeat(65)), Err(NAME_TOO_LONG));
    }

    #[test]
    fn fr_c2_enrol_debug_of_the_views_hides_the_code() {
        let v = EnrolView::WaitingForNode { code: "SECRET-CODE".into(), expires_ms: 5 };
        assert!(!format!("{v:?}").contains("SECRET"));
        assert!(!format!("{:?}", Some(v)).contains("SECRET"));
    }

    #[test]
    fn fr_c2_enrol_error_lines_are_fixed_text() {
        assert!(confirm_error_text(&EnrolCallError::NameTaken).contains("이름"));
        assert_ne!(confirm_error_text(&EnrolCallError::WrongState), confirm_error_text(&EnrolCallError::NameTaken));
        assert_ne!(mint_error_text(&EnrolCallError::Off), mint_error_text(&EnrolCallError::NotFound));
    }

    #[test]
    fn fr_c2_enrol_review_stage_is_debug_only() {
        assert_eq!(review_stage(Some("enrol-waiting"), true), Some(Stage::Waiting));
        assert_eq!(review_stage(Some("enrol-confirm"), true), Some(Stage::Confirm));
        assert_eq!(review_stage(Some("enrol-joined"), true), Some(Stage::Joined));
        assert_eq!(review_stage(Some("enrol-confirm"), false), None);
        assert_eq!(review_stage(Some("add-computer"), true), None);
        assert!(matches!(demo_view(Stage::Confirm, 0), EnrolView::AwaitingConfirm { .. }));
    }

    #[test]
    fn fr_c2_enrol_code_size_fits_one_line() {
        use crate::ui::cr::theme::Fs;
        assert_eq!(code_font_size(17, 320.0), Fs::Xl2);
        assert_eq!(code_font_size(10, 320.0), Fs::Xl3);
        assert_eq!(code_font_size(17, 200.0), Fs::Xl);
        assert_eq!(code_font_size(17, 150.0), Fs::Lg);
        for len in 8..=24usize {
            let fs = code_font_size(len, 320.0);
            assert!(fs == Fs::Lg || len as f32 * fs.size() * 0.62 <= 320.0);
        }
    }

    #[test]
    fn fr_c2_enrol_name_stage_and_new_lines() {
        assert_eq!(review_stage(Some("enrol-name"), true), Some(Stage::Name));
        assert_eq!(review_stage(Some("enrol-name"), false), None);
        assert_eq!(demo_view(Stage::Name, 0), EnrolView::Naming);
        assert_eq!(DENY_LABEL, "거부");
        assert!(ZSH_QUOTE_NOTE.contains("따옴표") && NODE_CMD_NOTE.contains("확인을 묻습니다"));
        assert!(JOINED_NEXT.contains("컴퓨터 목록"));
    }

    fn peer_status(url: Option<&str>, registered_by: &str) -> EnrolStatus {
        let mut info = serde_json::json!({"hostname": "lab-pc", "peer_id": "12D3KooWabc"});
        if let Some(u) = url {
            info["url"] = u.into();
        }
        st(serde_json::json!({
            "status": "awaiting_confirmation", "name": "lab", "node_info": info,
            "observed_addr": "10.0.0.5:4000", "proposed_url": "http://10.0.0.5:8741", "fingerprint": "ab12 cd34 ef56 7890",
            "registered_by": registered_by
        }))
    }

    #[test]
    fn fr_c2_enrol_peer_only_needs_a_peer_id_and_no_url() {
        let only = confirm_rows(&peer_status(None, "peer")).unwrap();
        assert!(peer_only(&only));
        // The row stays in the data; the card hides it only for registered_by peer.
        assert!(hide_proposed(&only));
        let both = confirm_rows(&peer_status(Some("http://10.0.0.5:8741"), "url")).unwrap();
        assert!(!peer_only(&both));
        // No peer id at all: not peer-only, and the proposal stays.
        let none = confirm_rows(&awaiting()).unwrap();
        assert!(!peer_only(&none));
    }

    fn status_by(url: Option<&str>, registered_by: Option<&str>) -> EnrolStatus {
        let mut v = serde_json::json!({
            "status": "awaiting_confirmation", "name": "lab",
            "node_info": {"hostname": "lab-pc", "peer_id": "12D3KooWabc"},
            "observed_addr": "10.0.0.5:4000", "proposed_url": "http://10.0.0.5:8741", "fingerprint": "ab12 cd34 ef56 7890"
        });
        if let Some(u) = url {
            v["node_info"]["url"] = u.into();
        }
        if let Some(b) = registered_by {
            v["registered_by"] = b.into();
        }
        st(v)
    }

    #[test]
    fn fr_c2_enrol_registered_by_observed_keeps_the_proposed_address_and_needs_no_typing() {
        let r = confirm_rows(&status_by(None, Some("observed"))).unwrap();
        assert!(!peer_only(&r) && !hide_proposed(&r));
        assert_eq!(r.proposed_addr.as_deref(), Some("http://10.0.0.5:8741"));
        assert!(fingerprint_ok(&r, ""));
        assert_eq!(expect_fingerprint(&r, "ab12cd34ef567890"), None);
        assert_eq!(fingerprint_hint(&r, ""), None);
    }

    #[test]
    fn fr_c2_enrol_registered_by_peer_needs_the_typed_value_and_hides_the_proposal() {
        let r = confirm_rows(&status_by(None, Some("peer"))).unwrap();
        assert!(peer_only(&r) && hide_proposed(&r));
        assert!(!fingerprint_ok(&r, "ab12"));
        assert_eq!(fingerprint_hint(&r, "ab12"), Some(FINGERPRINT_PENDING_HINT));
        assert_eq!(fingerprint_hint(&r, "ab12cd34ef567890"), None);
        // Server says peer even though a url was reported: the server wins.
        assert!(peer_only(&confirm_rows(&status_by(Some("http://h:1"), Some("peer"))).unwrap()));
        // Server says url: no typing, even without a reported url.
        assert!(!peer_only(&confirm_rows(&status_by(None, Some("url"))).unwrap()));
    }

    #[test]
    fn fr_c2_enrol_absent_registered_by_reads_peer_id_without_url_as_peer_only_but_keeps_the_proposal() {
        let r = confirm_rows(&status_by(None, None)).unwrap();
        assert!(peer_only(&r) && !hide_proposed(&r));
        assert_eq!(r.proposed_addr.as_deref(), Some("http://10.0.0.5:8741"));
        assert!(!fingerprint_ok(&r, ""));
        let with_url = confirm_rows(&status_by(Some("http://h:1"), None)).unwrap();
        assert!(!peer_only(&with_url));
    }

    #[test]
    fn fr_c2_enrol_a_whitespace_only_url_counts_as_no_url() {
        let r = confirm_rows(&status_by(Some("  \t "), None)).unwrap();
        assert_eq!(r.reported_addr, None);
        assert!(peer_only(&r));
    }

    #[test]
    fn fr_c2_enrol_peer_only_sends_the_typed_text_and_others_send_nothing() {
        let only = confirm_rows(&peer_status(None, "peer")).unwrap();
        assert_eq!(expect_fingerprint(&only, " AB12 cd34 ef56 7890 ").as_deref(), Some("AB12 cd34 ef56 7890"));
        assert!(fingerprint_ok(&only, "ab12cd34ef567890"));
        assert!(!fingerprint_ok(&only, ""));
        assert!(!fingerprint_ok(&only, "ab12cd34ef567891"));
        let both = confirm_rows(&peer_status(Some("http://10.0.0.5:8741"), "url")).unwrap();
        assert_eq!(expect_fingerprint(&both, "ab12cd34ef567890"), None);
        assert!(fingerprint_ok(&both, ""));
    }

    #[test]
    fn fr_c2_enrol_fingerprint_matches() {
        assert!(fingerprint_matches("ab12 cd34 ef56 7890", "AB12CD34EF567890"));
        assert!(fingerprint_matches(" ab12\tcd34 ef56 7890\n", "ab12 cd34 ef56 7890"));
        assert!(!fingerprint_matches("ab12 cd34 ef56 789", "ab12 cd34 ef56 7890"));
        assert!(!fingerprint_matches("ab12 cd34 ef56 78901", "ab12 cd34 ef56 7890"));
        assert!(!fingerprint_matches("ab12 cd34 ef56 7891", "ab12 cd34 ef56 7890"));
        assert!(!fingerprint_matches("", ""));
        // Not hex: never equal, even to itself.
        assert!(!fingerprint_matches("zzzzzzzzzzzzzzzz", "zzzzzzzzzzzzzzzz"));
    }

    #[test]
    fn fr_c2_enrol_registered_by_url_with_a_peer_id_shows_the_unused_line() {
        let both = confirm_rows(&peer_status(Some("http://10.0.0.5:8741"), "url")).unwrap();
        assert_eq!(both.registered_by.as_deref(), Some("url"));
        assert!(peer_id_unused(&both));
        assert!(PEER_ID_UNUSED.contains("peer id는 쓰이지 않습니다"));
        let only = confirm_rows(&peer_status(None, "peer")).unwrap();
        assert!(!peer_id_unused(&only));
        // No peer id, or an older server without the field: no line.
        assert!(!peer_id_unused(&confirm_rows(&awaiting()).unwrap()));
    }

    #[test]
    fn fr_c2_enrol_registered_by_mismatch_error_is_a_fixed_line_that_keeps_the_request() {
        assert_eq!(confirm_error_text(&EnrolCallError::FingerprintMismatch), "입력한 지문이 노드가 보고한 지문과 다릅니다. 아무것도 추가되지 않았습니다. 노드 화면의 지문을 다시 확인하세요");
        assert_eq!(view_after_confirm_error(&EnrolCallError::FingerprintMismatch), None);
        assert_eq!(confirm_error_text(&EnrolCallError::FingerprintRequired), "이 노드는 지문 확인이 필요합니다. 노드 화면의 지문을 입력하세요");
        assert_eq!(view_after_confirm_error(&EnrolCallError::FingerprintRequired), None);
        assert_eq!(review_stage(Some("enrol-confirm-peer"), true), Some(Stage::ConfirmPeer));
        assert_eq!(review_stage(Some("enrol-confirm-peer"), false), None);
        match demo_view(Stage::ConfirmPeer, 0) {
            EnrolView::AwaitingConfirm { rows } => assert!(peer_only(&rows)),
            v => panic!("{v:?}"),
        }
    }

    #[test]
    fn fr_c2_enrol_sentences_follow_nfr_l4() {
        for s in FLOW_STRINGS.iter().chain([&UNKNOWN_STATE]) {
            let t = s.trim();
            assert!(!t.contains('\u{2014}'), "{t}");
            assert!(
                !t.ends_with('다') || t.ends_with("입니다") || t.ends_with("합니다") || t.ends_with("습니다") || t.ends_with("됩니다"),
                "{t}"
            );
        }
    }

    #[test]
    fn fr_c2_enrol_run_guard_drops_stale_answers() {
        let mut g = RunGuard::default();
        g.open();
        let n = g.begin_mint();
        assert!(g.accept_mint(n, "a"));
        assert!(g.is_current("a") && !g.is_current("b"));
        g.clear();
        assert!(!g.is_current("a"));
    }

    #[test]
    fn fr_c2_enrol_run_guard_refuses_late_mints() {
        let mut g = RunGuard::default();
        g.open();
        let first = g.begin_mint();
        g.close();
        assert!(!g.accept_mint(first, "a") && !g.is_current("a"));
        g.open();
        let old = g.begin_mint();
        let newer = g.begin_mint();
        assert!(!g.accept_mint(old, "x"));
        assert!(g.accept_mint(newer, "y") && g.is_current("y"));
        let again = g.begin_mint();
        g.clear();
        assert!(!g.accept_mint(again, "z"));
    }

    #[test]
    fn fr_c2_enrol_run_guard_end_states_clear_the_run() {
        for v in [EnrolView::Joined, EnrolView::Denied, EnrolView::Expired, EnrolView::Failed("x".into())] {
            assert!(is_end_state(&v));
        }
        assert!(!is_end_state(&EnrolView::Naming) && !should_poll(&EnrolView::Expired));
        let mut g = RunGuard::default();
        g.open();
        let n = g.begin_mint();
        g.accept_mint(n, "a");
        g.clear();
        assert!(!g.is_current("a"));
        assert_eq!(view_after_confirm_error(&EnrolCallError::Expired), Some(EnrolView::Expired));
        assert!(matches!(view_after_confirm_error(&EnrolCallError::NotFound), Some(EnrolView::Failed(_))));
        assert!(matches!(view_after_confirm_error(&EnrolCallError::WrongState), Some(EnrolView::Failed(_))));
        assert_eq!(view_after_confirm_error(&EnrolCallError::NameTaken), None);
        let w = EnrolView::WaitingForNode { code: "c".into(), expires_ms: 5_000 };
        assert_eq!(view_after_tick(&w, 3_999), None);
        assert_eq!(view_after_tick(&w, 5_000), Some(EnrolView::Expired));
        assert_eq!(view_after_tick(&EnrolView::Joined, 9_000), None);
    }

    #[test]
    fn fr_c2_enrol_run_guard_close_bumps_the_generation() {
        let mut g = RunGuard::default();
        g.open();
        let n = g.begin_mint();
        g.accept_mint(n, "a");
        g.close();
        assert!(!g.mint_current(n) && !g.is_current("a"));
        g.open();
        assert!(!g.mint_current(n));
    }

    #[test]
    fn fr_c2_enrol_run_guard_leave_after_confirm_does_not_cancel() {
        let w = EnrolView::WaitingForNode { code: "c".into(), expires_ms: 1 };
        let a = view_from_status(&awaiting(), "c", 1);
        assert!(should_cancel_on_leave(Some(&w), false, false, false));
        assert!(should_cancel_on_leave(Some(&a), false, false, false));
        assert!(!should_cancel_on_leave(Some(&a), true, false, false));
        assert!(!should_cancel_on_leave(Some(&a), false, true, false));
        assert!(!should_cancel_on_leave(Some(&w), false, false, true));
        assert!(!should_cancel_on_leave(Some(&EnrolView::Joined), false, false, false));
        assert!(!should_cancel_on_leave(None, false, false, false));
    }

    fn open_sheet(view: EnrolView) -> SheetState {
        let mut s = SheetState::default();
        s.guard.open();
        let n = s.guard.begin_mint();
        assert!(reduce(&mut s, Event::MintAnswered { generation: n, id: "a".into() }).is_empty());
        s.view = Some(view);
        s
    }

    fn waiting() -> EnrolView {
        EnrolView::WaitingForNode { code: "c".into(), expires_ms: 5_000 }
    }

    #[test]
    fn fr_c2_enrol_reducer_late_mint_after_leave_is_refused_and_cancelled() {
        let mut s = SheetState::default();
        s.guard.open();
        let n = s.guard.begin_mint();
        reduce(&mut s, Event::Leave);
        let fx = reduce(&mut s, Event::MintAnswered { generation: n, id: "late".into() });
        assert_eq!(fx, vec![Effect::Cancel("late".into())]);
        assert!(!s.guard.is_current("late"));
    }

    #[test]
    fn fr_c2_enrol_reducer_stale_answers_are_ignored() {
        let mut s = open_sheet(waiting());
        let a = view_from_status(&awaiting(), "c", 1);
        assert!(reduce(&mut s, Event::PollAnswered { id: "old".into(), view: a }).is_empty());
        assert!(reduce(&mut s, Event::PollAnswered { id: "old".into(), view: EnrolView::Joined }).is_empty());
        assert!(reduce(&mut s, Event::ConfirmAnswered { id: "old".into(), ok: true }).is_empty());
        assert!(!s.confirmed);
        assert!(reduce(&mut s, Event::DenyAnswered { id: "old".into(), ok: true }).is_empty());
        assert_eq!(s.view, Some(waiting()));
        assert!(s.guard.is_current("a"));
    }

    #[test]
    fn fr_c2_enrol_reducer_every_end_state_clears_secrets() {
        for v in [EnrolView::Joined, EnrolView::Denied, EnrolView::Expired, EnrolView::Failed("x".into())] {
            let mut s = open_sheet(waiting());
            let fx = reduce(&mut s, Event::PollAnswered { id: "a".into(), view: v.clone() });
            assert!(fx.contains(&Effect::ClearSecrets) && fx.contains(&Effect::SetView(v)), "{fx:?}");
            assert!(!s.guard.is_current("a"));
        }
        let mut s = open_sheet(waiting());
        let fx = reduce(&mut s, Event::DenyAnswered { id: "a".into(), ok: true });
        assert_eq!(fx, vec![Effect::ClearSecrets, Effect::SetView(EnrolView::Denied)]);
        let mut s = open_sheet(waiting());
        assert!(reduce(&mut s, Event::Tick { now_ms: 3_999 }).is_empty());
        let fx = reduce(&mut s, Event::Tick { now_ms: 5_000 });
        assert_eq!(fx, vec![Effect::ClearSecrets, Effect::SetView(EnrolView::Expired)]);
        let mut s = open_sheet(waiting());
        assert!(reduce(&mut s, Event::Leave).contains(&Effect::ClearSecrets));
    }

    #[test]
    fn fr_c2_enrol_reducer_poll_arms_the_confirm_and_refreshes_on_join() {
        let mut s = open_sheet(waiting());
        let a = view_from_status(&awaiting(), "c", 1);
        let fx = reduce(&mut s, Event::PollAnswered { id: "a".into(), view: a.clone() });
        assert_eq!(fx, vec![Effect::ArmConfirm, Effect::SetView(a.clone())]);
        assert!(reduce(&mut s, Event::PollAnswered { id: "a".into(), view: a }).is_empty());
        let fx = reduce(&mut s, Event::PollAnswered { id: "a".into(), view: EnrolView::Joined });
        assert_eq!(fx[0], Effect::RefreshComputers);
    }

    #[test]
    fn fr_c2_enrol_reducer_leave_cancels_only_unconfirmed() {
        let mut s = open_sheet(waiting());
        assert_eq!(reduce(&mut s, Event::Leave), vec![Effect::Cancel("a".into()), Effect::ClearSecrets]);
        let a = view_from_status(&awaiting(), "c", 1);
        let mut s = open_sheet(a.clone());
        reduce(&mut s, Event::ConfirmAnswered { id: "a".into(), ok: true });
        assert!(s.confirmed);
        assert_eq!(reduce(&mut s, Event::Leave), vec![Effect::ClearSecrets]);
        let mut s = open_sheet(a);
        s.confirm_in_flight = true;
        assert_eq!(reduce(&mut s, Event::Leave), vec![Effect::ClearSecrets]);
    }

    fn left_during_confirm() -> SheetState {
        let mut s = open_sheet(view_from_status(&awaiting(), "c", 1));
        s.confirm_in_flight = true;
        assert_eq!(reduce(&mut s, Event::Leave), vec![Effect::ClearSecrets]);
        s
    }

    #[test]
    fn fr_c2_enrol_reducer_confirm_after_leave_is_still_reported() {
        let mut s = left_during_confirm();
        let fx = reduce(&mut s, Event::ConfirmAnswered { id: "a".into(), ok: true });
        assert_eq!(fx, vec![Effect::RefreshComputers, Effect::Notice(ADDED_NOTICE.into())]);
        assert!(!fx.iter().any(|e| matches!(e, Effect::SetView(_))));
        assert!(reduce(&mut s, Event::ConfirmAnswered { id: "a".into(), ok: true }).is_empty());
        let mut s = left_during_confirm();
        let fx = reduce(&mut s, Event::ConfirmAnswered { id: "a".into(), ok: false });
        assert_eq!(fx, vec![Effect::Notice(CONFIRM_FAILED_LATE.into())]);
        assert!(reduce(&mut s, Event::ConfirmAnswered { id: "a".into(), ok: false }).is_empty());
        let mut s = left_during_confirm();
        assert!(reduce(&mut s, Event::ConfirmAnswered { id: "other".into(), ok: true }).is_empty());
        assert!(!reduce(&mut s, Event::ConfirmAnswered { id: "a".into(), ok: true }).is_empty());
    }

    #[test]
    fn fr_c2_enrol_reducer_unknown_confirm_is_never_cancelled() {
        let mut s = open_sheet(view_from_status(&awaiting(), "c", 1));
        s.confirm_in_flight = true;
        reduce(&mut s, Event::ConfirmUnknown { id: "a".into() });
        assert!(s.confirm_unknown && !s.confirm_in_flight);
        assert_eq!(reduce(&mut s, Event::Leave), vec![Effect::ClearSecrets]);
        let mut s = left_during_confirm();
        let fx = reduce(&mut s, Event::ConfirmUnknown { id: "a".into() });
        assert_eq!(fx, vec![Effect::RefreshComputers, Effect::Notice(CONFIRM_UNKNOWN.into())]);
    }

    #[test]
    fn fr_c2_enrol_reducer_accepted_confirm_clears_the_typed_fingerprint() {
        let mut s = open_sheet(view_from_status(&awaiting(), "c", 1));
        s.confirm_in_flight = true;
        let fx = reduce(&mut s, Event::ConfirmAnswered { id: "a".into(), ok: true });
        assert_eq!(fx, vec![Effect::ClearTypedFingerprint]);
    }

    #[test]
    fn fr_c2_enrol_reducer_confirm_answer_clears_the_flight_flag() {
        let mut s = open_sheet(view_from_status(&awaiting(), "c", 1));
        s.confirm_in_flight = true;
        reduce(&mut s, Event::ConfirmAnswered { id: "a".into(), ok: false });
        assert!(!s.confirm_in_flight && !s.confirmed);
        assert_eq!(reduce(&mut s, Event::Leave), vec![Effect::Cancel("a".into()), Effect::ClearSecrets]);
    }

    #[test]
    fn fr_c2_enrol_stdin_form_replaces_only_the_whole_code_argument() {
        assert_eq!(stdin_form("ember-node enrol AB12 --server http://h:1", "AB12").as_deref(), Some("ember-node enrol - --server http://h:1"));
        assert_eq!(stdin_form("ember-node enrol 'AB12' --server x", "AB12").as_deref(), Some("ember-node enrol - --server x"));
        // The code inside another argument is not an argument of its own.
        assert_eq!(stdin_form("ember-node enrol --server http://AB12.example", "AB12"), None);
        assert_eq!(stdin_form("ember-node enrol X", "AB12"), None);
        assert_eq!(stdin_form("ember-node enrol AB12", ""), None);
    }
}
