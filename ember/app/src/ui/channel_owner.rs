//! The channel owner's side of invites, guests and allowances (FR-T14, FR-T15, FR-T17; the sheet is
//! part of the room view, FR-L12).
//!
//! Pure decisions, tested without a renderer: the state of an invite from the server's times, the
//! remaining-time text, the allowance badge, the hour range, the honest state of the guest door,
//! the label of a guest's messages and the Korean sentence of a server error code. Nothing here
//! invents a time or a state: a text is drawn only from a value the server sent.

use ember_client::channels::{Guest, GuestDoorState, Invite};

/// The lifetimes the owner can pick for a new invite, in hours (FR-T14 allows 1 to 168).
pub const TTL_CHOICES: [u32; 4] = [1, 24, 72, 168];
/// The default lifetime of an invite; the server's own default is the same.
pub const DEFAULT_TTL_HOURS: u32 = 24;
/// The allowance presets, in hours; any whole hour 1..=168 is also allowed (FR-T17).
pub const ALLOW_PRESETS: [u32; 5] = [1, 8, 24, 72, 168];
pub const MIN_HOURS: u32 = 1;
pub const MAX_HOURS: u32 = 168;

/// Whether `hours` is a value the server accepts (whole hours, 1..=168).
pub fn hours_valid(hours: u32) -> bool {
    (MIN_HOURS..=MAX_HOURS).contains(&hours)
}

/// The custom-hours field's text as hours: whole digits only, inside the range. A fraction, a
/// sign, text, empty or out-of-range input is `None`, so nothing is sent for it.
pub fn parse_hours(text: &str) -> Option<u32> {
    let t = text.trim();
    if t.is_empty() || !t.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    t.parse::<u32>().ok().filter(|h| hours_valid(*h))
}

/// The custom-hours text as hours, pulled into 1..=168 when it is out of range; the flag says it was
/// moved. Whole digits only: anything else (fraction, sign, text, empty) is `None` and stays
/// rejected. A run of digits too long for a number clamps to the top.
pub fn clamp_hours(text: &str) -> Option<(u32, bool)> {
    let t = text.trim();
    if t.is_empty() || !t.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let n = t.parse::<u64>().unwrap_or(u64::MAX);
    let h = n.clamp(MIN_HOURS as u64, MAX_HOURS as u64) as u32;
    Some((h, h as u64 != n))
}

/// The note shown when a typed value was pulled into the range.
pub const CLAMPED_NOTE: &str = "1에서 168 사이로 맞췄습니다";

/// The state of an invite, derived from the server's times (Unix ms) and the clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InviteState {
    Pending,
    Used,
    Expired,
    Revoked,
}

impl InviteState {
    pub fn label(self) -> &'static str {
        match self {
            InviteState::Pending => "대기 중",
            InviteState::Used => "사용됨",
            InviteState::Expired => "만료됨",
            InviteState::Revoked => "취소됨",
        }
    }
}

/// Revoked wins over used, used over expired: a revoked invite stays revoked whatever its times.
pub fn invite_state(invite: &Invite, now_ms: i64) -> InviteState {
    if invite.revoked_at.is_some() {
        InviteState::Revoked
    } else if invite.used_at.is_some() {
        InviteState::Used
    } else if invite.expires_at <= now_ms {
        InviteState::Expired
    } else {
        InviteState::Pending
    }
}

/// Time left until `until_ms`, in Korean; `None` when it is not in the future. Under a minute is
/// "1분 미만", so a value that is still ahead never reads as 0.
pub fn remaining_text(now_ms: i64, until_ms: i64) -> Option<String> {
    if until_ms <= now_ms {
        return None;
    }
    let minutes = (until_ms - now_ms).div_euclid(60_000);
    let (d, h, m) = (minutes / 1440, (minutes % 1440) / 60, minutes % 60);
    Some(if d > 0 {
        if h > 0 { format!("{d}일 {h}시간 남음") } else { format!("{d}일 남음") }
    } else if h > 0 {
        if m > 0 { format!("{h}시간 {m}분 남음") } else { format!("{h}시간 남음") }
    } else if m > 0 {
        format!("{m}분 남음")
    } else {
        "1분 미만 남음".to_string()
    })
}

/// The badge of a guest whose allowance is in force: "허용 중" with the time left. Hidden (`None`)
/// when there is no allowance or its end has passed.
pub fn allowance_badge(allow_until: Option<i64>, now_ms: i64) -> Option<String> {
    let left = remaining_text(now_ms, allow_until?)?;
    Some(format!("허용 중 · {left}"))
}

/// What the owner is told about the guest door (FR-T14 PR 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DoorStatus {
    /// No invite has been made yet, so the server has not said.
    Unknown,
    /// The door is off, or the link has no address: the link cannot be used.
    Off,
    /// The door is on; `warning` asks the owner to check the address reaches from outside.
    Open { warning: bool },
}

/// The door as the create-invite answer shows it. `None` for the door and no link is "not known
/// yet". A relative link (`/join#...`) is a useless link whatever the door says.
pub fn door_status(door: Option<&GuestDoorState>, link: Option<&str>, link_warning: bool) -> DoorStatus {
    let Some(link) = link else { return DoorStatus::Unknown };
    if link.starts_with('/') || door.is_some_and(|d| !d.is_on()) {
        return DoorStatus::Off;
    }
    DoorStatus::Open { warning: link_warning }
}

impl DoorStatus {
    pub fn text(self) -> &'static str {
        match self {
            DoorStatus::Unknown => "게스트 문 상태: 아직 알 수 없습니다",
            DoorStatus::Off => "게스트 문이 꺼져 있습니다. 링크는 문이 켜져야 쓸 수 있습니다",
            DoorStatus::Open { .. } => "게스트 문이 켜져 있습니다",
        }
    }

    /// The extra line when the address may not reach from outside.
    pub fn warning(self) -> Option<&'static str> {
        matches!(self, DoorStatus::Open { warning: true }).then_some("이 주소가 밖에서 닿는지 확인하십시오")
    }

    /// A link is offered for copying only when the door is on.
    pub fn can_copy(self) -> bool {
        matches!(self, DoorStatus::Open { .. })
    }
}

/// The name a guest's message shows, and whether it carries the 게스트 tag. The sender
/// `guest:<id>` never reaches the screen: with no name the label is plain "게스트".
pub fn guest_author_label(from: &str, author_name: &str, author_kind: &str) -> Option<String> {
    if !(from.starts_with("guest:") || author_kind == "guest") {
        return None;
    }
    let name = author_name.trim();
    Some(if name.is_empty() || name.starts_with("guest:") { "게스트".to_string() } else { name.to_string() })
}

/// A fixed Korean sentence for a server error code, never the server's own text.
pub fn error_message(code: Option<&str>) -> &'static str {
    match code {
        Some("invalid_hours") => "시간은 1시간에서 168시간 사이의 정수여야 합니다",
        Some("not_found") => "대상을 찾을 수 없습니다. 이미 취소되었거나 제거되었을 수 있습니다",
        Some("unauthorized") => "권한이 없습니다",
        _ => "요청을 처리하지 못했습니다. 잠시 뒤 다시 시도하십시오",
    }
}

/// The note after setting an allowance: only when the server cut it to the guest's own expiry.
pub fn capped_note(capped: bool) -> Option<&'static str> {
    capped.then_some("게스트 자격의 만료 시각까지로 줄었습니다")
}

/// A second tap counts only this long after the first armed the row, so a double tap cannot skip
/// the confirmation.
pub const CONFIRM_DELAY_MS: i64 = 600;

/// Whether a second tap at `now_ms` counts, for a row armed at `armed_at_ms`.
pub fn confirm_tap(armed_at_ms: i64, now_ms: i64) -> bool {
    now_ms.saturating_sub(armed_at_ms) >= CONFIRM_DELAY_MS
}

/// The one irreversible action waiting for a second tap, for one row only (FR-T14, FR-T15).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Confirm {
    #[default]
    None,
    Remove(String),
    Revoke(String),
}

impl Confirm {
    /// A tap on a guest's 제거: the first arms that row, the second on the same row fires.
    /// Returns the new state and whether to run the action now.
    pub fn tap_remove(&self, guest_id: &str, armed_at_ms: i64, now_ms: i64) -> (Confirm, bool) {
        if *self == Confirm::Remove(guest_id.to_string()) {
            if !confirm_tap(armed_at_ms, now_ms) {
                return (self.clone(), false);
            }
            (Confirm::None, true)
        } else {
            (Confirm::Remove(guest_id.to_string()), false)
        }
    }

    /// A tap on an invite's 취소, the same way.
    pub fn tap_revoke(&self, invite_id: &str, armed_at_ms: i64, now_ms: i64) -> (Confirm, bool) {
        if *self == Confirm::Revoke(invite_id.to_string()) {
            if !confirm_tap(armed_at_ms, now_ms) {
                return (self.clone(), false);
            }
            (Confirm::None, true)
        } else {
            (Confirm::Revoke(invite_id.to_string()), false)
        }
    }

    pub fn is_remove(&self, guest_id: &str) -> bool {
        *self == Confirm::Remove(guest_id.to_string())
    }

    pub fn is_revoke(&self, invite_id: &str) -> bool {
        *self == Confirm::Revoke(invite_id.to_string())
    }
}

/// What the sheet drops when it closes or the room changes: the link (shown once), the confirm
/// and the armed create. `true` means clear.
pub fn clears_sheet_state(open: bool, room_changed: bool) -> bool {
    !open || room_changed
}

/// True for an address that does not reach from outside: loopback or unspecified.
pub fn listen_unreachable(listen: Option<&str>) -> bool {
    let Some(l) = listen else { return false };
    let host = l.trim().rsplit_once(':').map(|(h, _)| h).unwrap_or(l.trim());
    let host = host.trim_matches(|c| c == '[' || c == ']');
    matches!(host, "127.0.0.1" | "0.0.0.0" | "::" | "::1" | "localhost") || host.starts_with("127.")
}

/// The door as last reported by the server, before a link exists (kept after the sheet closes:
/// it holds no secret). A loopback or unspecified listen address asks for a reach check.
pub fn known_door(door: Option<&GuestDoorState>) -> DoorStatus {
    match door {
        None => DoorStatus::Unknown,
        Some(d) if !d.is_on() => DoorStatus::Off,
        Some(d) => DoorStatus::Open { warning: d.public_url.is_none() && listen_unreachable(d.listen.as_deref()) },
    }
}

/// The label of the create button: a door known to be off needs a deliberate second tap.
pub fn create_label(door: DoorStatus, armed: bool) -> &'static str {
    match (door, armed) {
        (DoorStatus::Off, true) => "그래도 초대 만들기",
        (DoorStatus::Off, false) => "초대 링크 만들기 (문이 꺼져 있음)",
        _ => "초대 링크 만들기",
    }
}

/// Whether a tap on create runs now: not while the door is known off and not yet armed.
pub fn create_runs(door: DoorStatus, armed: bool) -> bool {
    !matches!(door, DoorStatus::Off) || armed
}

/// One invite row of the sheet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InviteRow {
    pub id: String,
    pub state: InviteState,
    /// The guest's name once the invite is used.
    pub guest_name: Option<String>,
    /// Time left, for a pending invite only.
    pub remaining: Option<String>,
    pub can_revoke: bool,
}

/// Pending invites first (soonest to expire first), then the rest newest first.
pub fn invite_rows(invites: &[Invite], now_ms: i64) -> Vec<InviteRow> {
    let mut v: Vec<(&Invite, InviteState)> = invites.iter().map(|i| (i, invite_state(i, now_ms))).collect();
    v.sort_by_key(|(i, s)| match s {
        InviteState::Pending => (0, i.expires_at),
        _ => (1, -i.created_at),
    });
    v.into_iter()
        .map(|(i, s)| InviteRow {
            id: i.id.clone(),
            state: s,
            guest_name: i.guest_name.clone().filter(|n| !n.is_empty()),
            remaining: (s == InviteState::Pending).then(|| remaining_text(now_ms, i.expires_at)).flatten(),
            can_revoke: s == InviteState::Pending,
        })
        .collect()
}

/// One guest row of the sheet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestRow {
    pub id: String,
    pub name: String,
    pub badge: Option<String>,
    pub can_clear: bool,
}

/// The usable guests: revoked and expired ones are not listed (the owner cannot act on them).
pub fn guest_rows(guests: &[Guest], now_ms: i64) -> Vec<GuestRow> {
    guests
        .iter()
        .filter(|g| g.revoked_at.is_none() && g.expires_at > now_ms)
        .map(|g| {
            let badge = allowance_badge(g.allow_until, now_ms);
            GuestRow { id: g.id.clone(), name: g.name.clone(), can_clear: badge.is_some(), badge }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_000_000_000_000;
    const H: i64 = 3_600_000;

    fn invite(expires: i64, used: Option<i64>, revoked: Option<i64>) -> Invite {
        Invite {
            id: "i".into(),
            channel_id: "c".into(),
            created_by: "user".into(),
            created_at: NOW - H,
            expires_at: expires,
            used_at: used,
            revoked_at: revoked,
            guest_name: None,
        }
    }

    fn guest(allow: Option<i64>, revoked: Option<i64>, expires: i64) -> Guest {
        Guest { id: "g".into(), name: "민수".into(), created_at: NOW - H, expires_at: expires, revoked_at: revoked, allow_until: allow }
    }

    fn door(state: &str) -> GuestDoorState {
        GuestDoorState { state: state.into(), listen: None, public_url: None, link_warning: false }
    }

    #[test]
    fn fr_t14_invite_state_comes_from_the_times_and_revoked_wins() {
        assert_eq!(invite_state(&invite(NOW + H, None, None), NOW), InviteState::Pending);
        assert_eq!(invite_state(&invite(NOW + H, Some(NOW - 1), None), NOW), InviteState::Used);
        assert_eq!(invite_state(&invite(NOW - 1, None, None), NOW), InviteState::Expired);
        assert_eq!(invite_state(&invite(NOW, None, None), NOW), InviteState::Expired);
        assert_eq!(invite_state(&invite(NOW + H, Some(NOW - 5), Some(NOW - 1)), NOW), InviteState::Revoked);
        // Used before it ran out stays used, not expired.
        assert_eq!(invite_state(&invite(NOW - H, Some(NOW - 2 * H), None), NOW), InviteState::Used);
    }

    #[test]
    fn fr_t14_only_a_pending_invite_can_be_revoked_and_shows_time_left() {
        let rows = invite_rows(&[invite(NOW + 2 * H, None, None), invite(NOW - 1, None, None)], NOW);
        assert_eq!(rows[0].state, InviteState::Pending);
        assert!(rows[0].can_revoke);
        assert_eq!(rows[0].remaining.as_deref(), Some("2시간 남음"));
        assert!(!rows[1].can_revoke);
        assert_eq!(rows[1].remaining, None);
    }

    #[test]
    fn fr_t14_remaining_text_is_never_invented() {
        assert_eq!(remaining_text(NOW, NOW), None);
        assert_eq!(remaining_text(NOW, NOW - 5), None);
        assert_eq!(remaining_text(NOW, NOW + 1_000).as_deref(), Some("1분 미만 남음"));
        assert_eq!(remaining_text(NOW, NOW + 5 * 60_000).as_deref(), Some("5분 남음"));
        assert_eq!(remaining_text(NOW, NOW + H + 30 * 60_000).as_deref(), Some("1시간 30분 남음"));
        assert_eq!(remaining_text(NOW, NOW + 3 * 24 * H).as_deref(), Some("3일 남음"));
        assert_eq!(remaining_text(NOW, NOW + 25 * H).as_deref(), Some("1일 1시간 남음"));
    }

    #[test]
    fn fr_t14_the_door_is_unknown_before_any_invite() {
        let s = door_status(None, None, false);
        assert_eq!(s, DoorStatus::Unknown);
        assert_eq!(s.text(), "게스트 문 상태: 아직 알 수 없습니다");
        assert!(!s.can_copy());
    }

    #[test]
    fn fr_t14_a_door_that_is_off_or_a_relative_link_offers_no_copy() {
        let off = door_status(Some(&door("off")), Some("http://h:1/join#t"), false);
        assert_eq!(off, DoorStatus::Off);
        assert_eq!(off.text(), "게스트 문이 꺼져 있습니다. 링크는 문이 켜져야 쓸 수 있습니다");
        assert!(!off.can_copy());
        let relative = door_status(None, Some("/join#t"), false);
        assert_eq!(relative, DoorStatus::Off);
        // A relative link is useless even when the door says on.
        assert_eq!(door_status(Some(&door("on")), Some("/join#t"), false), DoorStatus::Off);
    }

    #[test]
    fn fr_t14_an_open_door_copies_and_a_warning_asks_to_check_reach() {
        let ok = door_status(Some(&door("on")), Some("https://x.example/join#t"), false);
        assert!(ok.can_copy() && ok.warning().is_none());
        let warn = door_status(Some(&door("on")), Some("http://10.0.0.2:9/join#t"), true);
        assert!(warn.can_copy());
        assert_eq!(warn.warning(), Some("이 주소가 밖에서 닿는지 확인하십시오"));
    }

    #[test]
    fn fr_t15_a_guest_author_never_shows_the_id() {
        assert_eq!(guest_author_label("guest:g9", "민수", "guest").as_deref(), Some("민수"));
        assert_eq!(guest_author_label("guest:g9", "", "").as_deref(), Some("게스트"));
        assert_eq!(guest_author_label("guest:g9", "guest:g9", "guest").as_deref(), Some("게스트"));
        // The kind alone marks a guest even when the sender is already the name.
        assert_eq!(guest_author_label("guest", "소라", "guest").as_deref(), Some("소라"));
        assert_eq!(guest_author_label("user", "나", "person"), None);
        assert_eq!(guest_author_label("a1", "ember-리더", "agent"), None);
    }

    #[test]
    fn fr_t17_hours_are_whole_and_inside_1_to_168() {
        assert!(!hours_valid(0));
        assert!(hours_valid(1) && hours_valid(168));
        assert!(!hours_valid(169));
        assert_eq!(parse_hours("24"), Some(24));
        assert_eq!(parse_hours(" 168 "), Some(168));
        for bad in ["", "0", "169", "1.5", "-3", "+3", "열", "1e2"] {
            assert_eq!(parse_hours(bad), None, "{bad}");
        }
        assert!(ALLOW_PRESETS.iter().all(|h| hours_valid(*h)));
        assert!(TTL_CHOICES.iter().all(|h| hours_valid(*h)) && TTL_CHOICES.contains(&DEFAULT_TTL_HOURS));
    }

    #[test]
    fn fr_t17_the_badge_shows_only_while_the_allowance_is_in_the_future() {
        assert_eq!(allowance_badge(None, NOW), None);
        assert_eq!(allowance_badge(Some(NOW - 1), NOW), None);
        assert_eq!(allowance_badge(Some(NOW), NOW), None);
        assert_eq!(allowance_badge(Some(NOW + 3 * H), NOW).as_deref(), Some("허용 중 · 3시간 남음"));
    }

    #[test]
    fn fr_t17_guest_rows_list_usable_guests_with_the_badge_and_a_clear_only_when_allowed() {
        let rows = guest_rows(
            &[
                guest(Some(NOW + H), None, NOW + 9 * H),
                guest(None, None, NOW + 9 * H),
                guest(Some(NOW + H), Some(NOW - 1), NOW + 9 * H),
                guest(None, None, NOW - 1),
            ],
            NOW,
        );
        assert_eq!(rows.len(), 2);
        assert!(rows[0].can_clear && rows[0].badge.is_some());
        assert!(!rows[1].can_clear && rows[1].badge.is_none());
    }

    #[test]
    fn fr_t17_errors_are_fixed_korean_sentences_and_capped_is_told() {
        assert!(error_message(Some("invalid_hours")).contains("168"));
        assert!(error_message(Some("not_found")).contains("찾을 수 없습니다"));
        assert_eq!(error_message(Some("boom raw text")), error_message(None));
        assert!(!error_message(Some("boom raw text")).contains("boom"));
        assert!(capped_note(true).is_some());
        assert_eq!(capped_note(false), None);
    }

    #[test]
    fn fr_t14_remove_needs_a_second_tap() {
        let c = Confirm::None;
        let (c, fire) = c.tap_remove("g1", 0, 0);
        assert!(!fire && c.is_remove("g1"));
        let (c, fire) = c.tap_remove("g1", 0, 600);
        assert!(fire && c == Confirm::None);
        let (c, fire) = Confirm::None.tap_revoke("i1", 0, 0);
        assert!(!fire && c.is_revoke("i1"));
        assert!(c.tap_revoke("i1", 0, 700).1);
    }

    #[test]
    fn fr_t14_a_fast_second_tap_does_not_skip_the_confirmation() {
        assert!(!confirm_tap(1_000, 1_000));
        assert!(!confirm_tap(1_000, 1_599));
        let (c, _) = Confirm::None.tap_remove("g1", 0, 1_000);
        let (c, fire) = c.tap_remove("g1", 1_000, 1_300);
        assert!(!fire && c.is_remove("g1"), "still armed, nothing fired");
        let (c, fire) = Confirm::None.tap_revoke("i1", 0, 1_000).0.tap_revoke("i1", 1_000, 1_100);
        assert!(!fire && c.is_revoke("i1"));
    }

    #[test]
    fn fr_t14_a_tap_after_600_ms_confirms() {
        assert!(confirm_tap(1_000, 1_600));
        assert!(confirm_tap(1_000, 5_000));
        let (c, _) = Confirm::None.tap_remove("g1", 0, 1_000);
        let (c, fire) = c.tap_remove("g1", 1_000, 1_600);
        assert!(fire && c == Confirm::None);
    }

    #[test]
    fn fr_t17_hours_are_clamped_with_a_note() {
        assert_eq!(clamp_hours("24"), Some((24, false)));
        assert_eq!(clamp_hours(" 168 "), Some((168, false)));
        assert_eq!(clamp_hours("0"), Some((1, true)));
        assert_eq!(clamp_hours("169"), Some((168, true)));
        assert_eq!(clamp_hours("99999999999999999999999"), Some((168, true)));
        for bad in ["", " ", "1.5", "-3", "+3", "abc", "2h"] {
            assert_eq!(clamp_hours(bad), None, "{bad}");
        }
        assert_eq!(CLAMPED_NOTE, "1에서 168 사이로 맞췄습니다");
    }

    #[test]
    fn fr_t14_confirm_resets_when_another_row_is_tapped() {
        let (c, _) = Confirm::None.tap_remove("g1", 0, 0);
        let (c, fire) = c.tap_remove("g2", 0, 10);
        assert!(!fire && c.is_remove("g2") && !c.is_remove("g1"));
        let (c, fire) = c.tap_revoke("i1", 0, 20);
        assert!(!fire && c.is_revoke("i1") && !c.is_remove("g2"));
    }

    #[test]
    fn fr_t14_link_is_cleared_on_close_and_room_switch() {
        assert!(clears_sheet_state(false, false));
        assert!(clears_sheet_state(true, true));
        assert!(!clears_sheet_state(true, false));
    }

    #[test]
    fn fr_t14_a_known_off_door_needs_a_second_tap_and_loopback_warns() {
        assert!(!create_runs(DoorStatus::Off, false));
        assert!(create_runs(DoorStatus::Off, true));
        assert!(create_runs(DoorStatus::Unknown, false));
        assert_eq!(create_label(DoorStatus::Off, true), "그래도 초대 만들기");
        assert_eq!(known_door(None), DoorStatus::Unknown);
        assert_eq!(known_door(Some(&door("off"))), DoorStatus::Off);
        let lo = GuestDoorState { state: "on".into(), listen: Some("127.0.0.1:7071".into()), public_url: None, link_warning: false };
        assert_eq!(known_door(Some(&lo)), DoorStatus::Open { warning: true });
        let un = GuestDoorState { state: "on".into(), listen: Some("[::]:7071".into()), public_url: None, link_warning: false };
        assert_eq!(known_door(Some(&un)), DoorStatus::Open { warning: true });
        let ok = GuestDoorState { state: "on".into(), listen: Some("10.0.0.2:7071".into()), public_url: None, link_warning: false };
        assert_eq!(known_door(Some(&ok)), DoorStatus::Open { warning: false });
    }
}
