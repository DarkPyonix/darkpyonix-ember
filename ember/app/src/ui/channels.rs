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

use dioxus_compose::prelude::WindowSizeClass;

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
    /// The server's display name of the author (FR-T15); empty on an older server.
    pub author_name: String,
    /// `person`, `agent`, `guest` or `system`; empty on an older server.
    pub author_kind: String,
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

/// The rooms before anything is loaded: none. The screen's own load state says why
/// (loading, failed, empty); nothing here invents rooms.
pub fn rooms_snapshot() -> Vec<Room> {
    Vec::new()
}

// ---------------------------------------------------------------------------------------------
// Server data (FR-T10): the wire types become rooms
// ---------------------------------------------------------------------------------------------

/// The person's member id on the server (`user`); the screen shows it as 나.
pub const USER_ID: &str = "user";
/// What the person is called in a room.
pub const USER_NAME: &str = "나";

/// Whether a message is on its way to the server (FR-T10). One send at a time, so a double
/// click or a second Enter cannot post the same text twice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SendState {
    #[default]
    Idle,
    Sending,
}

impl SendState {
    /// A send may start only when idle and the text is not blank.
    pub fn can_send(self, text: &str) -> bool {
        self == SendState::Idle && !text.trim().is_empty()
    }
}

/// The draft after the server answered: cleared on success (if the person did not type on
/// meanwhile), kept as typed on failure so nothing is lost.
pub fn draft_after_send(ok: bool, draft: &str, sent: &str) -> String {
    if ok && draft == sent { String::new() } else { draft.to_string() }
}

/// What the channel list shows while and after it loads.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Load {
    /// The first request has not answered.
    #[default]
    Loading,
    /// The request failed; the text is the reason. The last good rooms stay on screen.
    Failed(String),
    Ready,
}

/// The one line for a state with no rooms to draw, or `None` when rooms are shown.
pub fn empty_text(load: &Load, rooms: usize) -> Option<String> {
    match load {
        Load::Loading if rooms == 0 => Some("채널을 불러오는 중입니다".to_string()),
        Load::Failed(_) if rooms == 0 => Some("채널을 불러오지 못했습니다. 잠시 뒤 다시 시도합니다.".to_string()),
        Load::Ready if rooms == 0 => Some("채널이 없습니다".to_string()),
        _ => None,
    }
}

/// The sequence number of a message (the server's `id` is the seq as text); 0 when unreadable.
pub fn seq_of(m: &Message) -> i64 {
    m.id.parse().unwrap_or(0)
}

/// The highest seq in `messages`: the `after` cursor of the next poll.
pub fn last_seq(messages: &[Message]) -> i64 {
    messages.iter().map(seq_of).max().unwrap_or(0)
}

/// Adds `incoming` to `messages` without duplicates (by id) and keeps the log order (seq, then
/// time). A message already there is replaced by the newer copy.
pub fn merge_messages(messages: &mut Vec<Message>, incoming: Vec<Message>) {
    for m in incoming {
        match messages.iter_mut().find(|x| x.id == m.id) {
            Some(x) => *x = m,
            None => messages.push(m),
        }
    }
    messages.sort_by_key(|m| (seq_of(m), m.at));
}

pub fn member_from(m: &ember_client::channels::Member) -> Member {
    let is_user = m.id == USER_ID;
    let name = if is_user {
        USER_NAME.to_string()
    } else if m.name.is_empty() {
        m.id.clone()
    } else {
        m.name.clone()
    };
    Member { id: m.id.clone(), name, is_agent: m.is_agent && !is_user, harness: m.harness.clone() }
}

pub fn message_from(m: &ember_client::channels::ChannelMessage) -> Message {
    let id = if m.id.is_empty() { m.seq.to_string() } else { m.id.clone() };
    Message {
        id,
        from: m.from.clone(),
        text: m.text.clone(),
        at: m.at,
        unread: m.unread,
        author_name: m.author_name.clone(),
        author_kind: m.author_kind.clone(),
    }
}

/// A server channel as a room. A list answer has no `messages`; its `last_message` stands in so
/// the row shows a preview and sorts by activity.
pub fn room_from(c: &ember_client::channels::Channel) -> Room {
    let mut messages: Vec<Message> = c.messages.iter().map(message_from).collect();
    if messages.is_empty() {
        messages.extend(c.last_message.as_ref().map(message_from));
    }
    messages.sort_by_key(|m| (seq_of(m), m.at));
    Room {
        id: c.id.clone(),
        name: c.name.clone(),
        project: c.project.clone(),
        members: c.members.iter().map(member_from).collect(),
        messages,
        unread: c.unread,
    }
}

/// The list answer as rooms, keeping the messages already loaded for an open room (the list
/// carries only the last one).
pub fn rooms_from(list: &[ember_client::channels::Channel], old: &[Room]) -> Vec<Room> {
    list.iter()
        .map(|c| {
            let mut r = room_from(c);
            if let Some(o) = old.iter().find(|o| o.id == r.id) {
                if o.messages.len() > r.messages.len() {
                    let incoming = std::mem::take(&mut r.messages);
                    r.messages = o.messages.clone();
                    merge_messages(&mut r.messages, incoming);
                }
            }
            r
        })
        .collect()
}

/// The member id that is the person in `room`: `user` for a server room, `me` in the samples.
pub fn me_id(room: &Room) -> &str {
    if room.member(USER_ID).is_some() { USER_ID } else { "me" }
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
    /// A guest's message (FR-T15): the name is the guest's display name and a 게스트 tag shows.
    pub guest: bool,
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
    msgs.sort_by_key(|m| (m.at, seq_of(m)));
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
        let guest_name = crate::ui::channel_owner::guest_author_label(&m.from, &m.author_name, &m.author_kind);
        let first_of_other = side == Side::Left && !continues;
        out.push(FeedItem::Bubble(BubbleView {
            id: m.id.clone(),
            side,
            from: m.from.clone(),
            name: first_of_other.then(|| match &guest_name {
                Some(g) => g.clone(),
                None => member.map(|x| x.name.clone()).unwrap_or(m.from.clone()),
            }),
            show_avatar: first_of_other,
            is_agent: member.is_some_and(|x| x.is_agent),
            guest: guest_name.is_some(),
            text: m.text.clone(),
            time: time_label(m.at, tz),
            unread: (m.unread > 0).then_some(m.unread),
        }));
        prev = Some(m);
    }
    out
}

/// Whether the list and the room share the window. On a compact window they replace each other.
pub fn split_view(class: WindowSizeClass) -> bool {
    class != WindowSizeClass::Compact
}

/// What a compact window shows: the room when one is open, the list otherwise.
pub fn compact_shows_room(open: &Option<String>) -> bool {
    open.is_some()
}

#[cfg(test)]
mod tests {
    #[test]
    fn fr_t10_send_blocks_while_in_flight() {
        assert!(SendState::Idle.can_send("hi"));
        assert!(!SendState::Sending.can_send("hi"));
        assert!(!SendState::Idle.can_send("  "));
    }

    #[test]
    fn fr_t10_draft_kept_on_failure() {
        assert_eq!(draft_after_send(false, "hello", "hello"), "hello");
        assert_eq!(draft_after_send(true, "hello", "hello"), "");
        assert_eq!(draft_after_send(true, "hello more", "hello"), "hello more");
    }

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
            author_name: String::new(),
            author_kind: String::new(),
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

    #[test]
    fn fr_t15_a_guest_bubble_shows_the_name_and_tag_never_the_id() {
        let mut m = msg("m1", "guest:g7", "안녕하세요", 100, 0);
        m.author_name = "민수".into();
        m.author_kind = "guest".into();
        let mut nameless = msg("m2", "guest:g8", "저도요", 4000, 0);
        nameless.author_kind = "guest".into();
        let r = room("c", "ember", 0, vec![m, nameless]);
        let items = feed(&r, USER_ID, 0);
        let bubbles: Vec<&BubbleView> = items.iter().filter_map(|i| if let FeedItem::Bubble(b) = i { Some(b) } else { None }).collect();
        assert_eq!(bubbles[0].name.as_deref(), Some("민수"));
        assert_eq!(bubbles[1].name.as_deref(), Some("게스트"));
        assert!(bubbles.iter().all(|b| b.guest && !b.name.clone().unwrap_or_default().contains("guest:")));
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

    fn blank() -> ember_client::channels::Channel {
        ember_client::channels::Channel {
            id: String::new(),
            name: String::new(),
            project: String::new(),
            default: false,
            members: Vec::new(),
            last_message: None,
            messages: Vec::new(),
            unread: 0,
            guests: Vec::new(),
        }
    }

    fn wire_msg(seq: i64, from: &str, at: i64) -> ember_client::channels::ChannelMessage {
        ember_client::channels::ChannelMessage { id: seq.to_string(), seq, from: from.into(), text: format!("t{seq}"), at, unread: 0, mentions: Vec::new(), author_name: String::new(), author_kind: String::new() }
    }

    #[test]
    fn fr_t10_channel_maps_to_a_room_with_the_user_as_na() {
        let c = ember_client::channels::Channel {
            id: "c1".into(),
            name: "전체".into(),
            project: "ember".into(),
            default: true,
            members: vec![
                ember_client::channels::Member { id: "user".into(), name: "user".into(), is_agent: false, harness: String::new() },
                ember_client::channels::Member { id: "s1".into(), name: "".into(), is_agent: true, harness: "codex".into() },
            ],
            messages: vec![wire_msg(2, "s1", 20), wire_msg(1, "user", 10)],
            ..blank()
        };
        let r = room_from(&c);
        assert_eq!(r.member("user").unwrap().name, "나");
        assert_eq!(r.member("s1").unwrap().name, "s1");
        assert!(r.member("s1").unwrap().is_agent);
        assert_eq!(r.messages.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["1", "2"]);
        assert_eq!(me_id(&r), "user");
        let items = feed(&r, me_id(&r), 0);
        assert!(items.iter().any(|i| matches!(i, FeedItem::Bubble(b) if b.side == Side::Right && b.from == "user")));
    }

    #[test]
    fn fr_t10_a_list_answer_shows_the_last_message() {
        let c = ember_client::channels::Channel {
            id: "c1".into(),
            last_message: Some(wire_msg(5, "user", 50)),
            ..blank()
        };
        assert_eq!(room_from(&c).last_message().unwrap().text, "t5");
    }

    #[test]
    fn fr_t10_after_seq_merge_has_no_duplicates_and_keeps_order() {
        let mut v: Vec<Message> = [1, 2, 3].iter().map(|s| message_from(&wire_msg(*s, "user", *s * 10))).collect();
        assert_eq!(last_seq(&v), 3);
        merge_messages(&mut v, vec![message_from(&wire_msg(3, "user", 30)), message_from(&wire_msg(4, "s1", 40))]);
        assert_eq!(v.iter().map(seq_of).collect::<Vec<_>>(), [1, 2, 3, 4]);
        merge_messages(&mut v, vec![]);
        assert_eq!(v.len(), 4);
        assert_eq!(last_seq(&[]), 0);
    }

    #[test]
    fn fr_t10_same_second_messages_keep_seq_order() {
        let mut v = Vec::new();
        merge_messages(&mut v, vec![message_from(&wire_msg(9, "s1", 5)), message_from(&wire_msg(8, "user", 5))]);
        assert_eq!(v.iter().map(seq_of).collect::<Vec<_>>(), [8, 9]);
    }

    #[test]
    fn fr_t10_a_refreshed_list_keeps_the_open_rooms_messages() {
        let old = vec![room_from(&ember_client::channels::Channel {
            id: "c1".into(),
            messages: vec![wire_msg(1, "user", 1), wire_msg(2, "user", 2)],
            ..blank()
        })];
        let list = vec![ember_client::channels::Channel { id: "c1".into(), last_message: Some(wire_msg(3, "s1", 3)), ..blank() }];
        let rooms = rooms_from(&list, &old);
        assert_eq!(rooms[0].messages.iter().map(seq_of).collect::<Vec<_>>(), [1, 2, 3]);
    }

    #[test]
    fn fr_l12_empty_loading_and_failed_states_are_honest() {
        assert_eq!(empty_text(&Load::Loading, 0).unwrap(), "채널을 불러오는 중입니다");
        assert!(!empty_text(&Load::Failed("503".into()), 0).unwrap().contains("503"));
        assert_eq!(empty_text(&Load::Ready, 0).unwrap(), "채널이 없습니다");
        // Rooms already loaded stay on screen through a later failure.
        assert!(empty_text(&Load::Failed("x".into()), 2).is_none());
        assert!(empty_text(&Load::Ready, 2).is_none());
    }

    #[test]
    fn fr_l12_unread_zero_has_no_badge() {
        assert_eq!(badge_text(0), None);
    }
}
