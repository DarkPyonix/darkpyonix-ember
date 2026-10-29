//! compose-rust version of the Channels screen (#220 phase 4; FR-L12, `docs/DESIGN.md` section
//! 6.4): a list of rooms grouped by project and a KakaoTalk-style room view. On a compact window
//! the list and the room replace each other and the room has a back button.
//!
//! The decisions (grouping, ordering, unread badges, bubble side, date dividers, filter and
//! search) are `ui::channels`' plain functions, used as they are. The server has no channel data
//! yet (FR-T10), so [`ChannelsScreen`] reads an empty room list and says "준비 중입니다";
//! [`ChannelsScreenWith`] takes rooms, for the preview window and for when the data exists.

#![allow(non_snake_case)]

use compose_rust::runtime::{mutable_state_of, remember, MutableState};
use compose_rust::ui as cu;
use compose_rust::ui::Modifier;
use compose_rust::{Alignment, IconRole, SpaceRole};

pub use crate::ui::channels::{
    bubble_side, compact_shows_room, feed, filter_chips, group_rooms, rooms_snapshot,
    split_view, time_label, BubbleView, FeedItem, ListFilter, Member, Message, Room, RoomRow,
    Section, Side,
};
use super::components::{
    pill_colors, AppButton, ButtonKind, EmberField, FieldOptions, FilterChip, Label, Pill, PillTone,
};
use super::theme::*;
use crate::ui::project::{display_name, harness_tone, SOON};

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

/// The screen with no data: the server has no channels yet. With `EMBER_CR_ROUTE=channels` the
/// sample rooms stand in, for review captures.
pub fn ChannelsScreen() {
    let rooms = if std::env::var("EMBER_CR_ROUTE").as_deref() == Ok("channels") {
        sample_rooms()
    } else {
        rooms_snapshot()
    };
    ChannelsScreenWith(rooms);
}

pub fn ChannelsScreenWith(rooms: Vec<Room>) {
    let p = ember_theme().palette;
    let filter = remember(|| mutable_state_of(ListFilter::default()));
    // `EMBER_CR_ROOM=<index>` opens a room at the start, for review captures.
    let open = remember(|| {
        let index = std::env::var("EMBER_CR_ROOM").ok().and_then(|v| v.parse::<usize>().ok());
        mutable_state_of(index.and_then(|i| rooms.get(i)).map(|r| r.id.clone()))
    });
    let window = compose_rust::runtime::current_window_size();
    let split = split_view(window.class);
    let cur = open.get();
    let show_room = compact_shows_room(&cur);

    if rooms.is_empty() {
        cu::Box().alignment(Alignment::Center).modifier(Modifier::new().fill_max_width().fill_max_height()).content(|| {
            cu::Column().alignment(Alignment::Center).space_role(SpaceRole::Sm).content(|| {
                Label("채널", Fs::Xl3, Weight::Medium, None, None);
                Label(SOON, Fs::Md, Weight::Regular, Some(paint(p.muted)), None);
            });
        });
        return;
    }

    let order: Vec<String> = Vec::new();
    let room = cur.as_ref().and_then(|id| rooms.iter().find(|r| &r.id == id)).cloned();
    let list = || RoomList(&rooms, &order, &filter, &open, window.width_dp <= 900.0);
    let detail = || match room.clone() {
        Some(r) => {
            let back = open.clone();
            RoomView(&r, !split, move || back.set(None));
        }
        None => {
            cu::Box()
                .alignment(Alignment::Center)
                .modifier(Modifier::new().fill_max_width().fill_max_height())
                .content(|| Label("채널을 선택하십시오", Fs::Md, Weight::Regular, Some(paint(p.muted)), None));
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
}

fn RoomList(rooms: &[Room], order: &[String], filter: &MutableState<ListFilter>, open: &MutableState<Option<String>>, narrow: bool) {
    let p = ember_theme().palette;
    let current = filter.get();
    let sections = group_rooms(rooms, order, &current);
    let chips = filter_chips(rooms, order);
    let selected = open.get();
    cu::Column().modifier(Modifier::new().fill_max_width().fill_max_height()).content(|| {
        // Header: the title at 15 px and a round "+" (new channel, FR-T10 not built).
        cu::Row()
            .alignment(Alignment::CenterStart)
            .modifier(Modifier::new().fill_max_width().height(56.0))
            .content(|| {
                cu::Spacer().modifier(Modifier::new().width(SP[3]));
                Label("채널", if narrow { Fs::Xl2 } else { Fs::Lg }, Weight::Semibold, None, None);
                cu::Spacer().modifier(Modifier::new().weight(1.0));
                // TODO(FR-T10): creates a channel; disabled until the server has them.
                cu::Box()
                    .alignment(Alignment::Center)
                    .modifier(
                        Modifier::new()
                            .size(H_CTL, H_CTL)
                            .background(paint(p.surface))
                            .border(1.0, paint(p.line))
                            .shape_role(compose_rust::ShapeRole::Full)
                            .alpha(0.4),
                    )
                    .content(|| super::shell::NavIcon(super::shell::ICON_PLUS_SVG, p.text_2.to_argb(), 16.0));
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
                if sections.is_empty() {
                    Label("채널이 없습니다", Fs::Md, Weight::Regular, Some(paint(p.text_2)), None);
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
    let fill = if selected { paint(p.sel) } else { paint(compose_rust::Color::argb(0)) };
    super::components::Clickable(
        Modifier::new().fill_max_width().background(fill).corner_radius(CORNER_SM).padding(SP[1]),
        Alignment::CenterStart,
        move || open.set(Some(id.clone())),
        || {
            cu::Row().space_role(SpaceRole::Sm).alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width()).content(|| {
                RoomIcon(project, 40.0);
                cu::Column().modifier(Modifier::new().weight(1.0)).content(|| {
                    cu::Row().alignment(Alignment::CenterStart).space_role(SpaceRole::Xs).content(|| {
                        Label(&row.name, Fs::Md, Weight::Medium, Some(paint(p.text)), Some(1));
                        Label(&row.member_count.to_string(), Fs::Xs, Weight::Regular, Some(paint(p.muted)), None);
                    });
                    let last = if row.last_text.is_empty() { "메시지가 없습니다".to_string() } else { row.last_text.clone() };
                    Label(&last, Fs::Sm, Weight::Regular, Some(paint(p.muted)), Some(1));
                });
                cu::Column().alignment(Alignment::CenterEnd).content(|| {
                    Label(&time, Fs::Xs, Weight::Regular, Some(paint(p.muted)), None);
                    if row.unread > 0 {
                        super::shell::RedBadge(row.unread as usize);
                    }
                });
            });
        },
    );
}

fn RoomView(room: &Room, show_back: bool, on_back: impl FnMut() + 'static) {
    let p = ember_theme().palette;
    // The signed-in user's member id. TODO: the server names it once channels exist (FR-T10).
    let items = feed(room, "me", local_tz());
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
                    AppButton("목록", Some(IconRole::Back), ButtonKind::Default, true, true, false, on_back);
                }
                RoomIcon(&room.project, 36.0);
                cu::Column().content(|| {
                    cu::Row().alignment(Alignment::CenterStart).space_role(SpaceRole::Xs).content(|| {
                        Label(&room.name, Fs::Md, Weight::Semibold, None, Some(1));
                        Label(&room.members.len().to_string(), Fs::Xs, Weight::Regular, Some(paint(p.muted)), None);
                    });
                    cu::Row().alignment(Alignment::CenterStart).space_role(SpaceRole::Xs).content(|| {
                        super::compat::Dot(crate::model::Tone::Neutral);
                        Label(&display_name(&room.project), Fs::Xs, Weight::Medium, Some(paint(p.text_2)), Some(1));
                    });
                });
                cu::Spacer().modifier(Modifier::new().weight(1.0));
                for svg in [super::shell::ICON_USERS_SVG, super::shell::ICON_MORE_SVG] {
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
        InputBar();
    });
}

/// `.inputbar`: a white pill with a 1 px border, 740 wide at most and centered, holding a plus
/// button, the field, an emoji button and the ink send button.
/// TODO(compose-rust#376, #375): the field keeps the toolkit's own frame inside the pill, and
/// the bar has no `--shadow-lg`.
fn InputBar() {
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
                cu::Spacer().modifier(Modifier::new().width(6.0));
                for svg in [super::shell::ICON_PLUS_SVG] {
                    cu::Box()
                        .alignment(Alignment::Center)
                        .modifier(Modifier::new().size(H_CTL, H_CTL))
                        .content(|| super::shell::NavIcon(svg, p.text_2.to_argb(), 18.0));
                }
                EmberField(FieldOptions {
                    weight: Some(1.0),
                    placeholder: "메시지 입력".to_string(),
                    ..Default::default()
                });
                cu::Box()
                    .alignment(Alignment::Center)
                    .modifier(Modifier::new().size(H_CTL, H_CTL))
                    .content(|| super::shell::NavIcon(super::shell::ICON_SMILE_SVG, p.text_2.to_argb(), 18.0));
                // Sending needs the channel feed (FR-T11, not built): the button stays disabled.
                AppButton("전송", None, ButtonKind::Primary, false, false, false, || {});
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
    };
    let team = vec![member("me", "나", false, ""), member("kim", "김하나", false, ""), member("a1", "ember-리더", true, "claude-code")];
    vec![
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
    ]
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

    fn text_of(content: fn()) -> String {
        let mut host = Host::new(move || Box::new(Recomposer::new(content)));
        let batch = compose_rust::protocol::decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        batch.iter().map(|m| format!("{m:?}")).collect()
    }

    #[test]
    fn fr_l12_without_channels_the_screen_says_it_is_not_ready() {
        let text = text_of(ChannelsScreen);
        assert!(text.contains("채널") && text.contains(SOON));
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
