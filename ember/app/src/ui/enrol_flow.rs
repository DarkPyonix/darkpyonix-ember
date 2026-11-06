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
pub const NODE_CMD_PENDING: &str = "노드의 enrol 명령은 개발 중입니다. 노드 쪽이 준비되면 이 명령으로 가입합니다";
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
pub const WARN_INFORMS: &str = "경고는 알려 줄 뿐 추가를 막지 않습니다";

/// Every sentence of the code flow, for the NFR-L4 ending check in `ui::session`.
pub const FLOW_STRINGS: &[&str] = &[
    NAME_HELP, STDIN_NOTE, WAITING_LINE, CODE_ONCE, DOOR_OFF, DOOR_URL_UNKNOWN, NO_COMMAND, HTTP_WARNING, CONFIRM_QUESTION, CONFIRM_CHECK,
    CONFIRMED_WAIT, JOINED_LINE, JOINED_NEXT, NODE_CMD_PENDING, ZSH_QUOTE_NOTE, DENIED_LINE, EXPIRED_LINE, GONE_LINE, DEMO_LINE, WARN_ADDR_DIFFERS, WARN_NAME_DIFFERS,
    WARN_PUBLIC_ADDR, NOTE_PROPOSED, WARN_INFORMS,
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
    Some(ConfirmRows {
        reported_name: clean(&info.hostname, MAX_NAME_CHARS),
        system,
        observed_addr: opt(&st.observed_addr).unwrap_or_default(),
        reported_addr: opt(&info.url),
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
        assert!(ZSH_QUOTE_NOTE.contains("따옴표") && NODE_CMD_PENDING.contains("개발 중"));
        assert!(JOINED_NEXT.contains("컴퓨터 목록"));
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
}
