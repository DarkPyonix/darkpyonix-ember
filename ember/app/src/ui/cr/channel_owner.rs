//! compose-rust sheet for the channel owner: 멤버/게스트 (FR-T14 invites, FR-T15 guests, FR-T17
//! allowances; opened from the room header's members button, FR-L12).
//!
//! The decisions (invite state, remaining time, badge, hour range, door status, error sentences)
//! are `ui::channel_owner`'s plain functions. This file reads and writes through the same
//! services bridge the channels screen uses (`run_shared`): the sheet fetches invites and guests
//! when it opens and again after every action. The link of a new invite is held only in the
//! sheet's state and is gone when the sheet closes, because the server shows it once.

#![allow(non_snake_case)]

use compose_rust::runtime::{disposable_effect, launched_effect, on_dispose, mutable_state_of, remember_with_key, MutableState};
use compose_rust::ui as cu;
use compose_rust::ui::Modifier;
use compose_rust::{Alignment, SpaceRole};
use ember_client::channels::{CreatedInvite, Guest, GuestDoorState, Invite};

use super::components::{
    AppButton, ButtonKind, EmberField, EmberSheet, FieldOptions, FilterChip, Label, Pill, PillTone, SheetFooter,
};
use super::home::text_px;
use super::notice;
use super::project::wrap_lines;
use super::theme::*;
use crate::ui::channel_owner::{
    capped_note, clears_sheet_state, create_label, create_runs, door_status, error_message, guest_rows, invite_rows,
    known_door, merge_door, clamp_hours, DOOR_READ_FAILED_NOTE, Confirm, DoorStatus, CLAMPED_NOTE,
    InviteState, ALLOW_PRESETS, DEFAULT_TTL_HOURS, MAX_HOURS, MIN_HOURS, TTL_CHOICES,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OwnerLoad {
    Idle,
    Loading,
    Ready,
    Failed,
}

/// Everything the sheet holds for one room.
#[derive(Clone)]
pub struct OwnerState {
    pub open: MutableState<bool>,
    ttl: MutableState<u32>,
    created: MutableState<Option<CreatedInvite>>,
    invites: MutableState<Vec<Invite>>,
    guests: MutableState<Vec<Guest>>,
    load: MutableState<OwnerLoad>,
    /// The hours the next allowance gets: a preset, or the valid custom value.
    hours: MutableState<u32>,
    custom: MutableState<String>,
    /// Bumped to draw a fresh custom field.
    custom_gen: MutableState<u64>,
    /// An action is in flight: the buttons wait, so one tap is one request.
    busy: MutableState<bool>,
    /// The last action's outcome sentence (a fixed text, never the server's).
    note: MutableState<Option<String>>,
    /// The row waiting for a second tap (remove or revoke).
    confirm: MutableState<Confirm>,
    /// When the waiting row was armed (Unix ms): a second tap counts 600 ms later.
    armed_at: MutableState<i64>,
    /// The door as the server last said, kept after close (it holds no secret).
    door: MutableState<Option<GuestDoorState>>,
    /// The last door read failed and no door state is known.
    door_failed: MutableState<bool>,
    /// Create was tapped once while the door is known off.
    create_armed: MutableState<bool>,
    /// Review-window sample data is shown: nothing is fetched and no action reaches a server.
    sample: bool,
}

/// What a review window shows in the owner sheet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewSample {
    pub invites: Vec<Invite>,
    pub guests: Vec<Guest>,
    pub created: Option<CreatedInvite>,
    pub note: Option<String>,
}

/// SAMPLE DATA FOR REVIEW WINDOWS ONLY, compiled in debug builds only (release has no sample guests). Built relative to `now_ms` so the remaining times read
/// plausibly. `door` is `off`, `warn`, `on` or anything else for "unknown".
#[cfg(debug_assertions)]
pub fn review_sample_data(now_ms: i64, door: &str) -> ReviewSample {
    const H: i64 = 3_600_000;
    let invite = |id: &str, expires: i64, used: Option<i64>, revoked: Option<i64>, name: Option<&str>| Invite {
        id: id.into(),
        channel_id: "sample".into(),
        created_by: "user".into(),
        created_at: now_ms - 4 * H,
        expires_at: expires,
        used_at: used,
        revoked_at: revoked,
        guest_name: name.map(str::to_string),
    };
    let guest = |id: &str, name: &str, allow: Option<i64>| Guest {
        id: id.into(),
        name: name.into(),
        created_at: now_ms - 6 * H,
        expires_at: now_ms + 48 * H,
        revoked_at: None,
        allow_until: allow,
    };
    let link = "https://ember.example/join#sample-token";
    let (door_state, warning, shown) = match door {
        "off" => (Some("off"), false, true),
        "warn" => (Some("on"), true, true),
        "on" => (Some("on"), false, true),
        _ => (None, false, false),
    };
    let created = shown.then(|| CreatedInvite {
        id: "inv-new".into(),
        token: "sample-token".into(),
        link: link.into(),
        expires_at: now_ms + 24 * H,
        link_warning: warning,
        guest_door: door_state.map(|s| GuestDoorState { state: s.into(), listen: None, public_url: None, link_warning: false }),
    });
    ReviewSample {
        invites: vec![
            invite("inv-pending", now_ms + 20 * H, None, None, None),
            invite("inv-used", now_ms + 10 * H, Some(now_ms - 2 * H), None, Some("민수")),
            invite("inv-expired", now_ms - H, None, None, None),
            invite("inv-revoked", now_ms + 5 * H, None, Some(now_ms - 3 * H), None),
        ],
        guests: vec![
            guest("g-allowed", "민수", Some(now_ms + 3 * H)),
            guest("g-plain", "소라", None),
            guest("g-capped", "지우", Some(now_ms + 48 * H)),
        ],
        created,
        note: capped_note(true).map(str::to_string),
    }
}

/// The sample, only when the review window asked for it: `EMBER_CR_ROUTE=channels` and
/// `EMBER_CR_OWNER_SHEET=1`. The inputs are passed in so the gate is testable.
#[cfg(debug_assertions)]
pub fn review_sample_from(route: Option<&str>, sheet: Option<&str>, door: Option<&str>, now_ms: i64) -> Option<ReviewSample> {
    (route == Some("channels") && sheet == Some("1")).then(|| review_sample_data(now_ms, door.unwrap_or("unknown")))
}

#[cfg(not(debug_assertions))]
fn review_sample() -> Option<ReviewSample> {
    None
}

#[cfg(debug_assertions)]
fn review_sample() -> Option<ReviewSample> {
    let get = |k: &str| std::env::var(k).ok();
    review_sample_from(
        get("EMBER_CR_ROUTE").as_deref(),
        get("EMBER_CR_OWNER_SHEET").as_deref(),
        get("EMBER_CR_OWNER_DOOR").as_deref(),
        crate::model::now_ms(),
    )
}

impl OwnerState {
    pub fn new(room_id: &str) -> OwnerState {
        remember_with_key(room_id.to_string(), || {
            let sample = review_sample();
            OwnerState {
            sample: sample.is_some(),
            open: mutable_state_of(sample.is_some()),
            ttl: mutable_state_of(DEFAULT_TTL_HOURS),
            created: mutable_state_of(sample.as_ref().and_then(|s| s.created.clone())),
            invites: mutable_state_of(sample.as_ref().map(|s| s.invites.clone()).unwrap_or_default()),
            guests: mutable_state_of(sample.as_ref().map(|s| s.guests.clone()).unwrap_or_default()),
            load: mutable_state_of(if sample.is_some() { OwnerLoad::Ready } else { OwnerLoad::Idle }),
            hours: mutable_state_of(DEFAULT_TTL_HOURS),
            custom: mutable_state_of(String::new()),
            custom_gen: mutable_state_of(0),
            busy: mutable_state_of(false),
            confirm: mutable_state_of(Confirm::None),
            armed_at: mutable_state_of(0),
            door: mutable_state_of(sample.as_ref().and_then(|s| s.created.as_ref().and_then(|c| c.guest_door.clone()))),
            create_armed: mutable_state_of(false),
            door_failed: mutable_state_of(false),
            note: mutable_state_of(sample.and_then(|s| s.note)),
            }
        })
    }

    fn close(&self) {
        self.open.set(false);
        self.reset();
    }

    /// Drops what must not outlive the sheet: the link, the confirm, the armed create, the
    /// custom hours text (the field is redrawn empty).
    fn reset(&self) {
        if self.sample {
            return;
        }
        self.created.set(None);
        self.note.set(None);
        self.confirm.set(Confirm::None);
        self.armed_at.set(0);
        self.create_armed.set(false);
        self.custom.set(String::new());
        self.custom_gen.set(self.custom_gen.get_untracked() + 1);
    }

    /// Reads invites and guests; a failure keeps what is shown and says so.
    fn refresh(&self, channel: &str) {
        if self.load.get_untracked() != OwnerLoad::Ready {
            self.load.set(OwnerLoad::Loading);
        }
        let (st, c) = (self.clone(), channel.to_string());
        crate::services::run_shared(
            async move {
                let i = crate::services::services().client.api().invites(&c).await;
                let g = crate::services::services().client.api().guests(&c).await;
                (i, g)
            },
            move |(i, g)| match (i, g) {
                (Ok(i), Ok(g)) => {
                    st.invites.set(i);
                    st.guests.set(g);
                    st.load.set(OwnerLoad::Ready);
                }
                (a, b) => {
                    let code = a.err().or(b.err()).map(|e| e.code().map(str::to_string));
                    tracing::warn!("owner lists failed: {code:?}");
                    st.load.set(OwnerLoad::Failed);
                }
            },
        );
    }

    /// Reads the guest door once when the sheet opens, so its state is known before any invite.
    /// A failed read keeps what was known. Review windows keep their sample door.
    fn read_door(&self) {
        if self.sample {
            return;
        }
        let st = self.clone();
        crate::services::run_shared(
            async move { crate::services::services().client.api().guest_door().await },
            move |r| {
                let fresh = match r {
                    Ok(d) => Some(d),
                    Err(e) => {
                        tracing::warn!("guest door read failed: {:?}", e.code());
                        None
                    }
                };
                let merged = merge_door(st.door.get_untracked(), fresh.clone());
                st.door_failed.set(fresh.is_none() && merged.is_none());
                st.door.set(merged);
            },
        );
    }

    /// Runs one owner request. On success `ok` runs and the lists are read again; on failure the
    /// fixed sentence of the error code is shown.
    fn act<T: Send + 'static>(
        &self,
        channel: &str,
        work: impl std::future::Future<Output = Result<T, ember_client::api::ApiError>> + Send + 'static,
        ok: impl FnOnce(&OwnerState, T) + Send + 'static,
    ) {
        if self.sample || self.busy.get_untracked() {
            return;
        }
        self.busy.set(true);
        self.note.set(None);
        let (st, c) = (self.clone(), channel.to_string());
        crate::services::run_shared(work, move |r| {
            st.busy.set(false);
            match r {
                Ok(v) => {
                    ok(&st, v);
                    st.refresh(&c);
                }
                Err(e) => {
                    tracing::warn!("owner action failed: {e}");
                    let text = error_message(e.code()).to_string();
                    st.note.set(Some(text.clone()));
                    notice::post(text);
                }
            }
        });
    }

    fn create_invite(&self, channel: &str) {
        let (c, ttl) = (channel.to_string(), self.ttl.get_untracked());
        self.act(
            channel,
            async move { crate::services::services().client.api().create_invite(&c, Some(ttl)).await },
            |st, made| {
                st.door.set(merge_door(st.door.get_untracked(), made.guest_door.clone()));
                st.door_failed.set(false);
                st.create_armed.set(false);
                st.created.set(Some(made));
            },
        );
    }

    fn revoke(&self, channel: &str, invite: &str) {
        let (c, i) = (channel.to_string(), invite.to_string());
        self.act(channel, async move { crate::services::services().client.api().revoke_invite(&c, &i).await }, |st, _| {
            st.note.set(Some("초대를 취소했습니다".to_string()))
        });
    }

    fn remove(&self, channel: &str, guest: &str) {
        let (c, g) = (channel.to_string(), guest.to_string());
        self.act(channel, async move { crate::services::services().client.api().remove_guest(&c, &g).await }, |st, _| {
            st.note.set(Some("게스트를 제거했습니다".to_string()))
        });
    }

    fn allow(&self, channel: &str, guest: &str, hours: u32) {
        let (c, g) = (channel.to_string(), guest.to_string());
        self.act(
            channel,
            async move { crate::services::services().client.api().set_allowance(&c, &g, hours).await },
            |st, a| {
                st.note.set(Some(capped_note(a.capped).unwrap_or("허용했습니다").to_string()));
            },
        );
    }

    fn clear(&self, channel: &str, guest: &str) {
        let (c, g) = (channel.to_string(), guest.to_string());
        self.act(channel, async move { crate::services::services().client.api().clear_allowance(&c, &g).await }, |st, _| {
            st.note.set(Some("허용을 해제했습니다".to_string()))
        });
    }
}

/// The sheet. `live` is false in demo and review windows: there is no server to ask, so nothing
/// is fetched and the sheet says so.
pub fn MembersSheet(channel: &str, channel_name: &str, st: &OwnerState, live: bool) {
    // Review sample data stands in for a server; it is never fetched over.
    let live = live || st.sample;
    let fetch = live && !st.sample;
    // Entering a room starts with the sheet shut and nothing left from an earlier visit.
    {
        let st2 = st.clone();
        launched_effect(format!("enter:{channel}"), move || async move {
            if clears_sheet_state(st2.open.get_untracked(), true) && !st2.sample {
                st2.open.set(false);
                st2.reset();
            }
        });
    }
    // Leaving the room (back, or another room) drops the link and the confirm too: the cleanup
    // runs when this call site leaves the composition or the room changes.
    {
        let st2 = st.clone();
        disposable_effect(channel.to_string(), move || {
            on_dispose(move || {
                if !st2.sample {
                    st2.open.set(false);
                    st2.reset();
                }
            })
        });
    }
    // Fetch when the sheet opens (the key changes with the open flag).
    {
        let (st2, c, open) = (st.clone(), channel.to_string(), st.open.get());
        launched_effect(format!("{channel}:{open}"), move || async move {
            if clears_sheet_state(open, false) {
                st2.reset();
            }
            if open && fetch {
                st2.refresh(&c);
                st2.read_door();
            }
        });
    }
    let close = {
        let st = st.clone();
        move || st.close()
    };
    EmberSheet(&format!("{channel_name} 멤버/게스트"), st.open.get(), close.clone(), None, || {
        cu::ScrollColumn().modifier(Modifier::new().fill_max_width().weight(1.0)).content(|| {
            cu::Column().spacing(SP[3]).modifier(Modifier::new().fill_max_width().padding(SP[4])).content(|| {
                if !live {
                    Label("서버에 연결되어 있지 않아 초대와 게스트를 관리할 수 없습니다", Fs::Sm, Weight::Regular, Some(paint(ember_theme().palette.muted)), None);
                    return;
                }
                InviteSection(channel, st);
                InviteList(channel, st);
                GuestList(channel, st);
            });
        });
        SheetFooter(|| {
            AppButton("닫기", None, ButtonKind::Default, true, true, false, close.clone());
        });
    });
}

fn Heading(text: &str) {
    Label(text, Fs::Md, Weight::Semibold, None, Some(1));
}

/// Width of one hour chip: the 12 px label between two `SP[2]` paddings. `text_px` runs short of
/// the rendered Korean glyphs (the 390 dp capture clipped `168시간`), so the label gets 30 percent.
fn hour_chip_width(h: u32) -> f32 {
    2.0 * SP[2] + 1.3 * text_px(&format!("{h}시간"), Fs::Sm.size())
}

/// Rows the hour chips pack into, as indices of `choices`, in `avail` dp (chips wrap, never clip).
pub fn hour_chip_rows(choices: &[u32], avail: f32) -> Vec<Vec<usize>> {
    let widths: Vec<f32> = choices.iter().map(|h| hour_chip_width(*h)).collect();
    wrap_lines(&widths, SP[0], avail)
}

/// The room the chips have: the sheet's width (bottom sheet: the window) less its padding.
fn chip_avail() -> f32 {
    let w = compose_rust::runtime::current_window_size().width_dp;
    let sheet = if w > 0.0 { w.min(760.0) } else { 760.0 };
    sheet - 2.0 * SP[4]
}

fn HourChips(choices: &[u32], current: u32, on_pick: impl Fn(u32) + Clone + 'static) {
    let rows = hour_chip_rows(choices, chip_avail());
    cu::Column().spacing(SP[0]).modifier(Modifier::new().fill_max_width()).content(|| {
        for row in rows {
            cu::Row().spacing(SP[0]).alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width()).content(|| {
                for i in row {
                    let h = choices[i];
                    let pick = on_pick.clone();
                    FilterChip(&format!("{h}시간"), h == current, move || pick(h));
                }
            });
        }
    });
}

fn InviteSection(channel: &str, st: &OwnerState) {
    let p = ember_theme().palette;
    Heading("초대 만들기");
    Label("링크는 한 번만 보이며, 한 사람이 한 번만 쓸 수 있습니다", Fs::Sm, Weight::Regular, Some(paint(p.muted)), None);
    let current = st.ttl.get();
    let ttl = st.ttl.clone();
    let st_c = st.clone();
    HourChips(&TTL_CHOICES, current, move |h| {
        st_c.confirm.set(Confirm::None);
        ttl.set(h)
    });
    let (s, c) = (st.clone(), channel.to_string());
    // The door: unknown until an invite has been made, then what the server last said.
    let created = st.created.get();
    let status = match &created {
        Some(c) => door_status(c.guest_door.as_ref(), Some(&c.link), c.link_warning),
        None => known_door(st.door.get().as_ref()),
    };
    // A door known to be off makes a dead invite: the owner taps once more, knowingly.
    let armed = st.create_armed.get();
    AppButton(create_label(status, armed), None, ButtonKind::Primary, true, !st.busy.get(), false, move || {
        if create_runs(status, armed) {
            s.create_invite(&c);
        } else {
            s.create_armed.set(true);
        }
    });
    let tone = if matches!(status, DoorStatus::Off) { p.red.strong } else { p.text_2 };
    Label(status.text(), Fs::Sm, Weight::Regular, Some(paint(tone)), None);
    if created.is_none() && st.door_failed.get() && matches!(status, DoorStatus::Unknown) {
        Label(DOOR_READ_FAILED_NOTE, Fs::Xs, Weight::Regular, Some(paint(p.muted)), None);
    }
    if let Some(w) = status.warning() {
        Label(w, Fs::Sm, Weight::Regular, Some(paint(p.orange.strong)), None);
        Label("127.0.0.1 같은 내 컴퓨터 주소나 0.0.0.0 주소는 밖에서 닿지 않습니다", Fs::Xs, Weight::Regular, Some(paint(p.muted)), None);
    }
    if let Some(c) = created.filter(|_| status.can_copy()) {
        // TODO(compose-rust#471): no clipboard facility in the app or the toolkit, so the link is
        // shown as text to select and the 복사 button waits.
        // A read-only look: the field starts with the link, enabled so it can be selected and
        // copied with Cmd+C / Ctrl+C. The toolkit has no read_only flag, so typing into it changes
        // only what is drawn, never the link.
        // TODO(compose-rust#471): replace with a 복사 button once the clipboard can be written.
        EmberField(FieldOptions { fill_max_width: true, text: Some(c.link.clone()), ..Default::default() });
        Label("이 링크는 지금만 보입니다. 닫으면 다시 볼 수 없습니다", Fs::Xs, Weight::Regular, Some(paint(p.muted)), None);
    }
    if let Some(n) = st.note.get() {
        Label(&n, Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
    }
}

fn InviteList(channel: &str, st: &OwnerState) {
    let p = ember_theme().palette;
    Heading("초대");
    match st.load.get() {
        OwnerLoad::Loading | OwnerLoad::Idle => {
            Label("불러오는 중입니다", Fs::Sm, Weight::Regular, Some(paint(p.muted)), None);
            return;
        }
        OwnerLoad::Failed => Label("초대와 게스트를 불러오지 못했습니다", Fs::Sm, Weight::Regular, Some(paint(p.red.strong)), None),
        OwnerLoad::Ready => {}
    }
    let rows = invite_rows(&st.invites.get(), crate::model::now_ms());
    if rows.is_empty() && st.load.get() == OwnerLoad::Ready {
        Label("초대가 없습니다", Fs::Sm, Weight::Regular, Some(paint(p.muted)), None);
    }
    for r in rows {
        cu::Row().space_role(SpaceRole::Sm).alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width()).content(|| {
            let tone = match r.state {
                InviteState::Pending => PillTone::Green,
                InviteState::Used => PillTone::Blue,
                InviteState::Expired | InviteState::Revoked => PillTone::Grey,
            };
            Pill(r.state.label(), tone, true);
            let text = match (&r.guest_name, &r.remaining) {
                (Some(n), _) => n.clone(),
                (None, Some(t)) => t.clone(),
                (None, None) => String::new(),
            };
            Label(&text, Fs::Sm, Weight::Regular, Some(paint(p.text_2)), Some(1));
            cu::Spacer().modifier(Modifier::new().weight(1.0));
            if r.can_revoke {
                let (s, c, id) = (st.clone(), channel.to_string(), r.id.clone());
                let sure = st.confirm.get().is_revoke(&r.id);
                if sure {
                    let s2 = st.clone();
                    AppButton("아니요", None, ButtonKind::Default, true, true, false, move || s2.confirm.set(Confirm::None));
                }
                let label = if sure { "정말 취소합니다" } else { "취소" };
                AppButton(label, None, ButtonKind::Destructive, true, !st.busy.get(), false, move || {
                    let now = crate::model::now_ms();
                    let cur = s.confirm.get_untracked();
                    let (next, fire) = cur.tap_revoke(&id, s.armed_at.get_untracked(), now);
                    if next != cur {
                        s.armed_at.set(now);
                    }
                    s.confirm.set(next);
                    if fire {
                        s.revoke(&c, &id);
                    }
                });
            }
        });
    }
}

fn GuestList(channel: &str, st: &OwnerState) {
    let p = ember_theme().palette;
    Heading("게스트");
    if st.load.get() != OwnerLoad::Ready {
        return;
    }
    // Hours the allowance gets: presets, or a custom whole number within the range.
    Label("허용 시간 (이 시간 동안 게스트의 멘션이 에이전트에 닿습니다)", Fs::Sm, Weight::Regular, Some(paint(p.muted)), None);
    let current = st.hours.get();
    let hours = st.hours.clone();
    let st_c = st.clone();
    HourChips(&ALLOW_PRESETS, current, move |h| {
        // A preset replaces a typed value: the field is redrawn empty (generation bump).
        st_c.confirm.set(Confirm::None);
        st_c.custom.set(String::new());
        st_c.custom_gen.set(st_c.custom_gen.get_untracked() + 1);
        hours.set(h)
    });
    let custom = st.custom.get();
    cu::Row().space_role(SpaceRole::Sm).alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width()).content(|| {
        // Read here so a generation bump redraws the field empty.
        let _gen = st.custom_gen.get();
        // TODO(compose-rust#374): the field is uncontrolled with no initial value.
        let (text, hours) = (st.custom.clone(), st.hours.clone());
        EmberField(FieldOptions {
            weight: Some(1.0),
            placeholder: format!("직접 입력 ({MIN_HOURS}~{MAX_HOURS}시간)"),
            on_value_change: Some(Box::new(move |v| {
                if let Some((h, _)) = clamp_hours(&v) {
                    hours.set(h);
                }
                text.set(v);
            })),
            ..Default::default()
        });
    });
    match clamp_hours(&custom) {
        None if !custom.trim().is_empty() => {
            Label(error_message(Some("invalid_hours")), Fs::Sm, Weight::Regular, Some(paint(p.red.strong)), None);
        }
        Some((_, true)) => Label(CLAMPED_NOTE, Fs::Sm, Weight::Regular, Some(paint(p.orange.strong)), None),
        _ => {}
    }
    // A custom value that is not valid leaves the chosen hours as they were; say which they are.
    let hours = st.hours.get();
    Label(&format!("선택한 허용 시간: {hours}시간"), Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);

    let rows = guest_rows(&st.guests.get(), crate::model::now_ms());
    if rows.is_empty() {
        Label("게스트가 없습니다", Fs::Sm, Weight::Regular, Some(paint(p.muted)), None);
    }
    let custom_ok = custom.trim().is_empty() || clamp_hours(&custom).is_some();
    for g in rows {
        cu::Column().spacing(SP[0]).modifier(Modifier::new().fill_max_width()).content(|| {
            cu::Row().space_role(SpaceRole::Sm).alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width()).content(|| {
                Label(&g.name, Fs::Md, Weight::Medium, None, Some(1));
                Pill("게스트", PillTone::Grey, true);
                // Hidden unless an allowance is in force now.
                if let Some(b) = &g.badge {
                    Pill(b, PillTone::Green, true);
                }
                cu::Spacer().modifier(Modifier::new().weight(1.0));
            });
            cu::Row().space_role(SpaceRole::Sm).alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width()).content(|| {
                let (s, c, id) = (st.clone(), channel.to_string(), g.id.clone());
                AppButton(&format!("{hours}시간 허용"), None, ButtonKind::Default, true, !st.busy.get() && custom_ok, false, move || {
                    s.confirm.set(Confirm::None);
                    s.allow(&c, &id, hours)
                });
                if g.can_clear {
                    let (s, c, id) = (st.clone(), channel.to_string(), g.id.clone());
                    AppButton("허용 해제", None, ButtonKind::Default, true, !st.busy.get(), false, move || {
                        s.confirm.set(Confirm::None);
                        s.clear(&c, &id)
                    });
                }
            });
            // Remove sits on its own row, away from the allowance buttons.
            cu::Row().space_role(SpaceRole::Sm).alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width()).content(|| {
                cu::Spacer().modifier(Modifier::new().weight(1.0));
                let sure = st.confirm.get().is_remove(&g.id);
                if sure {
                    let s2 = st.clone();
                    AppButton("아니요", None, ButtonKind::Default, true, true, false, move || s2.confirm.set(Confirm::None));
                }
                let (s, c, id) = (st.clone(), channel.to_string(), g.id.clone());
                let label = if sure { "정말 제거합니다" } else { "제거" };
                AppButton(label, None, ButtonKind::Destructive, true, !st.busy.get(), false, move || {
                    let now = crate::model::now_ms();
                    let cur = s.confirm.get_untracked();
                    let (next, fire) = cur.tap_remove(&id, s.armed_at.get_untracked(), now);
                    if next != cur {
                        s.armed_at.set(now);
                    }
                    s.confirm.set(next);
                    if fire {
                        s.remove(&c, &id);
                    }
                });
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use compose_rust::{Host, Recomposer};

    fn text_of(content: fn()) -> String {
        let mut host = Host::new(move || Box::new(Recomposer::new(content)));
        let batch = compose_rust::protocol::decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        batch.iter().map(|m| format!("{m:?}")).collect()
    }

    fn open_sheet() {
        let st = OwnerState::new("c1");
        st.open.set(true);
        MembersSheet("c1", "전체", &st, false);
    }

    #[test]
    fn fr_t14_allowance_chips_wrap_to_a_second_row_at_390() {
        let avail = 390.0 - 2.0 * SP[4];
        let rows = hour_chip_rows(&ALLOW_PRESETS, avail);
        assert!(rows.len() >= 2, "{rows:?}");
        assert_eq!(rows.concat(), vec![0, 1, 2, 3, 4]);
        for row in &rows {
            let w: f32 = row.iter().map(|i| hour_chip_width(ALLOW_PRESETS[*i])).sum::<f32>() + SP[0] * (row.len() - 1) as f32;
            assert!(w <= avail, "{row:?} {w}");
        }
        assert_eq!(hour_chip_rows(&ALLOW_PRESETS, 760.0 - 2.0 * SP[4]).len(), 1);
    }

    #[test]
    fn fr_t14_an_open_sheet_without_a_server_says_so_and_invents_nothing() {
        let text = text_of(open_sheet);
        assert!(text.contains("멤버/게스트") && text.contains("서버에 연결되어 있지 않아"), "{text}");
        assert!(!text.contains("허용 중"));
    }
}

#[cfg(all(test, debug_assertions))]
mod review_tests {
    use super::*;
    use crate::ui::channel_owner::{allowance_badge, guest_rows, invite_rows};

    const NOW: i64 = 1_000_000_000_000;

    #[test]
    fn fr_t14_review_sample_has_every_invite_state_and_the_badge() {
        let s = review_sample_from(Some("channels"), Some("1"), Some("warn"), NOW).expect("sample");
        let states: Vec<_> = invite_rows(&s.invites, NOW).iter().map(|r| r.state).collect();
        for want in [InviteState::Pending, InviteState::Used, InviteState::Expired, InviteState::Revoked] {
            assert!(states.contains(&want), "{want:?}");
        }
        let guests = guest_rows(&s.guests, NOW);
        assert!(guests.iter().any(|g| g.badge.as_deref() == Some("허용 중 · 3시간 남음")));
        assert!(guests.iter().any(|g| g.badge.is_none()));
        assert!(allowance_badge(s.guests[0].allow_until, NOW).is_some());
        assert!(s.created.expect("created").link_warning);
        // The door variants.
        assert!(review_sample_data(NOW, "unknown").created.is_none());
        let off = review_sample_data(NOW, "off").created.expect("off");
        assert!(!off.guest_door.expect("door").is_on());
    }

    #[test]
    fn fr_t14_review_sample_never_leaks_without_the_review_vars() {
        assert!(review_sample_from(None, None, None, NOW).is_none());
        assert!(review_sample_from(Some("channels"), None, Some("on"), NOW).is_none());
        assert!(review_sample_from(Some("home"), Some("1"), None, NOW).is_none());
        assert!(review_sample_from(None, Some("1"), None, NOW).is_none());
    }
}
