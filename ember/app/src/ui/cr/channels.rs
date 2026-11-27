//! compose-rust version of the Channels screen (#220 phase 4; FR-L12, `docs/DESIGN.md` section
//! 6.4): a list of rooms grouped by project and a KakaoTalk-style room view. On a compact window
//! the list and the room replace each other and the room has a back button.
//!
//! The decisions (grouping, ordering, unread badges, bubble side, date dividers, filter and
//! search, wire to room mapping, after-seq merging) are `ui::channels`' plain functions, used as
//! they are. [`ChannelsScreen`] reads the server's channels (FR-T10, `GET /api/channels`), loads
//! the open room's messages once, and reads `?after=seq` again when the server pushes a channel
//! message or push reconnects (PR-1, #1049: the bridge bumps `live.channels`; no timer), and
//! sends with `POST`. In demo mode the sample rooms stand in. [`ChannelsScreenWith`] takes rooms,
//! for the preview window. Unread counts are not built on the server, so no badge is drawn.

#![allow(non_snake_case)]

use compose_rust::runtime::{
    launched_effect, mutable_state_of, remember, remember_with_key, MutableState,
};
use compose_rust::ui as cu;
use compose_rust::ui::Modifier;
use compose_rust::{Alignment, SpaceRole};

pub use crate::ui::channels::{
    bubble_side, compact_shows_room, draft_after_send, empty_text, SendState, feed, filter_chips, group_rooms, last_seq,
    me_id, merge_messages, message_from, room_from, rooms_from, rooms_snapshot, split_view,
    time_label, BubbleView, FeedItem, ListFilter, Load, Member, Message, Room, RoomRow, Section,
    should_mark_read, Side, room_head_shows_project, room_title_max, LIST_HEAD_PAD_BOTTOM, LIST_HEAD_PAD_SIDE,
    LIST_HEAD_PAD_TOP, LIST_PANE_INSET, ROOM_HEAD_H, ROOM_HEAD_ICON, ROOM_HEAD_PAD, ROOM_ROW_GAP, ROOM_ROW_PAD_H,
};
use super::components::{
    pill_colors, AppButton, ButtonKind, EmberField, FieldOptions, Label, Pill, PillTone, SearchField,
};
use super::theme::*;
use super::notice;
use crate::ui::project::{display_name, harness_tone};

/// The shell's "a detail view is open" flag; `None` in unit tests without services.
fn detail_state() -> Option<MutableState<bool>> {
    crate::services::try_services().map(|s| s.ui.detail_open.clone())
}

/// Whether an open room closes because the detail flag was cleared: only on a phone, where the
/// room fills the screen while the flag is set (FR-L28, #885). `None`: no services (unit tests).
pub fn room_closed_by_back(split: bool, room_open: bool, detail_open: Option<bool>) -> bool {
    !split && room_open && detail_open == Some(false)
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
    { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new()
                .size(size, size)
                .background(paint(bg))
                .shape_role(compose_rust::ShapeRole::Full), alignment = Alignment::Center, content = || Label(&initial, Fs::Sm, Weight::Semibold, Some(paint(fg)), None)) };
}

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

    /// Loads the room's latest messages once. Later messages come by [`Feed::catch_up`], on a
    /// push (PR-1).
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
            None => {}
        }
    }

    /// A channel message was pushed or push reconnected (PR-1, #1049): the list (unread counts)
    /// is read again, and the open room `open` reads the messages after its last seq and is
    /// marked read when any came. A failed read leaves things as they are until the next push.
    async fn catch_up(&self, open: Option<String>) {
        self.refresh().await;
        let Some(id) = open else { return };
        let after = self.after(&id);
        let answer = {
            let id = id.clone();
            on_runtime(async move { crate::services::services().client.api().channel_messages(&id, &ember_client::channels::MessagesQuery { after: Some(after), ..Default::default() }).await }).await
        };
        match answer {
            Some(Ok(list)) if !list.is_empty() => {
                self.merge(&id, list.iter().map(message_from).collect());
                self.mark_read(&id);
            }
            Some(Ok(_)) => {}
            Some(Err(e)) => tracing::warn!("channel catch-up failed: {e}"),
            None => {}
        }
    }

    /// The person opened room `id`: zero its badge at once, then tell the server. A failure is
    /// only logged; the next refresh shows the server's count. Out of scope: marking read when a
    /// message arrives in the already open room.
    fn mark_read(&self, id: &str) {
        self.rooms.update(|rooms| {
            if let Some(r) = rooms.iter_mut().find(|r| r.id == id) {
                r.unread = 0;
            }
        });
        let id = id.to_string();
        crate::services::run_shared(
            async move { crate::services::services().client.api().mark_channel_read(&id, None).await },
            |r| {
                if let Err(e) = r {
                    tracing::warn!("channel mark read failed: {e}");
                }
            },
        );
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
    let window = crate::ui::window_size::current_window_size();
    let split = split_view(window.class);
    // The system back clears the shell's detail flag (`BackStep::CloseDetail`); on a phone the
    // open room closes with it, so the list and the tab bar come back together (FR-L28, #885).
    if room_closed_by_back(split, open.get().is_some(), detail_state().map(|d| d.get())) {
        open.set(None);
    }
    // Side by side with nothing open, the first listed room is shown (FR-L12, #509); the
    // compact layout keeps the list first.
    let order: Vec<String> = Vec::new();
    let cur = open.get().or_else(|| crate::ui::channels::initial_room(&rooms, &order, &filter.get(), split));
    let show_room = compact_shows_room(&cur);

    // Nothing to draw yet (loading) or nothing could be read (failed): say so, plainly. An empty
    // answer falls through to the list, which has the new-channel button.
    if let Some(text) = empty_text(&load, rooms.len()).filter(|_| load != Load::Ready) {
        { use ::compose_rust::ui::Box; Box!(alignment = Alignment::Center, modifier = Modifier::new().fill_max_width().fill_max_height(), {
            { use ::compose_rust::ui::Column; Column!(alignment = Alignment::Center, space_role = SpaceRole::Sm, {
                Label("채널", Fs::Xl3, Weight::Medium, None, Some(1));
                Label(&text, Fs::Md, Weight::Regular, Some(paint(p.muted)), None);
            }) };
        }) };
        return;
    }

    // Load the open room's messages; the effect ends when the room or the screen closes.
    if let Some(f) = feed.clone() {
        let key = cur.clone();
        let loader = f.clone();
        launched_effect(key.clone(), move || async move {
            if let Some(id) = key {
                loader.follow(id).await;
            }
        });
        // A pushed channel message (or a reconnect) since this screen opened: read again
        // (PR-1, #1049). The count at the first composition is the baseline, so opening the
        // screen does not read twice.
        let pushed = crate::services::try_services().map(|s| s.ui.live.channels.get()).unwrap_or(0);
        let baseline = remember(|| pushed);
        let open_room = cur.clone();
        launched_effect(pushed, move || async move {
            if pushed != baseline {
                f.catch_up(open_room).await;
            }
        });
    }
    let room = cur.as_ref().and_then(|id| rooms.iter().find(|r| &r.id == id)).cloned();
    let list = || RoomList(&rooms, &order, &filter, &open, cur.clone(), window.width_dp <= 900.0, &load, &new_ch, feed.is_some(), feed.clone());
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
            { use ::compose_rust::ui::Box; Box!(alignment = Alignment::Center, modifier = Modifier::new().fill_max_width().fill_max_height(), {
                    // The mockup opens the first room, so its only empty wording is the list's
                    // "채널이 없습니다". A filter that hides every room keeps the old prompt.
                    let text = if rooms.is_empty() { "채널이 없습니다" } else { "선택한 채널이 없습니다" };
                    Label(text, Fs::Md, Weight::Regular, Some(paint(p.muted)), None)
                }) };
        }
    };
    if split {
        { use ::compose_rust::ui::Row; Row!(modifier = Modifier::new().fill_max_width().fill_max_height(), {
            { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().width(list_width(window.width_dp)).fill_max_height(), content = list) };
            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(1.0).fill_max_height().background(paint(p.line))) };
            { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().weight(1.0).fill_max_height(), content = detail) };
        }) };
    } else if show_room {
        { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().fill_max_width().fill_max_height(), content = detail) };
    } else {
        { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().fill_max_width().fill_max_height(), content = list) };
    }
    super::new_channel::NewChannelSheet(&new_ch, &rooms, feed.clone(), &open);
}

/// The list title is 18 (Fs::Xl) Semibold at every width (FR-L12).
pub fn list_title_size() -> Fs {
    Fs::Xl
}
/// A room row is a 44 icon with 10 above and below, so 64 high (FR-L12); the list insets the sides.
pub const ROOM_ICON: f32 = 44.0;
pub const ROOM_ROW_PAD_V: f32 = 10.0;
pub const ROOM_ROW_HEIGHT: f32 = ROOM_ICON + 2.0 * ROOM_ROW_PAD_V;

fn RoomList(rooms: &[Room], order: &[String], filter: &MutableState<ListFilter>, open: &MutableState<Option<String>>, current_room: Option<String>, narrow: bool, load: &Load, new_ch: &super::new_channel::NewChannelState, live: bool, feed: Option<Feed>) {
    let p = ember_theme().palette;
    let current = filter.get();
    let sections = group_rooms(rooms, order, &current);
    let chips = filter_chips(rooms, order);
    let selected = current_room;
    // `.listpane`: the whole pane on `surface-2` (review CH3; it was the page's `bg`, so in dark
    // the list read as part of the page). Rows run edge to edge; the title row, the search field
    // and the segments carry their own side insets (CH4, #630).
    // On a phone the list is the page itself: on `surface-2` it left a band of `bg` above it
    // (the window's top inset) and around the tab bar (captures channels-list-390, 2026-11-15).
    let pane = if narrow { p.bg } else { p.surface_2 };
    { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().fill_max_width().fill_max_height().background(paint(pane)), {
        // `.ph`: 16 above and at the sides, 8 below; the title 18/600 and the 32 dp "+".
        { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().height(LIST_HEAD_PAD_TOP)) };
        { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, modifier = Modifier::new().fill_max_width().height(H_CTL), {
                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(LIST_HEAD_PAD_SIDE)) };
                Label("채널", list_title_size(), Weight::Semibold, None, Some(1));
                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().weight(1.0)) };
                // FR-T10, #499: the button opens the 새 채널 sheet; in demo it says why not.
                super::new_channel::NewChannelButton(new_ch, live, current.project.clone());
                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(LIST_HEAD_PAD_SIDE)) };
            }) };
        { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().height(LIST_HEAD_PAD_BOTTOM)) };
        // v0.0.7: #376 and #374 fixed (`TextField::text`); workaround kept until checked on a
        // display (a controlled value must not move the caret), so the field is uncontrolled.
        // `.searchbox{display:none}` up to 900 dp: no search field on a phone. Its margin is
        // `0 12 8`.
        if !narrow {
            let query = filter.clone();
            ListInset(|| {
                SearchField("채널, 메시지 검색", H_BTN, move |v| {
                    let mut f = query.get_untracked();
                    f.query = v;
                    query.set(f);
                });
            });
            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().height(SP[1])) };
        }
        // `.seg.scrollx` with `padding: 0 12 8`: bordered segments 8 apart (CH5).
        { use ::compose_rust::ui::ScrollRow; ScrollRow!(modifier = Modifier::new().fill_max_width().height(H_CTL), {
            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(LIST_PANE_INSET)) };
            for (i, c) in chips.iter().enumerate() {
                if i > 0 {
                    { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(SP[1])) };
                }
                let (filter, c2) = (filter.clone(), c.clone());
                let text = c.as_ref().map(|x| display_name(x)).unwrap_or_else(|| "전체".to_string());
                SegChip(&text, *c == current.project, move || {
                    let mut f = filter.get_untracked();
                    f.project = c2.clone();
                    filter.set(f);
                });
            }
            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(LIST_PANE_INSET)) };
        }) };
        { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().height(SP[1])) };
        { use ::compose_rust::ui::ScrollColumn; ScrollColumn!(modifier = Modifier::new().fill_max_width().weight(1.0), {
            if let Load::Failed(why) = load {
                // Rooms stay on screen through a failed refresh; the reason is told once.
                let _ = why;
                ListInset(|| {
                    Label("채널을 불러오지 못했습니다. 잠시 뒤 다시 시도합니다.", Fs::Sm, Weight::Regular, Some(paint(p.red.strong)), None);
                });
            }
            if sections.is_empty() && *load == Load::Ready {
                ListInset(|| {
                    Label(&empty_text(&Load::Ready, 0).unwrap_or_default(), Fs::Md, Weight::Regular, Some(paint(p.text_2)), None);
                });
            }
            // The mockup list is flat, one row per room, newest first; the project is a chip.
            { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().fill_max_width(), {
                for s in sections.iter() {
                    for r in s.rooms.iter() {
                        let members = rooms.iter().find(|x| x.id == r.id).map(|x| x.members.clone()).unwrap_or_default();
                        RoomItem(r, &s.project, &members, selected.as_deref() == Some(r.id.as_str()), open, feed.clone());
                    }
                }
            }) };
        }) };
    }) };
}

/// The list pane's content inset: 12 in from each side (`margin: 0 var(--sp3)`).
fn ListInset(content: impl FnOnce()) {
    { use ::compose_rust::ui::Row; Row!(modifier = Modifier::new().fill_max_width(), {
        { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(LIST_PANE_INSET)) };
        { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().weight(1.0), content = content) };
        { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(LIST_PANE_INSET)) };
    }) };
}

/// A filter segment (`.seg button`): 32 high, a pill, 13/500, `surface` on a 1 dp `line` border
/// with `text-2`, or `ink` with `ink-text` when chosen (review CH5: borderless soft chips).
pub fn SegChip(text: &str, selected: bool, on_click: impl FnMut() + 'static) {
    let p = ember_theme().palette;
    let (bg, border, fg) = super::routines::seg_colors(selected, p);
    super::components::Clickable(
        Modifier::new()
            .height(H_CTL)
            .background(paint(bg))
            .border(1.0, paint(border))
            .shape_role(compose_rust::ShapeRole::Full)
            .selected(selected),
        Alignment::Center,
        on_click,
        || {
            { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, {
                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(SP[2])) };
                Label(text, Fs::Sm, Weight::Medium, Some(paint(fg)), Some(1));
                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(SP[2])) };
            }) };
        },
    );
}

const ROOM_PULSE_SVG: &str = include_str!("../../../assets/nav/room-pulse.svg");
const ROOM_BARS_SVG: &str = include_str!("../../../assets/nav/room-bars.svg");
const ROOM_RADAR_SVG: &str = include_str!("../../../assets/nav/room-radar.svg");
const ROOM_ORBIT_SVG: &str = include_str!("../../../assets/nav/room-orbit.svg");
const ROOM_WAVE_SVG: &str = include_str!("../../../assets/nav/room-wave.svg");
const ROOM_SPARK_SVG: &str = include_str!("../../../assets/nav/room-spark.svg");

/// The room's mark (`channels.js` roomIcon): a rounded tile in the project's colour with the
/// room's own motif, chosen from the room id (`room_motif`). The motifs are drawn still; the
/// mockup's animation is not built (TODO(channel icons)).
fn RoomIcon(room_id: &str, project: &str, size: f32) {
    use crate::ui::channels::{room_motif, RoomMotif};
    let p = ember_theme().palette;
    let tones = [p.blue, p.orange, p.green, p.sky, p.red];
    let a = tones[(crate::ui::characters::seed_hash(project) as usize) % tones.len()];
    let svg = match room_motif(room_id) {
        RoomMotif::Pulse => ROOM_PULSE_SVG,
        RoomMotif::Bars => ROOM_BARS_SVG,
        RoomMotif::Radar => ROOM_RADAR_SVG,
        RoomMotif::Orbit => ROOM_ORBIT_SVG,
        RoomMotif::Wave => ROOM_WAVE_SVG,
        RoomMotif::Spark => ROOM_SPARK_SVG,
    };
    { use ::compose_rust::ui::Box; Box!(alignment = Alignment::Center, modifier = Modifier::new().size(size, size).background(paint(a.soft)).corner_radius(CORNER_MD), content = || super::shell::NavIcon(svg, a.strong.to_argb(), size * 0.6)) };
}

/// One room in the list: the container's `on_click` (compose-rust#372), named by its text.
fn RoomItem(row: &RoomRow, project: &str, _members: &[Member], selected: bool, open: &MutableState<Option<String>>, feed: Option<Feed>) {
    let p = ember_theme().palette;
    let time = row.last_at.map(|t| time_label(t, local_tz())).unwrap_or_default();
    let (open, id) = (open.clone(), row.id.clone());
    let detail = detail_state();
    let unread = row.unread;
    let fill = if selected { paint(p.sel) } else { paint(compose_rust::Color::argb(0)) };
    // Edge to edge of the pane with square corners (`.room-item.on { background: var(--sel) }`,
    // review CH4). The toolkit's padding is uniform, so the 10 goes all round and a 6 dp spacer
    // at each end makes the mockup's 16 at the sides (#630).
    super::components::Clickable(
        Modifier::new().fill_max_width().background(fill).padding(ROOM_ROW_PAD_V),
        Alignment::CenterStart,
        move || {
            if let Some(d) = &detail {
                d.set(true);
            }
            // A click is the person opening the room; `initial_room` never reaches here.
            if let Some(f) = &feed {
                if should_mark_read(true, unread) {
                    f.mark_read(&id);
                }
            }
            open.set(Some(id.clone()));
        },
        || {
            { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, modifier = Modifier::new().fill_max_width(), {
                // Row = 44 icon + 2 x 10 padding = 64 high. The row runs edge to edge, so a 6 dp
                // spacer at each end puts the icon 16 in, at the list title's 16 (#630); 12
                // between the icon, the text and the time (`.room-item { gap: var(--sp3) }`).
                let side = ROOM_ROW_PAD_H - ROOM_ROW_PAD_V;
                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(side)) };
                RoomIcon(&row.id, project, ROOM_ICON);
                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(ROOM_ROW_GAP)) };
                { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().weight(1.0), {
                    { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, space_role = SpaceRole::Xs, {
                        Label(&row.name, Fs::Md, Weight::Medium, Some(paint(p.text)), Some(1));
                        Label(&row.member_count.to_string(), Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                    }) };
                    let last = if row.last_text.is_empty() { "메시지가 없습니다".to_string() } else { row.last_text.clone() };
                    Label(&last, Fs::Sm, Weight::Regular, Some(paint(p.muted)), Some(1));
                }) };
                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(ROOM_ROW_GAP)) };
                { use ::compose_rust::ui::Column; Column!(alignment = Alignment::CenterEnd, {
                    Label(&time, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                    if row.unread > 0 {
                        super::shell::RedBadge(row.unread as usize);
                    }
                }) };
                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(side)) };
            }) };
        },
    );
}

fn RoomView(room: &Room, show_back: bool, live: Option<Feed>, on_back: impl FnMut() + 'static) {
    let p = ember_theme().palette;
    // The person is the member `user` (shown as 나) on the server, `me` in the samples.
    let items = feed(room, me_id(room), local_tz());
    let owner = super::channel_owner::OwnerState::new(&room.id);
    // `.room{background:var(--room)}`: the room is on `--surface`, not the page.
    { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().fill_max_width().fill_max_height().background(paint(p.surface)), {
        // `.topbar` on one line (review CH7): 20 in, the 30 dp room mark, the name at 14/600, the
        // member count, "• project" 12/500 `text-2` (not at 560 dp and below, `.hide-sm`), then
        // the round buttons right after the title (`.topbar > * { flex: none }`), 8 apart.
        let width = crate::ui::window_size::current_window_size().width_dp;
        let title = super::home::fit_label(&room.name, room_title_max(width), Fs::Md.size());
        { use ::compose_rust::ui::Row; Row!(spacing = SP[1], alignment = Alignment::CenterStart, modifier = Modifier::new().fill_max_width().height(ROOM_HEAD_H), {
                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(ROOM_HEAD_PAD - SP[1])) };
                if show_back {
                    // `.icon-btn.sm.backbtn`: an icon-only 32 px square named 뒤로 (review CH8).
                    super::components::BackButton(on_back);
                }
                RoomIcon(&room.id, &room.project, ROOM_HEAD_ICON);
                Label(&title, Fs::Md, Weight::Semibold, None, Some(1));
                Label(&room.members.len().to_string(), Fs::Md, Weight::Regular, Some(paint(p.muted)), Some(1));
                if room_head_shows_project(width) {
                    { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = SP[0], {
                        super::compat::Dot(crate::model::Tone::Neutral);
                        let project = super::home::fit_label(&display_name(&room.project), 160.0, Fs::Xs.size());
                        Label(&project, Fs::Xs, Weight::Medium, Some(paint(p.text_2)), Some(1));
                    }) };
                }
                // The members button opens the owner sheet (FR-T14, FR-T15, FR-T17), named "멤버"
                // (compose-rust v0.0.7, #429). The more glyph does nothing yet, so it is hidden
                // from assistive technology.
                {
                    let st = owner.clone();
                    super::components::NamedClickable(
                        super::a11y::MEMBERS,
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
                    { use ::compose_rust::ui::Box; Box!(alignment = Alignment::Center, modifier = Modifier::new()
                                .size(H_CTL, H_CTL)
                                .background(paint(p.surface))
                                .border(1.0, paint(p.line))
                                .shape_role(compose_rust::ShapeRole::Full)
                                .hidden(true), content = || super::shell::NavIcon(svg, p.text_2.to_argb(), 16.0)) };
                }
                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().weight(1.0)) };
            }) };
        { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().fill_max_width().height(1.0).background(paint(p.line))) };
        { use ::compose_rust::ui::ScrollColumn; ScrollColumn!(modifier = Modifier::new().fill_max_width().weight(1.0), {
            { use ::compose_rust::ui::Column; Column!(spacing = 6.0, modifier = Modifier::new().fill_max_width().padding(SP[4]), {
                for item in items.iter() {
                    match item {
                        FeedItem::Divider(d) => {
                            // `.datediv`: the label centered between two 1 px rules.
                            { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, space_role = SpaceRole::Sm, modifier = Modifier::new().fill_max_width().height(32.0), {
                                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().weight(1.0).height(1.0).background(paint(p.line))) };
                                Label(d, Fs::Xs, Weight::Regular, Some(paint(p.muted)), None);
                                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().weight(1.0).height(1.0).background(paint(p.line))) };
                            }) };
                        }
                        FeedItem::Bubble(b) => {
                            let harness = room.member(&b.from).map(|m| m.harness.clone()).unwrap_or_default();
                            BubbleRow(b, &harness, &room.project);
                        }
                    }
                }
            }) };
        }) };
        InputBar(&room.id, live.clone());
        super::channel_owner::MembersSheet(&room.id, &room.name, &owner, live.is_some());
    }) };
}

/// `.inputbar`: a white pill with a 1 px border, 740 wide at most and centered, holding a plus
/// button, the field, an emoji button and the ink send button.
/// v0.0.7: #376 and #375 fixed (the field takes the node's frame, and `Modifier::shadows`);
/// workaround kept until checked on a display (the bare field in the pill and the bar's
/// `--shadow-lg` must be compared with the mockup).
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
    let window = crate::ui::window_size::current_window_size().width_dp;
    let room_w = if window > 900.0 { window - list_width(window) - 1.0 - 234.0 } else { window };
    let width = (room_w - 32.0).clamp(240.0, 740.0);
    { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, modifier = Modifier::new().fill_max_width().padding(SP[3]), {
        { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().weight(1.0)) };
        { use ::compose_rust::ui::Row; Row!(space_role = SpaceRole::Sm, alignment = Alignment::CenterStart, modifier = Modifier::new()
                    .width(width)
                    .height(50.0)
                    .background(paint(p.surface))
                    .border(1.0, paint(p.line))
                    .shape_role(compose_rust::ShapeRole::Full), {
                let _generation = generation.get();
                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(6.0)) };
                // The attach (+) and emoji glyphs do nothing yet, so they are hidden from
                // assistive technology (compose-rust v0.0.7, #429) until they have a handler.
                for svg in [super::shell::ICON_PLUS_SVG] {
                    { use ::compose_rust::ui::Box; Box!(alignment = Alignment::Center, modifier = Modifier::new().size(H_CTL, H_CTL).hidden(true), content = || super::shell::NavIcon(svg, p.text_2.to_argb(), 18.0)) };
                }
                // FR-L10.2 (#844): the pill draws the frame, so the field is bare (mockup
                // `.cin input{border:0;background:none}`), 40 px tall inside the 50 px pill.
                EmberField(FieldOptions {
                    weight: Some(1.0),
                    bare: true,
                    inset: Some((14.0, 0.0)),
                    height: Some(H_BTN),
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
                { use ::compose_rust::ui::Box; Box!(alignment = Alignment::Center, modifier = Modifier::new().size(H_CTL, H_CTL).hidden(true), content = || super::shell::NavIcon(super::shell::ICON_SMILE_SVG, p.text_2.to_argb(), 18.0)) };
                // Sending is for the server's rooms; the sample rooms (demo) cannot send.
                let sending = state.get() == SendState::Sending;
                let can_send = live.is_some() && !sending && !draft.get().trim().is_empty();
                let (draft, send) = (draft.clone(), send.clone());
                // The visible label "전송" names the send button.
                AppButton(if sending { "전송 중" } else { "전송" }, None, ButtonKind::Primary, false, can_send, false, move || send(draft.get_untracked()));
                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(6.0)) };
            }) };
        { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().weight(1.0)) };
    }) };
}

fn BubbleRow(bubble: &BubbleView, harness: &str, project: &str) {
    let p = ember_theme().palette;
    let mine = bubble.side == Side::Right;
    let (fill, ink) = if mine { (p.ink, p.ink_text) } else { (p.surface_2, p.text) };
    let stamp = || {
        { use ::compose_rust::ui::Column; Column!(alignment = if mine { Alignment::BottomEnd } else { Alignment::BottomStart }, {
                if let Some(n) = bubble.unread {
                    Label(&n.to_string(), Fs::Xs, Weight::Semibold, Some(paint(p.orange.strong)), None);
                }
                Label(&bubble.time, Fs::Xs, Weight::Regular, Some(paint(p.muted)), None);
            }) };
    };
    let body = || {
        { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().background(paint(fill)).corner_radius(CORNER_BUBBLE).padding(SP[2]), content = || Label(&bubble.text, Fs::Md, Weight::Regular, Some(paint(ink)), None)) };
    };
    if mine {
        { use ::compose_rust::ui::Row; Row!(space_role = SpaceRole::Xs, alignment = Alignment::BottomEnd, modifier = Modifier::new().fill_max_width(), {
                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().weight(1.0)) };
                stamp();
                body();
            }) };
    } else {
        { use ::compose_rust::ui::Row; Row!(space_role = SpaceRole::Sm, alignment = Alignment::TopStart, modifier = Modifier::new().fill_max_width(), {
                if bubble.show_avatar {
                    let name = bubble.name.clone().unwrap_or_default();
                    match crate::ui::channels::avatar_kind(bubble.is_agent, harness) {
                        // The agent's team character, like the session list (FR-L22).
                        crate::ui::channels::AvatarKind::Character => {
                            super::characters::Character(harness, false, 36.0, &name, project, super::characters::CharState::Idle, None)
                        }
                        crate::ui::channels::AvatarKind::Letter => Avatar(&name, harness, 36.0),
                    }
                } else {
                    { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(36.0)) };
                }
                { use ::compose_rust::ui::Column; Column!(space_role = SpaceRole::Xs, {
                    if let Some(n) = bubble.name.clone() {
                        { use ::compose_rust::ui::Row; Row!(space_role = SpaceRole::Xs, alignment = Alignment::CenterStart, {
                            Label(&n, Fs::Sm, Weight::Regular, Some(paint(p.text_2)), None);
                            if bubble.is_agent {
                                Pill("에이전트", PillTone::Orange, true);
                            }
                            if bubble.guest {
                                Pill("게스트", PillTone::Grey, true);
                            }
                        }) };
                    }
                    { use ::compose_rust::ui::Row; Row!(space_role = SpaceRole::Xs, alignment = Alignment::BottomStart, {
                        body();
                        stamp();
                    }) };
                }) };
                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().weight(1.0)) };
            }) };
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
#[compose_rust::runtime::Composable]
pub fn preview_channels() {
    let p = ember_theme().palette;
    { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().fill_max_size().background(paint(p.bg)), {
        ChannelsScreenWith(sample_rooms());
    }) };
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
        let event = HostEvent::new(0, 0, EventPayload::WindowSizeChanged {
                width_dp: width,
                height_dp: 900.0,
                class,
                height_class: WindowHeightClass::Medium,
                safe_area_top: 0.0,
                safe_area_leading: 0.0,
                safe_area_trailing: 0.0,
                safe_area_bottom: 0.0,
            });
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
    fn fr_l28_the_system_back_closes_a_phone_room() {
        assert!(room_closed_by_back(false, true, Some(false)));
        assert!(!room_closed_by_back(false, true, Some(true)), "the room is still the detail view");
        assert!(!room_closed_by_back(true, true, Some(false)), "side by side the room stays");
        assert!(!room_closed_by_back(false, false, Some(false)));
        assert!(!room_closed_by_back(false, true, None));
    }

    #[test]
    fn fr_l12_sample_rooms_feed_dividers_and_sides() {
        let rooms = sample_rooms();
        let items = feed(&rooms[0], "me", 0);
        assert!(items.iter().any(|i| matches!(i, FeedItem::Divider(_))));
        assert!(items.iter().any(|i| matches!(i, FeedItem::Bubble(b) if b.side == Side::Right)));
    }

    #[test]
    fn fr_l12_opening_and_closing_channels_and_switching_rooms_repeatedly_does_not_touch_released_owner_state() {
        use compose_rust::protocol::{decode_batch, HostEvent};
        use compose_rust::{EventPayload, WindowHeightClass, WindowSizeClass};
        let _guard = crate::services::test_guard();
        compose_rust::window::reset_window_size();
        let ui = crate::services::test_services().ui.clone();
        ui.set_menu(crate::ui::NavMenu::Projects);

        let mut host = Host::new(|| Box::new(Recomposer::new(super::super::shell::shell)));
        let _ = decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        // Set desktop 1440x900 so split view opens `RoomView` with `OwnerState` and `MembersSheet`.
        let (bytes, _) = host
            .dispatch(HostEvent::new(0, 0, EventPayload::WindowSizeChanged {
                    width_dp: 1440.0,
                    height_dp: 900.0,
                    class: WindowSizeClass::Expanded,
                    height_class: WindowHeightClass::Medium,
                    safe_area_top: 0.0,
                    safe_area_leading: 0.0,
                    safe_area_trailing: 0.0,
                    safe_area_bottom: 0.0,
                }))
            .expect("resize to desktop");
        let _ = decode_batch(bytes).expect("decode resize");

        // Switch into 채널 and back to 프로젝트 repeatedly: leaving 채널 disposes `RoomView` and
        // `OwnerState`, which previously panicked at `channel_owner.rs:158` inside `MembersSheet`'s
        // `on_dispose` cleanup.
        for round in 0..5 {
            ui.set_menu(crate::ui::NavMenu::Channels);
            let bytes = host.render_frame((round * 2 + 1) * 16_000_000).expect("render channels");
            let text: String = decode_batch(bytes).expect("decode channels").iter().map(|m| format!("{m:?}")).collect();
            assert!(text.contains("배포 논의"), "round {round} should render channels: {text}");

            ui.set_menu(crate::ui::NavMenu::Projects);
            let bytes = host.render_frame((round * 2 + 2) * 16_000_000).expect("render projects");
            let _ = decode_batch(bytes).expect("decode projects");
        }

        // Also switch between rooms inside `ChannelsScreenWith` repeatedly and open/close the
        // owner sheet so `OwnerState::new(room_id)` keys are replaced and disposed cleanly.
        let active_room = compose_rust::runtime::mutable_state_of(0_usize);
        let sheet_open = compose_rust::runtime::mutable_state_of(false);
        let r_state = active_room;
        let s_state = sheet_open;
        let mut room_host = compose_rust::ComposeHost::with_content(move || {
            let rooms = sample_rooms();
            let idx = r_state.get();
            if idx < rooms.len() {
                let room = &rooms[idx];
                let owner = super::super::channel_owner::OwnerState::new(&room.id);
                if s_state.get() {
                    owner.open.set(true);
                }
                RoomView(room, false, None, || {});
            }
        });
        let _ = decode_batch(room_host.rebuild().expect("rebuild room host")).expect("decode");
        for step in 0..6 {
            sheet_open.set(step % 2 == 0);
            active_room.set(step % 4); // 0, 1, 2, and 3 (unmounted)
            let bytes = room_host.render_frame((step as u64 + 1) * 16_000_000).expect("render room switch");
            let _ = decode_batch(bytes).expect("decode room switch");
        }
        compose_rust::window::reset_window_size();
    }
}

/// Review rows CH3 to CH7 (#894) and the #603 review follow-ups (#630): what the list pane and
/// the room header send to the toolkit.
#[cfg(test)]
mod review_tests {
    use super::*;
    use compose_rust::protocol::{decode_batch, Mutation};
    use compose_rust::{Host, Recomposer};

    #[derive(Debug)]
    struct Node {
        id: u32,
        parent: Option<u32>,
        mods: Vec<String>,
        props: Vec<String>,
    }

    fn nodes_of(content: fn()) -> Vec<Node> {
        let mut host = Host::new(move || Box::new(Recomposer::new(content)));
        let batch = decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        let mut nodes: Vec<Node> = Vec::new();
        for m in batch.iter() {
            match m {
                Mutation::Create { node_id, .. } => {
                    nodes.push(Node { id: *node_id, parent: None, mods: Vec::new(), props: Vec::new() })
                }
                Mutation::Insert { parent_id, node_id, .. } => {
                    if let Some(n) = nodes.iter_mut().find(|n| n.id == *node_id) {
                        n.parent = Some(*parent_id);
                    }
                }
                Mutation::SetModifier { node_id, modifier, .. } => {
                    if let Some(n) = nodes.iter_mut().find(|n| n.id == *node_id) {
                        n.mods.push(format!("{modifier:?}"));
                    }
                }
                Mutation::SetProp { node_id, property, value } => {
                    if let Some(n) = nodes.iter_mut().find(|n| n.id == *node_id) {
                        n.props.push(format!("{property:?}={value:?}"));
                    }
                }
                _ => {}
            }
        }
        nodes
    }

    fn node<'a>(nodes: &'a [Node], id: u32) -> &'a Node {
        nodes.iter().find(|n| n.id == id).expect("node")
    }

    fn children<'a>(nodes: &'a [Node], parent: u32) -> Vec<&'a Node> {
        nodes.iter().filter(|n| n.parent == Some(parent)).collect()
    }

    fn text<'a>(nodes: &'a [Node], t: &str) -> &'a Node {
        let want = format!("Text=String({t:?})");
        nodes.iter().find(|n| n.props.contains(&want)).unwrap_or_else(|| panic!("no text {t}"))
    }

    fn ancestors(nodes: &[Node], id: u32) -> Vec<u32> {
        let mut out = Vec::new();
        let mut cur = node(nodes, id).parent;
        while let Some(p) = cur {
            out.push(p);
            cur = nodes.iter().find(|n| n.id == p).and_then(|n| n.parent);
        }
        out
    }

    fn bg(c: compose_rust::Color) -> String {
        format!("{:?}", compose_rust::schema::Modifier::Background(paint(c)))
    }

    fn selected_row() {
        let open = remember(|| mutable_state_of(None::<String>));
        let rooms = sample_rooms();
        let sections = group_rooms(&rooms, &[], &ListFilter::default());
        RoomItem(&sections[0].rooms[0], &sections[0].project, &[], true, &open, None);
    }

    /// CH4 and #630: the chosen row runs edge to edge with square corners, and its icon sits
    /// 10 + 6 = 16 in, the list title's inset.
    #[test]
    fn ch4_selected_row_is_full_bleed_with_the_icon_16_in() {
        let _guard = crate::services::test_guard();
        let nodes = nodes_of(selected_row);
        let p = ember_theme().palette;
        let row = nodes.iter().find(|n| n.mods.contains(&bg(p.sel))).expect("the sel row");
        assert!(row.mods.iter().any(|m| m == "Padding(10.0)"), "{row:?}");
        assert!(
            !row.mods.iter().any(|m| m.starts_with("Shape") || m.contains("Corner")),
            "no rounded inset row: {row:?}"
        );
        let inner = children(&nodes, row.id);
        assert_eq!(inner.len(), 1);
        let parts = children(&nodes, inner[0].id);
        assert!(parts[0].mods.iter().any(|m| m == "Width(6.0)"), "6 dp before the icon: {:?}", parts[0]);
        assert!(parts.last().unwrap().mods.iter().any(|m| m == "Width(6.0)"), "6 dp after the time");
    }

    fn one_chip() {
        SegChip("ember", false, || {});
    }

    /// CH5: a filter segment is `surface` on a 1 dp `line` border, 32 high, 13/500 `text-2`.
    #[test]
    fn ch5_filter_segments_are_bordered() {
        let _guard = crate::services::test_guard();
        let nodes = nodes_of(one_chip);
        let p = ember_theme().palette;
        let border = format!("{:?}", compose_rust::schema::Modifier::Border { width: 1.0, paint: paint(p.line) });
        let chip = nodes.iter().find(|n| n.mods.contains(&border)).expect("a bordered chip");
        assert!(chip.mods.contains(&bg(p.surface)), "{chip:?}");
        assert!(chip.mods.iter().any(|m| m == &format!("Height({:?})", H_CTL)));
        let label = text(&nodes, "ember");
        assert!(ancestors(&nodes, label.id).contains(&chip.id));
        let src = include_str!("channels.rs");
        let list = src.split("fn RoomList(").nth(1).unwrap().split("\n}\n").next().unwrap();
        assert!(list.contains("SegChip(&text,"), "the list uses the bordered segment");
        assert!(!list.contains("FilterChip("));
    }

    fn sample_screen() {
        ChannelsScreenWith(sample_rooms());
    }

    /// CH3: the list pane is on `surface-2`, around the title.
    #[test]
    fn ch3_list_pane_is_on_surface_2() {
        let _guard = crate::services::test_guard();
        compose_rust::window::reset_window_size();
        let nodes = nodes_of(sample_screen);
        let p = ember_theme().palette;
        let title = text(&nodes, "채널");
        let up = ancestors(&nodes, title.id);
        // The test window is the narrow one (no size reported), where the list is the page
        // and stays on `bg`; beside the room the pane is on `surface-2`.
        let on = |c| up.iter().any(|id| nodes.iter().any(|n| n.id == *id && n.mods.contains(&bg(c))));
        assert!(on(p.bg) && !on(p.surface_2), "the narrow list is on the page background");
        let src = include_str!("channels.rs");
        let list = src.split("fn RoomList(").nth(1).unwrap().split("\n}\n").next().unwrap();
        assert!(list.contains("let pane = if narrow { p.bg } else { p.surface_2 };"));
        assert!(list.contains(".background(paint(pane))"));
    }

    fn sample_room() {
        let rooms = sample_rooms();
        RoomView(&rooms[0], false, None, || {});
    }

    /// CH7: the room header is one 56 dp line: mark, name, count and project side by side, the
    /// buttons right after them and the leftover width at the end.
    #[test]
    fn ch7_room_header_is_one_line_with_the_buttons_after_the_title() {
        let _guard = crate::services::test_guard();
        compose_rust::window::reset_window_size();
        let nodes = nodes_of(sample_room);
        let rooms = sample_rooms();
        let name = text(&nodes, &rooms[0].name);
        let head = node(&nodes, name.parent.expect("the name has a row"));
        assert!(head.mods.iter().any(|m| m == "Height(56.0)"), "{head:?}");
        let count = text(&nodes, &rooms[0].members.len().to_string());
        assert_eq!(count.parent, Some(head.id), "the count is on the name's line");
        let kids = children(&nodes, head.id);
        let last = kids.last().unwrap();
        assert!(last.mods.iter().any(|m| m == "Weight(1.0)"), "the leftover width is at the end: {last:?}");
        let name_at = kids.iter().position(|n| n.id == name.id).unwrap();
        assert!(kids[name_at..].iter().all(|n| !n.mods.iter().any(|m| m == "Weight(1.0)") || n.id == last.id));
    }
}
