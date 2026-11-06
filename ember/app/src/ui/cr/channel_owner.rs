//! compose-rust sheet for the channel owner: 멤버/게스트 (FR-T14 invites, FR-T15 guests, FR-T17
//! allowances; opened from the room header's members button, FR-L12).
//!
//! The decisions (invite state, remaining time, badge, hour range, door status, error sentences)
//! are `ui::channel_owner`'s plain functions. This file reads and writes through the same
//! services bridge the channels screen uses (`run_shared`): the sheet fetches invites and guests
//! when it opens and again after every action. The link of a new invite is held only in the
//! sheet's state and is gone when the sheet closes, because the server shows it once.

#![allow(non_snake_case)]

use compose_rust::runtime::{launched_effect, mutable_state_of, remember_with_key, MutableState};
use compose_rust::ui as cu;
use compose_rust::ui::Modifier;
use compose_rust::{Alignment, SpaceRole};
use ember_client::channels::{CreatedInvite, Guest, Invite};

use super::components::{
    AppButton, ButtonKind, EmberField, EmberSheet, FieldOptions, FilterChip, Label, Pill, PillTone, SheetFooter,
};
use super::notice;
use super::theme::*;
use crate::ui::channel_owner::{
    capped_note, door_status, error_message, guest_rows, invite_rows, parse_hours, DoorStatus,
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
}

impl OwnerState {
    pub fn new(room_id: &str) -> OwnerState {
        remember_with_key(room_id.to_string(), || OwnerState {
            open: mutable_state_of(false),
            ttl: mutable_state_of(DEFAULT_TTL_HOURS),
            created: mutable_state_of(None),
            invites: mutable_state_of(Vec::new()),
            guests: mutable_state_of(Vec::new()),
            load: mutable_state_of(OwnerLoad::Idle),
            hours: mutable_state_of(DEFAULT_TTL_HOURS),
            custom: mutable_state_of(String::new()),
            custom_gen: mutable_state_of(0),
            busy: mutable_state_of(false),
            note: mutable_state_of(None),
        })
    }

    fn close(&self) {
        self.open.set(false);
        self.created.set(None);
        self.note.set(None);
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

    /// Runs one owner request. On success `ok` runs and the lists are read again; on failure the
    /// fixed sentence of the error code is shown.
    fn act<T: Send + 'static>(
        &self,
        channel: &str,
        work: impl std::future::Future<Output = Result<T, ember_client::api::ApiError>> + Send + 'static,
        ok: impl FnOnce(&OwnerState, T) + Send + 'static,
    ) {
        if self.busy.get_untracked() {
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
            |st, made| st.created.set(Some(made)),
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
    // Fetch when the sheet opens (the key changes with the open flag).
    {
        let (st2, c, open) = (st.clone(), channel.to_string(), st.open.get());
        launched_effect(format!("{channel}:{open}"), move || async move {
            if open && live {
                st2.refresh(&c);
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

fn InviteSection(channel: &str, st: &OwnerState) {
    let p = ember_theme().palette;
    Heading("초대 만들기");
    Label("링크는 한 번만 보이며, 한 사람이 한 번만 쓸 수 있습니다", Fs::Sm, Weight::Regular, Some(paint(p.muted)), None);
    cu::ScrollRow().modifier(Modifier::new().fill_max_width().height(H_CTL + SP[0])).content(|| {
        let current = st.ttl.get();
        for h in TTL_CHOICES {
            let ttl = st.ttl.clone();
            FilterChip(&format!("{h}시간"), h == current, move || ttl.set(h));
            cu::Spacer().modifier(Modifier::new().width(SP[0]));
        }
    });
    let (s, c) = (st.clone(), channel.to_string());
    AppButton("초대 링크 만들기", None, ButtonKind::Primary, true, !st.busy.get(), false, move || s.create_invite(&c));

    // The door: unknown until an invite has been made, then what the server said.
    let created = st.created.get();
    let status = match &created {
        Some(c) => door_status(c.guest_door.as_ref(), Some(&c.link), c.link_warning),
        None => door_status(None, None, false),
    };
    let tone = if matches!(status, DoorStatus::Off) { p.red.strong } else { p.text_2 };
    Label(status.text(), Fs::Sm, Weight::Regular, Some(paint(tone)), None);
    if let Some(w) = status.warning() {
        Label(w, Fs::Sm, Weight::Regular, Some(paint(p.orange.strong)), None);
    }
    if let Some(c) = created.filter(|_| status.can_copy()) {
        // TODO(compose-rust#471): no clipboard facility in the app or the toolkit, so the link is
        // shown as text to select and the 복사 button waits. File the issue and replace NNN.
        Label(&c.link, Fs::Sm, Weight::Regular, None, None);
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
        OwnerLoad::Failed => Label("초대와 게스트를 불러오지 못했습니다. 잠시 뒤 다시 시도하십시오", Fs::Sm, Weight::Regular, Some(paint(p.red.strong)), None),
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
                AppButton("취소", None, ButtonKind::Destructive, true, !st.busy.get(), false, move || s.revoke(&c, &id));
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
    cu::ScrollRow().modifier(Modifier::new().fill_max_width().height(H_CTL + SP[0])).content(|| {
        let current = st.hours.get();
        for h in ALLOW_PRESETS {
            let hours = st.hours.clone();
            FilterChip(&format!("{h}시간"), h == current, move || hours.set(h));
            cu::Spacer().modifier(Modifier::new().width(SP[0]));
        }
    });
    let _gen = st.custom_gen.get();
    let custom = st.custom.get();
    cu::Row().space_role(SpaceRole::Sm).alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width()).content(|| {
        // TODO(compose-rust#374): the field is uncontrolled with no initial value.
        let (text, hours) = (st.custom.clone(), st.hours.clone());
        EmberField(FieldOptions {
            weight: Some(1.0),
            placeholder: format!("직접 입력 ({MIN_HOURS}~{MAX_HOURS}시간)"),
            on_value_change: Some(Box::new(move |v| {
                if let Some(h) = parse_hours(&v) {
                    hours.set(h);
                }
                text.set(v);
            })),
            ..Default::default()
        });
    });
    if !custom.trim().is_empty() && parse_hours(&custom).is_none() {
        Label(error_message(Some("invalid_hours")), Fs::Sm, Weight::Regular, Some(paint(p.red.strong)), None);
    }
    // A custom value that is not valid leaves the chosen hours as they were; say which they are.
    let hours = st.hours.get();
    Label(&format!("선택한 허용 시간: {hours}시간"), Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);

    let rows = guest_rows(&st.guests.get(), crate::model::now_ms());
    if rows.is_empty() {
        Label("게스트가 없습니다", Fs::Sm, Weight::Regular, Some(paint(p.muted)), None);
    }
    let custom_ok = custom.trim().is_empty() || parse_hours(&custom).is_some();
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
                AppButton(&format!("{hours}시간 허용"), None, ButtonKind::Default, true, !st.busy.get() && custom_ok, false, move || s.allow(&c, &id, hours));
                if g.can_clear {
                    let (s, c, id) = (st.clone(), channel.to_string(), g.id.clone());
                    AppButton("허용 해제", None, ButtonKind::Default, true, !st.busy.get(), false, move || s.clear(&c, &id));
                }
                cu::Spacer().modifier(Modifier::new().weight(1.0));
                let (s, c, id) = (st.clone(), channel.to_string(), g.id.clone());
                AppButton("제거", None, ButtonKind::Destructive, true, !st.busy.get(), false, move || s.remove(&c, &id));
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
    fn fr_t14_an_open_sheet_without_a_server_says_so_and_invents_nothing() {
        let text = text_of(open_sheet);
        assert!(text.contains("멤버/게스트") && text.contains("서버에 연결되어 있지 않아"), "{text}");
        assert!(!text.contains("허용 중"));
    }
}
