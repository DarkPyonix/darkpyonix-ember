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
    todo!()
}

/// One section per project, in the order of `project_order` (the project list, FR-L1); projects
/// the order does not name follow in first-seen order. Rooms in a section run newest first.
/// Sections with no room left after filtering are dropped.
pub fn group_rooms(rooms: &[Room], project_order: &[String], f: &ListFilter) -> Vec<Section> {
    todo!()
}

/// The badge text: nothing for 0, the number up to 99, then "99+".
pub fn badge_text(unread: u32) -> Option<String> {
    todo!()
}

/// The filter chips: 전체, then every project that has a room, in project order.
pub fn filter_chips(rooms: &[Room], project_order: &[String]) -> Vec<Option<String>> {
    todo!()
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
    todo!()
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
    todo!()
}

/// "2026년 10월 25일 토요일" for a day number.
pub fn date_label(day: i64) -> String {
    todo!()
}

/// "오후 3:07" for a unix time in the zone `tz`.
pub fn time_label(at: i64, tz: i64) -> String {
    todo!()
}

/// The feed of a room, oldest first: a divider before the first message of every day, and a run
/// of messages from one sender in one minute shares a single picture and name.
pub fn feed(room: &Room, me: &str, tz: i64) -> Vec<FeedItem> {
    todo!()
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
            harness: if agent { "claude".into() } else { String::new() },
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
            members: vec![member("me", false), member("kim", false), member("bot", true)],
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
            room("a", "ember", 0, vec![msg("1", "kim", "Deploy done", NOON, 0)]),
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
        assert_eq!(items[0], FeedItem::Divider("2026년 10월 25일 일요일".into()));
        assert_eq!(items[3], FeedItem::Divider("2026년 10월 26일 월요일".into()));
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
