//! The Channels screen (FR-L12; `docs/DESIGN.md` section 6.4).
//!
//! A list of rooms grouped by project, and a KakaoTalk-style room view: others on the left with a
//! picture and a name, me on the right, the time and the unread-reader count beside each bubble,
//! date dividers, agents as members. On a compact window the list and the room replace each other
//! and the room has an explicit back button (there is no back stack).
//!
//! The decisions (grouping, ordering, unread badges, bubble side, date dividers, filter and
//! search) are plain functions at the top of this file, tested without a renderer.
//!
//! The server has no channel data yet (FR-T10 is specified, not built), so the screen reads an
//! empty room list and shows "준비 중입니다". Nothing here invents rooms or messages.

use dioxus_compose::prelude::*;

use crate::ui::components::{
    AppButton, ButtonKind, EmberCard, Label, PickChip, Pill, PillTone, pill_colors,
};
use crate::ui::project::{SOON, display_name, harness_tone};
use crate::ui::theme::*;

/// A member of a room: a person or an agent (an agent shows the "에이전트" tag).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub id: String,
    pub name: String,
    pub is_agent: bool,
    /// The harness of an agent, for the placeholder character colour.
    pub harness: String,
}

/// One message. `at` is unix seconds; `unread` is how many members have not read it yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub id: String,
    pub from: String,
    pub text: String,
    pub at: i64,
    pub unread: u32,
}

/// One channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Room {
    pub id: String,
    pub name: String,
    pub project: String,
    pub members: Vec<Member>,
    pub messages: Vec<Message>,
    /// Messages the signed-in user has not read, pushed by the server (PR-1).
    pub unread: u32,
}

impl Room {
    pub fn last_message(&self) -> Option<&Message> {
        self.messages.iter().max_by_key(|m| m.at)
    }

    /// The time the room was last active; a room with no message sorts last.
    pub fn last_at(&self) -> i64 {
        self.last_message().map(|m| m.at).unwrap_or(i64::MIN)
    }

    pub fn member(&self, id: &str) -> Option<&Member> {
        self.members.iter().find(|m| m.id == id)
    }
}

/// The rooms the server has pushed. None yet: FR-T10 is not built.
pub fn rooms_snapshot() -> Vec<Room> {
    Vec::new()
}

// ---------------------------------------------------------------------------------------------
// List
// ---------------------------------------------------------------------------------------------

/// What the list is narrowed to.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ListFilter {
    /// `None` is 전체.
    pub project: Option<String>,
    pub query: String,
}

/// A room as the list draws it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomRow {
    pub id: String,
    pub name: String,
    pub member_count: usize,
    pub last_text: String,
    pub last_at: Option<i64>,
    pub unread: u32,
}

/// One project's rooms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub project: String,
    pub rooms: Vec<RoomRow>,
}

impl Section {
    pub fn unread(&self) -> u32 {
        self.rooms.iter().map(|r| r.unread).sum()
    }
}

/// Whether a room passes the filter: the project, then the query against the room name and the
/// last message (case-insensitive).
pub fn matches(room: &Room, f: &ListFilter) -> bool {
    if let Some(p) = &f.project {
        if &room.project != p {
            return false;
        }
    }
    let q = f.query.trim().to_lowercase();
    if q.is_empty() {
        return true;
    }
    let last = room.last_message().map(|m| m.text.to_lowercase());
    room.name.to_lowercase().contains(&q) || last.is_some_and(|t| t.contains(&q))
}

/// One section per project, in the order of `project_order` (the project list, FR-L1); projects
/// the order does not name follow in first-seen order. Rooms in a section run newest first.
/// Sections with no room left after filtering are dropped.
pub fn group_rooms(rooms: &[Room], project_order: &[String], f: &ListFilter) -> Vec<Section> {
    let mut order: Vec<String> = project_order.to_vec();
    for r in rooms {
        if !order.contains(&r.project) {
            order.push(r.project.clone());
        }
    }
    order
        .into_iter()
        .filter_map(|project| {
            let mut mine: Vec<&Room> = rooms
                .iter()
                .filter(|r| r.project == project && matches(r, f))
                .collect();
            if mine.is_empty() {
                return None;
            }
            mine.sort_by(|a, b| b.last_at().cmp(&a.last_at()));
            let rooms = mine
                .into_iter()
                .map(|r| RoomRow {
                    id: r.id.clone(),
                    name: r.name.clone(),
                    member_count: r.members.len(),
                    last_text: r.last_message().map(|m| m.text.clone()).unwrap_or_default(),
                    last_at: r.last_message().map(|m| m.at),
                    unread: r.unread,
                })
                .collect();
            Some(Section { project, rooms })
        })
        .collect()
}

/// The badge text: nothing for 0, the number up to 99, then "99+".
pub fn badge_text(unread: u32) -> Option<String> {
    match unread {
        0 => None,
        1..=99 => Some(unread.to_string()),
        _ => Some("99+".to_string()),
    }
}

/// The filter chips: 전체, then every project that has a room, in project order.
pub fn filter_chips(rooms: &[Room], project_order: &[String]) -> Vec<Option<String>> {
    let mut out = vec![None];
    for s in group_rooms(rooms, project_order, &ListFilter::default()) {
        out.push(Some(s.project));
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Room
// ---------------------------------------------------------------------------------------------

/// Which side a bubble sits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// Others: left, with a picture and a name.
    Left,
    /// Me: right, in an ink bubble.
    Right,
}

pub fn bubble_side(from: &str, me: &str) -> Side {
    if from == me { Side::Right } else { Side::Left }
}

/// One line of the room feed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeedItem {
    Divider(String),
    Bubble(BubbleView),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BubbleView {
    pub id: String,
    pub side: Side,
    pub from: String,
    /// The sender's display name, shown on the first bubble of a run from someone else.
    pub name: Option<String>,
    /// Draw the picture (the first bubble of a run from someone else).
    pub show_avatar: bool,
    pub is_agent: bool,
    pub text: String,
    pub time: String,
    pub unread: Option<u32>,
}

/// Days since 1970-01-01 of a unix time in the zone `tz` (seconds east of UTC).
pub fn day_of(at: i64, tz: i64) -> i64 {
    (at + tz).div_euclid(86_400)
}

/// "2026년 10월 25일 토요일" for a day number.
pub fn date_label(day: i64) -> String {
    // Howard Hinnant's civil_from_days.
    let z = day + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    // 1970-01-01 was a Thursday.
    const DAYS: [&str; 7] = ["일", "월", "화", "수", "목", "금", "토"];
    let wd = DAYS[(day + 4).rem_euclid(7) as usize];
    format!("{y}년 {m}월 {d}일 {wd}요일")
}

/// "오후 3:07" for a unix time in the zone `tz`.
pub fn time_label(at: i64, tz: i64) -> String {
    let secs = (at + tz).rem_euclid(86_400);
    let (h, m) = (secs / 3_600, (secs % 3_600) / 60);
    let half = if h < 12 { "오전" } else { "오후" };
    let h12 = match h % 12 {
        0 => 12,
        n => n,
    };
    format!("{half} {h12}:{m:02}")
}

/// The feed of a room, oldest first: a divider before the first message of every day, and a run
/// of messages from one sender in one minute shares a single picture and name.
pub fn feed(room: &Room, me: &str, tz: i64) -> Vec<FeedItem> {
    let mut msgs: Vec<&Message> = room.messages.iter().collect();
    msgs.sort_by_key(|m| m.at);
    let mut out = Vec::new();
    let mut prev: Option<&Message> = None;
    for m in msgs {
        let new_day = prev.is_none_or(|p| day_of(p.at, tz) != day_of(m.at, tz));
        if new_day {
            out.push(FeedItem::Divider(date_label(day_of(m.at, tz))));
        }
        let side = bubble_side(&m.from, me);
        let continues = !new_day
            && prev.is_some_and(|p| p.from == m.from && p.at.div_euclid(60) == m.at.div_euclid(60));
        let member = room.member(&m.from);
        let first_of_other = side == Side::Left && !continues;
        out.push(FeedItem::Bubble(BubbleView {
            id: m.id.clone(),
            side,
            from: m.from.clone(),
            name: first_of_other.then(|| member.map(|x| x.name.clone()).unwrap_or(m.from.clone())),
            show_avatar: first_of_other,
            is_agent: member.is_some_and(|x| x.is_agent),
            text: m.text.clone(),
            time: time_label(m.at, tz),
            unread: (m.unread > 0).then_some(m.unread),
        }));
        prev = Some(m);
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Components
// ---------------------------------------------------------------------------------------------

/// Whether the list and the room share the window. On a compact window they replace each other.
pub fn split_view(class: WindowSizeClass) -> bool {
    class != WindowSizeClass::Compact
}

/// What a compact window shows: the room when one is open, the list otherwise.
pub fn compact_shows_room(open: &Option<String>) -> bool {
    open.is_some()
}

/// The local zone's offset in seconds. TODO: read the device zone; UTC until the app can.
fn local_tz() -> i64 {
    0
}

/// The placeholder picture: a circle in the harness colour (an agent) or the grey family (a
/// person), with the first letter. TODO(FR-L22): the team characters replace it for agents.
#[component]
fn Avatar(name: String, harness: String, size: f32) -> Element {
    let theme = use_ember_theme();
    let (fg, bg) = pill_colors(harness_tone(&harness), theme.palette);
    let initial: String = name
        .chars()
        .next()
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_default();
    rsx! {
        dioxus_compose::Box {
            width: size,
            height: size,
            shape_role: ShapeRole::Full,
            background: paint(bg),
            alignment: Alignment::Center,
            Label { text: initial, fs: Fs::Sm, weight: Weight::Semibold, color: paint(fg) }
        }
    }
}

/// The room picture for the list: a stack of up to three member pictures.
/// TODO(channel icons): each channel gets its own animated mark (`channels.js` roomIcon); the
/// toolkit has no animated vector yet, so the stack stands in.
#[component]
fn AvatarStack(members: Vec<Member>, size: f32) -> Element {
    rsx! {
        Row {
            for m in members.into_iter().take(3) {
                Avatar { key: "{m.id}", name: m.name.clone(), harness: m.harness.clone(), size }
            }
        }
    }
}

#[component]
pub fn ChannelsScreen() -> Element {
    let theme = use_ember_theme();
    let p = theme.palette;
    let window = use_window_size();
    let mut filter = use_signal(ListFilter::default);
    let open = use_signal(|| None::<String>);
    let rooms = rooms_snapshot();
    let split = split_view(window.class);
    let cur = open();
    let show_room = compact_shows_room(&cur);

    if rooms.is_empty() {
        return rsx! {
            Column {
                fill_max_width: true,
                fill_max_height: true,
                alignment: Alignment::Center,
                space_role: SpaceRole::Sm,
                Label { text: "채널", fs: Fs::Xl3, weight: Weight::Medium }
                Label { text: SOON, color: paint(p.muted) }
            }
        };
    }

    let order: Vec<String> = Vec::new();
    let room = cur
        .as_ref()
        .and_then(|id| rooms.iter().find(|r| &r.id == id))
        .cloned();
    let list = rsx! {
        RoomList {
            rooms: rooms.clone(),
            order: order.clone(),
            filter: filter(),
            selected: cur.clone(),
            on_query: move |q: String| filter.write().query = q,
            on_project: move |pr: Option<String>| filter.write().project = pr,
            on_open: move |id: String| {
                let mut o = open;
                o.set(Some(id));
            },
        }
    };
    let detail = rsx! {
        if let Some(r) = room.clone() {
            RoomView {
                key: "{r.id}",
                room: r,
                show_back: !split,
                on_back: move |()| {
                    let mut o = open;
                    o.set(None);
                },
            }
        } else {
            Column {
                fill_max_width: true,
                fill_max_height: true,
                alignment: Alignment::Center,
                Label { text: "채널을 선택하십시오", color: paint(p.muted) }
            }
        }
    };
    rsx! {
        if split {
            Row {
                fill_max_width: true,
                fill_max_height: true,
                Column { width: 340.0, fill_max_height: true, {list} }
                Column { weight: 1.0, fill_max_height: true, {detail} }
            }
        } else if show_room {
            Column { fill_max_width: true, fill_max_height: true, {detail} }
        } else {
            Column { fill_max_width: true, fill_max_height: true, {list} }
        }
    }
}

#[component]
fn RoomList(
    rooms: Vec<Room>,
    order: Vec<String>,
    filter: ListFilter,
    selected: Option<String>,
    on_query: EventHandler<String>,
    on_project: EventHandler<Option<String>>,
    on_open: EventHandler<String>,
) -> Element {
    let theme = use_ember_theme();
    let p = theme.palette;
    let sections = group_rooms(&rooms, &order, &filter);
    let chips = filter_chips(&rooms, &order);
    rsx! {
        Column {
            fill_max_width: true,
            fill_max_height: true,
            padding: SP[2],
            space_role: SpaceRole::Sm,
            Label { text: "채널", fs: Fs::Xl2, weight: Weight::Semibold }
            // TODO(dioxus-compose): TextField is uncontrolled and has no initial value.
            TextField {
                fill_max_width: true,
                placeholder: "채널, 메시지 검색",
                on_value_change: move |v: String| on_query.call(v),
            }
            ScrollRow {
                fill_max_width: true,
                height: 48.0,
                for c in chips {
                    PickChip {
                        key: "{c.clone().unwrap_or_default()}",
                        text: c.as_ref().map(|x| display_name(x)).unwrap_or_else(|| "전체".to_string()),
                        selected: c == filter.project,
                        on_click: move |()| on_project.call(c.clone()),
                    }
                    Spacer { width: SP[0] }
                }
            }
            ScrollColumn {
                fill_max_width: true,
                weight: 1.0,
                Column {
                    fill_max_width: true,
                    space_role: SpaceRole::Sm,
                    if sections.is_empty() {
                        Label { text: "채널이 없습니다", color: paint(p.text_2) }
                    }
                    for s in sections {
                        SectionView {
                            key: "{s.project}",
                            section: s.clone(),
                            members: rooms.clone(),
                            selected: selected.clone(),
                            on_open: move |id: String| on_open.call(id),
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn SectionView(
    section: Section,
    members: Vec<Room>,
    selected: Option<String>,
    on_open: EventHandler<String>,
) -> Element {
    let theme = use_ember_theme();
    let p = theme.palette;
    let unread = section.unread();
    rsx! {
        Row {
            fill_max_width: true,
            alignment: Alignment::CenterStart,
            space_role: SpaceRole::Sm,
            Label { text: display_name(&section.project), fs: Fs::Sm, weight: Weight::Semibold, color: paint(p.text_2) }
            Spacer { weight: 1.0 }
            if let Some(b) = badge_text(unread) {
                Pill { text: b, tone: PillTone::Red, small: true }
            }
        }
        EmberCard {
            fill_max_width: true,
            Column {
                fill_max_width: true,
                padding: SP[1],
                for r in section.rooms {
                    RoomItem {
                        key: "{r.id}",
                        row: r.clone(),
                        members: members.iter().find(|x| x.id == r.id).map(|x| x.members.clone()).unwrap_or_default(),
                        selected: selected.as_deref() == Some(r.id.as_str()),
                        on_open: move |id: String| on_open.call(id),
                    }
                }
            }
        }
    }
}

#[component]
fn RoomItem(
    row: RoomRow,
    members: Vec<Member>,
    selected: bool,
    on_open: EventHandler<String>,
) -> Element {
    let theme = use_ember_theme();
    let p = theme.palette;
    let id = row.id.clone();
    let time = row
        .last_at
        .map(|t| time_label(t, local_tz()))
        .unwrap_or_default();
    rsx! {
        Row {
            fill_max_width: true,
            padding: SP[1],
            space_role: SpaceRole::Sm,
            alignment: Alignment::CenterStart,
            AvatarStack { members, size: 28.0 }
            Column {
                weight: 1.0,
                // TODO(dioxus-compose clickable containers): the name is the hit target of the row.
                Button {
                    text: format!("{}{}  {}", if selected { "\u{25cf} " } else { "" }, row.name, row.member_count),
                    variant: ButtonVariant::Text,
                    color: paint(p.text),
                    on_click: move |_| on_open.call(id.clone()),
                }
                Label {
                    text: if row.last_text.is_empty() { "메시지가 없습니다".to_string() } else { row.last_text.clone() },
                    fs: Fs::Sm,
                    color: paint(p.text_2),
                    max_lines: 1,
                }
            }
            Column {
                alignment: Alignment::CenterEnd,
                Label { text: time, fs: Fs::Xs, color: paint(p.muted) }
                if let Some(b) = badge_text(row.unread) {
                    Pill { text: b, tone: PillTone::Red, small: true }
                }
            }
        }
    }
}

#[component]
fn RoomView(room: Room, show_back: bool, on_back: EventHandler<()>) -> Element {
    let theme = use_ember_theme();
    let p = theme.palette;
    // The signed-in user's member id. TODO: the server names it once channels exist (FR-T10).
    let me = "me".to_string();
    let items = feed(&room, &me, local_tz());
    rsx! {
        Column {
            fill_max_width: true,
            fill_max_height: true,
            Row {
                fill_max_width: true,
                padding: SP[2],
                space_role: SpaceRole::Sm,
                alignment: Alignment::CenterStart,
                if show_back {
                    AppButton { text: "목록", icon: IconRole::Back, small: true, on_click: move |()| on_back.call(()) }
                }
                Label { text: room.name.clone(), fs: Fs::Lg, weight: Weight::Semibold }
                Label { text: format!("{}", room.members.len()), color: paint(p.muted) }
                Spacer { weight: 1.0 }
                Pill { text: display_name(&room.project), tone: PillTone::Grey, small: true }
            }
            ScrollColumn {
                fill_max_width: true,
                weight: 1.0,
                Column {
                    fill_max_width: true,
                    padding: SP[2],
                    space_role: SpaceRole::Sm,
                    for (i, item) in items.into_iter().enumerate() {
                        match item {
                            FeedItem::Divider(d) => rsx! {
                                Row {
                                    key: "{i}",
                                    fill_max_width: true,
                                    alignment: Alignment::Center,
                                    Label { text: d, fs: Fs::Xs, color: paint(p.muted) }
                                }
                            },
                            FeedItem::Bubble(b) => rsx! { BubbleRow { key: "{b.id}", bubble: b, harness: room.member(&b.from).map(|m| m.harness.clone()).unwrap_or_default() } },
                        }
                    }
                }
            }
            Row {
                fill_max_width: true,
                padding: SP[2],
                space_role: SpaceRole::Sm,
                alignment: Alignment::CenterStart,
                TextField { weight: 1.0, placeholder: "메시지 입력" }
                // Sending needs the channel feed (FR-T11, not built): the button stays disabled.
                AppButton { text: "전송", kind: ButtonKind::Primary, enabled: false }
            }
        }
    }
}

#[component]
fn BubbleRow(bubble: BubbleView, harness: String) -> Element {
    let theme = use_ember_theme();
    let p = theme.palette;
    let mine = bubble.side == Side::Right;
    let (fill, ink) = if mine {
        (p.ink, p.ink_text)
    } else {
        (p.surface_2, p.text)
    };
    let stamp = rsx! {
        Column {
            alignment: if mine { Alignment::BottomEnd } else { Alignment::BottomStart },
            if let Some(n) = bubble.unread {
                Label { text: n.to_string(), fs: Fs::Xs, weight: Weight::Semibold, color: paint(p.orange.strong) }
            }
            Label { text: bubble.time.clone(), fs: Fs::Xs, color: paint(p.muted) }
        }
    };
    let body = rsx! {
        dioxus_compose::Box {
            background: paint(fill),
            shape_role: ShapeRole::Large,
            Column {
                padding: SP[2],
                Label { text: bubble.text.clone(), color: paint(ink) }
            }
        }
    };
    rsx! {
        if mine {
            Row {
                fill_max_width: true,
                space_role: SpaceRole::Xs,
                alignment: Alignment::BottomEnd,
                Spacer { weight: 1.0 }
                {stamp}
                {body}
            }
        } else {
            Row {
                fill_max_width: true,
                space_role: SpaceRole::Sm,
                alignment: Alignment::TopStart,
                if bubble.show_avatar {
                    Avatar { name: bubble.name.clone().unwrap_or_default(), harness: harness.clone(), size: 36.0 }
                } else {
                    Spacer { width: 36.0 }
                }
                Column {
                    space_role: SpaceRole::Xs,
                    if let Some(n) = bubble.name.clone() {
                        Row {
                            space_role: SpaceRole::Xs,
                            alignment: Alignment::CenterStart,
                            Label { text: n, fs: Fs::Sm, color: paint(p.text_2) }
                            if bubble.is_agent {
                                Pill { text: "에이전트", tone: PillTone::Orange, small: true }
                            }
                        }
                    }
                    Row {
                        space_role: SpaceRole::Xs,
                        alignment: Alignment::BottomStart,
                        {body}
                        {stamp}
                    }
                }
                Spacer { weight: 1.0 }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ME: &str = "me";
    // 2026-10-25 12:00:00 UTC (a Sunday).
    const NOON: i64 = 1_792_929_600;

    fn member(id: &str, agent: bool) -> Member {
        Member {
            id: id.into(),
            name: id.to_uppercase(),
            is_agent: agent,
            harness: if agent {
                "claude".into()
            } else {
                String::new()
            },
        }
    }

    fn msg(id: &str, from: &str, text: &str, at: i64, unread: u32) -> Message {
        Message {
            id: id.into(),
            from: from.into(),
            text: text.into(),
            at,
            unread,
        }
    }

    fn room(id: &str, project: &str, unread: u32, msgs: Vec<Message>) -> Room {
        Room {
            id: id.into(),
            name: format!("room {id}"),
            project: project.into(),
            members: vec![
                member("me", false),
                member("kim", false),
                member("bot", true),
            ],
            messages: msgs,
            unread,
        }
    }

    fn order() -> Vec<String> {
        vec!["ember".into(), "core".into(), "unsorted".into()]
    }

    #[test]
    fn fr_l12_sections_follow_the_project_order_and_unnamed_projects_follow() {
        let rooms = vec![
            room("a", "unsorted", 0, vec![msg("1", "kim", "x", NOON, 0)]),
            room("b", "zeta", 0, vec![msg("2", "kim", "x", NOON, 0)]),
            room("c", "core", 0, vec![msg("3", "kim", "x", NOON, 0)]),
            room("d", "ember", 0, vec![msg("4", "kim", "x", NOON, 0)]),
        ];
        let names: Vec<_> = group_rooms(&rooms, &order(), &ListFilter::default())
            .into_iter()
            .map(|s| s.project)
            .collect();
        assert_eq!(names, ["ember", "core", "unsorted", "zeta"]);
    }

    #[test]
    fn fr_l12_rooms_in_a_section_run_newest_first_and_empty_rooms_last() {
        let rooms = vec![
            room("old", "ember", 0, vec![msg("1", "kim", "x", NOON, 0)]),
            room("empty", "ember", 0, vec![]),
            room("new", "ember", 0, vec![msg("2", "kim", "y", NOON + 60, 0)]),
        ];
        let s = group_rooms(&rooms, &order(), &ListFilter::default());
        let ids: Vec<_> = s[0].rooms.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["new", "old", "empty"]);
        assert_eq!(s[0].rooms[0].last_text, "y");
        assert_eq!(s[0].rooms[2].last_at, None);
    }

    #[test]
    fn fr_l12_unread_sums_per_section_and_the_badge_caps_at_99() {
        let rooms = vec![
            room("a", "ember", 3, vec![msg("1", "kim", "x", NOON, 0)]),
            room("b", "ember", 4, vec![msg("2", "kim", "x", NOON, 0)]),
            room("c", "core", 0, vec![msg("3", "kim", "x", NOON, 0)]),
        ];
        let s = group_rooms(&rooms, &order(), &ListFilter::default());
        assert_eq!(s[0].unread(), 7);
        assert_eq!(s[1].unread(), 0);
        assert_eq!(badge_text(0), None);
        assert_eq!(badge_text(7).as_deref(), Some("7"));
        assert_eq!(badge_text(99).as_deref(), Some("99"));
        assert_eq!(badge_text(100).as_deref(), Some("99+"));
    }

    #[test]
    fn fr_l12_project_filter_keeps_one_section_and_chips_list_projects_with_rooms() {
        let rooms = vec![
            room("a", "ember", 0, vec![msg("1", "kim", "x", NOON, 0)]),
            room("b", "core", 0, vec![msg("2", "kim", "x", NOON, 0)]),
        ];
        let f = ListFilter {
            project: Some("core".into()),
            query: String::new(),
        };
        let s = group_rooms(&rooms, &order(), &f);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].project, "core");
        assert_eq!(
            filter_chips(&rooms, &order()),
            [None, Some("ember".into()), Some("core".into())]
        );
    }

    #[test]
    fn fr_l12_search_matches_the_room_name_or_the_last_message_ignoring_case() {
        let rooms = vec![
            room(
                "a",
                "ember",
                0,
                vec![msg("1", "kim", "Deploy done", NOON, 0)],
            ),
            room("b", "ember", 0, vec![msg("2", "kim", "hello", NOON, 0)]),
        ];
        let f = |q: &str| ListFilter {
            project: None,
            query: q.into(),
        };
        let ids = |q: &str| -> Vec<String> {
            group_rooms(&rooms, &order(), &f(q))
                .into_iter()
                .flat_map(|s| s.rooms.into_iter().map(|r| r.id))
                .collect()
        };
        assert_eq!(ids("deploy"), ["a"]);
        assert_eq!(ids("ROOM B"), ["b"]);
        assert_eq!(ids("  "), ["a", "b"]);
        assert!(ids("zzz").is_empty());
    }

    #[test]
    fn fr_l12_my_bubbles_are_right_and_others_left() {
        assert_eq!(bubble_side("me", ME), Side::Right);
        assert_eq!(bubble_side("kim", ME), Side::Left);
        assert_eq!(bubble_side("bot", ME), Side::Left);
    }

    #[test]
    fn fr_l12_a_divider_comes_before_the_first_message_of_each_day() {
        let r = room(
            "a",
            "ember",
            0,
            vec![
                msg("2", "kim", "b", NOON + 60, 0),
                msg("1", "kim", "a", NOON, 0),
                msg("3", "kim", "c", NOON + 86_400, 0),
            ],
        );
        let items = feed(&r, ME, 0);
        let kinds: Vec<_> = items
            .iter()
            .map(|i| matches!(i, FeedItem::Divider(_)))
            .collect();
        assert_eq!(kinds, [true, false, false, true, false]);
        assert_eq!(
            items[0],
            FeedItem::Divider("2026년 10월 25일 일요일".into())
        );
        assert_eq!(
            items[3],
            FeedItem::Divider("2026년 10월 26일 월요일".into())
        );
    }

    #[test]
    fn fr_l12_the_zone_moves_the_day_boundary() {
        // 23:30 UTC on the 25th is 08:30 on the 26th in Seoul (UTC+9).
        let at = NOON + 11 * 3_600 + 1_800;
        assert_eq!(day_of(at, 0) + 1, day_of(at, 9 * 3_600));
        assert_eq!(time_label(at, 9 * 3_600), "오전 8:30");
        assert_eq!(time_label(NOON, 0), "오후 12:00");
        assert_eq!(time_label(NOON - 12 * 3_600, 0), "오전 12:00");
    }

    #[test]
    fn fr_l12_a_run_from_one_sender_in_one_minute_shares_the_picture_and_name() {
        let r = room(
            "a",
            "ember",
            0,
            vec![
                msg("1", "kim", "a", NOON, 2),
                msg("2", "kim", "b", NOON + 10, 2),
                msg("3", "kim", "c", NOON + 120, 0),
                msg("4", "bot", "d", NOON + 125, 0),
                msg("5", "me", "e", NOON + 130, 1),
            ],
        );
        let b: Vec<BubbleView> = feed(&r, ME, 0)
            .into_iter()
            .filter_map(|i| match i {
                FeedItem::Bubble(b) => Some(b),
                _ => None,
            })
            .collect();
        assert!(b[0].show_avatar && b[0].name.as_deref() == Some("KIM"));
        assert!(!b[1].show_avatar && b[1].name.is_none());
        assert!(b[2].show_avatar, "a new minute starts a new run");
        assert!(b[3].show_avatar && b[3].is_agent, "an agent shows the tag");
        assert!(!b[4].show_avatar && b[4].name.is_none() && b[4].side == Side::Right);
        assert_eq!(b[0].unread, Some(2));
        assert_eq!(b[2].unread, None);
        assert_eq!(b[4].unread, Some(1));
    }

    #[test]
    fn fr_l12_no_rooms_are_invented() {
        assert!(rooms_snapshot().is_empty());
    }
}

#[cfg(test)]
mod layout_tests {
    use super::*;

    #[test]
    fn fr_l12_compact_switches_between_list_and_room_and_wider_windows_split() {
        assert!(!split_view(WindowSizeClass::Compact));
        assert!(split_view(WindowSizeClass::Medium));
        assert!(split_view(WindowSizeClass::Expanded));
        assert!(!compact_shows_room(&None));
        assert!(compact_shows_room(&Some("a".into())));
    }
}
