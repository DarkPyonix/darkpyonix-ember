//! The pure model of the add-computer sheet (FR-C1 / FR-C2, D39, #490): the account's hub devices
//! as read-only rows, the state pill and the online state of each, the cleaning of what the hub
//! returned, and the fixed Korean sentence for each failure. The sheet takes no secret: a new
//! computer joins with a one-time enrol code (D39), and that screen is later.

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
/// Under the list: the honest status of adding a computer (D39).
pub const ENROL_NOTE: &str = "새 컴퓨터는 1회용 코드로 추가합니다. 이 화면의 코드 방식은 준비 중입니다.";

pub const MAX_NAME_CHARS: usize = 64;
pub const MAX_ADDRESS_CHARS: usize = 80;

/// Text from the hub made safe to show: whitespace runs (and line breaks) become one space,
/// control characters and Unicode bidi or format characters (U+202A..202E, U+2066..2069,
/// U+200B..200F, U+FEFF) are dropped, and text over `max` characters is cut to `max` with an
/// ellipsis. The hub is not trusted to send clean text.
pub fn sanitize_device_text(s: &str, max: usize) -> String {
    let mut out = String::new();
    let mut pending_space = false;
    for c in s.chars() {
        if c.is_whitespace() {
            pending_space = true;
            continue;
        }
        let hidden = c.is_control()
            || matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200B}'..='\u{200F}' | '\u{FEFF}');
        if hidden {
            continue;
        }
        if pending_space && !out.is_empty() {
            out.push(' ');
        }
        pending_space = false;
        out.push(c);
    }
    if out.chars().count() > max {
        let cut: String = out.chars().take(max.saturating_sub(1)).collect();
        return format!("{}\u{2026}", cut.trim_end());
    }
    out
}

/// How a device row is described. Every text is already sanitised.
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceRow {
    pub endpoint_id: String,
    pub name: String,
    /// Role and app, e.g. `컴퓨터 · ember-node 0.9`; only what the hub said.
    pub kind: String,
    pub pill: (&'static str, PillTone),
    /// 온라인, 오프라인, or 상태 알 수 없음 when the hub did not say.
    pub online: (&'static str, PillTone),
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

/// The online state as the hub reported it.
pub fn online_label(online: Option<bool>) -> (&'static str, PillTone) {
    match online {
        Some(true) => ("온라인", PillTone::Green),
        Some(false) => ("오프라인", PillTone::Grey),
        None => ("상태 알 수 없음", PillTone::Grey),
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
    if let Some(a) = d.app.as_ref().filter(|a| !sanitize_device_text(&a.kind, MAX_NAME_CHARS).is_empty()) {
        let kind = sanitize_device_text(&a.kind, MAX_NAME_CHARS);
        let version = sanitize_device_text(&a.version, MAX_NAME_CHARS);
        parts.push(if version.is_empty() { kind } else { format!("{kind} {version}") });
    }
    if parts.is_empty() {
        "종류 알 수 없음".to_string()
    } else {
        parts.join(" \u{b7} ")
    }
}

/// Rows in the hub's order, this server first. The name is cut to 64 characters and the address
/// to 80; a device with no usable name is shown by the first 8 characters of its id.
pub fn device_rows(devices: &[HubDevice], sample: bool) -> Vec<DeviceRow> {
    let mut rows: Vec<DeviceRow> = devices
        .iter()
        .map(|d| {
            let endpoint_id = sanitize_device_text(&d.endpoint_id, MAX_ADDRESS_CHARS);
            let name = sanitize_device_text(&d.name, MAX_NAME_CHARS);
            DeviceRow {
                name: if name.is_empty() { endpoint_id.chars().take(8).collect() } else { name },
                endpoint_id,
                kind: device_kind(d),
                pill: device_state_pill(d),
                online: online_label(d.online),
                sample,
            }
        })
        .collect();
    rows.sort_by_key(|r| r.pill.0 != "이 서버");
    rows
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
        HubDevice { endpoint_id: "demo-server".into(), name: "이 서버".into(), role: "main_server".into(), online: Some(true), this_server: true, allowed: true, ..Default::default() },
        HubDevice { endpoint_id: "demo-lab".into(), name: "연구실 컴퓨터".into(), role: "computer".into(), online: Some(true), ..Default::default() },
    ]
}

/// What a capture asks of the add-computer sheet (debug builds only).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewMode {
    Off,
    /// `EMBER_CR_PROJECT_SHEET=add-computer`: the sheet open with sample devices.
    List,
}

/// Decides the review mode from the env value and the debug flag; a release build is always
/// `Off`, so the hook cannot open a sheet or skip the server for a user.
pub fn review_mode(sheet: Option<&str>, debug: bool) -> ReviewMode {
    if debug && sheet == Some("add-computer") {
        ReviewMode::List
    } else {
        ReviewMode::Off
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ember_client::api::HubDeviceApp;

    fn dev(id: &str) -> HubDevice {
        HubDevice { endpoint_id: id.into(), name: id.into(), ..Default::default() }
    }

    #[test]
    fn fr_c2_device_text_is_sanitised_and_capped() {
        assert_eq!(sanitize_device_text("a\u{0}b\u{7}c\u{1b}[31m", 64), "abc[31m");
        assert_eq!(sanitize_device_text("  lab \n\t  pc  ", 64), "lab pc");
        assert_eq!(sanitize_device_text("evil\u{202E}txt.exe\u{2066}x\u{2069}\u{200B}\u{FEFF}\u{200F}", 64), "eviltxt.exex");
        assert_eq!(sanitize_device_text("\u{202E}\u{200B}", 64), "");
        assert_eq!(sanitize_device_text("가나다", 64), "가나다");
        let long = "가".repeat(100);
        let cut = sanitize_device_text(&long, MAX_NAME_CHARS);
        assert_eq!(cut.chars().count(), 64);
        assert!(cut.ends_with('\u{2026}'));
        assert_eq!(sanitize_device_text(&"a".repeat(64), 64), "a".repeat(64));
        assert_eq!(sanitize_device_text(&"b".repeat(200), MAX_ADDRESS_CHARS).chars().count(), 80);
        // Empty names become the short id; hidden characters in the id are dropped too.
        let nameless = HubDevice { name: "\u{202E} \n".into(), ..dev("0123\u{200B}456789") };
        let rows = device_rows(&[nameless], false);
        assert_eq!(rows[0].name, "01234567");
        assert_eq!(rows[0].endpoint_id, "0123456789");
        let big = HubDevice { name: "n".repeat(500), ..dev(&"i".repeat(500)) };
        let rows = device_rows(&[big], false);
        assert_eq!((rows[0].name.chars().count(), rows[0].endpoint_id.chars().count()), (64, 80));
    }

    #[test]
    fn fr_c2_add_computer_state_pill_by_device() {
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
    fn fr_c2_add_computer_online_state_is_shown_only_as_reported() {
        assert_eq!(online_label(Some(true)).0, "온라인");
        assert_eq!(online_label(Some(false)).0, "오프라인");
        assert_eq!(online_label(None).0, "상태 알 수 없음");
        let rows = device_rows(&[dev("a")], false);
        assert_eq!(rows[0].online.0, "상태 알 수 없음");
    }

    #[test]
    fn fr_c2_add_computer_rows_put_this_server_first_and_mark_samples() {
        let mut me = dev("me");
        me.this_server = true;
        let mut reg = dev("reg");
        reg.computer_id = Some("reg".into());
        let free = HubDevice { name: " ".into(), ..dev("0123456789") };
        let rows = device_rows(&[free, reg, me], false);
        assert_eq!(rows[0].endpoint_id, "me");
        // The sort is stable: this server first, then the input order (free, reg).
        assert_eq!(rows[1].endpoint_id, "0123456789");
        assert_eq!(rows[1].name, "01234567");
        assert!(rows.iter().all(|r| !r.sample));
        assert!(device_rows(&[dev("x")], true)[0].sample);
    }

    #[test]
    fn fr_c2_add_computer_kind_line_has_only_what_the_hub_said() {
        let mut d = dev("a");
        assert_eq!(device_kind(&d), "종류 알 수 없음");
        d.role = "computer".into();
        d.app = Some(HubDeviceApp { kind: "ember-node".into(), version: "0.9".into() });
        assert_eq!(device_kind(&d), "컴퓨터 \u{b7} ember-node 0.9");
        d.app = Some(HubDeviceApp { kind: "ember\u{202E}-node".into(), version: String::new() });
        assert_eq!(device_kind(&d), "컴퓨터 \u{b7} ember-node");
    }

    #[test]
    fn fr_c2_add_computer_list_states_are_honest() {
        assert_eq!(devices_state(Err(HubCallError::Off)), DevicesState::HubOff);
        assert_eq!(devices_state(Ok(vec![])), DevicesState::Ready(vec![]));
        match devices_state(Err(HubCallError::Api(ember_client::api::ApiError::Peer("raw text".into())))) {
            DevicesState::Failed(m) => assert!(!m.contains("raw text")),
            other => panic!("{other:?}"),
        }
        assert_ne!(list_error_text(Some(500)), list_error_text(None));
        assert!(ENROL_NOTE.contains("1회용 코드") && ENROL_NOTE.contains("준비 중"));
    }

    #[test]
    fn fr_c2_add_computer_review_hook_is_debug_only() {
        assert_eq!(review_mode(Some("add-computer"), true), ReviewMode::List);
        assert_eq!(review_mode(Some("add-computer"), false), ReviewMode::Off);
        assert_eq!(review_mode(Some("settings"), true), ReviewMode::Off);
        assert_eq!(review_mode(None, true), ReviewMode::Off);
    }

    #[test]
    fn fr_c2_add_computer_demo_devices_include_this_server_and_a_computer() {
        let rows = device_rows(&demo_devices(), true);
        assert_eq!(rows[0].pill.0, "이 서버");
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.sample && r.online.0 == "온라인"));
    }
}
