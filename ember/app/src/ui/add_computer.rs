//! The pure model of the add-computer sheet (FR-X7 / FR-N2, #490): the account's hub devices as
//! rows, the state pill of each, the check of the typed token and name, and the fixed Korean
//! sentence for each failure. The server's own error text never reaches the screen. The node
//! token is typed by the owner, sent once and never logged or kept.

use ember_client::api::{HubCallError, HubDevice};

use crate::ui::components::PillTone;

/// Where the sheet is in loading the devices.
#[derive(Debug, Clone, PartialEq)]
pub enum DevicesState {
    Loading,
    Ready(Vec<HubDevice>),
    /// 503: the server runs with the hub off.
    HubOff,
    Failed(String),
}

pub const HUB_OFF: &str = "허브가 꺼져 있어 기기를 불러올 수 없습니다";
pub const NO_DEVICES: &str = "계정에 등록된 기기가 없습니다";
pub const DEMO_CANNOT_ADD: &str = "데모에서는 컴퓨터를 추가할 수 없습니다";
pub const TOKEN_LABEL: &str = "노드 토큰";
/// Where the token comes from, as far as `docs/design/COMPUTERS.md` says: the node serves its API
/// with a token, so the owner copies it from the machine that runs ember node.
pub const TOKEN_HELP: &str = "컴퓨터에서 실행 중인 ember node의 토큰입니다. 그 컴퓨터에서 확인해 붙여 넣으세요";
pub const NAME_LABEL: &str = "이름 (선택)";

/// How a device row is described.
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceRow {
    pub endpoint_id: String,
    pub name: String,
    /// Role and app, e.g. `컴퓨터 · ember-node 0.9`; only what the hub said.
    pub kind: String,
    pub pill: (&'static str, PillTone),
    /// A computer can be added from this device.
    pub can_add: bool,
    /// Shown as a sample (demo mode).
    pub sample: bool,
}

/// The state of one device: 이 서버, 컴퓨터로 등록됨, 허용됨 or 등록 안 됨.
pub fn device_state_pill(d: &HubDevice) -> (&'static str, PillTone) {
    if d.this_server {
        ("이 서버", PillTone::Blue)
    } else if d.computer_id.is_some() {
        ("컴퓨터로 등록됨", PillTone::Green)
    } else if d.allowed {
        ("허용됨", PillTone::Sky)
    } else {
        ("등록 안 됨", PillTone::Grey)
    }
}

fn role_label(role: &str) -> Option<&'static str> {
    match role {
        "main_server" => Some("서버"),
        "computer" => Some("컴퓨터"),
        "client" => Some("클라이언트"),
        _ => None,
    }
}

/// `역할 · 앱 버전`, leaving out what the hub did not report.
pub fn device_kind(d: &HubDevice) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(r) = role_label(&d.role) {
        parts.push(r.to_string());
    }
    if let Some(a) = d.app.as_ref().filter(|a| !a.kind.is_empty()) {
        parts.push(if a.version.is_empty() { a.kind.clone() } else { format!("{} {}", a.kind, a.version) });
    }
    if parts.is_empty() {
        "종류 알 수 없음".to_string()
    } else {
        parts.join(" \u{b7} ")
    }
}

/// Rows in the hub's order, this server first; a nameless device is shown by a short id.
pub fn device_rows(devices: &[HubDevice], sample: bool) -> Vec<DeviceRow> {
    let mut rows: Vec<DeviceRow> = devices
        .iter()
        .map(|d| DeviceRow {
            endpoint_id: d.endpoint_id.clone(),
            name: if d.name.trim().is_empty() { d.endpoint_id.chars().take(8).collect() } else { d.name.clone() },
            kind: device_kind(d),
            pill: device_state_pill(d),
            can_add: !d.this_server && d.computer_id.is_none(),
            sample,
        })
        .collect();
    rows.sort_by_key(|r| r.pill.0 != "이 서버");
    rows
}

/// Why the typed values cannot be sent yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddError {
    TokenMissing,
    NameTooLong,
}

impl AddError {
    pub fn message(self) -> &'static str {
        match self {
            AddError::TokenMissing => "노드 토큰을 입력하세요",
            AddError::NameTooLong => "이름은 64자 이하로 입력하세요",
        }
    }
}

pub const MAX_NAME_CHARS: usize = 64;

/// The token must not be blank; a name is optional and short. Returns the values to send.
pub fn validate_add<'a>(token: &'a str, name: &'a str) -> Result<(&'a str, Option<&'a str>), AddError> {
    let token = token.trim();
    if token.is_empty() {
        return Err(AddError::TokenMissing);
    }
    let name = name.trim();
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(AddError::NameTooLong);
    }
    Ok((token, (!name.is_empty()).then_some(name)))
}

pub const CONFIRM_QUESTION: &str = "이 기기를 컴퓨터로 추가합니다. 추가한 컴퓨터는 서버 사용자와 같은 권한으로 쓰입니다.";

/// Where the sheet is in adding: the form, or the confirmation before the POST. The token is
/// not in this state, so the confirm view cannot show it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum AddStep {
    #[default]
    Form,
    Confirm { device_id: String, name: Option<String> },
}

/// What the confirm view shows: the device name, its address and the name the computer gets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmRows {
    pub device_name: String,
    pub address: String,
    pub address_short: String,
    pub computer_name: String,
}

/// The address shortened for a row (first 8 and last 6 characters); the full value is shown too.
pub fn short_address(id: &str) -> String {
    let n = id.chars().count();
    if n <= 16 {
        return id.to_string();
    }
    let head: String = id.chars().take(8).collect();
    let tail: String = id.chars().skip(n - 6).collect();
    format!("{head}...{tail}")
}

/// 컴퓨터로 추가: checks the typed values and opens the confirmation. It never sends.
pub fn request_confirm(device: &DeviceRow, token: &str, name: &str) -> Result<AddStep, AddError> {
    let (_, name) = validate_add(token, name)?;
    Ok(AddStep::Confirm { device_id: device.endpoint_id.clone(), name: name.map(str::to_string) })
}

impl AddStep {
    pub fn is_confirm(&self) -> bool {
        matches!(self, AddStep::Confirm { .. })
    }

    /// The rows of the confirm view; none in the form.
    pub fn rows(&self, device: &DeviceRow) -> Option<ConfirmRows> {
        let AddStep::Confirm { name, .. } = self else { return None };
        Some(ConfirmRows {
            device_name: device.name.clone(),
            address: device.endpoint_id.clone(),
            address_short: short_address(&device.endpoint_id),
            computer_name: name.clone().unwrap_or_else(|| device.name.clone()),
        })
    }

    /// A tap on 확인하고 추가, armed at `armed_at_ms`: true only after the delay, so a fast second
    /// tap cannot skip the confirmation.
    pub fn confirm_counts(&self, armed_at_ms: i64, now_ms: i64) -> bool {
        self.is_confirm() && crate::ui::channel_owner::confirm_tap(armed_at_ms, now_ms)
    }

    /// Cancel or close: back to an empty form.
    pub fn cleared() -> AddStep {
        AddStep::Form
    }
}

/// The fixed sentence of a failed add, by status (the hub routes carry no `code`).
pub fn add_error_text(status: Option<u16>) -> &'static str {
    match status {
        Some(503) => HUB_OFF,
        Some(400) => "토큰이 올바르지 않거나 컴퓨터에 연결하지 못했습니다. 토큰을 확인하세요",
        Some(401) | Some(403) => "이 서버에서 컴퓨터를 추가할 권한이 없습니다",
        Some(404) => "계정에서 이 기기를 찾을 수 없습니다. 목록을 다시 불러오세요",
        Some(409) => "이미 컴퓨터로 등록되어 있습니다",
        Some(s) if s >= 500 => "서버가 요청을 처리하지 못했습니다. 잠시 뒤 다시 시도하세요",
        _ => "컴퓨터를 추가하지 못했습니다. 연결을 확인하고 다시 시도하세요",
    }
}

/// The fixed sentence for a failed device list.
pub fn list_error_text(status: Option<u16>) -> &'static str {
    match status {
        Some(s) if s >= 500 => "서버가 기기 목록을 불러오지 못했습니다. 잠시 뒤 다시 시도하세요",
        _ => "기기 목록을 불러오지 못했습니다. 연결을 확인하고 다시 시도하세요",
    }
}

/// The list state of a finished `hub_devices()` call.
pub fn devices_state(r: Result<Vec<HubDevice>, HubCallError>) -> DevicesState {
    match r {
        Ok(v) => DevicesState::Ready(v),
        Err(HubCallError::Off) => DevicesState::HubOff,
        Err(e) => DevicesState::Failed(list_error_text(e.status()).to_string()),
    }
}

/// Sample devices for the demo, shown marked 예시.
pub fn demo_devices() -> Vec<HubDevice> {
    vec![
        HubDevice { endpoint_id: "demo-server".into(), name: "이 서버".into(), role: "main_server".into(), online: true, this_server: true, allowed: true, ..Default::default() },
        HubDevice { endpoint_id: "demo-lab".into(), name: "연구실 컴퓨터".into(), role: "computer".into(), online: true, ..Default::default() },
    ]
}

/// The unsaved-changes guard input: something typed.
pub fn is_blank(token: &str, name: &str) -> bool {
    token.trim().is_empty() && name.trim().is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ember_client::api::HubDeviceApp;

    fn dev(id: &str) -> HubDevice {
        HubDevice { endpoint_id: id.into(), name: id.into(), ..Default::default() }
    }

    fn row(id: &str, name: &str) -> DeviceRow {
        DeviceRow { endpoint_id: id.into(), name: name.into(), kind: String::new(), pill: ("등록 안 됨", PillTone::Grey), can_add: true, sample: false }
    }

    #[test]
    fn fr_x7_add_computer_needs_a_confirm_step_before_the_post() {
        let d = row("abcdef0123456789abcdef", "연구실");
        assert_eq!(request_confirm(&d, " ", ""), Err(AddError::TokenMissing));
        assert_eq!(request_confirm(&d, "t", &"가".repeat(65)), Err(AddError::NameTooLong));
        let step = request_confirm(&d, "t", " lab ").unwrap();
        assert_eq!(step, AddStep::Confirm { device_id: d.endpoint_id.clone(), name: Some("lab".into()) });
        assert!(step.is_confirm() && !AddStep::Form.is_confirm());
    }

    #[test]
    fn fr_x7_add_computer_confirm_shows_name_and_address_not_the_token() {
        let d = row("abcdef0123456789abcdef", "연구실");
        let step = request_confirm(&d, "SECRET-TOKEN", "").unwrap();
        let r = step.rows(&d).unwrap();
        assert_eq!((r.device_name.as_str(), r.computer_name.as_str()), ("연구실", "연구실"));
        assert_eq!(r.address, d.endpoint_id);
        assert_eq!(r.address_short, "abcdef01...abcdef");
        assert!(!format!("{step:?}{r:?}").contains("SECRET"));
        let named = request_confirm(&d, "t", "lab").unwrap().rows(&d).unwrap();
        assert_eq!(named.computer_name, "lab");
        assert!(AddStep::Form.rows(&d).is_none());
    }

    #[test]
    fn fr_x7_add_computer_fast_second_tap_does_not_confirm() {
        let step = AddStep::Confirm { device_id: "a".into(), name: None };
        assert!(!step.confirm_counts(1_000, 1_000));
        assert!(!step.confirm_counts(1_000, 1_599));
        assert!(step.confirm_counts(1_000, 1_600));
        assert!(!AddStep::Form.confirm_counts(0, 10_000));
    }

    #[test]
    fn fr_x7_add_computer_state_is_cleared_on_close() {
        assert_eq!(AddStep::cleared(), AddStep::Form);
        assert!(!AddStep::cleared().is_confirm());
    }

    #[test]
    fn fr_x7_add_computer_state_pill_by_device() {
        let mut d = dev("a");
        assert_eq!(device_state_pill(&d).0, "등록 안 됨");
        d.allowed = true;
        assert_eq!(device_state_pill(&d).0, "허용됨");
        d.computer_id = Some("a".into());
        assert_eq!(device_state_pill(&d).0, "컴퓨터로 등록됨");
        d.this_server = true;
        assert_eq!(device_state_pill(&d).0, "이 서버");
    }

    #[test]
    fn fr_x7_add_computer_rows_put_this_server_first_and_offer_add_only_when_free() {
        let mut me = dev("me");
        me.this_server = true;
        let mut reg = dev("reg");
        reg.computer_id = Some("reg".into());
        let free = HubDevice { name: " ".into(), ..dev("0123456789") };
        let rows = device_rows(&[free, reg, me], false);
        assert_eq!(rows[0].endpoint_id, "me");
        // The sort is stable: this server first, then the input order (free, reg).
        assert_eq!(rows[1].endpoint_id, "0123456789");
        assert!(!rows[0].can_add && rows[1].can_add && !rows[2].can_add);
        assert_eq!(rows[1].name, "01234567");
        assert!(rows.iter().all(|r| !r.sample));
        assert!(device_rows(&[dev("x")], true)[0].sample);
    }

    #[test]
    fn fr_x7_add_computer_kind_line_has_only_what_the_hub_said() {
        let mut d = dev("a");
        assert_eq!(device_kind(&d), "종류 알 수 없음");
        d.role = "computer".into();
        d.app = Some(HubDeviceApp { kind: "ember-node".into(), version: "0.9".into() });
        assert_eq!(device_kind(&d), "컴퓨터 \u{b7} ember-node 0.9");
    }

    #[test]
    fn fr_x7_add_computer_validate_requires_a_token_and_trims() {
        assert_eq!(validate_add("  ", "x"), Err(AddError::TokenMissing));
        assert_eq!(validate_add(" t ", " "), Ok(("t", None)));
        assert_eq!(validate_add("t", " lab "), Ok(("t", Some("lab"))));
        assert_eq!(validate_add("t", &"가".repeat(65)), Err(AddError::NameTooLong));
        assert!(validate_add("t", &"가".repeat(64)).is_ok());
        assert!(is_blank(" ", "") && !is_blank("t", ""));
    }

    #[test]
    fn fr_x7_add_computer_error_text_is_fixed_by_status() {
        assert_eq!(add_error_text(Some(503)), HUB_OFF);
        assert!(add_error_text(Some(400)).contains("토큰"));
        assert!(add_error_text(Some(409)).contains("이미"));
        assert_ne!(add_error_text(Some(500)), add_error_text(None));
        for s in [None, Some(400), Some(404), Some(409), Some(500), Some(502)] {
            assert!(!add_error_text(s).is_empty() && !add_error_text(s).contains("server answered"));
        }
    }

    #[test]
    fn fr_x7_add_computer_list_states_are_honest() {
        assert_eq!(devices_state(Err(HubCallError::Off)), DevicesState::HubOff);
        assert_eq!(devices_state(Ok(vec![])), DevicesState::Ready(vec![]));
        match devices_state(Err(HubCallError::Api(ember_client::api::ApiError::Peer("raw text".into())))) {
            DevicesState::Failed(m) => assert!(!m.contains("raw text")),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn fr_x7_add_computer_demo_devices_include_this_server_and_a_free_device() {
        let rows = device_rows(&demo_devices(), true);
        assert_eq!(rows[0].pill.0, "이 서버");
        assert!(rows.iter().any(|r| r.can_add));
    }
}
