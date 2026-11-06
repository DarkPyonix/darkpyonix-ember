//! compose-rust version of the Channels screen (#220 phase 4; FR-L12, `docs/DESIGN.md` section
//! 6.4): a list of rooms grouped by project and a KakaoTalk-style room view. On a compact window
//! the list and the room replace each other and the room has a back button.
//!
//! The decisions (grouping, ordering, unread badges, bubble side, date dividers, filter and
//! search, wire to room mapping, after-seq merging) are `ui::channels`' plain functions, used as
//! they are. [`ChannelsScreen`] reads the server's channels (FR-T10, `GET /api/channels`), loads
//! the open room's messages, polls `?after=seq` every [`POLL_EVERY`] while a room is open (the
//! effect is keyed on the open room, so closing the room or leaving the screen stops it) and
//! sends with `POST`. In demo mode the sample rooms stand in. [`ChannelsScreenWith`] takes rooms,
//! for the preview window. Unread counts are not built on the server, so no badge is drawn.

#![allow(non_snake_case)]

use std::time::Duration;

use compose_rust::runtime::{
    delay, launched_effect, mutable_state_of, remember, remember_with_key, MutableState,
};
use compose_rust::ui as cu;
use compose_rust::ui::Modifier;
use compose_rust::{Alignment, IconRole, SpaceRole};

pub use crate::ui::channels::{
    bubble_side, compact_shows_room, draft_after_send, empty_text, SendState, feed, filter_chips, group_rooms, last_seq,
    me_id, merge_messages, message_from, room_from, rooms_from, rooms_snapshot, split_view,
    time_label, BubbleView, FeedItem, ListFilter, Load, Member, Message, Room, RoomRow, Section,
    Side,
};
use super::components::{
    pill_colors, AppButton, ButtonKind, EmberField, FieldOptions, FilterChip, Label, Pill, PillTone,
};
use super::theme::*;
use super::notice;
use crate::ui::project::{display_name, harness_tone};

/// The shell's "a detail view is open" flag; `None` in unit tests without services.
fn detail_state() -> Option<MutableState<bool>> {
    crate::services::try_services().map(|s| s.ui.detail_open.clone())
}

/// The list panel's width (`.split`): 340 above 1279 dp, 280 up to 1279, 260 up to 900.
pub fn list_width(width_dp: f32) -> f32 {
    if width_dp > 1279.0 {
        340.0
    } else if width_dp > 900.0 {
        280.0
    } else {
        260.0
    }
}

/// The local zone's offset in seconds. TODO: read the device zone; UTC until the app can.
fn local_tz() -> i64 {
    0
}

/// The placeholder picture: a circle in the harness colour (an agent) or the grey family (a
/// person), with the first letter. TODO(FR-L22): the team characters replace it for agents.
pub fn Avatar(name: &str, harness: &str, size: f32) {
    let (fg, bg) = pill_colors(harness_tone(harness).into(), ember_theme().palette);
    let initial: String = name.chars().next().map(|c| c.to_uppercase().collect()).unwrap_or_default();
    cu::Box()
        .modifier(
            Modifier::new()
                .size(size, size)
                .background(paint(bg))
                .shape_role(compose_rust::ShapeRole::Full),
        )
        .alignment(Alignment::Center)
        .content(|| Label(&initial, Fs::Sm, Weight::Semibold, Some(paint(fg)), None));
}

/// How often an open room asks the server for messages after the last seq it has.
pub const POLL_EVERY: Duration = Duration::from_secs(2);

/// What the live screen holds: the rooms read from the server and the state of that read.
#[derive(Clone)]
pub(super) struct Feed {
    pub(super) rooms: MutableState<Vec<Room>>,
    load: MutableState<Load>,
}

/// Runs `work` on the tokio runtime and resumes the (UI-thread) caller with the answer. `None`
/// when the runtime dropped it. Dropping the caller drops the wait, so a cancelled effect never
/// writes late.
async fn on_runtime<T, F>(work: F) -> Option<T>
where
    T: Send + 'static,
    F: std::future::Future<Output = T> + Send + 'static,
{
    let (tx, rx) = tokio::sync::oneshot::channel();
    crate::services::services().rt.spawn(async move {
        let _ = tx.send(work.await);
    });
    rx.await.ok()
}

impl Feed {
    /// Reads every channel. A failure keeps the rooms already shown and records the reason.
    async fn refresh(&self) {
        let answer = on_runtime(async { crate::services::services().client.api().channels(None).await }).await;
        match answer {
            Some(Ok(list)) => {
                let old = self.rooms.get_untracked();
                self.rooms.set(rooms_from(&list, &old));
                self.load.set(Load::Ready);
            }
            Some(Err(e)) => {
                tracing::warn!("channel list failed: {e}");
                self.load.set(Load::Failed(String::new()))
            }
            None => self.load.set(Load::Failed("연결이 끊겼습니다".to_string())),
        }
    }

    /// Adds `incoming` to the room `id` (no duplicates, log order).
    fn merge(&self, id: &str, incoming: Vec<Message>) {
        self.rooms.update(|rooms| {
            if let Some(r) = rooms.iter_mut().find(|r| r.id == id) {
                merge_messages(&mut r.messages, incoming);
            }
        });
    }

    fn after(&self, id: &str) -> i64 {
        self.rooms.get_untracked().iter().find(|r| r.id == id).map(|r| last_seq(&r.messages)).unwrap_or(0)
    }

    /// Loads the room's latest messages once, then polls `after=seq` until cancelled.
    async fn follow(&self, id: String) {
        let first = {
            let id = id.clone();
            on_runtime(async move { crate::services::services().client.api().channel(&id).await }).await
        };
        match first {
            Some(Ok(c)) => self.merge(&id, c.messages.iter().map(message_from).collect()),
            Some(Err(e)) => {
                tracing::warn!("channel messages failed: {e}");
                notice::post("메시지를 불러오지 못했습니다. 잠시 뒤 다시 시도합니다.".to_string())
            }
            None => return,
        }
        loop {
            delay(POLL_EVERY).await;
            let after = self.after(&id);
            let answer = {
                let id = id.clone();
                on_runtime(async move { crate::services::services().client.api().channel_messages(&id, &ember_client::channels::MessagesQuery { after: Some(after), ..Default::default() }).await }).await
            };
            match answer {
                Some(Ok(list)) if !list.is_empty() => self.merge(&id, list.iter().map(message_from).collect()),
                // A failed poll is tried again on the next beat; the room stays as it is.
                Some(_) => {}
                None => return,
            }
        }
    }

    /// Posts `text` as the person; the answer joins the room at once, a failure is told.
    fn send(&self, id: &str, text: &str, done: impl FnOnce(bool) + Send + 'static) {
        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }
        let (feed, id) = (self.clone(), id.to_string());
        crate::services::run_shared(
            {
                let (id, text) = (id.clone(), text);
                async move { crate::services::services().client.api().post_channel_message(&id, &ember_client::channels::NewChannelMessage { text, from: None }).await }
            },
            move |r| match r {
                Ok(m) => {
                    feed.merge(&id, vec![message_from(&m)]);
                    done(true);
                }
                Err(e) => {
                    tracing::warn!("channel send failed: {e}");
                    notice::post("메시지를 보내지 못했습니다. 입력한 내용은 그대로 남아 있습니다.".to_string());
                    done(false);
                }
            },
        );
    }
}

/// The screen: the server's channels, or the sample rooms in demo mode and for review captures
/// (`EMBER_CR_ROUTE=channels`).
pub fn ChannelsScreen() {
    let sample = std::env::var("EMBER_CR_ROUTE").as_deref() == Ok("channels");
    let live = crate::services::try_services().filter(|s| s.demo.is_none() && !sample);
    if live.is_none() {
        let rooms = if sample || crate::services::try_services().is_some() { sample_rooms() } else { rooms_snapshot() };
        ChannelsView(rooms, Load::Ready, None);
        return;
    }
    let feed = remember(|| Feed { rooms: mutable_state_of(Vec::new()), load: mutable_state_of(Load::Loading) });
    {
        let f = feed.clone();
        launched_effect((), move || async move {
            f.refresh().await;
        });
    }
    ChannelsView(feed.rooms.get(), feed.load.get(), Some(feed));
}

pub fn ChannelsScreenWith(rooms: Vec<Room>) {
    ChannelsView(rooms, Load::Ready, None);
}

fn ChannelsView(rooms: Vec<Room>, load: Load, feed: Option<Feed>) {
    let p = ember_theme().palette;
    let filter = remember(|| mutable_state_of(ListFilter::default()));
    let new_ch = super::new_channel::NewChannelState::new();
    // `EMBER_CR_ROOM=<index>` opens a room at the start, for review captures.
    let open = remember(|| {
        let index = std::env::var("EMBER_CR_ROOM").ok().and_then(|v| v.parse::<usize>().ok());
        let id = index.and_then(|i| rooms.get(i)).map(|r| r.id.clone());
        if let (Some(d), true) = (detail_state(), id.is_some()) {
            d.set(true);
        }
        mutable_state_of(id)
    });
    let window = compose_rust::runtime::current_window_size();
    let split = split_view(window.class);
    let cur = open.get();
    let show_room = compact_shows_room(&cur);

    // Nothing to draw yet (loading) or nothing could be read (failed): say so, plainly. An empty
    // answer falls through to the list, which has the new-channel button.
    if let Some(text) = empty_text(&load, rooms.len()).filter(|_| load != Load::Ready) {
        cu::Box().alignment(Alignment::Center).modifier(Modifier::new().fill_max_width().fill_max_height()).content(|| {
            cu::Column().alignment(Alignment::Center).space_role(SpaceRole::Sm).content(|| {
                Label("채널", Fs::Xl3, Weight::Medium, None, Some(1));
                Label(&text, Fs::Md, Weight::Regular, Some(paint(p.muted)), None);
            });
        });
        return;
    }

    // Load and poll the open room's messages; the effect ends when the room or the screen closes.
    if let Some(f) = feed.clone() {
        let key = cur.clone();
        launched_effect(key.clone(), move || async move {
            if let Some(id) = key {
                f.follow(id).await;
            }
        });
    }
    let order: Vec<String> = Vec::new();
    let room = cur.as_ref().and_then(|id| rooms.iter().find(|r| &r.id == id)).cloned();
    let list = || RoomList(&rooms, &order, &filter, &open, window.width_dp <= 900.0, &load, &new_ch, feed.is_some());
    let detail = || match room.clone() {
        Some(r) => {
            let back = open.clone();
            let detail = detail_state();
            RoomView(&r, !split, feed.clone(), move || {
                if let Some(d) = &detail {
                    d.set(false);
                }
                back.set(None);
            });
        }
        None => {
            cu::Box()
                .alignment(Alignment::Center)
                .modifier(Modifier::new().fill_max_width().fill_max_height())
                .content(|| Label("채널을 선택하세요", Fs::Md, Weight::Regular, Some(paint(p.muted)), None));
        }
    };
    if split {
        cu::Row().modifier(Modifier::new().fill_max_width().fill_max_height()).content(|| {
            cu::Column().modifier(Modifier::new().width(list_width(window.width_dp)).fill_max_height()).content(list);
            cu::Spacer().modifier(Modifier::new().width(1.0).fill_max_height().background(paint(p.line)));
            cu::Column().modifier(Modifier::new().weight(1.0).fill_max_height()).content(detail);
        });
    } else if show_room {
        cu::Column().modifier(Modifier::new().fill_max_width().fill_max_height()).content(detail);
    } else {
        cu::Column().modifier(Modifier::new().fill_max_width().fill_max_height()).content(list);
    }
    super::new_channel::NewChannelSheet(&new_ch, &rooms, feed.clone(), &open);
}

/// The list title is 18 (Fs::Xl) Semibold at every width (FR-L12).
pub fn list_title_size() -> Fs {
    Fs::Xl
}
/// A room row is a 44 icon with 10 above and below, so 64 high (FR-L12); 16 at the sides.
pub const ROOM_ICON: f32 = 44.0;
pub const ROOM_ROW_PAD_V: f32 = 10.0;
pub const ROOM_ROW_PAD_H: f32 = 16.0;
pub const ROOM_ROW_HEIGHT: f32 = ROOM_ICON + 2.0 * ROOM_ROW_PAD_V;

fn RoomList(rooms: &[Room], order: &[String], filter: &MutableState<ListFilter>, open: &MutableState<Option<String>>, narrow: bool, load: &Load, new_ch: &super::new_channel::NewChannelState, live: bool) {
    let p = ember_theme().palette;
    let current = filter.get();
    let sections = group_rooms(rooms, order, &current);
    let chips = filter_chips(rooms, order);
    let selected = open.get();
    cu::Column().modifier(Modifier::new().fill_max_width().fill_max_height()).content(|| {
        // Header: the title at 15 px and a round "+" (new channel).
        cu::Row()
            .alignment(Alignment::CenterStart)
            .modifier(Modifier::new().fill_max_width().height(56.0))
            .content(|| {
                cu::Spacer().modifier(Modifier::new().width(SP[3]));
                Label("채널", list_title_size(), Weight::Semibold, None, Some(1));
                cu::Spacer().modifier(Modifier::new().weight(1.0));
                // FR-T10, #499: the button opens the 새 채널 sheet; in demo it says why not.
                super::new_channel::NewChannelButton(new_ch, live, current.project.clone());
                cu::Spacer().modifier(Modifier::new().width(SP[3]));
            });
        cu::Spacer().modifier(Modifier::new().fill_max_width().height(1.0).background(paint(p.line)));
        cu::Column().space_role(SpaceRole::Sm).modifier(Modifier::new().fill_max_width().fill_max_height().padding(SP[2])).content(|| {
            // TODO(compose-rust#376, #374): the field frame and the leading magnifier; the
            // field is uncontrolled and has no initial value.
            // `.searchbox{display:none}` up to 900 dp: no search field on a phone.
            if !narrow {
                let query = filter.clone();
                EmberField(FieldOptions {
                    fill_max_width: true,
                    placeholder: "채널, 메시지 검색".to_string(),
                    on_value_change: Some(Box::new(move |v| {
                        let mut f = query.get_untracked();
                        f.query = v;
                        query.set(f);
                    })),
                    ..Default::default()
                });
            }
            cu::ScrollRow().modifier(Modifier::new().fill_max_width().height(H_CTL + SP[0])).content(|| {
                for c in chips.iter() {
                    let (filter, c2) = (filter.clone(), c.clone());
                    let text = c.as_ref().map(|x| display_name(x)).unwrap_or_else(|| "전체".to_string());
                    FilterChip(&text, *c == current.project, move || {
                        let mut f = filter.get_untracked();
                        f.project = c2.clone();
                        filter.set(f);
                    });
                    cu::Spacer().modifier(Modifier::new().width(SP[0]));
                }
            });
            cu::ScrollColumn().modifier(Modifier::new().fill_max_width().weight(1.0)).content(|| {
                if let Load::Failed(why) = load {
                    // Rooms stay on screen through a failed refresh; the reason is told once.
                    let _ = why;
                    Label("채널을 불러오지 못했습니다. 잠시 뒤 다시 시도합니다.", Fs::Sm, Weight::Regular, Some(paint(p.red.strong)), None);
                }
                if sections.is_empty() && *load == Load::Ready {
                    Label(&empty_text(&Load::Ready, 0).unwrap_or_default(), Fs::Md, Weight::Regular, Some(paint(p.text_2)), None);
                }
                // The mockup list is flat, one row per room, newest first; the project is a chip.
                cu::Column().modifier(Modifier::new().fill_max_width()).content(|| {
                    for s in sections.iter() {
                        for r in s.rooms.iter() {
                            let members = rooms.iter().find(|x| x.id == r.id).map(|x| x.members.clone()).unwrap_or_default();
                            RoomItem(r, &s.project, &members, selected.as_deref() == Some(r.id.as_str()), open);
                        }
                    }
                });
            });
        });
    });
}

/// The room's mark: a 40 px rounded tile in the project's colour family with a hash glyph.
/// TODO(channel icons): each channel gets its own animated mark (`channels.js` roomIcon).
fn RoomIcon(project: &str, size: f32) {
    let p = ember_theme().palette;
    let tones = [p.blue, p.orange, p.green, p.sky, p.red];
    let a = tones[(crate::ui::characters::seed_hash(project) as usize) % tones.len()];
    cu::Box()
        .alignment(Alignment::Center)
        .modifier(Modifier::new().size(size, size).background(paint(a.soft)).corner_radius(CORNER_MD))
        .content(|| super::shell::NavIcon(super::shell::ICON_HASH_SVG, a.strong.to_argb(), size * 0.5));
}

/// One room in the list. TODO(compose-rust#372): the row is a pointer-up `Clickable`.
fn RoomItem(row: &RoomRow, project: &str, _members: &[Member], selected: bool, open: &MutableState<Option<String>>) {
    let p = ember_theme().palette;
    let time = row.last_at.map(|t| time_label(t, local_tz())).unwrap_or_default();
    let (open, id) = (open.clone(), row.id.clone());
    let detail = detail_state();
    let fill = if selected { paint(p.sel) } else { paint(compose_rust::Color::argb(0)) };
    super::components::Clickable(
        Modifier::new().fill_max_width().background(fill).corner_radius(CORNER_SM).padding(ROOM_ROW_PAD_V),
        Alignment::CenterStart,
        move || {
            if let Some(d) = &detail {
                d.set(true);
            }
            open.set(Some(id.clone()));
        },
        || {
            cu::Row().space_role(SpaceRole::Sm).alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width()).content(|| {
                // Row = 44 icon + 2 x 10 padding = 64 high; the extra 6 makes the side gap 16.
                cu::Spacer().modifier(Modifier::new().width(ROOM_ROW_PAD_H - ROOM_ROW_PAD_V));
                RoomIcon(project, ROOM_ICON);
                cu::Column().modifier(Modifier::new().weight(1.0)).content(|| {
                    cu::Row().alignment(Alignment::CenterStart).space_role(SpaceRole::Xs).content(|| {
                        Label(&row.name, Fs::Md, Weight::Medium, Some(paint(p.text)), Some(1));
                        Label(&row.member_count.to_string(), Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                    });
                    let last = if row.last_text.is_empty() { "메시지가 없습니다".to_string() } else { row.last_text.clone() };
                    Label(&last, Fs::Sm, Weight::Regular, Some(paint(p.muted)), Some(1));
                });
                cu::Column().alignment(Alignment::CenterEnd).content(|| {
                    Label(&time, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                    if row.unread > 0 {
                        super::shell::RedBadge(row.unread as usize);
                    }
                });
            });
        },
    );
}

fn RoomView(room: &Room, show_back: bool, live: Option<Feed>, on_back: impl FnMut() + 'static) {
    let p = ember_theme().palette;
    // The person is the member `user` (shown as 나) on the server, `me` in the samples.
    let items = feed(room, me_id(room), local_tz());
    let owner = super::channel_owner::OwnerState::new(&room.id);
    // `.room{background:var(--room)}`: the room is on `--surface`, not the page.
    cu::Column().modifier(Modifier::new().fill_max_width().fill_max_height().background(paint(p.surface))).content(|| {
        // `.topbar`: the room mark, the name with the member count and the project, round
        // buttons at the right, a 1 px line below.
        cu::Row()
            .space_role(SpaceRole::Sm)
            .alignment(Alignment::CenterStart)
            .modifier(Modifier::new().fill_max_width().height(56.0))
            .content(|| {
                cu::Spacer().modifier(Modifier::new().width(SP[3]));
                if show_back {
                    // TODO(compose-rust#429): accessible name (back).
                    AppButton("목록", Some(IconRole::Back), ButtonKind::Default, true, true, false, on_back);
                }
                RoomIcon(&room.project, 36.0);
                cu::Column().content(|| {
                    cu::Row().alignment(Alignment::CenterStart).space_role(SpaceRole::Xs).content(|| {
                        Label(&room.name, Fs::Md, Weight::Semibold, None, Some(1));
                        Label(&room.members.len().to_string(), Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                    });
                    cu::Row().alignment(Alignment::CenterStart).space_role(SpaceRole::Xs).content(|| {
                        super::compat::Dot(crate::model::Tone::Neutral);
                        Label(&display_name(&room.project), Fs::Xs, Weight::Medium, Some(paint(p.text_2)), Some(1));
                    });
                });
                cu::Spacer().modifier(Modifier::new().weight(1.0));
                // TODO(compose-rust#429): accessible name (members, menu).
                // The members button opens the owner sheet (FR-T14, FR-T15, FR-T17).
                {
                    let st = owner.clone();
                    super::components::Clickable(
                        Modifier::new()
                            .size(H_CTL, H_CTL)
                            .background(paint(p.surface))
                            .border(1.0, paint(p.line))
                            .shape_role(compose_rust::ShapeRole::Full),
                        Alignment::Center,
                        move || st.open.set(true),
                        || super::shell::NavIcon(super::shell::ICON_USERS_SVG, p.text_2.to_argb(), 16.0),
                    );
                }
                for svg in [super::shell::ICON_MORE_SVG] {
                    cu::Box()
                        .alignment(Alignment::Center)
                        .modifier(
                            Modifier::new()
                                .size(H_CTL, H_CTL)
                                .background(paint(p.surface))
                                .border(1.0, paint(p.line))
                                .shape_role(compose_rust::ShapeRole::Full),
                        )
                        .content(|| super::shell::NavIcon(svg, p.text_2.to_argb(), 16.0));
                }
                cu::Spacer().modifier(Modifier::new().width(SP[3]));
            });
        cu::Spacer().modifier(Modifier::new().fill_max_width().height(1.0).background(paint(p.line)));
        cu::ScrollColumn().modifier(Modifier::new().fill_max_width().weight(1.0)).content(|| {
            cu::Column().spacing(6.0).modifier(Modifier::new().fill_max_width().padding(SP[4])).content(|| {
                for item in items.iter() {
                    match item {
                        FeedItem::Divider(d) => {
                            // `.datediv`: the label centered between two 1 px rules.
                            cu::Row().alignment(Alignment::CenterStart).space_role(SpaceRole::Sm).modifier(Modifier::new().fill_max_width().height(32.0)).content(|| {
                                cu::Spacer().modifier(Modifier::new().weight(1.0).height(1.0).background(paint(p.line)));
                                Label(d, Fs::Xs, Weight::Regular, Some(paint(p.muted)), None);
                                cu::Spacer().modifier(Modifier::new().weight(1.0).height(1.0).background(paint(p.line)));
                            });
                        }
                        FeedItem::Bubble(b) => {
                            let harness = room.member(&b.from).map(|m| m.harness.clone()).unwrap_or_default();
                            BubbleRow(b, &harness);
                        }
                    }
                }
            });
        });
        InputBar(&room.id, live.clone());
        super::channel_owner::MembersSheet(&room.id, &room.name, &owner, live.is_some());
    });
}

/// `.inputbar`: a white pill with a 1 px border, 740 wide at most and centered, holding a plus
/// button, the field, an emoji button and the ink send button.
/// TODO(compose-rust#376, #375): the field keeps the toolkit's own frame inside the pill, and
/// the bar has no `--shadow-lg`.
fn InputBar(room_id: &str, live: Option<Feed>) {
    // The text typed so far, kept per room; sending bumps `generation`, which draws a fresh,
    // empty field (the field is uncontrolled and has no way to be cleared).
    let draft = remember_with_key(room_id.to_string(), || mutable_state_of(String::new()));
    let generation = remember_with_key(room_id.to_string(), || mutable_state_of(0_u64));
    let state = remember_with_key(room_id.to_string(), || mutable_state_of(SendState::Idle));
    // The draft stays until the server answers Ok; on failure it is left as typed.
    let send = {
        let (draft, generation, live, id, state) =
            (draft.clone(), generation.clone(), live.clone(), room_id.to_string(), state.clone());
        move |text: String| {
            if let Some(f) = &live {
                if state.get_untracked().can_send(&text) {
                    state.set(SendState::Sending);
                    let (draft, generation, state) = (draft.clone(), generation.clone(), state.clone());
                    let sent = text.clone();
                    f.send(&id, &text, move |ok| {
                        state.set(SendState::Idle);
                        let now = draft.get_untracked();
                        let next = draft_after_send(ok, &now, sent.trim());
                        if next != now {
                            draft.set(next);
                            generation.update(|g| *g += 1);
                        }
                    });
                }
            }
        }
    };
    let p = ember_theme().palette;
    // The room's own width is not known here, so it is the window less the sidebar's rail share,
    // the list panel and the margins, kept to 740 at most.
    let window = compose_rust::runtime::current_window_size().width_dp;
    let room_w = if window > 900.0 { window - list_width(window) - 1.0 - 234.0 } else { window };
    let width = (room_w - 32.0).clamp(240.0, 740.0);
    cu::Row().alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width().padding(SP[3])).content(|| {
        cu::Spacer().modifier(Modifier::new().weight(1.0));
        cu::Row()
            .space_role(SpaceRole::Sm)
            .alignment(Alignment::CenterStart)
            .modifier(
                Modifier::new()
                    .width(width)
                    .height(50.0)
                    .background(paint(p.surface))
                    .border(1.0, paint(p.line))
                    .shape_role(compose_rust::ShapeRole::Full),
            )
            .content(|| {
                let _generation = generation.get();
                cu::Spacer().modifier(Modifier::new().width(6.0));
                // TODO(compose-rust#429): accessible name (attach (+)).
                for svg in [super::shell::ICON_PLUS_SVG] {
                    cu::Box()
                        .alignment(Alignment::Center)
                        .modifier(Modifier::new().size(H_CTL, H_CTL))
                        .content(|| super::shell::NavIcon(svg, p.text_2.to_argb(), 18.0));
                }
                EmberField(FieldOptions {
                    weight: Some(1.0),
                    placeholder: "메시지 입력".to_string(),
                    enabled: Some(live.is_some()),
                    on_value_change: Some(Box::new({
                        let draft = draft.clone();
                        move |v| draft.set(v)
                    })),
                    on_submit: Some(Box::new({
                        let send = send.clone();
                        move |v| send(v)
                    })),
                    ..Default::default()
                });
                // TODO(compose-rust#429): accessible name (emoji).
                cu::Box()
                    .alignment(Alignment::Center)
                    .modifier(Modifier::new().size(H_CTL, H_CTL))
                    .content(|| super::shell::NavIcon(super::shell::ICON_SMILE_SVG, p.text_2.to_argb(), 18.0));
                // Sending is for the server's rooms; the sample rooms (demo) cannot send.
                let sending = state.get() == SendState::Sending;
                let can_send = live.is_some() && !sending && !draft.get().trim().is_empty();
                let (draft, send) = (draft.clone(), send.clone());
                // TODO(compose-rust#429): accessible name (send, icon-less label only, no icon role).
                AppButton(if sending { "전송 중" } else { "전송" }, None, ButtonKind::Primary, false, can_send, false, move || send(draft.get_untracked()));
                cu::Spacer().modifier(Modifier::new().width(6.0));
            });
        cu::Spacer().modifier(Modifier::new().weight(1.0));
    });
}

fn BubbleRow(bubble: &BubbleView, harness: &str) {
    let p = ember_theme().palette;
    let mine = bubble.side == Side::Right;
    let (fill, ink) = if mine { (p.ink, p.ink_text) } else { (p.surface_2, p.text) };
    let stamp = || {
        cu::Column()
            .alignment(if mine { Alignment::BottomEnd } else { Alignment::BottomStart })
            .content(|| {
                if let Some(n) = bubble.unread {
                    Label(&n.to_string(), Fs::Xs, Weight::Semibold, Some(paint(p.orange.strong)), None);
                }
                Label(&bubble.time, Fs::Xs, Weight::Regular, Some(paint(p.muted)), None);
            });
    };
    let body = || {
        cu::Box()
            .modifier(Modifier::new().background(paint(fill)).corner_radius(CORNER_BUBBLE).padding(SP[2]))
            .content(|| Label(&bubble.text, Fs::Md, Weight::Regular, Some(paint(ink)), None));
    };
    if mine {
        cu::Row()
            .space_role(SpaceRole::Xs)
            .alignment(Alignment::BottomEnd)
            .modifier(Modifier::new().fill_max_width())
            .content(|| {
                cu::Spacer().modifier(Modifier::new().weight(1.0));
                stamp();
                body();
            });
    } else {
        cu::Row()
            .space_role(SpaceRole::Sm)
            .alignment(Alignment::TopStart)
            .modifier(Modifier::new().fill_max_width())
            .content(|| {
                if bubble.show_avatar {
                    Avatar(&bubble.name.clone().unwrap_or_default(), harness, 36.0);
                } else {
                    cu::Spacer().modifier(Modifier::new().width(36.0));
                }
                cu::Column().space_role(SpaceRole::Xs).content(|| {
                    if let Some(n) = bubble.name.clone() {
                        cu::Row().space_role(SpaceRole::Xs).alignment(Alignment::CenterStart).content(|| {
                            Label(&n, Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
                            if bubble.is_agent {
                                Pill("에이전트", PillTone::Orange, true);
                            }
                            if bubble.guest {
                                Pill("게스트", PillTone::Grey, true);
                            }
                        });
                    }
                    cu::Row().space_role(SpaceRole::Xs).alignment(Alignment::BottomStart).content(|| {
                        body();
                        stamp();
                    });
                });
                cu::Spacer().modifier(Modifier::new().weight(1.0));
            });
    }
}

/// Sample rooms for the preview window: two projects, an agent member, unread counts and two
/// days of messages.
pub fn sample_rooms() -> Vec<Room> {
    let now = crate::model::now_ms() / 1000;
    let member = |id: &str, name: &str, is_agent: bool, harness: &str| Member {
        id: id.into(),
        name: name.into(),
        is_agent,
        harness: harness.into(),
    };
    let msg = |id: &str, from: &str, text: &str, ago: i64, unread: u32| Message {
        id: id.into(),
        from: from.into(),
        text: text.into(),
        at: now - ago,
        unread,
        author_name: String::new(),
        author_kind: String::new(),
    };
    // A guest's message (FR-T15): shows the name and a 게스트 tag, never the id. Sample data
    // only, compiled in debug builds only (release has no sample guests).
    #[cfg(debug_assertions)]
    let guest_msg = Message {
        id: "m3g".into(),
        from: "guest:g1".into(),
        text: "저도 같이 확인해 볼게요".into(),
        at: now - 450,
        unread: 0,
        author_name: "민수".into(),
        author_kind: "guest".into(),
    };
    let team = vec![member("me", "나", false, ""), member("kim", "김하나", false, ""), member("a1", "ember-리더", true, "claude-code")];
    #[allow(unused_mut)]
    let mut rooms = vec![
        Room {
            id: "r1".into(),
            name: "배포 논의".into(),
            project: "ember".into(),
            members: team.clone(),
            messages: vec![
                msg("m1", "kim", "어제 빌드 확인해 주세요", 100_000, 0),
                msg("m2", "me", "확인했습니다. 한 군데 실패해요", 99_000, 1),
                msg("m3", "a1", "원인을 찾아서 고치겠습니다.", 600, 2),
                msg("m4", "me", "고마워요", 300, 0),
            ],
            unread: 3,
        },
        Room {
            id: "r2".into(),
            name: "디자인 리뷰".into(),
            project: "ember".into(),
            members: team.clone(),
            messages: vec![msg("m5", "kim", "사이드바 간격 다시 볼게요", 7_200, 0)],
            unread: 0,
        },
        Room {
            id: "r3".into(),
            name: "자유 채널".into(),
            project: "darkpyonix".into(),
            members: team,
            messages: vec![],
            unread: 0,
        },
    ];
    #[cfg(debug_assertions)]
    rooms[0].messages.insert(3, guest_msg);
    rooms
}

/// A review window (`EMBER_CR_ROUTE=channels`): the channels screen with sample rooms; the
/// first room is open on a wide window.
#[compose_rust::runtime::composable]
pub fn preview() {
    let p = ember_theme().palette;
    cu::Box().modifier(Modifier::new().fill_max_size().background(paint(p.bg))).content(|| {
        ChannelsScreenWith(sample_rooms());
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use compose_rust::{Host, Recomposer};

    #[test]
    fn fr_l12_channels_title_is_18_at_every_width() {
        assert_eq!(list_title_size(), Fs::Xl);
    }

    #[test]
    fn fr_l12_channels_row_is_64_high() {
        assert_eq!(ROOM_ICON, 44.0);
        assert_eq!(ROOM_ROW_HEIGHT, 64.0);
        assert_eq!(ROOM_ROW_PAD_H, 16.0);
    }

    fn text_of(content: fn()) -> String {
        let mut host = Host::new(move || Box::new(Recomposer::new(content)));
        let batch = compose_rust::protocol::decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        batch.iter().map(|m| format!("{m:?}")).collect()
    }

    #[test]
    fn fr_l12_a_loading_screen_says_it_is_loading() {
        fn loading() {
            ChannelsView(Vec::new(), Load::Loading, None);
        }
        let text = text_of(loading);
        assert!(text.contains("채널을 불러오는 중입니다"), "{text}");
    }

    #[test]
    fn fr_l12_a_failed_read_with_no_rooms_says_why() {
        fn failed() {
            ChannelsView(Vec::new(), Load::Failed("503".into()), None);
        }
        let text = text_of(failed);
        assert!(text.contains("채널을 불러오지 못했습니다. 잠시 뒤 다시 시도합니다.") && !text.contains("503"), "{text}");
    }

    #[test]
    fn fr_l12_an_empty_answer_says_there_are_no_channels() {
        fn empty() {
            ChannelsScreenWith(Vec::new());
        }
        assert!(text_of(empty).contains("채널이 없습니다"));
    }

    #[test]
    fn fr_l12_no_unread_means_no_badge_text() {
        let rooms = sample_rooms();
        let rows = group_rooms(&rooms[2..], &[], &ListFilter::default());
        assert_eq!(rows[0].rooms[0].unread, 0);
    }

    /// Everything emitted composing the sample list, then after the window reports `width`.
    fn list_text_at(width: f32, class: compose_rust::WindowSizeClass) -> String {
        use compose_rust::protocol::{decode_batch, HostEvent};
        use compose_rust::{EventPayload, WindowHeightClass};
        compose_rust::window::reset_window_size();
        let mut host = Host::new(|| Box::new(Recomposer::new(|| ChannelsScreenWith(sample_rooms()))));
        let mut text: String = decode_batch(host.rebuild().expect("rebuild"))
            .expect("decode")
            .iter()
            .map(|m| format!("{m:?}"))
            .collect();
        let event = HostEvent {
            node_id: 0,
            handler_id: 0,
            payload: EventPayload::WindowSizeChanged {
                width_dp: width,
                height_dp: 900.0,
                class,
                height_class: WindowHeightClass::Medium,
                safe_area_top: 0.0,
                safe_area_leading: 0.0,
                safe_area_trailing: 0.0,
            },
        };
        let (bytes, _) = host.dispatch(event).expect("dispatch");
        text.extend(decode_batch(bytes).expect("decode").iter().map(|m| format!("{m:?}")));
        compose_rust::window::reset_window_size();
        text
    }

    #[test]
    fn fr_l12_the_wide_list_groups_rooms_and_has_the_search_field() {
        let text = list_text_at(1440.0, compose_rust::WindowSizeClass::Expanded);
        for want in ["배포 논의", "디자인 리뷰", "자유 채널", "전체", "채널, 메시지 검색"] {
            assert!(text.contains(want), "missing {want}");
        }
    }

    #[test]
    fn fr_l12_the_phone_list_has_no_search_field() {
        let text = list_text_at(390.0, compose_rust::WindowSizeClass::Compact);
        for want in ["배포 논의", "전체"] {
            assert!(text.contains(want), "missing {want}");
        }
        assert!(!text.contains("채널, 메시지 검색"));
    }

    #[test]
    fn fr_l12_sample_rooms_feed_dividers_and_sides() {
        let rooms = sample_rooms();
        let items = feed(&rooms[0], "me", 0);
        assert!(items.iter().any(|i| matches!(i, FeedItem::Divider(_))));
        assert!(items.iter().any(|i| matches!(i, FeedItem::Bubble(b) if b.side == Side::Right)));
    }
}
