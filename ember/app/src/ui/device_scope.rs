//! Per-device scope in the app (FR-N8, #366, #817): the label of each scope, the picker of a
//! device row, the confirmation rule for narrowing the device the app runs on, and the one
//! sentence a refused call (403 `scope_denied`) shows. Pure logic; the settings sheet
//! (`ui/cr/device_scope.rs`) draws it and `Services` makes the calls.

use std::sync::Mutex;

use ember_client::api::{ApiError, DeviceRecord, DeviceScope, Whoami};

/// The fixed sentence for a call the server refused with 403 `scope_denied`.
pub const SCOPE_DENIED: &str = "이 기기에는 이 작업을 할 권한이 없습니다. 기기 권한은 서버에서 바꿀 수 있습니다.";

/// The question before narrowing the device the app runs on.
pub const CONFIRM_SELF: &str =
    "이 기기의 권한을 좁힙니다. 이 앱에서 할 수 있는 작업이 줄어들고, 다시 넓히려면 서버에서 바꿔야 합니다. 계속합니다";

/// Shown when the list is empty.
pub const NO_DEVICES: &str = "서버에 등록된 기기가 없습니다";

/// Shown when the scope could not be changed for another reason.
pub const SET_FAILED: &str = "기기 권한을 바꾸지 못했습니다. 연결을 확인하고 다시 시도합니다";

/// The peer id of the row that is the device the app runs on. The local door is the server's
/// own machine and never in the device list; the transport door names its peer; an unknown
/// answer (the call failed) has no self row.
pub fn self_peer(who: Option<&Whoami>) -> Option<String> {
    let w = who?;
    if w.door != "transport" {
        return None;
    }
    w.peer_id.clone().filter(|p| !p.trim().is_empty())
}

/// "이 기기 권한: ..." for the transport door only; the local door is always full.
pub fn own_scope_line(who: Option<&Whoami>) -> Option<String> {
    let w = who.filter(|w| w.door == "transport")?;
    let label = match w.scope.as_str() {
        "full" => scope_label(DeviceScope::Full),
        "read_only" => scope_label(DeviceScope::ReadOnly),
        "chat_only" => scope_label(DeviceScope::ChatOnly),
        _ => return None,
    };
    Some(format!("이 기기 권한: {label}"))
}

pub fn demo_whoami() -> Whoami {
    Whoami { door: "local".into(), peer_id: None, scope: "full".into() }
}

/// The picker order, widest first.
pub const PICKER: [DeviceScope; 3] = [DeviceScope::Full, DeviceScope::ReadOnly, DeviceScope::ChatOnly];

pub fn scope_label(s: DeviceScope) -> &'static str {
    match s {
        DeviceScope::Full => "전체",
        DeviceScope::ReadOnly => "읽기 전용",
        DeviceScope::ChatOnly => "채팅만",
    }
}

/// How much a scope allows: `full` > `read_only` > `chat_only`.
fn width(s: DeviceScope) -> u8 {
    match s {
        DeviceScope::Full => 2,
        DeviceScope::ReadOnly => 1,
        DeviceScope::ChatOnly => 0,
    }
}

/// `to` allows less than `from`. `read_only` and `chat_only` are not subsets of each other, so
/// moving between them counts as narrowing too (each loses something the other had).
pub fn is_narrowing(from: DeviceScope, to: DeviceScope) -> bool {
    from != to && (width(to) < width(from) || to != DeviceScope::Full)
}

/// What picking `to` on a row does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickAction {
    /// The scope is already `to`: no call.
    Nothing,
    /// Call `set_device_scope` now.
    Set,
    /// Ask [`CONFIRM_SELF`] first; call only on yes.
    Confirm,
}

pub fn pick_action(row_peer: &str, self_peer: Option<&str>, from: DeviceScope, to: DeviceScope) -> PickAction {
    if from == to {
        return PickAction::Nothing;
    }
    let is_self = self_peer.is_some_and(|p| !p.trim().is_empty() && p.trim() == row_peer);
    if is_self && is_narrowing(from, to) {
        PickAction::Confirm
    } else {
        PickAction::Set
    }
}

/// One chip of a row's picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chip {
    pub scope: DeviceScope,
    pub label: &'static str,
    pub selected: bool,
}

/// One device row: name, short peer id, "이 기기" marker, and the picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeRow {
    pub peer_id: String,
    pub name: String,
    pub short_peer: String,
    pub is_self: bool,
    pub scope: DeviceScope,
    pub chips: Vec<Chip>,
}

pub fn scope_rows(devices: &[DeviceRecord], self_peer: Option<&str>) -> Vec<ScopeRow> {
    let mut v: Vec<ScopeRow> = devices
        .iter()
        .map(|d| {
            let name = crate::ui::add_computer::sanitize_device_text(&d.name, 60);
            let short: String = d.peer_id.chars().take(12).collect();
            ScopeRow {
                peer_id: d.peer_id.clone(),
                name: if name.is_empty() { short.clone() } else { name },
                short_peer: short,
                is_self: self_peer.is_some_and(|p| !p.trim().is_empty() && p.trim() == d.peer_id),
                scope: d.scope,
                chips: PICKER
                    .iter()
                    .map(|&s| Chip { scope: s, label: scope_label(s), selected: s == d.scope })
                    .collect(),
            }
        })
        .collect();
    // This device first, then by name: the row a person most likely looks for leads.
    v.sort_by(|a, b| b.is_self.cmp(&a.is_self).then_with(|| a.name.cmp(&b.name)));
    v
}

/// The row after a successful call: the server's answer replaces the old record.
pub fn apply_answer(devices: &mut [DeviceRecord], answer: DeviceRecord) {
    if let Some(d) = devices.iter_mut().find(|d| d.peer_id == answer.peer_id) {
        *d = answer;
    }
}

/// `true` for a 403 `scope_denied` from `ember_client`.
// TODO(#784): use `ApiError::is_scope_denied` once it exists.
pub fn is_scope_denied(e: &ApiError) -> bool {
    matches!(e, ApiError::Status { status: 403, code: Some(c), .. } if c == "scope_denied")
}

/// `true` for error text that carries a 403 `scope_denied`: the code itself, or the server's
/// message (`this device's scope (...) does not allow this route`) behind a 403, as
/// `ServerError` and `ApiError` print them.
pub fn text_is_scope_denied(text: &str) -> bool {
    text.contains("scope_denied") || (text.contains("403") && text.contains("does not allow this route"))
}

/// The text a failure shows: [`SCOPE_DENIED`] for a refused scope, otherwise unchanged.
pub fn failure_text(text: &str) -> String {
    if text_is_scope_denied(text) {
        SCOPE_DENIED.to_string()
    } else {
        text.to_string()
    }
}

/// The failure of a scope change, in Korean.
pub fn set_error_text(e: &ApiError) -> &'static str {
    if is_scope_denied(e) {
        SCOPE_DENIED
    } else {
        SET_FAILED
    }
}

// ---- demo source (#199) -----------------------------------------------------------------

static DEMO: Mutex<Option<Vec<DeviceRecord>>> = Mutex::new(None);

/// The demo's peer id for its laptop row (not the app's own device: see `Services::whoami`).
pub const DEMO_SELF_PEER: &str = "12D3KooWDemoThisLaptop";

fn demo_seed() -> Vec<DeviceRecord> {
    let d = |peer: &str, name: &str, scope| DeviceRecord {
        peer_id: peer.into(),
        name: name.into(),
        created_at: 1_760_000_000_000,
        source: "enrol".into(),
        scope,
    };
    vec![
        d(DEMO_SELF_PEER, "내 노트북", DeviceScope::Full),
        d("12D3KooWDemoLabDesktop", "연구실 컴퓨터", DeviceScope::ReadOnly),
        d("12D3KooWDemoPhone", "휴대폰", DeviceScope::ChatOnly),
    ]
}

pub fn demo_devices() -> Vec<DeviceRecord> {
    DEMO.lock().unwrap().get_or_insert_with(demo_seed).clone()
}

pub fn demo_set_scope(peer_id: &str, scope: DeviceScope) -> Option<DeviceRecord> {
    let mut g = DEMO.lock().unwrap();
    let list = g.get_or_insert_with(demo_seed);
    let d = list.iter_mut().find(|d| d.peer_id == peer_id)?;
    d.scope = scope;
    Some(d.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(peer: &str, name: &str, scope: DeviceScope) -> DeviceRecord {
        DeviceRecord { peer_id: peer.into(), name: name.into(), created_at: 0, source: "manual".into(), scope }
    }

    #[test]
    fn fr_n8_app_picker_lists_three_scopes_and_marks_the_current_one() {
        let rows = scope_rows(&[rec("p2", "b", DeviceScope::ReadOnly), rec("p1", "a", DeviceScope::Full)], Some("p2"));
        assert_eq!(rows[0].peer_id, "p2", "this device leads");
        assert!(rows[0].is_self && !rows[1].is_self);
        let labels: Vec<_> = rows[0].chips.iter().map(|c| c.label).collect();
        assert_eq!(labels, ["전체", "읽기 전용", "채팅만"]);
        let sel: Vec<_> = rows[0].chips.iter().filter(|c| c.selected).map(|c| c.scope).collect();
        assert_eq!(sel, [DeviceScope::ReadOnly]);
        assert_eq!(rows[1].chips.iter().find(|c| c.selected).unwrap().scope, DeviceScope::Full);
    }

    #[test]
    fn fr_n8_app_nameless_device_shows_its_short_peer_id() {
        let rows = scope_rows(&[rec("12D3KooWabcdefghijk", " ", DeviceScope::Full)], None);
        assert_eq!(rows[0].name, "12D3KooWabcd");
    }

    #[test]
    fn fr_n8_app_narrowing_this_device_asks_first() {
        use DeviceScope::*;
        assert_eq!(pick_action("me", Some("me"), Full, ReadOnly), PickAction::Confirm);
        assert_eq!(pick_action("me", Some("me"), Full, ChatOnly), PickAction::Confirm);
        assert_eq!(pick_action("me", Some("me"), ReadOnly, ChatOnly), PickAction::Confirm);
        assert_eq!(pick_action("me", Some("me"), ChatOnly, ReadOnly), PickAction::Confirm);
        assert_eq!(pick_action("me", Some("me"), ReadOnly, Full), PickAction::Set, "widening never asks");
        assert_eq!(pick_action("other", Some("me"), Full, ChatOnly), PickAction::Set, "another device never asks");
        assert_eq!(pick_action("me", None, Full, ChatOnly), PickAction::Set);
        assert_eq!(pick_action("me", Some(" "), Full, ChatOnly), PickAction::Set);
        assert_eq!(pick_action("me", Some("me"), Full, Full), PickAction::Nothing);
    }

    fn who(door: &str, peer: Option<&str>, scope: &str) -> Whoami {
        Whoami { door: door.into(), peer_id: peer.map(str::to_string), scope: scope.into() }
    }

    #[test]
    fn fr_n8_app_self_row_comes_from_whoami() {
        assert_eq!(self_peer(Some(&who("local", None, "full"))), None, "local door: no self row");
        assert_eq!(self_peer(Some(&who("local", Some("p"), "full"))), None);
        assert_eq!(self_peer(Some(&who("transport", Some("p1"), "read_only"))).as_deref(), Some("p1"));
        assert_eq!(self_peer(Some(&who("transport", None, "full"))), None);
        assert_eq!(self_peer(None), None, "error: unknown, no self row");
        assert_eq!(self_peer(Some(&demo_whoami())), None, "demo is the local door");
        assert_eq!(own_scope_line(Some(&who("transport", Some("p1"), "chat_only"))).as_deref(), Some("이 기기 권한: 채팅만"));
        assert_eq!(own_scope_line(Some(&who("local", None, "full"))), None);
        assert_eq!(own_scope_line(None), None);
    }

    #[test]
    fn fr_n8_app_answer_replaces_the_row() {
        let mut list = vec![rec("p1", "a", DeviceScope::Full), rec("p2", "b", DeviceScope::Full)];
        apply_answer(&mut list, rec("p2", "b", DeviceScope::ChatOnly));
        assert_eq!(list[1].scope, DeviceScope::ChatOnly);
        assert_eq!(list[0].scope, DeviceScope::Full);
    }

    #[test]
    fn fr_n8_app_scope_denied_maps_to_one_korean_sentence() {
        let denied = ApiError::Status {
            status: 403,
            message: "this device's scope (read_only) does not allow this route".into(),
            code: Some("scope_denied".into()),
        };
        assert!(is_scope_denied(&denied));
        assert_eq!(set_error_text(&denied), SCOPE_DENIED);
        let other = ApiError::Status { status: 403, message: "x".into(), code: Some("forbidden".into()) };
        assert!(!is_scope_denied(&other));
        assert_eq!(set_error_text(&other), SET_FAILED);
        assert_eq!(failure_text(&denied.to_string()), SCOPE_DENIED);
        let server = "server answered 403: this device's scope (read_only) does not allow this route";
        assert_eq!(failure_text(server), SCOPE_DENIED);
        assert_eq!(failure_text("server answered 500: boom"), "server answered 500: boom");
        assert!(!SCOPE_DENIED.contains('\u{2014}'));
    }

    #[test]
    fn fr_n8_app_demo_source_changes_the_scope() {
        assert!(demo_devices().iter().any(|d| d.peer_id == DEMO_SELF_PEER));
        let r = demo_set_scope("12D3KooWDemoPhone", DeviceScope::ReadOnly).unwrap();
        assert_eq!(r.scope, DeviceScope::ReadOnly);
        assert!(demo_set_scope("nope", DeviceScope::Full).is_none());
        demo_set_scope("12D3KooWDemoPhone", DeviceScope::ChatOnly);
    }
}
