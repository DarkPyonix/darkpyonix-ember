//! compose-rust version of `ui::conversation` (#220 phase 6; FR-L5 to FR-L7, FR-T6, FR-T7,
//! FR-A5, FR-S5):
//!
//! - Header: title, project, agent, account, model, status, the session's current computer
//!   (with a switch menu, FR-X3) and "IDE 열기" (Ember / VS Code / Gateway, FR-L7).
//! - Transcript in a `LazyColumn`, keyed by event sequence so a streaming reply growing at the
//!   bottom does not disturb the rows above: user and assistant messages, collapsible tool-call
//!   cards with their results, approval cards (allow once / always / deny), turn-ended and
//!   error rows.
//! - Composer: Enter sends, Shift+Enter is a new line. While the agent is busy a message is
//!   queued and delivered at the next turn boundary (FR-L6), and an interrupt button shows.
//! - Mentions (FR-T6): typing `@@` shows matching sessions in a list above the field.
//! - Team panel (FR-T7): when this session leads or belongs to a team, its members (with live
//!   status, open, end) and the shared task list, kept current by push.

#![allow(non_snake_case)]

use std::collections::HashSet;

use compose_rust::runtime::{
    MutableState, launched_effect, mutable_state_of, remember, remember_with_key,
};
// Ember's reading: a desktop window that has not reported its size yet reads its opened size
// (FR-L10.3).
use crate::ui::window_size::current_window_size;
use compose_rust::ui as cu;
use compose_rust::ui::Modifier;
use compose_rust::{
    Alignment, ColorRole, IconRole, Paint, ShapeRole, SpaceRole, TextAlign, TextOverflow, TypeRole,
};

use ember_client::api::{QuestionCallError, QuestionReply};
use ember_client::transcript::{ApprovalState, QuestionState, TranscriptItem};
use compose_rust::runtime::delay;
use std::time::Duration;
use ember_client::wire::{ApprovalDecision, MentionCandidate, Question, SessionPatch, TurnOutcome};
use ember_client::LauncherStatus;

use crate::ui::conversation::{
    ConversationDisplayItem, group_shell_calls, shell_group_key, shell_group_label,
};

pub fn item_key(item: &TranscriptItem) -> String {
    crate::ui::conversation::item_key(item)
}

use crate::ide::IdeKind;
use crate::model::{self, Tone};
use crate::server::{CurrentComputer, IdeLaunch};
use crate::services::{run_shared, services};
use crate::ui::coexist::{SharedUi, shared_ui};
use crate::ui::question::{self as qv, QuestionDraft};
use crate::ui::tool_card;
use crate::ui::Route;

use super::compat::{Badge, Banner, CodeText, MessageText, Spinner, StatusIndicator};
use super::components::{
    AppButton, BackButton, ButtonKind, Clickable, EmberCard, EmberField, FieldOptions, IconButton,
    Label, NameThenPill, NamedClickable, Pill, PillTone, PopKind, PopMenu, PopRow, PopoverMenu,
    Txt,
};
use super::ko;
use super::launcher::{ConnectionBanner, export, patch_session};
use super::notice;
use super::shell::{ICON_PLUS_SVG, NavIcon};
use super::theme::*;

pub const ICON_SHIELD_SVG: &str = include_str!("../../../assets/nav/shield.svg");
pub const ICON_TERMINAL_SVG: &str = include_str!("../../../assets/nav/terminal.svg");
pub const ICON_SEND_SVG: &str = include_str!("../../../assets/nav/send.svg");
/// The More menu's row icons, from the mockup's icon set (`icons.js` `star`, `arrow-up-tray`,
/// `zap`; the fork row uses the nav `git` icon).
const ICON_STAR_SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M11.48 3.499a.562.562 0 0 1 1.04 0l2.125 5.111a.563.563 0 0 0 .475.345l5.518.442c.499.04.701.663.321.988l-4.204 3.602a.563.563 0 0 0-.182.557l1.285 5.385a.562.562 0 0 1-.84.61l-4.725-2.885a.563.563 0 0 0-.586 0L6.982 20.54a.562.562 0 0 1-.84-.61l1.285-5.386a.562.562 0 0 0-.182-.557l-4.204-3.602a.562.562 0 0 1 .321-.988l5.518-.442a.563.563 0 0 0 .475-.345L11.48 3.5Z"/></svg>"#;
const ICON_EXPORT_SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M3 16.5v2.25A2.25 2.25 0 0 0 5.25 21h13.5A2.25 2.25 0 0 0 21 18.75V16.5m-13.5-9L12 3m0 0 4.5 4.5M12 3v13.5"/></svg>"#;
const ICON_ZAP_SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="m3.75 13.5 10.5-11.25L12 10.5h8.25L9.75 21.75 12 13.5H3.75Z"/></svg>"#;
const ICON_FORK_SVG: &str = include_str!("../../../assets/nav/git.svg");

/// The rows of the conversation's More menu (대화 메뉴, #888): each has an icon and a line that
/// says what it does, as the computer and IDE menus' rows do, so the rows are the menu's 55 dp
/// drop rows with the title and description centred in them.
pub(crate) fn more_pop_rows(pinned: bool, approve_all: bool) -> Vec<PopRow> {
    vec![
        PopRow {
            icon: Some(ICON_STAR_SVG),
            title: if pinned { "고정 해제" } else { "고정" }.into(),
            description: Some(if pinned { "고정 목록에서 이 대화를 뺍니다" } else { "이 대화를 고정 목록에 둡니다" }.into()),
            chosen: pinned,
            ..Default::default()
        },
        PopRow {
            icon: Some(ICON_EXPORT_SVG),
            title: "내보내기".into(),
            description: Some("대화를 JSON 파일로 데이터 폴더에 저장합니다".into()),
            ..Default::default()
        },
        PopRow {
            icon: Some(ICON_FORK_SVG),
            title: ko::FORK_MENU_LABEL.into(),
            description: Some("마지막으로 끝난 턴까지를 새 세션으로 엽니다".into()),
            ..Default::default()
        },
        PopRow {
            icon: Some(ICON_ZAP_SVG),
            title: ko::approve_all_menu_label(approve_all).into(),
            description: Some(
                if approve_all { "도구를 실행하기 전에 다시 확인을 받습니다" } else { "이 세션의 도구 실행을 묻지 않고 승인합니다" }.into(),
            ),
            chosen: approve_all,
            danger: !approve_all,
            ..Default::default()
        },
    ]
}

/// The height of a transcript summary row (mockup `.cmd`: `padding:3px 0` around a 24 dp pill).
pub const CMD_ROW_H: f32 = 30.0;
/// The gap between a summary row's icon, text and pills (`.cmd{gap:10px}`).
pub const CMD_ROW_GAP: f32 = 10.0;
/// The start inset of the lines under a summary row (`.out{padding:8px 0 8px 28px}`).
pub const OUT_INSET: f32 = 28.0;
/// The padding above and below those lines.
pub const OUT_PAD_V: f32 = 8.0;
/// The height one mono line of a file change is counted at when the transcript is measured.
const DIFF_LINE_H: f32 = 17.0;

/// The pending approval card's padding (`.approve{padding:14px var(--sp4)}`; in a pane
/// `.pane-b .approve{padding:10px 12px}`): above and below, then at the sides.
pub fn approve_pad(pane: bool) -> (f32, f32) {
    if pane { (10.0, 12.0) } else { (14.0, 16.0) }
}

/// The changed lines of a file-change card (mockup `session.css` `.out .add/.del`): monospace,
/// added lines in the green token and removed ones in the red token, 28 from the start with 8
/// above and below. Context lines are not drawn (the mockup's card lists `+` and `-` lines only).
fn DiffLines(d: &tool_card::FileDiff, cap: usize) {
    let p = ember_theme().palette;
    let changed = tool_card::changed_lines(d);
    let shown: Vec<(String, compose_rust::Color)> = changed
        .iter()
        .take(cap)
        .map(|l| {
            let col = match l {
                tool_card::DiffLine::Del(_) => p.red.strong,
                _ => p.green.strong,
            };
            (l.shown(), col)
        })
        .collect();
    let more = changed.len().saturating_sub(cap);
    { use ::compose_rust::ui::Row; Row!(modifier = Modifier::new().fill_max_width(), content = move || {
        { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(OUT_INSET)) };
        // Zero wide and weighted, so a long line is cut at the card's edge and does not widen it.
        { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().width(0.0).weight(1.0), content = move || {
            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().height(OUT_PAD_V)) };
            for (text, col) in shown.iter() {
                Txt(
                    text,
                    TypeRole::Mono,
                    Some(paint(*col)),
                    Some(1),
                    Some(TextOverflow::Ellipsis),
                    None,
                    None,
                );
            }
            if more > 0 {
                Label(&format!("{more}줄 더"), Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
            }
            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().height(OUT_PAD_V)) };
        }) };
    }) };
}

/// The height the transcript counts for a file-change card: its summary row, and, unless it is
/// folded, the changed lines (at most [`tool_card::MAX_DIFF_LINES`], then one "N줄 더" line)
/// with their padding.
pub fn file_change_height(changed: usize, folded: bool) -> f32 {
    if folded || changed == 0 {
        return CMD_ROW_H;
    }
    let lines = changed.min(tool_card::MAX_DIFF_LINES) + usize::from(changed > tool_card::MAX_DIFF_LINES);
    CMD_ROW_H + 2.0 * OUT_PAD_V + DIFF_LINE_H * lines as f32
}

/// A file change in the transcript (#535, mockup `session.js` `m.k==="edit"`): no card frame, a
/// summary row (the code glyph, the file name in the mono face, a green `+N` and a red `-N` pill
/// and a chevron at the end) over the changed lines. It is open until the row is pressed, as the
/// mockup's `<details open>`; the row is a button that says whether the lines are shown. While
/// the tool runs the glyph is a spinner, and a failed change says 오류 and shows the tool's
/// message under the lines.
fn FileChangeCard(
    d: &tool_card::FileDiff,
    running: bool,
    error: Option<&str>,
    folded: bool,
    on_toggle: impl FnMut() + 'static,
) {
    let p = ember_theme().palette;
    let (plus, minus) = tool_card::count_pills(d.added, d.removed);
    let failed = error.is_some();
    { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().fill_max_width(), content = move || {
        Clickable(
            Modifier::new()
                .fill_max_width()
                .height(CMD_ROW_H)
                .role(cu::Role::Button)
                .expanded(!folded)
                .click_action(true),
            Alignment::CenterStart,
            on_toggle,
            || {
                { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = CMD_ROW_GAP, modifier = Modifier::new().fill_max_width(), {
                        if running {
                            Spinner(12.0);
                        } else {
                            NavIcon(super::session::ICON_CODE_SVG, p.text_2.to_argb(), 16.0);
                        }
                        Txt(
                            &d.file,
                            TypeRole::Mono,
                            Some(paint(p.text_2)),
                            Some(1),
                            Some(TextOverflow::Ellipsis),
                            None,
                            None,
                        );
                        if let Some(t) = &plus {
                            Pill(t, PillTone::Green, false);
                        }
                        if let Some(t) = &minus {
                            Pill(t, PillTone::Red, false);
                        }
                        if failed {
                            Pill("오류", PillTone::Red, false);
                        }
                        // `.cmd .chev{margin-left:auto}`: the chevron sits at the row's end and
                        // points down while the lines are shown.
                        { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().weight(1.0)) };
                        NavIcon(
                            if folded { super::session::ICON_CHEVRON_RIGHT_SVG } else { super::shell::ICON_CHEVRON_DOWN_SVG },
                            p.muted.to_argb(),
                            16.0,
                        );
                    }) };
            },
        );
        if !folded {
            DiffLines(d, tool_card::MAX_DIFF_LINES);
            if let Some(e) = error.filter(|e| !e.trim().is_empty()) {
                CodeText(e, None, true);
            }
        }
    }) };
}

/// A resolved approval (#536, mockup `session.js`: a `.cmd` row, not a card): the shield, what
/// was asked in the mono face on one line, and the green or red pill saying what was decided.
fn ResolvedApprovalRow(body: &str, decision: ApprovalDecision) {
    let p = ember_theme().palette;
    let (txt, ok) = tool_card::resolved_label(decision);
    { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = CMD_ROW_GAP, modifier = Modifier::new().fill_max_width().height(CMD_ROW_H), {
            NavIcon(ICON_SHIELD_SVG, p.text_2.to_argb(), 16.0);
            // The pill right after the command (review st B, mockup `.cmd`); a long command is
            // cut and the pill keeps its own width.
            super::components::NameThenPillFace(body, Fs::Xs, Weight::Regular, Some(paint(p.text_2)), true, CMD_ROW_GAP, || {
                Pill(txt, if ok { PillTone::Green } else { PillTone::Red }, false);
            });
        }) };
}

/// One button of the pending approval card (mockup `.btn.sm`: 32 high, 12 at the sides, 13 px
/// medium; the first is `.btn.primary`, the others `surface` with a `line` border, 거부 in red).
fn ApprovalButton(text: &'static str, primary: bool, danger: bool, on_click: impl FnMut() + 'static) {
    let p = ember_theme().palette;
    let (fill, border, fg) = if primary {
        (p.ink, p.ink, p.ink_text)
    } else {
        (p.surface, p.line, if danger { p.red.strong } else { p.text })
    };
    Clickable(
        Modifier::new().height(H_CTL).background(paint(fill)).border(1.0, paint(border)).shape_role(ShapeRole::Full),
        Alignment::Center,
        on_click,
        move || {
            { use ::compose_rust::ui::Row; Row!(alignment = Alignment::Center, content = move || {
                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(SP[2])) };
                Label(text, Fs::Sm, Weight::Medium, Some(paint(fg)), Some(1));
                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(SP[2])) };
            }) };
        },
    );
}

/// Estimate the rendered height (dp) of a single transcript item so constrained viewports
/// can anchor to the latest messages at the bottom. v0.0.7: #385 fixed (`ScrollState::scroll_to`
/// and `animate_scroll_to`); workaround kept until checked on a display (scrolling to the end
/// needs the column's `max_offset` after layout, which must be tried on screen).
pub fn estimate_item_height(item: &TranscriptItem, open: bool) -> f32 {
    match item {
        TranscriptItem::User { .. } => 48.0,
        TranscriptItem::Assistant { .. } => 40.0,
        TranscriptItem::ToolCall { name, input, result, .. } => {
            let out = result.as_ref().map(|r| r.output.as_str()).unwrap_or("");
            if let Some(d) = tool_card::file_diff(name, input, out) {
                // For a file change the set holds the cards the person folded.
                return file_change_height(tool_card::changed_lines(&d).len(), open);
            }
            if open {
                180.0
            } else if result.as_ref().is_some_and(|r| !r.output.trim().is_empty()) {
                104.0
            } else {
                54.0
            }
        }
        TranscriptItem::Approval { state, .. } => match state {
            ApprovalState::Pending => 188.0,
            ApprovalState::Resolved(_) => CMD_ROW_H,
            _ => 120.0,
        },
        TranscriptItem::Question { questions, state, .. } => {
            qv::estimate_height(questions, matches!(state, QuestionState::Pending))
        }
        _ => 34.0,
    }
}

/// Return the starting index of `items` so the tail slice fits within `avail_h` dp.
pub fn tail_start_index(
    items: &[TranscriptItem],
    expanded: &HashSet<String>,
    avail_h: f32,
) -> usize {
    if items.len() <= 1 || avail_h <= 0.0 {
        return 0;
    }
    let budget = (avail_h - 16.0).max(160.0);
    let mut used = 0.0;
    let mut start = items.len();
    for (idx, item) in items.iter().enumerate().rev() {
        let open = match item {
            TranscriptItem::ToolCall { call_id, .. } => expanded.contains(call_id),
            _ => false,
        };
        let h = estimate_item_height(item, open) + 8.0;
        if used + h > budget && start < items.len() {
            break;
        }
        used += h;
        start = idx;
    }
    start
}

/// Estimate the rendered height (dp) of a [`ConversationDisplayItem`] (FR-L5, #1075).
pub fn estimate_display_item_height(
    item: &ConversationDisplayItem,
    expanded: &HashSet<String>,
) -> f32 {
    match item {
        ConversationDisplayItem::Single(inner) => {
            let open = match inner {
                TranscriptItem::ToolCall { call_id, .. } => expanded.contains(call_id),
                _ => false,
            };
            estimate_item_height(inner, open)
        }
        ConversationDisplayItem::ShellGroup(calls) => {
            let key = shell_group_key(calls);
            if !expanded.contains(&key) {
                CMD_ROW_H
            } else {
                let inner_h: f32 = calls
                    .iter()
                    .map(|c| {
                        let open = match c {
                            TranscriptItem::ToolCall { call_id, .. } => expanded.contains(call_id),
                            _ => false,
                        };
                        estimate_item_height(c, open) + SP[1]
                    })
                    .sum();
                CMD_ROW_H + SP[0] + inner_h
            }
        }
    }
}

/// Return the starting index of grouped `items` so the tail slice fits within `avail_h` dp
/// (FR-L5, #1075).
pub fn tail_display_start_index(
    items: &[ConversationDisplayItem],
    expanded: &HashSet<String>,
    avail_h: f32,
) -> usize {
    if items.len() <= 1 || avail_h <= 0.0 {
        return 0;
    }
    let budget = (avail_h - 16.0).max(160.0);
    let mut used = 0.0;
    let mut start = items.len();
    for (idx, item) in items.iter().enumerate().rev() {
        let h = estimate_display_item_height(item, expanded) + 8.0;
        if used + h > budget && start < items.len() {
            break;
        }
        used += h;
        start = idx;
    }
    start
}

fn demo_blocked_cr() -> bool {
    match crate::demo::unavailable(services().demo.is_some()) {
        Some(m) => {
            notice::post(m);
            true
        }
        None => false,
    }
}

/// Fork `id` after its completed turn `seq` (the last one when `None`) and open the new
/// session (FR-S5).
pub fn fork_from(ui: &SharedUi, id: String, seq: Option<i64>) {
    if demo_blocked_cr() {
        return;
    }
    let ui = ui.clone();
    run_shared(
        async move {
            services()
                .client
                .fork_session(&id, seq)
                .await
                .map_err(|e| e.to_string())
        },
        move |r| match r {
            Ok(rec) => ui.open_conversation(&rec.id),
            Err(e) => notice::post(format!("포크하지 못했습니다: {e}")),
        },
    );
}

/// Anchored popover menu for session actions (pin, export, fork, approve-all).
/// Used in the visible single-session topbar and in standalone conversation headers.
pub fn ConversationActionsMenu(id: &str) {
    let p = ember_theme().palette;
    let ui = shared_ui().clone();
    let s = services();
    let sid = id.to_string();
    let actions_menu = remember_with_key(format!("{sid}:actions_menu"), || {
        mutable_state_of(std::env::var("EMBER_CR_SESSION_MENU").as_deref() == Ok("more"))
    });

    let info = {
        let _ = ui.live.launcher.get();
        s.client.read(|st| {
            st.session(&sid)
                .map(|v| (v.record.title.clone(), v.record.pinned, st.approve_all(&sid)))
        })
    };
    let Some((title, pinned, approve_all)) = info else {
        return;
    };

    let more_rows = more_pop_rows(pinned, approve_all);
    let open = actions_menu.get();
    let dismiss = actions_menu;
    let toggle = actions_menu;
    let sid_pick = sid.clone();
    let title_pick = title;
    let ui_pick = ui;
    PopoverMenu(
        open,
        PopMenu {
            name: super::a11y::MORE,
            kind: PopKind::Drop,
            head: "대화 메뉴",
            sub: None,
            rows: &more_rows,
            align: compose_rust::PopoverAlign::End,
        },
        move || dismiss.set(false),
        move |idx| match idx {
            0 => patch_session(
                sid_pick.clone(),
                SessionPatch {
                    pinned: Some(!pinned),
                    ..Default::default()
                },
            ),
            1 => export(sid_pick.clone(), title_pick.clone()),
            2 => fork_from(&ui_pick, sid_pick.clone(), None),
            3 => {
                let id_ap = sid_pick.clone();
                let enable = !approve_all;
                run_shared(
                    async move { services().set_approve_all(&id_ap, enable).await },
                    |r| {
                        if let Err(e) = r {
                            notice::post(format!("모드를 바꾸지 못했습니다: {e}"));
                        }
                    },
                );
            }
            _ => {}
        },
        move || {
            NamedClickable(
                super::a11y::MORE,
                Modifier::new()
                    .size(H_CTL, H_CTL)
                    .background(paint(p.surface))
                    .border(1.0, paint(p.line))
                    .shape_role(ShapeRole::Full)
                    .expanded(open),
                Alignment::Center,
                move || toggle.set(!toggle.get_untracked()),
                move || NavIcon(super::session::ICON_MORE_H_SVG, p.text_2.to_argb(), 15.0),
            );
        },
        None,
    );
}

/// The computer menu's rows. The session's current computer is checked, as the IDE menu checks
/// its current IDE. Before the session's computer is known (and in the demo, which never asks),
/// the current one is the local computer, the same fallback the trigger label uses.
pub(crate) fn computer_pop_rows(computers: &[ember_client::state::Computer], current_id: Option<&str>) -> Vec<PopRow> {
    if computers.is_empty() {
        return vec![PopRow { title: "등록된 컴퓨터가 없습니다".into(), ..Default::default() }];
    }
    let current = current_id.unwrap_or("local");
    computers
        .iter()
        .map(|c| PopRow {
            title: c.name.clone(),
            description: Some(ko::reachability_label(c.reachability).to_string()),
            chosen: c.id == current,
            ..Default::default()
        })
        .collect()
}

/// Anchored popover menu for switching the session's computer.
/// Uses a compact 32 dp icon trigger in the single-session topbar and a labelled button in
/// standalone conversation headers.
pub fn SessionComputerMenu(id: &str, compact_trigger: bool) {
    let p = ember_theme().palette;
    let ui = shared_ui().clone();
    let s = services();
    let sid = id.to_string();
    let computer = remember_with_key(format!("{sid}:comp_cur"), || {
        mutable_state_of(None::<CurrentComputer>)
    });
    let computer_menu = remember_with_key(format!("{sid}:comp_menu"), || {
        mutable_state_of(std::env::var("EMBER_CR_SESSION_MENU").as_deref() == Ok("computer"))
    });

    {
        let sid_load = sid.clone();
        let comp_load = computer;
        launched_effect(sid.clone(), move || async move {
            if services().demo.is_some() {
                return;
            }
            let a = sid_load.clone();
            let comp = comp_load;
            run_shared(
                async move {
                    services()
                        .server
                        .session_computer(&a)
                        .await
                        .map_err(|e| e.to_string())
                },
                move |r| match r {
                    Ok(c) => comp.set(Some(c)),
                    Err(e) => tracing::debug!("current computer: {e}"),
                },
            );
        });
    }

    let (busy, computers) = {
        let _ = ui.live.launcher.get();
        s.client.read(|st| {
            let busy = st
                .session(&sid)
                .is_some_and(|v| model::is_busy(v.status));
            (busy, st.computers().to_vec())
        })
    };
    let current = computer.get();
    let current_computer_id = current.as_ref().map(|c| c.computer.id.clone());
    let computer_name = current
        .as_ref()
        .map(|c| c.computer.name.clone())
        .unwrap_or_else(|| "이 컴퓨터".into());

    let comp_rows = computer_pop_rows(&computers, current_computer_id.as_deref());

    let open = computer_menu.get();
    let dismiss = computer_menu;
    let toggle = computer_menu;
    let comp_state = computer;
    let comps_for_pick = computers;
    let sid_comp = sid;
    PopoverMenu(
        open,
        PopMenu {
            name: super::a11y::COMPUTER_PICKER,
            kind: PopKind::Mode,
            head: "컴퓨터",
            sub: None,
            rows: &comp_rows,
            align: if compact_trigger {
                compose_rust::PopoverAlign::End
            } else {
                compose_rust::PopoverAlign::Start
            },
        },
        move || dismiss.set(false),
        move |idx| {
            let Some(c) = comps_for_pick.get(idx) else {
                return;
            };
            if demo_blocked_cr() {
                return;
            }
            let cid = c.id.clone();
            let s_id = sid_comp.clone();
            let comp_out = comp_state;
            run_shared(
                async move {
                    services()
                        .server
                        .switch_computer(&s_id, &cid)
                        .await
                        .map_err(|e| e.to_string())
                },
                move |r| match r {
                    Ok(out) => {
                        let name = out.computer.name.clone();
                        let designated = comp_out
                            .get_untracked()
                            .and_then(|c| c.designated_computer_id);
                        comp_out.set(Some(CurrentComputer {
                            session_id: out.session_id,
                            computer: out.computer,
                            implicit: false,
                            notice_pending: false,
                            designated_computer_id: designated,
                        }));
                        notice::post(format!("{name} 컴퓨터로 전환했습니다"));
                    }
                    Err(e) => {
                        notice::post(format!("컴퓨터를 전환하지 못했습니다: {e}"))
                    }
                },
            );
        },
        move || {
            if compact_trigger {
                let tint = if busy {
                    p.muted.to_argb()
                } else {
                    p.text_2.to_argb()
                };
                NamedClickable(
                    super::a11y::COMPUTER_PICKER,
                    Modifier::new()
                        .size(H_CTL, H_CTL)
                        .background(paint(p.surface))
                        .border(1.0, paint(p.line))
                        .shape_role(ShapeRole::Full)
                        .expanded(open),
                    Alignment::Center,
                    move || {
                        if !busy {
                            toggle.set(!toggle.get_untracked());
                        }
                    },
                    move || NavIcon(super::session::ICON_PC_SVG, tint, 15.0),
                );
            } else {
                AppButton(
                    &computer_name,
                    None,
                    ButtonKind::Default,
                    true,
                    !busy,
                    false,
                    move || toggle.set(!toggle.get_untracked()),
                );
            }
        },
        None,
    );
}

/// The conversation view for `id` (FR-L5..FR-L7).
///
/// When `embedded` is `true` (inside `SessionScreen`'s single view), the top-right "IDE 열기"
/// menu is omitted because the session screen provides its own chooser in the top bar (FR-L18),
/// and the model/mode `ComposerChips` are shown above the composer (FR-L15, FR-L16).
pub fn ConversationView(id: &str, embedded: bool) {
    let theme = ember_theme();
    let p = theme.palette;
    let ui = shared_ui().clone();
    let window = current_window_size();
    let compact = window.is_compact();
    let s = services();
    let sid = id.to_string();

    // ---- header data -------------------------------------------------------------------------
    let header = {
        let _ = ui.live.launcher.get();
        s.client
            .read(|st| st.session(&sid).map(|v| (v.record.clone(), v.status)))
    };

    let computer = remember_with_key(sid.clone(), || mutable_state_of(None::<CurrentComputer>));
    let ide = remember_with_key(sid.clone(), || {
        mutable_state_of(None::<Result<IdeLaunch, String>>)
    });
    let candidates = remember_with_key(sid.clone(), || {
        mutable_state_of(Vec::<MentionCandidate>::new())
    });

    let load_candidates = {
        let sid = sid.clone();
        let candidates = candidates.clone();
        move || {
            if services().demo.is_some() {
                return;
            }
            let s_id = sid.clone();
            let c_state = candidates.clone();
            run_shared(
                async move {
                    services()
                        .client
                        .mention_candidates(&s_id, "", Some(200))
                        .await
                        .map_err(|e| e.to_string())
                },
                move |r| match r {
                    Ok(list) => c_state.set(list),
                    Err(e) => {
                        tracing::debug!("mention candidates: {e}");
                        c_state.set(Vec::new());
                    }
                },
            );
        }
    };

    let load = {
        let sid = sid.clone();
        let computer = computer.clone();
        let ide = ide.clone();
        let load_candidates = load_candidates.clone();
        move || {
            load_candidates();
            let t = sid.clone();
            run_shared(
                async move {
                    services()
                        .client
                        .load_team(&t)
                        .await
                        .map(|_| ())
                        .map_err(|e| e.to_string())
                },
                |r| {
                    if let Err(e) = r {
                        tracing::debug!("team: {e}");
                    }
                },
            );
            let fk = sid.clone();
            run_shared(
                async move {
                    services()
                        .client
                        .load_fork_info(&fk)
                        .await
                        .map_err(|e| e.to_string())
                },
                |r| {
                    if let Err(e) = r {
                        tracing::debug!("fork info: {e}");
                    }
                },
            );
            let ap = sid.clone();
            run_shared(
                async move {
                    services()
                        .client
                        .load_approve_all(&ap)
                        .await
                        .map(|_| ())
                        .map_err(|e| e.to_string())
                },
                |r| {
                    if let Err(e) = r {
                        tracing::debug!("approve-all: {e}");
                    }
                },
            );
            if services().demo.is_some() {
                let (proj, cwd) = services()
                    .client
                    .read(|st| {
                        st.session(&sid)
                            .map(|v| (v.record.project.clone(), v.record.cwd.clone()))
                    })
                    .unwrap_or_default();
                ide.set(Some(Ok(crate::demo::ide_launch(&sid, &proj, &cwd))));
                return;
            }
            let a = sid.clone();
            let comp = computer.clone();
            run_shared(
                async move {
                    services()
                        .server
                        .session_computer(&a)
                        .await
                        .map_err(|e| e.to_string())
                },
                move |r| match r {
                    Ok(c) => comp.set(Some(c)),
                    Err(e) => tracing::debug!("current computer: {e}"),
                },
            );
            let b = sid.clone();
            let ide_state = ide.clone();
            run_shared(
                async move {
                    services()
                        .server
                        .ide_targets(&b, None)
                        .await
                        .map_err(|e| e.to_string())
                },
                move |r| ide_state.set(Some(r)),
            );
        }
    };

    {
        let load_once = load.clone();
        launched_effect(sid.clone(), move || async move {
            load_once();
        });
    }

    // ---- composer state ----------------------------------------------------------------------
    let draft = remember_with_key(sid.clone(), || mutable_state_of(String::new()));
    let picks = remember_with_key(sid.clone(), || {
        mutable_state_of(Vec::<(String, String)>::new())
    });
    let composer_generation = remember_with_key(sid.clone(), || mutable_state_of(0_u64));
    let expanded = remember_with_key(sid.clone(), || mutable_state_of(HashSet::<String>::new()));
    let answering = remember_with_key(sid.clone(), || mutable_state_of(HashSet::<String>::new()));
    let ide_menu = remember_with_key(sid.clone(), || mutable_state_of(false));

    let send = {
        let sid = sid.clone();
        let draft = draft.clone();
        let picks = picks.clone();
        let composer_generation = composer_generation.clone();
        move |text: String| {
            let text = text.trim().to_string();
            if text.is_empty() {
                return;
            }
            let text = model::apply_mentions(&text, &picks.get_untracked());
            let s = services();
            s.outbox.lock().unwrap().push(&sid, text);
            s.bump_outbox();
            s.drive_outbox(&sid);
            draft.set(String::new());
            picks.set(Vec::new());
            composer_generation.update(|g| *g += 1);
        }
    };

    let Some((record, status)) = header else {
        { use ::compose_rust::ui::Column; Column!(alignment = Alignment::Center, space_role = SpaceRole::Md, modifier = Modifier::new()
                    .fill_max_width()
                    .fill_max_height()
                    .padding_role(SpaceRole::Md), {
                Txt(
                    "이 세션을 찾을 수 없습니다.",
                    TypeRole::Title,
                    None,
                    None,
                    None,
                    None,
                    None,
                );
                Txt(
                    "삭제되었거나 아직 목록을 불러오는 중입니다.",
                    TypeRole::Body,
                    Some(Paint::Role(ColorRole::OnSurfaceVariant)),
                    None,
                    None,
                    None,
                    None,
                );
                let u = ui.clone();
                AppButton(
                    "프로젝트로 돌아가기",
                    Some(IconRole::Back),
                    ButtonKind::Default,
                    false,
                    true,
                    false,
                    move || u.go(Route::Projects),
                );
            }) };
        return;
    };

    let title = record.title.clone();
    let last_seq = record.last_seq;
    let account = model::account_label(
        record.account_id.as_deref(),
        &model::account_labels(&ui.live.accounts.get()),
    );
    let busy = model::is_busy(status);
    let mut meta = vec![record.project.clone(), record.agent.clone()];
    meta.push(account.unwrap_or_else(|| "서버 로그인".into()));
    if let Some(m) = &record.model {
        meta.push(m.clone());
    }
    let meta = meta.join(" \u{00b7} ");
    let approve_all = {
        let _ = ui.live.launcher.get();
        s.client.read(|st| st.approve_all(&sid))
    };

    // ---- transcript ----------------------------------------------------------------------------
    let (items, keys, pending, has_completed_turn) = {
        let _ = ui.live.transcripts.get();
        s.client.read(|st| match st.transcript(&sid) {
            Some(t) => (
                t.items.clone(),
                t.items.iter().map(item_key).collect::<Vec<_>>(),
                qv::needs_you_count(t),
                t.items.iter().any(|i| {
                    matches!(
                        i,
                        TranscriptItem::TurnEnded {
                            outcome: TurnOutcome::Completed,
                            ..
                        }
                    )
                }),
            ),
            None => (Vec::new(), Vec::new(), (0, 0), false),
        })
    };
    // Review captures: `EMBER_CR_QUESTION=single|multi|secret|answered` adds a sample question.
    let (items, _keys) = match qv::debug_sample(
        std::env::var("EMBER_CR_QUESTION").ok().as_deref(),
        cfg!(debug_assertions),
        s.client.read(|st| st.transcript(&sid).map_or(0, |t| t.last_seq)) + 1_000_000,
    ) {
        Some(extra) => {
            let (mut i, mut k) = (items, keys);
            if qv::debug_first(std::env::var("EMBER_CR_QUESTION_FIRST").ok().as_deref(), cfg!(debug_assertions)) {
                k.insert(0, item_key(&extra));
                i.insert(0, extra);
            } else {
                k.push(item_key(&extra));
                i.push(extra);
            }
            (i, k)
        }
        None => (items, keys),
    };
    let count = items.len();

    // ---- fork (FR-S5) --------------------------------------------------------------------------
    let (fork_info, fork_parent) = {
        let _ = ui.live.launcher.get();
        s.client.read(|st| {
            let info = st.fork_info(&sid);
            let parent = info.origin.as_ref().map(|o| {
                (
                    o.clone(),
                    st.session(&o.parent_id).map(|v| v.record.title.clone()),
                )
            });
            (info, parent)
        })
    };
    let _fork_enabled = model::fork_menu_enabled(&fork_info, has_completed_turn);
    let fork_label = fork_parent
        .as_ref()
        .map(|(o, title)| ko::fork_origin_label(o, title.as_deref()));
    let fork_parent_id = fork_parent
        .as_ref()
        .map(|(o, _)| o.parent_id.clone())
        .unwrap_or_default();

    // ---- outbox --------------------------------------------------------------------------------
    let (queued, queue_error) = {
        let _ = ui.live.outbox.get();
        let o = s.outbox.lock().unwrap();
        (
            o.queued_texts(&sid),
            o.error(&sid).map(String::from).unwrap_or_default(),
        )
    };

    // ---- IDE entries ---------------------------------------------------------------------------
    // The session's project's pick, else its computer's default (FR-W7b, D56).
    let ide_project = super::session::session_project(&sid);
    let ide_prefs = ui.prefs.get();
    let remembered = ide_prefs.ide_for(
        &ide_project,
        computer.get().as_ref().map(|c| c.computer.id.as_str()),
    );
    let ide_choices = crate::ui::session::ide_choices(remembered);
    let ide_now = ide.get();
    let ide_rows = super::session::with_ember_notice(
        super::session::ide_pop_rows(&ide_choices, remembered),
        &ide_choices,
        ide_now.as_ref(),
    );
    let current_computer = computer.get();
    let ide_sub = crate::ui::session::ide_menu_subtitle(
        current_computer.as_ref().map(|c| c.computer.name.as_str()),
        &record.cwd,
    );

    // ---- mentions (FR-T6) ----------------------------------------------------------------------
    let cur_draft = draft.get();
    let typed = model::typed_mention(&cur_draft);
    let cur_picks = picks.get();
    let suggestions: Vec<(MentionCandidate, String)> = match &typed {
        Some(t) if !cur_picks.iter().any(|(raw, _)| *raw == t.raw) => {
            let all = candidates.get();
            model::filter_mentions(&all, &t.query, 6)
                .into_iter()
                .map(|c| (c.clone(), model::mention_token(c, &all)))
                .collect()
        }
        _ => Vec::new(),
    };
    let typed_raw = typed.map(|t| t.raw).unwrap_or_default();
    let picked: Vec<String> = cur_picks.iter().map(|(_, token)| token.clone()).collect();
    let on_draft = {
        let draft = draft.clone();
        let load_candidates = load_candidates.clone();
        move |v: String| {
            if model::typed_mention(&v).is_some_and(|t| t.query.is_empty())
                && !model::typed_mention(&draft.get_untracked()).is_some_and(|t| t.query.is_empty())
            {
                load_candidates();
            }
            draft.set(v);
        }
    };

    let agent = record.agent.clone();

    // `.detail{background:var(--surface)}`: the conversation sits on `surface`, not the window
    // `bg` (review SE9), so its cards and composer do not float on grey.
    { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().fill_max_width().fill_max_height().background(paint(p.surface)), {
            { use ::compose_rust::ui::Column; Column!(space_role = SpaceRole::Sm, modifier = Modifier::new()
                        .fill_max_width()
                        .fill_max_height()
                        .padding(if embedded { SP[2] } else { SP[3] }), {
                    // ---- header (hidden when embedded in SessionScreen SingleView, which has its own compact topbar) ----
                    if !embedded {
                        { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, space_role = SpaceRole::Sm, modifier = Modifier::new().fill_max_width(), {
                                if compact {
                                    let u = ui.clone();
                                    BackButton(move || u.go(Route::Projects));
                                }
                                { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().weight(1.0), {
                                        Label(
                                            &title,
                                            Fs::Lg,
                                            Weight::Semibold,
                                            Some(paint(p.text)),
                                            Some(1),
                                        );
                                        Label(
                                            &meta,
                                            Fs::Xs,
                                            Weight::Regular,
                                            Some(paint(p.muted)),
                                            Some(1),
                                        );
                                    }) };
                                StatusIndicator(status);
                                ConversationActionsMenu(&sid);
                            }) };
                    }

                    if let Some(text) = ko::approve_all_banner(approve_all) {
                        Banner(text, Tone::Warning);
                    }

                    if let Some(text) = fork_label.clone() {
                        { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, space_role = SpaceRole::Sm, modifier = Modifier::new().fill_max_width(), {
                                Label(
                                    &text,
                                    Fs::Xs,
                                    Weight::Regular,
                                    Some(paint(p.muted)),
                                    Some(1),
                                );
                                let u = ui.clone();
                                let pid = fork_parent_id.clone();
                                AppButton(
                                    "원본 열기",
                                    None,
                                    ButtonKind::Default,
                                    true,
                                    true,
                                    false,
                                    move || u.open_conversation(&pid),
                                );
                            }) };
                    }

                    if !embedded {
                        // Computer and IDE row
                        { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, space_role = SpaceRole::Sm, modifier = Modifier::new().fill_max_width(), {
                                Label(
                                    "컴퓨터",
                                    Fs::Xs,
                                    Weight::Regular,
                                    Some(paint(p.muted)),
                                    Some(1),
                                );
                                SessionComputerMenu(&sid, false);
                                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().weight(1.0)) };
                                if let Some(label) = qv::needs_you_label(pending) {
                                    Badge(&label, Tone::Warning);
                                }
                                let ide_dismiss = ide_menu;
                                let ide_choices_pick = ide_choices.clone();
                                let ui_ide = ui.clone();
                                let pick_project = ide_project.clone();
                                let launch_res = ide_now.clone();
                                let im = ide_menu;
                                let ui_ide_btn = ui.clone();
                                let launch_btn = ide_now.clone();
                                let chev = ide_menu;
                                PopoverMenu(
                                    ide_menu.get(),
                                    PopMenu {
                                        name: super::a11y::IDE_PICKER,
                                        kind: PopKind::Drop,
                                        head: crate::ui::session::IDE_MENU_HEAD,
                                        sub: ide_sub.as_deref(),
                                        rows: &ide_rows,
                                        align: compose_rust::PopoverAlign::End,
                                    },
                                    move || ide_dismiss.set(false),
                                    move |idx| {
                                        let Some(choice) = ide_choices_pick.get(idx) else {
                                            return;
                                        };
                                        let kind = choice.kind;
                                        super::session::record_ide_pick(&ui_ide, &pick_project, kind);
                                        super::session::launch_ide_shared(&ui_ide, kind, launch_res.clone());
                                    },
                                    move || {
                                        { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, space_role = SpaceRole::Sm, content = move || {
                                            AppButton(
                                                "IDE 열기",
                                                Some(IconRole::Forward),
                                                ButtonKind::Default,
                                                true,
                                                true,
                                                false,
                                                move || {
                                                    super::session::ide_main_click(remembered, &im, |kind| {
                                                        super::session::launch_ide_shared(
                                                            &ui_ide_btn,
                                                            kind,
                                                            launch_btn.clone(),
                                                        )
                                                    })
                                                },
                                            );
                                            super::session::IdeChevron(chev, false);
                                        }) };
                                    },
                                    None,
                                );
                            }) };
                    }

                    ConnectionBanner();

                    if busy && !embedded {
                        { use ::compose_rust::ui::IndeterminateProgressIndicator; IndeterminateProgressIndicator!(circular = false, modifier = Modifier::new().fill_max_width()) };
                    }

                    TeamPanel(&sid);

                    // ---- transcript ------------------------------------------------------------
                    { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().fill_max_width().weight(1.0), {
                            if count == 0 {
                                { use ::compose_rust::ui::Box; Box!(alignment = Alignment::Center, modifier = Modifier::new().fill_max_width().fill_max_height(), {
                                        let empty_msg =
                                            empty_transcript_text(status, last_seq);
                                        Label(
                                            empty_msg,
                                            Fs::Md,
                                            Weight::Regular,
                                            Some(paint(p.muted)),
                                            None,
                                        );
                                    }) };
                            } else {
                                let avail_h = window.height_dp
                                    - (if embedded {
                                        60.0 + 52.0 + 110.0
                                    } else {
                                        150.0 + 90.0
                                    })
                                    - (if window.width_dp <= 560.0 { 112.0 } else { 0.0 });
                                let exp_set = expanded.get();
                                let display_items = group_shell_calls(&items);
                                let start_idx = tail_display_start_index(&display_items, &exp_set, avail_h);
                                let items_row = display_items[start_idx..].to_vec();
                                let sid_row = sid.clone();
                                let agent_row = agent.clone();
                                let exp_row = expanded.clone();
                                let ans_row = answering.clone();
                                let computer_row: Option<String> =
                                    computer.get().map(|c| c.computer.name.clone());
                                { use ::compose_rust::ui::ScrollColumn; ScrollColumn!(modifier = Modifier::new().fill_max_width().fill_max_height(), content = move || {
                                        { use ::compose_rust::ui::Column; Column!(spacing = SP[1], modifier = Modifier::new()
                                                    .fill_max_width()
                                                    .padding(SP[1]), content = move || {
                                                for item in items_row.iter() {
                                                    TranscriptDisplayRow(
                                                        &sid_row,
                                                        &agent_row,
                                                        item,
                                                        &exp_row,
                                                        &ans_row,
                                                        computer_row.as_deref(),
                                                        false,
                                                    );
                                                }
                                                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().height(12.0)) };
                                            }) };
                                    }) };
                            }
                        }) };

                    // ---- queue (FR-L6) ---------------------------------------------------------
                    if !queued.is_empty() {
                        { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, space_role = SpaceRole::Sm, modifier = Modifier::new().fill_max_width(), {
                                Badge(&format!("{}개 대기 중", queued.len()), Tone::Accent);
                                { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().weight(1.0), {
                                    Label(
                                        &format!(
                                            "이번 턴이 끝나면 순서대로 전송합니다. 다음: {}",
                                            model::one_line(&queued[0], 60)
                                        ),
                                        Fs::Xs,
                                        Weight::Regular,
                                        Some(paint(p.muted)),
                                        Some(1),
                                    );
                                }) };
                                let sid_clear = sid.clone();
                                AppButton(
                                    "비우기",
                                    None,
                                    ButtonKind::Default,
                                    true,
                                    true,
                                    false,
                                    move || {
                                        services().outbox.lock().unwrap().clear(&sid_clear);
                                        services().bump_outbox();
                                    },
                                );
                            }) };
                    }
                    if !queue_error.is_empty() {
                        { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, space_role = SpaceRole::Sm, modifier = Modifier::new().fill_max_width(), {
                                { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().weight(1.0), {
                                    Banner(
                                        &format!("전송하지 못했습니다: {queue_error}"),
                                        Tone::Error,
                                    );
                                }) };
                                let sid_retry = sid.clone();
                                AppButton(
                                    "다시 시도",
                                    None,
                                    ButtonKind::Default,
                                    true,
                                    true,
                                    false,
                                    move || services().drive_outbox(&sid_retry),
                                );
                            }) };
                    }

                    // ---- mention suggestions (FR-T6) -------------------------------------------
                    if !suggestions.is_empty() {
                        EmberCard(false, true, None, Some(SP[1]), None, || {
                            { use ::compose_rust::ui::Column; Column!(space_role = SpaceRole::Xs, modifier = Modifier::new().fill_max_width(), {
                                    Label(
                                        "세션 멘션",
                                        Fs::Xs,
                                        Weight::Regular,
                                        Some(paint(p.muted)),
                                        Some(1),
                                    );
                                    for (c, token) in suggestions.iter().cloned() {
                                        let raw = typed_raw.clone();
                                        let picks_c = picks.clone();
                                        AppButton(
                                            &format!(
                                                "{} \u{00b7} {} \u{00b7} {}",
                                                c.title, c.agent, c.project
                                            ),
                                            None,
                                            ButtonKind::Default,
                                            true,
                                            true,
                                            true,
                                            move || {
                                                picks_c.update(|v| {
                                                    v.push((raw.clone(), token.clone()))
                                                });
                                            },
                                        );
                                    }
                                }) };
                        });
                    }
                    if !picked.is_empty() {
                        { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, space_role = SpaceRole::Sm, modifier = Modifier::new().fill_max_width(), {
                                { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().weight(1.0), {
                                    Label(
                                        &format!("멘션 대상: {}", picked.join(", ")),
                                        Fs::Xs,
                                        Weight::Regular,
                                        Some(paint(p.muted)),
                                        Some(1),
                                    );
                                }) };
                                let picks_clear = picks.clone();
                                AppButton(
                                    "지우기",
                                    None,
                                    ButtonKind::Default,
                                    true,
                                    true,
                                    false,
                                    move || picks_clear.set(Vec::new()),
                                );
                            }) };
                    }

                    // ---- composer --------------------------------------------------------------
                    let _gen = composer_generation.get();
                    if embedded {
                        let nav_w = if window.width_dp >= 1280.0 {
                            SIDE_W
                        } else if window.width_dp > 560.0 {
                            RAIL_W
                        } else {
                            0.0
                        };
                        let side_w = if window.width_dp >= 900.0 {
                            crate::ui::session::SIDE_LIST_W
                        } else if window.width_dp > 560.0 {
                            64.0
                        } else {
                            0.0
                        };
                        let rpanel_w = if window.width_dp >= 840.0 {
                            crate::ui::session::INFO_PANEL_W
                        } else {
                            0.0
                        };
                        let avail_w =
                            (window.width_dp - nav_w - side_w - rpanel_w - 32.0).max(260.0);
                        let comp_w = avail_w.min(740.0);
                        let is_phone = window.width_dp <= 560.0;

                        { use ::compose_rust::ui::Box; Box!(alignment = Alignment::Center, modifier = Modifier::new().fill_max_width(), {
                                { use ::compose_rust::ui::Column; Column!(spacing = SP[0], modifier = Modifier::new()
                                            .width(comp_w)
                                            .background(paint(p.surface))
                                            .border(1.0, paint(p.line))
                                            .corner_radius(CORNER_XL)
                                            .padding(SP[2]), {
                                        let send_sub = send.clone();
                                        EmberField(FieldOptions {
                                            bare: true,
                                            // `.composer` padding 14: the card's 12 plus 2
                                            // (FR-L10.2, #844; 4 put the text 16 px in).
                                            inset: Some((2.0, 0.0)),
                                            fill_max_width: true,
                                            multiline: false,
                                            height: Some(36.0),
                                            placeholder: if busy {
                                                "다음 턴에 보낼 메시지를 입력하세요".to_string()
                                            } else if is_phone {
                                                "무엇이든 물어보세요".to_string()
                                            } else {
                                                "무엇이든 물어보세요. @로 파일, /로 명령"
                                                    .to_string()
                                            },
                                            on_value_change: Some(Box::new(on_draft.clone())),
                                            on_submit: Some(Box::new(move |v| send_sub(v))),
                                            ..Default::default()
                                        });
                                        { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = 6.0, modifier = Modifier::new().fill_max_width(), {
                                                Clickable(
                                                    Modifier::new()
                                                        .size(30.0, 30.0)
                                                        .background(paint(p.surface_2))
                                                        .shape_role(ShapeRole::Full),
                                                    Alignment::Center,
                                                    || notice::post("파일 첨부는 준비 중입니다"),
                                                    || {
                                                        NavIcon(
                                                            ICON_PLUS_SVG,
                                                            p.text_2.to_argb(),
                                                            16.0,
                                                        )
                                                    },
                                                );
                                                // The chips share what the row leaves after the
                                                // attach and send buttons and shorten their labels
                                                // (ellipsis) in it, so the send or stop button keeps
                                                // its size at 360 dp (FR-L28.3, Flip 4 review 3.5).
                                                { use ::compose_rust::ui::Box; Box!(alignment = Alignment::CenterStart, modifier = Modifier::new().weight(1.0), content = || super::session::ModeChip(&sid, is_phone)) };
                                                { use ::compose_rust::ui::Box; Box!(alignment = Alignment::CenterEnd, modifier = Modifier::new().weight(1.0), content = || super::session::ModelChip(&sid, is_phone)) };
                                                if busy {
                                                    let sid_int = sid.clone();
                                                    super::session::StopCircleButton(move || {
                                                        if demo_blocked_cr() {
                                                            return;
                                                        }
                                                        let id_c = sid_int.clone();
                                                        run_shared(
                                                            async move {
                                                                services()
                                                                    .client
                                                                    .interrupt(&id_c)
                                                                    .await
                                                                    .map_err(|e| e.to_string())
                                                            },
                                                            |r| {
                                                                if let Err(e) = r {
                                                                    notice::post(format!(
                                                                        "중단하지 못했습니다: {e}"
                                                                    ));
                                                                }
                                                            },
                                                        );
                                                    });
                                                }
                                                let send_btn = send.clone();
                                                let draft_btn = draft.clone();
                                                super::session::SendCircleButton(
                                                    32.0,
                                                    move || {
                                                        send_btn(draft_btn.get_untracked());
                                                    },
                                                );
                                            }) };
                                    }) };
                            }) };
                    } else {
                        { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, space_role = SpaceRole::Xs, modifier = Modifier::new()
                                    .fill_max_width()
                                    .background(paint(p.surface))
                                    .border(1.0, paint(p.line))
                                    .corner_radius(CORNER_XL)
                                    .padding(SP[1]), {
                                let send_sub = send.clone();
                                EmberField(FieldOptions {
                                    bare: true,
                                    inset: Some((8.0, 8.0)),
                                    weight: Some(1.0),
                                    multiline: true,
                                    placeholder: if busy {
                                        "다음 턴에 보낼 메시지를 입력하세요".to_string()
                                    } else {
                                        "메시지를 입력하세요 (@@로 세션 멘션)".to_string()
                                    },
                                    on_value_change: Some(Box::new(on_draft.clone())),
                                    on_submit: Some(Box::new(move |v| send_sub(v))),
                                    ..Default::default()
                                });
                                if busy {
                                    let sid_int = sid.clone();
                                    AppButton(
                                        if compact { "" } else { "중단" },
                                        Some(IconRole::Close),
                                        ButtonKind::Default,
                                        true,
                                        true,
                                        false,
                                        move || {
                                            if demo_blocked_cr() {
                                                return;
                                            }
                                            let id_c = sid_int.clone();
                                            run_shared(
                                                async move {
                                                    services()
                                                        .client
                                                        .interrupt(&id_c)
                                                        .await
                                                        .map_err(|e| e.to_string())
                                                },
                                                |r| {
                                                    if let Err(e) = r {
                                                        notice::post(format!(
                                                            "중단하지 못했습니다: {e}"
                                                        ));
                                                    }
                                                },
                                            );
                                        },
                                    );
                                }
                                let send_btn = send.clone();
                                let draft_btn = draft.clone();
                                IconButton(IconRole::Send, true, move || {
                                    send_btn(draft_btn.get_untracked());
                                });
                            }) };
                    }
                }) };
        }) };
}

/// A radio or check row of one option (single or multi select), with its description.
fn OptionRow(label: &str, description: Option<&str>, chosen: bool, multi: bool, enabled: bool, on_click: impl FnMut() + 'static) {
    let p = ember_theme().palette;
    let mark = match (multi, chosen) {
        (false, true) => "\u{25CF}",
        (false, false) => "\u{25CB}",
        (true, true) => "\u{25A0}",
        (true, false) => "\u{25A1}",
    };
    let label = label.to_string();
    let description = description.map(str::to_string);
    let mut on_click = on_click;
    Clickable(
        Modifier::new()
            .fill_max_width()
            .padding(6.0)
            .background(paint(if chosen { p.sel } else { p.surface }))
            .border(1.0, paint(p.line))
            .corner_radius(CORNER_SM),
        Alignment::CenterStart,
        move || {
            if enabled {
                on_click();
            }
        },
        move || {
            { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = SP[1], modifier = Modifier::new().fill_max_width(), content = move || {
                    Label(mark, Fs::Md, Weight::Regular, Some(paint(if enabled { p.text } else { p.muted })), Some(1));
                    { use ::compose_rust::ui::Column; Column!(spacing = 2.0, modifier = Modifier::new().weight(1.0), content = move || {
                        Label(&label, Fs::Sm, Weight::Medium, Some(paint(if enabled { p.text } else { p.muted })), Some(3));
                        if let Some(d) = &description {
                            Label(d, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(3));
                        }
                    }) };
                }) };
        },
    );
}

/// A pill button whose text is on the same step as the option rows (Fs::Sm).
fn QuestionButton(text: &str, primary: bool, enabled: bool, on_click: impl FnMut() + 'static) {
    let p = ember_theme().palette;
    let (fill, border, fg) = match (primary, enabled) {
        (true, true) => (p.ink, p.ink, p.ink_text),
        (_, false) => (p.surface, p.line, p.muted),
        (false, true) => (p.surface, p.line, p.text),
    };
    let text = text.to_string();
    let mut on_click = on_click;
    Clickable(
        Modifier::new().height(super::theme::H_CTL).background(paint(fill)).border(1.0, paint(border)).shape_role(ShapeRole::Full),
        Alignment::Center,
        move || {
            if enabled {
                on_click();
            }
        },
        move || {
            { use ::compose_rust::ui::Row; Row!(alignment = Alignment::Center, content = move || {
                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(14.0)) };
                Label(&text, Fs::Sm, Weight::Medium, Some(paint(fg)), Some(1));
                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(14.0)) };
            }) };
        },
    );
}

/// The agent's question (FR-T11, FR-A5): the questions with their options, 보내기 and 건너뛰기.
/// A secret question is never typed here (compose-rust has no masked field): the card says it
/// is answered on the computer's own terminal and offers only 건너뛰기. The draft is a local
/// signal, never in the transcript, a log or `Debug`; the server's error text is never shown.
pub fn QuestionCard(
    session: &str,
    question_id: String,
    questions: Vec<Question>,
    state: QuestionState,
    answering: &MutableState<HashSet<String>>,
) {
    let p = ember_theme().palette;
    let pending = state == QuestionState::Pending;
    let key = format!("{session}/{question_id}");
    let draft = remember_with_key(key.clone(), || mutable_state_of(QuestionDraft::default()));
    let sent = remember_with_key(key.clone(), || mutable_state_of(false));
    let failed = remember_with_key(key.clone(), || mutable_state_of(None::<QuestionCallError>));
    let skip_step = remember_with_key(key.clone(), || mutable_state_of(qv::SkipStep::Idle));
    let confirming = skip_step.get() == qv::SkipStep::Confirm;
    {
        // The first tap on 건너뛰기 is forgotten after a few seconds.
        let step = skip_step.clone();
        launched_effect(confirming, move || async move {
            if confirming {
                delay(Duration::from_secs(qv::SKIP_CONFIRM_SECS)).await;
                step.set(qv::SkipStep::Idle.reset());
            }
        });
    }
    let flight_key = format!("q:{question_id}");
    let in_flight = answering.get().contains(&flight_key);
    let dead = failed.get().is_some_and(QuestionCallError::is_terminal);
    let locked = in_flight || sent.get() || dead;
    let secret = qv::has_secret(&questions);
    let can_send = pending && !locked && qv::can_submit(&questions, &draft.get());
    let sample = question_id.starts_with("demo-question");

    let send = {
        let sid = session.to_string();
        let qid = question_id.clone();
        let questions = questions.clone();
        let answering = answering.clone();
        let (draft, sent, failed) = (draft.clone(), sent.clone(), failed.clone());
        move |skip: bool| {
            if answering.get_untracked().contains(&flight_key) || sent.get_untracked() {
                return;
            }
            if qid == qv::DEBUG_SAMPLE_ID {
                // The capture sample calls nothing.
                sent.set(true);
                return;
            }
            let reply = if skip {
                QuestionReply::Skip
            } else {
                match qv::reply(&questions, &draft.get_untracked()) {
                    Some(r) => r,
                    None => return,
                }
            };
            answering.update(|set| {
                set.insert(flight_key.clone());
            });
            failed.set(None);
            let (s_id, q_id) = (sid.clone(), qid.clone());
            let (done_key, answering_done) = (flight_key.clone(), answering.clone());
            let (draft, sent, failed) = (draft.clone(), sent.clone(), failed.clone());
            run_shared(
                async move { services().answer_question(&s_id, &q_id, &reply).await },
                move |r: Result<(), QuestionCallError>| {
                    answering_done.update(|set| {
                        set.remove(&done_key);
                    });
                    match r {
                        Ok(()) => {
                            draft.set(QuestionDraft::default());
                            sent.set(true);
                        }
                        Err(e) => failed.set(Some(e)),
                    }
                },
            );
        }
    };
    let (send_ok, send_skip) = (send.clone(), send);
    let tap_skip = {
        let step = skip_step.clone();
        move || {
            let (next, go) = step.get_untracked().tap();
            step.set(next);
            if go {
                send_skip(true);
            }
        }
    };

    let border = if pending { Some(p.orange.strong) } else { None };
    EmberCard(false, true, None, Some(SP[2]), border, move || {
        { use ::compose_rust::ui::Column; Column!(spacing = SP[1], modifier = Modifier::new().fill_max_width(), content = move || {
                { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = SP[1], modifier = Modifier::new().fill_max_width(), content = move || {
                        NavIcon(ICON_SHIELD_SVG, p.orange.strong.to_argb(), 16.0);
                        Label(qv::CARD_TITLE, Fs::Md, Weight::Semibold, None, Some(1));
                        { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().weight(1.0)) };
                        if pending {
                            Pill(qv::NEEDS_ANSWER, PillTone::Orange, true);
                        }
                    }) };
                if sample {
                    Label(qv::SAMPLE_NOTE, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(2));
                }
                match state {
                    QuestionState::Pending => {
                        for q in questions.iter().cloned() {
                            let d = draft.clone();
                            let ss = skip_step.clone();
                            { use ::compose_rust::ui::Column; Column!(spacing = 6.0, modifier = Modifier::new().fill_max_width(), content = move || {
                                    if let Some(h) = q.header.as_deref().map(qv::shown_header).filter(|h| !h.is_empty()) {
                                        Label(&h, Fs::Xs, Weight::Semibold, Some(paint(p.muted)), Some(1));
                                    }
                                    Label(&qv::shown(&q.text), Fs::Sm, Weight::Medium, None, Some(6));
                                    if q.secret {
                                        Label(qv::SECRET_ON_TERMINAL, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(3));
                                        Label(qv::SECRET_ONLY_SKIP, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(2));
                                        return;
                                    }
                                    for o in q.options.iter() {
                                        let (d2, q2, label) = (d.clone(), q.clone(), o.label.clone());
                                        let step2 = ss.clone();
                                        let desc = o.description.as_deref().map(qv::shown).filter(|s| !s.is_empty());
                                        OptionRow(
                                            &qv::shown(&o.label),
                                            desc.as_deref(),
                                            d.get().picked(&q.id, &o.label),
                                            q.multi_select,
                                            !locked,
                                            move || {
                                                step2.set(qv::SkipStep::Idle.reset());
                                                d2.update(|v| *v = qv::toggle(std::mem::take(v), &q2, &label))
                                            },
                                        );
                                    }
                                    if q.allow_other {
                                        let (d2, q2) = (d.clone(), q.clone());
                                        let step2 = ss.clone();
                                        EmberField(FieldOptions {
                                            fill_max_width: true,
                                            placeholder: qv::OTHER_PLACEHOLDER.to_string(),
                                            enabled: Some(!locked),
                                            on_value_change: Some(Box::new(move |text: String| {
                                                step2.set(qv::SkipStep::Idle.reset());
                                                d2.update(|v| *v = qv::set_other(std::mem::take(v), &q2, &text))
                                            })),
                                            ..Default::default()
                                        });
                                    }
                                }) };
                        }
                        { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = 6.0, modifier = Modifier::new().fill_max_width(), content = move || {
                                // Same text step as the options (Fs::Sm), not the toolkit button's.
                                if !secret {
                                    QuestionButton(qv::SEND_LABEL, true, can_send, move || send_ok(false));
                                }
                                QuestionButton(skip_step.get().label(), false, pending && !locked, tap_skip);
                                if in_flight {
                                    Spinner(14.0);
                                }
                            }) };
                        if confirming {
                            Label(qv::SKIP_CONFIRM_NOTE, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(3));
                        }
                        if sent.get() {
                            Label(qv::SENT_WAITING, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(2));
                        }
                        if let Some(e) = failed.get() {
                            Label(e.text_ko(), Fs::Xs, Weight::Medium, Some(paint(p.red.strong)), Some(3));
                        }
                    }
                    QuestionState::Answered(answers) => {
                        for line in qv::answered_lines(&questions, &answers) {
                            Label(&line, Fs::Xs, Weight::Medium, Some(paint(p.green.strong)), Some(4));
                        }
                    }
                    QuestionState::Skipped => {
                        Label(qv::SKIPPED_LINE, Fs::Xs, Weight::Medium, Some(paint(p.muted)), Some(1));
                    }
                    QuestionState::Abandoned => {
                        Label(qv::ABANDONED_LINE, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                    }
                    QuestionState::Withdrawn => {
                        Label(qv::WITHDRAWN_LINE, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(2));
                    }
                }
            }) };
    });
}

/// A collapsible summary row for 2 or more consecutive shell tool calls (FR-L5, #1075).
///
/// Collapsed by default (`명령 N개 실행`); pressing the row toggles expansion and renders each
/// individual shell tool call inside.
pub fn ShellGroupRow(
    session: &str,
    agent: &str,
    calls: &[TranscriptItem],
    expanded: &MutableState<HashSet<String>>,
    answering: &MutableState<HashSet<String>>,
    computer: Option<&str>,
    pane: bool,
) {
    let p = ember_theme().palette;
    let group_key = shell_group_key(calls);
    let open = expanded.get().contains(&group_key);
    let label = shell_group_label(calls.len());
    let any_running = calls
        .iter()
        .any(|c| matches!(c, TranscriptItem::ToolCall { result: None, .. }));
    let any_error = calls.iter().any(|c| {
        matches!(c, TranscriptItem::ToolCall { result: Some(r), .. } if r.is_error)
    });
    let exp_toggle = expanded.clone();
    let key_toggle = group_key.clone();
    let calls_owned = calls.to_vec();
    let sid = session.to_string();
    let agent_name = agent.to_string();
    let exp_children = expanded.clone();
    let ans_children = answering.clone();
    let computer_owned = computer.map(str::to_string);

    { use ::compose_rust::ui::Column; Column!(spacing = SP[0], modifier = Modifier::new().fill_max_width(), content = move || {
        let label_row = label.clone();
        Clickable(
            Modifier::new()
                .fill_max_width()
                .height(CMD_ROW_H)
                .role(cu::Role::Button)
                .expanded(open)
                .click_action(true),
            Alignment::CenterStart,
            move || {
                exp_toggle.update(|set| {
                    if !set.remove(&key_toggle) {
                        set.insert(key_toggle.clone());
                    }
                });
            },
            move || {
                { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = CMD_ROW_GAP, modifier = Modifier::new().fill_max_width(), content = move || {
                        if any_running {
                            Spinner(12.0);
                        } else {
                            NavIcon(ICON_TERMINAL_SVG, p.text_2.to_argb(), 16.0);
                        }
                        Label(
                            &label_row,
                            Fs::Sm,
                            Weight::Medium,
                            Some(paint(p.text_2)),
                            Some(1),
                        );
                        if any_error {
                            Pill("오류", PillTone::Red, false);
                        }
                        { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().weight(1.0)) };
                        NavIcon(
                            if open {
                                super::shell::ICON_CHEVRON_DOWN_SVG
                            } else {
                                super::session::ICON_CHEVRON_RIGHT_SVG
                            },
                            p.muted.to_argb(),
                            16.0,
                        );
                    }) };
            },
        );
        if open {
            { use ::compose_rust::ui::Row; Row!(modifier = Modifier::new().fill_max_width(), content = move || {
                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(SP[2])) };
                { use ::compose_rust::ui::Column; Column!(spacing = SP[1], modifier = Modifier::new().width(0.0).weight(1.0), content = move || {
                    for call in calls_owned.iter() {
                        TranscriptRow(
                            &sid,
                            &agent_name,
                            call,
                            &exp_children,
                            &ans_children,
                            computer_owned.as_deref(),
                            pane,
                        );
                    }
                }) };
            }) };
        }
    }) };
}

/// Render one [`ConversationDisplayItem`]: either a single transcript row or a collapsible
/// group of consecutive shell calls (FR-L5, #1075).
pub fn TranscriptDisplayRow(
    session: &str,
    agent: &str,
    item: &ConversationDisplayItem,
    expanded: &MutableState<HashSet<String>>,
    answering: &MutableState<HashSet<String>>,
    computer: Option<&str>,
    pane: bool,
) {
    match item {
        ConversationDisplayItem::Single(inner) => {
            TranscriptRow(session, agent, inner, expanded, answering, computer, pane);
        }
        ConversationDisplayItem::ShellGroup(calls) => {
            ShellGroupRow(session, agent, calls, expanded, answering, computer, pane);
        }
    }
}

/// One transcript entry (FR-L5..FR-L7, FR-S5).
pub fn TranscriptRow(
    session: &str,
    _agent: &str,
    item: &TranscriptItem,
    expanded: &MutableState<HashSet<String>>,
    answering: &MutableState<HashSet<String>>,
    computer: Option<&str>,
    pane: bool,
) {
    let theme = ember_theme();
    let p = theme.palette;
    let sid = session.to_string();

    match item.clone() {
        TranscriptItem::User { text, .. } => {
            { use ::compose_rust::ui::Box; Box!(alignment = Alignment::CenterEnd, modifier = Modifier::new().fill_max_width(), content = move || {
                    { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new()
                                .background(paint(p.hover))
                                .corner_radius(CORNER_BUBBLE)
                                .padding(10.0), content = move || {
                            MessageText(&text, None);
                        }) };
                }) };
        }
        TranscriptItem::Assistant {
            text, streaming, ..
        } => {
            { use ::compose_rust::ui::Box; Box!(alignment = Alignment::CenterStart, modifier = Modifier::new().fill_max_width().padding(2.0), content = move || {
                    { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = SP[1], content = move || {
                            let shown = if streaming {
                                format!("{text}\u{2589}")
                            } else {
                                text.clone()
                            };
                            MessageText(&shown, None);
                            if streaming {
                                Spinner(10.0);
                            }
                        }) };
                }) };
        }
        TranscriptItem::ToolCall {
            call_id,
            name,
            input,
            result,
            ..
        } => {
            let open = expanded.get().contains(&call_id);
            let summary = model::json_summary(&input, 100);
            let exp_toggle = expanded.clone();
            let cid_toggle = call_id.clone();
            let (state_text, tone) = match &result {
                None => ("실행 중", Tone::Accent),
                Some(r) if r.is_error => ("오류", Tone::Error),
                Some(_) => ("완료", Tone::Success),
            };
            let tool_name = if name.is_empty() {
                "도구".to_string()
            } else {
                name
            };
            let has_result = result.is_some();
            let (out_text, out_err) = result
                .as_ref()
                .map(|r| (r.output.clone(), r.is_error))
                .unwrap_or_default();
            // A file change draws as the mockup's edit card (#535): name, +N / -N, coloured lines.
            let diff = tool_card::file_diff(&tool_name, &input, &out_text);
            let show_tail = has_result && !out_text.trim().is_empty();
            if let Some(d) = diff {
                // No frame (mockup `<details open><summary class="cmd">`): for a file change
                // the set holds the cards the person folded.
                let err = out_err.then_some(out_text.as_str());
                FileChangeCard(&d, !has_result, err, open, move || {
                    exp_toggle.update(|set| {
                        if !set.remove(&cid_toggle) {
                            set.insert(cid_toggle.clone());
                        }
                    });
                });
                return;
            }

            EmberCard(false, true, None, Some(SP[1]), None, move || {
                { use ::compose_rust::ui::Column; Column!(spacing = SP[0], modifier = Modifier::new().fill_max_width(), content = move || {
                        { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = SP[1], modifier = Modifier::new().fill_max_width(), content = move || {
                                if !has_result {
                                    Spinner(12.0);
                                } else {
                                    NavIcon(ICON_TERMINAL_SVG, p.muted.to_argb(), 15.0);
                                }
                                Label(&tool_name, Fs::Sm, Weight::Semibold, None, Some(1));
                                { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().weight(1.0), content = move || {
                                        Label(
                                            &summary,
                                            Fs::Xs,
                                            Weight::Regular,
                                            Some(paint(p.muted)),
                                            Some(1),
                                        );
                                    }) };
                                Badge(state_text, tone);
                                Clickable(
                                    Modifier::new()
                                        .height(H_PILL)
                                        .background(paint(p.surface))
                                        .border(1.0, paint(p.line))
                                        .shape_role(ShapeRole::Full),
                                    Alignment::Center,
                                    move || {
                                        exp_toggle.update(|set| {
                                            if !set.remove(&cid_toggle) {
                                                set.insert(cid_toggle.clone());
                                            }
                                        });
                                    },
                                    move || {
                                        { use ::compose_rust::ui::Row; Row!(alignment = Alignment::Center, {
                                                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(8.0)) };
                                                Label(
                                                    if open { "접기" } else { "자세히" },
                                                    Fs::Xs,
                                                    Weight::Medium,
                                                    Some(paint(p.text_2)),
                                                    Some(1),
                                                );
                                                { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(8.0)) };
                                            }) };
                                    },
                                );
                            }) };
                        if open {
                            Label("입력", Fs::Sm, Weight::Medium, None, Some(1));
                            CodeText(&model::json_pretty(&input), None, false);
                            if has_result {
                                Label(
                                    if out_err { "오류" } else { "결과" },
                                    Fs::Sm,
                                    Weight::Medium,
                                    None,
                                    Some(1),
                                );
                                CodeText(&out_text, None, out_err);
                            }
                        } else if show_tail {
                            CodeText(&out_text, Some(2), out_err);
                        }
                    }) };
            });
        }
        TranscriptItem::Approval {
            approval_id,
            tool,
            input,
            state,
            ..
        } => {
            let in_flight = answering.get().contains(&approval_id);
            // The command (or file, or a one-line summary) only, never raw JSON (#536).
            let words = tool_card::approval_text(computer, &tool, &input);
            let offer_always = tool_card::offers_allow_always(&input);
            let (title, body) = (words.title.clone(), words.body.clone());
            let answer = {
                let sid = sid.clone();
                let aid = approval_id.clone();
                let ans = answering.clone();
                move |decision: ApprovalDecision| {
                    ans.update(|set| {
                        set.insert(aid.clone());
                    });
                    let s_id = sid.clone();
                    let a_id = aid.clone();
                    let done = aid.clone();
                    let ans_done = ans.clone();
                    run_shared(
                        async move { services().answer(&s_id, &a_id, decision).await },
                        move |r: Result<(), String>| {
                            ans_done.update(|set| {
                                set.remove(&done);
                            });
                            if let Err(e) = r {
                                notice::post(format!("응답하지 못했습니다: {e}"));
                            }
                        },
                    );
                }
            };
            if let ApprovalState::Resolved(d) = state {
                // Mockup: a resolved approval folds into one `.cmd` row, not a card.
                ResolvedApprovalRow(&body, d);
                return;
            }
            let (a1, a2, a3) = (answer.clone(), answer.clone(), answer);
            // `.approve`: `surface`, a 1 px `line` border (review SE6: it was orange), the
            // `--r` radius, the small shadow, 14/16 padding (10/12 in a pane).
            let (pad_v, pad_h) = approve_pad(pane);
            { use ::compose_rust::ui::Box; Box!(modifier = with_shadows(Modifier::new(), theme.shadow_sm())
                        .fill_max_width()
                        .background(paint(p.surface))
                        .border(1.0, paint(p.line))
                        .corner_radius(CORNER_MD)
                        .padding(pad_v), content = move || {
                    { use ::compose_rust::ui::Row; Row!(modifier = Modifier::new().fill_max_width(), content = move || {
                        // No asymmetric padding in the toolkit: spacers raise the sides.
                        { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(pad_h - pad_v)) };
                        { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().width(0.0).weight(1.0), content = move || {
                            { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, spacing = SP[1], modifier = Modifier::new().fill_max_width(), {
                                    NavIcon(ICON_SHIELD_SVG, p.text.to_argb(), 16.0);
                                    { use ::compose_rust::ui::Box; Box!(alignment = Alignment::CenterStart, modifier = Modifier::new().width(0.0).weight(1.0), {
                                        Label(&title, Fs::Md, Weight::Semibold, None, Some(1));
                                    }) };
                                    if state == ApprovalState::Pending {
                                        Pill("대기 중", PillTone::Orange, false);
                                    }
                                }) };
                            match state {
                                ApprovalState::Pending => {
                                    // `.out` with `padding-left:0`: the command in mono, 8
                                    // above and below, no box.
                                    { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().height(OUT_PAD_V)) };
                                    if !body.is_empty() {
                                        Txt(
                                            &body,
                                            TypeRole::Mono,
                                            Some(paint(p.text_2)),
                                            Some(4),
                                            Some(TextOverflow::Ellipsis),
                                            None,
                                            None,
                                        );
                                        { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().height(OUT_PAD_V)) };
                                    }
                                    // `.flex.wrap`: the buttons go to a second line when the
                                    // card is too narrow for the three.
                                    { use ::compose_rust::ui::FlowRow; FlowRow!(horizontal_spacing = SP[1], vertical_spacing = SP[1], modifier = Modifier::new().fill_max_width(), content = move || {
                                            ApprovalButton(tool_card::ALLOW_ONCE, true, false, move || {
                                                if !in_flight {
                                                    a1(ApprovalDecision::AllowOnce);
                                                }
                                            });
                                            // FR-T12 (#380): no 항상 허용 when the server marks
                                            // the request `allow_always: false`.
                                            if offer_always {
                                                ApprovalButton(tool_card::ALLOW_ALWAYS, false, false, move || {
                                                    if !in_flight {
                                                        a2(ApprovalDecision::AllowAlways);
                                                    }
                                                });
                                            }
                                            ApprovalButton(tool_card::DENY, false, true, move || {
                                                if !in_flight {
                                                    a3(ApprovalDecision::Deny);
                                                }
                                            });
                                            if in_flight {
                                                Spinner(14.0);
                                            }
                                        }) };
                                }
                                ApprovalState::Withdrawn => {
                                    { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().height(SP[1])) };
                                    Label(
                                        "에이전트가 이 요청을 거두었습니다",
                                        Fs::Xs,
                                        Weight::Regular,
                                        Some(paint(p.muted)),
                                        Some(2),
                                    );
                                }
                                ApprovalState::Abandoned => {
                                    { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().height(SP[1])) };
                                    Label(
                                        "턴이 끝나기 전에 응답하지 않았습니다",
                                        Fs::Xs,
                                        Weight::Regular,
                                        Some(paint(p.muted)),
                                        Some(1),
                                    );
                                }
                                ApprovalState::Resolved(_) => {}
                            }
                        }) };
                        { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(pad_h - pad_v)) };
                    }) };
                }) };
        }
        TranscriptItem::Question { question_id, questions, state, .. } => {
            QuestionCard(&sid, question_id, questions, state, answering);
        }
        TranscriptItem::TurnEnded { seq, outcome } => {
            let ui = shared_ui().clone();
            let can = {
                let _ = ui.live.launcher.get();
                services()
                    .client
                    .read(|st| model::fork_turn_enabled(&st.fork_info(&sid), outcome, seq))
            };
            let (text, col) = match outcome {
                TurnOutcome::Completed => ("턴 완료", p.muted),
                TurnOutcome::Interrupted => ("중단됨", p.muted),
                TurnOutcome::Failed => ("턴 실패", p.red.strong),
            };
            { use ::compose_rust::ui::Row; Row!(alignment = Alignment::Center, space_role = SpaceRole::Sm, modifier = Modifier::new().fill_max_width().padding_role(SpaceRole::Xs), content = move || {
                    { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().weight(1.0), {
                            // Token line: the toolkit divider is white in the dark scheme (#510).
                            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().fill_max_width().height(1.0).background(paint(ember_theme().palette.line))) };
                        }) };
                    Label(text, Fs::Xs, Weight::Regular, Some(paint(col)), Some(1));
                    if outcome == TurnOutcome::Completed {
                        let u_fork = ui.clone();
                        let s_fork = sid.clone();
                        let fg_fork = if can { p.text_2 } else { p.muted };
                        Clickable(
                            Modifier::new()
                                .height(H_PILL)
                                .background(paint(p.surface))
                                .border(1.0, paint(p.line))
                                .shape_role(ShapeRole::Full),
                            Alignment::Center,
                            move || {
                                if can {
                                    fork_from(&u_fork, s_fork.clone(), Some(seq));
                                }
                            },
                            move || {
                                { use ::compose_rust::ui::Row; Row!(alignment = Alignment::Center, {
                                        { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(8.0)) };
                                        Label(
                                            ko::FORK_TURN_LABEL,
                                            Fs::Xs,
                                            Weight::Medium,
                                            Some(paint(fg_fork)),
                                            Some(1),
                                        );
                                        { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().width(8.0)) };
                                    }) };
                            },
                        );
                    }
                    { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().weight(1.0), {
                            // Token line: the toolkit divider is white in the dark scheme (#510).
                            { use ::compose_rust::ui::Spacer; Spacer!(modifier = Modifier::new().fill_max_width().height(1.0).background(paint(ember_theme().palette.line))) };
                        }) };
                }) };
        }
        TranscriptItem::Error { message, .. } => {
            Banner(&message, Tone::Error);
        }
        TranscriptItem::Notice { text, .. } => {
            { use ::compose_rust::ui::Row; Row!(alignment = Alignment::Center, modifier = Modifier::new().fill_max_width().padding_role(SpaceRole::Xs), content = move || {
                    Txt(
                        &text,
                        TypeRole::Caption,
                        Some(Paint::Role(ColorRole::OnSurfaceVariant)),
                        None,
                        None,
                        None,
                        Some(TextAlign::Center),
                    );
                }) };
        }
    }
}

/// The team this session leads or belongs to (FR-T7): members with live status, and the
/// shared task list. Nothing when it is in no team.
pub fn TeamPanel(session: &str) {
    let ui = shared_ui().clone();
    let p = ember_theme().palette;
    let expanded = remember(|| mutable_state_of(true));
    let data = {
        let _ = ui.live.teams.get();
        let _ = ui.live.launcher.get();
        services().client.read(|st| {
            st.team_for_session(session).map(|t| {
                (
                    t.id.clone(),
                    t.tasks.clone(),
                    ko::team_member_rows(st, t, session),
                )
            })
        })
    };
    let Some((team_id, tasks, rows)) = data else {
        return;
    };
    let active = rows.iter().filter(|r| !r.leader && !r.ended).count();
    let open = tasks
        .iter()
        .filter(|t| {
            !matches!(
                t.status,
                ember_client::wire::TaskStatus::Done | ember_client::wire::TaskStatus::Cancelled
            )
        })
        .count();

    EmberCard(false, true, None, Some(SP[2]), None, move || {
        { use ::compose_rust::ui::Column; Column!(space_role = SpaceRole::Xs, modifier = Modifier::new().fill_max_width(), {
                let is_open = expanded.get();
                { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, space_role = SpaceRole::Sm, modifier = Modifier::new().fill_max_width(), {
                        { use ::compose_rust::ui::Box; Box!(modifier = Modifier::new().weight(1.0), {
                            Label(
                                &format!("팀 \u{00b7} 팀원 {active}명 \u{00b7} 열린 작업 {open}개"),
                                Fs::Md,
                                Weight::Medium,
                                None,
                                Some(1),
                            );
                        }) };
                        let exp = expanded.clone();
                        AppButton(
                            if is_open { "접기" } else { "펼치기" },
                            Some(if is_open {
                                IconRole::Collapse
                            } else {
                                IconRole::More
                            }),
                            ButtonKind::Default,
                            true,
                            true,
                            false,
                            move || exp.set(!is_open),
                        );
                    }) };
                if is_open {
                    for r in rows.iter().cloned() {
                        let u = ui.clone();
                        let tid = team_id.clone();
                        { use ::compose_rust::ui::Row; Row!(alignment = Alignment::CenterStart, space_role = SpaceRole::Sm, modifier = Modifier::new().fill_max_width(), content = move || {
                                let name_label = if r.leader {
                                    format!("{} (리더)", r.name)
                                } else {
                                    r.name.clone()
                                };
                                let sub = if r.ended {
                                    format!("{} \u{00b7} 종료됨", r.agent)
                                } else {
                                    format!("{} \u{00b7} {}", r.agent, r.status)
                                };
                                // The name is the only part that gives way: an unweighted
                                // name was measured first and took the whole row at 390, so
                                // 열기 and 종료 got no width and the agent text was drawn over
                                // the name (review r8 fix round). The agent and status text
                                // follows the name at its own width, as the header's pill.
                                NameThenPill(
                                    &name_label,
                                    Fs::Md,
                                    if r.this {
                                        Weight::Medium
                                    } else {
                                        Weight::Regular
                                    },
                                    None,
                                    SP[1],
                                    move || {
                                        Label(
                                            &sub,
                                            Fs::Xs,
                                            Weight::Regular,
                                            Some(paint(p.muted)),
                                            Some(1),
                                        );
                                    },
                                );
                                if r.open_tasks > 0 {
                                    Badge(&format!("작업 {}", r.open_tasks), Tone::Accent);
                                }
                                if !r.this {
                                    let sid = r.session_id.clone();
                                    let u_open = u.clone();
                                    AppButton(
                                        "열기",
                                        None,
                                        ButtonKind::Default,
                                        true,
                                        true,
                                        false,
                                        move || u_open.open_conversation(&sid),
                                    );
                                }
                                if !r.leader && !r.ended {
                                    let sid = r.session_id.clone();
                                    let mname = r.name.clone();
                                    AppButton(
                                        "종료",
                                        None,
                                        ButtonKind::Destructive,
                                        true,
                                        true,
                                        false,
                                        move || {
                                            if demo_blocked_cr() {
                                                return;
                                            }
                                            let (t_id, s_id, n_str) =
                                                (tid.clone(), sid.clone(), mname.clone());
                                            run_shared(
                                                async move {
                                                    services()
                                                        .client
                                                        .end_teammate(&t_id, &s_id)
                                                        .await
                                                        .map_err(|e| e.to_string())
                                                },
                                                move |res| match res {
                                                    Ok(_) => notice::post(format!(
                                                        "{n_str} 팀원을 종료했습니다"
                                                    )),
                                                    Err(e) => notice::post(format!(
                                                        "{n_str} 팀원을 종료하지 못했습니다: {e}"
                                                    )),
                                                },
                                            );
                                        },
                                    );
                                }
                            }) };
                    }
                    if tasks.is_empty() {
                        Label(
                            "등록된 작업이 없습니다",
                            Fs::Xs,
                            Weight::Regular,
                            Some(paint(p.muted)),
                            Some(1),
                        );
                    }
                    for t in tasks.iter() {
                        Txt(
                            &ko::task_line(t),
                            TypeRole::Caption,
                            None,
                            Some(2),
                            Some(TextOverflow::Ellipsis),
                            None,
                            None,
                        );
                    }
                }
            }) };
    });
}

/// What the transcript area says while it has no items (FR-L5, FR-L18, #505). The loading line
/// is honest only when the server reports events (`last_seq > 0`) that have not arrived yet; a
/// session with none has nothing to load, so it says so instead of waiting forever.
pub(crate) fn empty_transcript_text(status: LauncherStatus, last_seq: i64) -> &'static str {
    if status == LauncherStatus::Idle {
        "무엇을 작업할지 입력해 주세요."
    } else if last_seq > 0 {
        "대화를 불러오는 중입니다\u{2026}"
    } else {
        "아직 대화가 없습니다"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use compose_rust::protocol::{HostEvent, Mutation, PropertyValue, decode_batch};
    use compose_rust::{
        EventPayload, Host, Recomposer, WindowHeightClass, WindowSizeClass, Composable,
    };

    fn texts(batch: &[Mutation<'_>]) -> Vec<String> {
        batch
            .iter()
            .filter_map(|m| match m {
                Mutation::SetProp {
                    value: PropertyValue::String(s),
                    ..
                } => Some(s.to_string()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn fr_l18_single_view_does_not_wait_when_the_session_has_no_events() {
        assert_eq!(
            empty_transcript_text(LauncherStatus::Running, 0),
            "아직 대화가 없습니다"
        );
        assert_eq!(
            empty_transcript_text(LauncherStatus::Finished, 0),
            "아직 대화가 없습니다"
        );
        assert_eq!(
            empty_transcript_text(LauncherStatus::Running, 3),
            "대화를 불러오는 중입니다\u{2026}"
        );
        assert_eq!(
            empty_transcript_text(LauncherStatus::Idle, 0),
            "무엇을 작업할지 입력해 주세요."
        );
    }

    #[Composable]
    fn host_conversation_s1() {
        ConversationView("demo-s1", false);
    }

    #[test]
    fn fr_l28_3_the_composer_chips_shrink_and_the_send_button_keeps_its_size_at_360() {
        // Flip 4 review 3.5: the two chips kept their full width (116 and 120 dp) and pushed the
        // stop button to a 6 dp sliver. Each chip now sits in an equal weighted share of the row.
        // The demo test services hold no session record, so the chips do not render here and the
        // composer row is checked in the source.
        let src = include_str!("conversation.rs").split("mod tests {").next().unwrap();
        let row = src
            .split("|| notice::post(\"파일 첨부는 준비 중입니다\")")
            .nth(1)
            .and_then(|r| r.split("StopCircleButton(").next())
            .expect("the composer row");
        let shares = row.matches("modifier = Modifier::new().weight(1.0)").count();
        assert_eq!(shares, 2, "one weighted share per chip, no weighted spacer");
        assert!(!row.contains("Spacer!("), "no spacer pushes the send button");

        let mode = row.find("ModeChip(&sid, is_phone)").expect("the mode chip is in the row");
        let model = row.find("ModelChip(&sid, is_phone)").expect("the model chip is in the row");
        let first = row.find(".weight(1.0)").unwrap();
        let second = row.rfind(".weight(1.0)").unwrap();
        assert!(first < mode && mode < second && second < model, "each chip is in its own share");
    }

    #[test]
    fn fr_l5_conversation_view_renders_demo_transcript_and_approval_in_compose_rust() {
        let _guard = crate::services::test_guard();
        services().open_session("demo-s1");
        compose_rust::window::reset_window_size();
        let mut host = Host::new(|| Box::new(Recomposer::new(host_conversation_s1)));
        let mut all = texts(&decode_batch(host.rebuild().expect("rebuild")).expect("decode"));
        let event = HostEvent::new(0, 0, EventPayload::WindowSizeChanged {
                width_dp: 1440.0,
                height_dp: 900.0,
                class: WindowSizeClass::Expanded,
                height_class: WindowHeightClass::Medium,
                safe_area_top: 0.0,
                safe_area_leading: 0.0,
                safe_area_trailing: 0.0,
                safe_area_bottom: 0.0,
            });
        let (bytes, _) = host.dispatch(event).expect("dispatch");
        all.extend(texts(&decode_batch(bytes).expect("decode")));
        for want in ["포트 설정 추가", "한 번 허용", "항상 허용", "거부", "IDE 열기"] {
            assert!(
                all.iter().any(|t| t.contains(want)),
                "missing {want} in ConversationView: {all:?}"
            );
        }
        compose_rust::window::reset_window_size();
        // #535: the Edit card shows the file name, +N / -N and coloured lines, not the header.
        // #536: the approval shows the command and the mockup wording, not raw JSON.
        for want in [
            "config.rs",
            "+6",
            "+impl Config {",
            "에서 명령을 실행할까요?",
            "cargo test --locked",
            "이 프로젝트에서 항상 허용",
        ] {
            assert!(all.iter().any(|t| t == want || t.contains(want)), "missing {want}: {all:?}");
        }
        for unwanted in ["+++ b/", "--- a/", "old_string", "실행을 허용할까요?", "\"command\""] {
            assert!(!all.iter().any(|t| t.contains(unwanted)), "raw {unwanted} shown: {all:?}");
        }
        // An edit that only adds lines has no `-0` pill (#535).
        assert!(!all.iter().any(|t| t == "-0"), "a zero count is shown: {all:?}");
    }

    /// One node of a composition: its widget, its parent, its modifiers and its props.
    struct Emitted {
        id: u32,
        widget: String,
        parent: Option<u32>,
        mods: Vec<String>,
        props: Vec<String>,
    }

    fn emitted(content: fn()) -> Vec<Emitted> {
        let mut host = Host::new(move || Box::new(Recomposer::new(content)));
        let batch = decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        let mut nodes: Vec<Emitted> = Vec::new();
        for m in batch.iter() {
            match m {
                Mutation::Create { node_id, widget } => nodes.push(Emitted {
                    id: *node_id,
                    widget: format!("{widget:?}"),
                    parent: None,
                    mods: Vec::new(),
                    props: Vec::new(),
                }),
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

    fn has_text(nodes: &[Emitted], text: &str) -> bool {
        let want = format!("Text=String({text:?})");
        nodes.iter().any(|n| n.props.contains(&want))
    }

    fn text_of<'a>(nodes: &'a [Emitted], text: &str) -> &'a Emitted {
        let want = format!("Text=String({text:?})");
        nodes.iter().find(|n| n.props.contains(&want)).unwrap_or_else(|| panic!("no text {text}"))
    }

    /// Every ancestor of `node`, nearest first.
    fn ancestors<'a>(nodes: &'a [Emitted], node: &Emitted) -> Vec<&'a Emitted> {
        let mut out = Vec::new();
        let mut at = node.parent;
        while let Some(id) = at {
            let Some(n) = nodes.iter().find(|n| n.id == id) else { break };
            out.push(n);
            at = n.parent;
        }
        out
    }

    fn row_of(item: TranscriptItem, computer: Option<&str>, pane: bool) {
        let expanded = remember(|| mutable_state_of(HashSet::<String>::new()));
        let answering = remember(|| mutable_state_of(HashSet::<String>::new()));
        TranscriptRow("demo-s1", "claude", &item, &expanded, &answering, computer, pane);
    }

    fn edit_item() -> TranscriptItem {
        TranscriptItem::ToolCall {
            seq: 1,
            call_id: "c1".into(),
            name: "Edit".into(),
            input: serde_json::json!({"file_path": "/work/src/config.rs"}),
            result: Some(ember_client::transcript::ToolOutput {
                output: "--- a/src/config.rs\n+++ b/src/config.rs\n@@ -1,3 +1,4 @@\n fn keep() {}\n+fn added() {}\n+fn more() {}\n fn tail() {}\n".into(),
                is_error: false,
            }),
        }
    }

    fn edit_row() {
        row_of(edit_item(), None, false);
    }

    #[test]
    fn fr_l5_file_change_is_an_unframed_summary_row_over_the_changed_lines() {
        // #535, mockup `session.js` `m.k==="edit"`: `<details open><summary class="cmd">`, no
        // card. The app drew a bordered card, the context lines and a `-0` pill.
        let _guard = crate::services::test_guard();
        let nodes = emitted(edit_row);
        assert!(!nodes.iter().any(|n| n.mods.iter().any(|m| m.starts_with("Border"))), "no frame around a file change");
        for t in ["config.rs", "+2", "+fn added() {}", "+fn more() {}"] {
            assert!(has_text(&nodes, t), "missing {t}");
        }
        for t in ["-0", " fn keep() {}", " fn tail() {}", "자세히", "완료"] {
            assert!(!has_text(&nodes, t), "{t:?} is shown");
        }
        // The summary row: 30 high (`.cmd` 3 + 24 + 3), a button that says the lines are shown.
        let name = text_of(&nodes, "config.rs");
        let row = ancestors(&nodes, name)
            .into_iter()
            .find(|n| n.mods.iter().any(|m| m == "Height(30.0)"))
            .expect("the 30 dp summary row");
        assert!(row.mods.iter().any(|m| m.starts_with("Semantics") && m.contains("Button")), "{:?}", row.mods);
        // The expanded field of the semantics state is two bits, 2 for "on".
        let state: u32 = row
            .mods
            .iter()
            .find_map(|m| m.strip_prefix("Semantics")?.split("state: ").nth(1)?.trim_end_matches([' ', '}']).parse().ok())
            .expect("a semantics state");
        assert_eq!((state >> compose_rust::schema::SEMANTICS_EXPANDED_SHIFT) & 3, 2, "{:?}", row.mods);
        // `.cmd{gap:10px}` and `.out{padding:8px 0 8px 28px}`.
        let inner = ancestors(&nodes, name).into_iter().find(|n| n.widget == "Row").expect("the row");
        assert!(inner.props.iter().any(|p| p.contains("10.0")), "the 10 dp gap: {:?}", inner.props);
        let line = text_of(&nodes, "+fn added() {}");
        let lines_row = ancestors(&nodes, line).into_iter().find(|n| n.widget == "Row").expect("the lines row");
        let inset = nodes.iter().find(|n| n.parent == Some(lines_row.id) && n.mods.iter().any(|m| m == "Width(28.0)"));
        assert!(inset.is_some(), "the lines start 28 in");
        assert_eq!(file_change_height(2, false), 30.0 + 16.0 + 2.0 * 17.0);
        assert_eq!(file_change_height(2, true), 30.0);
        assert_eq!(file_change_height(45, false), 30.0 + 16.0 + 41.0 * 17.0);
    }

    fn approval_item(state: ApprovalState) -> TranscriptItem {
        TranscriptItem::Approval {
            seq: 2,
            approval_id: "a1".into(),
            tool: "Bash".into(),
            input: serde_json::json!({"command": "cargo test --locked"}),
            state,
        }
    }

    fn resolved_row() {
        row_of(approval_item(ApprovalState::Resolved(ApprovalDecision::AllowOnce)), Some("mac-mini"), false);
    }

    fn pending_row() {
        row_of(approval_item(ApprovalState::Pending), Some("mac-mini"), false);
    }

    fn pending_pane_row() {
        row_of(approval_item(ApprovalState::Pending), Some("mac-mini"), true);
    }

    #[test]
    fn fr_l5_resolved_approval_folds_into_one_row() {
        // #536, mockup: `<div class="cmd">` with the shield, the command and the pill. The app
        // kept the whole card with its title.
        let _guard = crate::services::test_guard();
        let nodes = emitted(resolved_row);
        assert!(!nodes.iter().any(|n| n.mods.iter().any(|m| m.starts_with("Border"))), "no card");
        assert!(has_text(&nodes, "cargo test --locked") && has_text(&nodes, "허용함"));
        assert!(!has_text(&nodes, "mac-mini에서 명령을 실행할까요?"), "the title is gone");
        assert!(!has_text(&nodes, tool_card::ALLOW_ONCE));
        let row = ancestors(&nodes, text_of(&nodes, "허용함")).into_iter().find(|n| n.widget == "Row" && n.mods.iter().any(|m| m == "Height(30.0)"));
        assert!(row.is_some(), "one 30 dp row");
        assert_eq!(estimate_item_height(&approval_item(ApprovalState::Resolved(ApprovalDecision::Deny)), false), 30.0);
    }

    #[test]
    fn fr_l5_the_resolved_pill_follows_the_command() {
        // Review st B: the mockup (`conv-multi-approved-1440`) puts the pill right after the
        // command; the app pushed it to the row's end. The command is now the name of
        // `NameThenPill`: a measuring copy and the visible text, both in the mono face, and the
        // pill in the measured box after it.
        let _guard = crate::services::test_guard();
        let nodes = emitted(resolved_row);
        let want = format!("Text=String({:?})", "cargo test --locked");
        let copies: Vec<&Emitted> = nodes.iter().filter(|n| n.props.contains(&want)).collect();
        assert_eq!(copies.len(), 2, "the measuring copy and the visible command");
        // The prop the mono face adds to a text, read from a mono and a plain reference text.
        // Caption is the same 12/400 step in the body face, so the props differ by the face only.
        let plain = emitted(|| Txt("ref", TypeRole::Caption, Some(Paint::Literal(compose_rust::Color::argb(0))), Some(1), None, None, None));
        let mono = emitted(|| Txt("ref", TypeRole::Mono, Some(Paint::Literal(compose_rust::Color::argb(0))), Some(1), None, None, None));
        let mono_text = text_of(&mono, "ref");
        let plain_text = text_of(&plain, "ref");
        let face: Vec<&String> = mono_text.props.iter().filter(|p| !plain_text.props.contains(p)).collect();
        assert!(!face.is_empty(), "the mono face sends a prop: {:?}", mono_text.props);
        for c in &copies {
            for f in &face {
                assert!(c.props.contains(f), "mono face {f} on {:?}", c.props);
            }
        }
        let pill = text_of(&nodes, "허용함");
        assert!(
            ancestors(&nodes, pill).iter().any(|n| n.mods.iter().any(|m| m.starts_with("ObserveSize"))),
            "the pill is measured by NameThenPill"
        );
    }

    #[test]
    fn fr_l5_pending_approval_card_has_the_line_border_and_the_mockup_buttons() {
        // Review SE6, `session.css` `.approve{border:1px solid var(--line);border-radius:var(--r);
        // padding:14px var(--sp4)}`; buttons `.btn.sm` (32 high, 13 px).
        let _guard = crate::services::test_guard();
        let p = ember_theme().palette;
        let nodes = emitted(pending_row);
        let line = format!("{:?}", compose_rust::schema::Modifier::Border { width: 1.0, paint: paint(p.line) });
        let orange = format!("{:?}", compose_rust::schema::Modifier::Border { width: 1.0, paint: paint(p.orange.strong) });
        let title = text_of(&nodes, "mac-mini에서 명령을 실행할까요?");
        let card = ancestors(&nodes, title)
            .into_iter()
            .find(|n| n.mods.iter().any(|m| m.starts_with("Border")))
            .expect("the card");
        assert!(card.mods.contains(&line), "{:?}", card.mods);
        assert!(!nodes.iter().any(|n| n.mods.contains(&orange)), "no orange border");
        assert!(card.mods.iter().any(|m| m == "Padding(14.0)"), "{:?}", card.mods);
        assert!(card.mods.iter().any(|m| m.starts_with("Shape { top_start: 12.0")), "{:?}", card.mods);
        assert!(nodes.iter().any(|n| n.widget == "FlowRow"), "the buttons wrap");
        for t in [tool_card::ALLOW_ONCE, tool_card::ALLOW_ALWAYS, tool_card::DENY] {
            let label = text_of(&nodes, t);
            assert!(label.props.iter().any(|p| p.contains("13.0")), "{t} is 13 px: {:?}", label.props);
            let button = ancestors(&nodes, label)
                .into_iter()
                .find(|n| n.mods.iter().any(|m| m == "Height(32.0)"))
                .unwrap_or_else(|| panic!("{t} is 32 high"));
            // 거부 keeps the `line` border; only its text is red.
            assert!(t == tool_card::ALLOW_ONCE || button.mods.contains(&line), "{t}: {:?}", button.mods);
        }
        assert!(has_text(&nodes, "cargo test --locked") && has_text(&nodes, "대기 중"));
        // In a pane the card is padded 10/12.
        let pane = emitted(pending_pane_row);
        let title = text_of(&pane, "mac-mini에서 명령을 실행할까요?");
        let card = ancestors(&pane, title).into_iter().find(|n| n.mods.iter().any(|m| m.starts_with("Border"))).expect("the card");
        assert!(card.mods.iter().any(|m| m == "Padding(10.0)"), "{:?}", card.mods);
        assert_eq!(approve_pad(false), (14.0, 16.0));
        assert_eq!(approve_pad(true), (10.0, 12.0));
    }

    fn pending_once_only_row() {
        let item = TranscriptItem::Approval {
            seq: 2,
            approval_id: "a2".into(),
            tool: "mcp__ember-channels__channel_post".into(),
            input: serde_json::json!({"note": "서버 이름을 확인하지 못했습니다.", "allow_always": false}),
            state: ApprovalState::Pending,
        };
        row_of(item, Some("mac-mini"), false);
    }

    #[test]
    fn fr_t12_card_without_allow_always_sends_once_and_deny_only() {
        // FR-T12 (#380) on this branch's card (FR-L5): `allow_always: false` draws 허용 and
        // 거부 in the FlowRow and no 항상 허용; the server note is the body.
        let _guard = crate::services::test_guard();
        let nodes = emitted(pending_once_only_row);
        assert!(nodes.iter().any(|n| n.widget == "FlowRow"));
        assert!(has_text(&nodes, tool_card::ALLOW_ONCE) && has_text(&nodes, tool_card::DENY));
        assert!(!has_text(&nodes, tool_card::ALLOW_ALWAYS), "no always button");
        assert!(has_text(&nodes, "서버 이름을 확인하지 못했습니다."), "the note is the body");
        // The default card still offers all three.
        assert!(has_text(&emitted(pending_row), tool_card::ALLOW_ALWAYS));
    }

    #[Composable]
    fn host_conversation_s1_embedded() {
        ConversationView("demo-s1", true);
    }

    #[test]
    fn fr_l5_conversation_body_is_painted_with_the_surface_token() {
        // Review SE9: the mockup's conversation (`.detail{background:var(--surface)}`) is white
        // (`#ffffff`, dark `#292927`); the app left it on the window `bg`, so the cards and the
        // composer floated on grey. The view's root box sends the surface fill itself.
        let _guard = crate::services::test_guard();
        services().open_session("demo-s1");
        for content in [host_conversation_s1 as fn(), host_conversation_s1_embedded as fn()] {
            let mut host = Host::new(move || Box::new(Recomposer::new(content)));
            let batch = decode_batch(host.rebuild().expect("rebuild")).expect("decode");
            let root = batch
                .iter()
                .find_map(|m| match m {
                    Mutation::Create { node_id, widget } if format!("{widget:?}") == "Box" => Some(*node_id),
                    _ => None,
                })
                .expect("the root box");
            let mods: Vec<String> = batch
                .iter()
                .filter_map(|m| match m {
                    Mutation::SetModifier { node_id, modifier, .. } if *node_id == root => Some(format!("{modifier:?}")),
                    _ => None,
                })
                .collect();
            let surface = format!("{:?}", compose_rust::schema::Modifier::Background(paint(ember_theme().palette.surface)));
            assert!(mods.contains(&surface), "root box {mods:?} lacks {surface}");
            assert!(mods.iter().any(|m| m == "FillMaxWidth") && mods.iter().any(|m| m == "FillMaxHeight"), "{mods:?}");
        }
    }

    #[test]
    fn fr_l10_4_computer_menu_checks_only_the_current_computer() {
        let cs = crate::demo::computers();
        let chosen = |rows: &[PopRow]| rows.iter().map(|r| r.chosen).collect::<Vec<_>>();
        assert_eq!(chosen(&computer_pop_rows(&cs, Some("studio"))), [false, true]);
        assert_eq!(chosen(&computer_pop_rows(&cs, Some("local"))), [true, false]);
        assert_eq!(chosen(&computer_pop_rows(&cs, None)), [true, false], "unknown falls back to the local computer");
    }

    /// #888: the More menu's rows carry an icon and a description like the other header menus,
    /// so a row is not a lone title at the top of a 55 dp row.
    #[test]
    fn fr_l10_4_more_menu_rows_have_icon_and_description() {
        for (pinned, approve_all) in [(false, false), (true, true)] {
            let rows = more_pop_rows(pinned, approve_all);
            assert_eq!(rows.len(), 4);
            for r in &rows {
                assert!(r.icon.is_some(), "{:?} has no icon", r.title);
                assert!(r.description.as_deref().is_some_and(|d| !d.trim().is_empty()), "{:?} has no description", r.title);
            }
            assert_eq!(rows[0].chosen, pinned);
            assert_eq!(rows[3].chosen, approve_all);
            assert_eq!(rows[3].danger, !approve_all);
        }
    }

    #[test]
    fn fr_l10_4_conversation_menus_are_anchored_popovers_not_option_sheets() {
        let _guard = crate::services::test_guard();
        crate::ui::window_size::set_opened_for_test(Some((1440.0, 900.0)));
        services().open_session("demo-s1");
        let mut host = Host::new(|| Box::new(Recomposer::new(host_conversation_s1)));
        let batch = decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        crate::ui::window_size::set_opened_for_test(None);

        let popover_ids: Vec<u32> = batch
            .iter()
            .filter_map(|m| match m {
                Mutation::Create { node_id, widget } if format!("{widget:?}") == "Popover" => Some(*node_id),
                _ => None,
            })
            .collect();
        assert_eq!(popover_ids.len(), 3, "More, Computer, and IDE menus should each be an anchored Popover");

        let has_dialog_or_sheet = batch.iter().any(|m| {
            matches!(m, Mutation::Create { widget, .. } if matches!(format!("{widget:?}").as_str(), "Dialog" | "Sheet"))
        });
        assert!(!has_dialog_or_sheet, "conversation header menus must not emit a centred OptionSheet Dialog or Sheet");

        let popover_mods: Vec<String> = batch
            .iter()
            .filter_map(|m| match m {
                Mutation::SetModifier { node_id, modifier, .. } if popover_ids.contains(node_id) => {
                    Some(format!("{modifier:?}"))
                }
                _ => None,
            })
            .collect();
        let menu_role_count = popover_mods.iter().filter(|m| m.contains("Semantics") && m.contains("Menu")).count();
        assert_eq!(menu_role_count, 3, "all 3 popovers must declare Role::Menu, got {popover_mods:?}");

        let popover_props: Vec<String> = batch
            .iter()
            .filter_map(|m| match m {
                Mutation::SetProp { node_id, property, value } if popover_ids.contains(node_id) => {
                    Some(format!("{property:?}={value:?}"))
                }
                _ => None,
            })
            .collect();
        for name in [super::super::a11y::MORE, super::super::a11y::COMPUTER_PICKER, super::super::a11y::IDE_PICKER] {
            assert!(
                popover_props.iter().any(|p| p.contains(name)),
                "expected a Popover prop named '{name}', got {popover_props:?}"
            );
        }

        let prod = include_str!("conversation.rs").split("#[cfg(test)]").next().unwrap();
        assert!(!prod.contains("OptionSheet("), "no centred OptionSheet remains in conversation.rs");
    }

    #[Composable]
    fn host_conversation_s2_embedded() {
        ConversationView("demo-s2", true);
    }

    #[Composable]
    fn host_conversation_s3_embedded() {
        ConversationView("demo-s3", true);
    }

    #[test]
    fn fr_l6_embedded_composer_shows_send_beside_stop_when_busy() {
        // FR-L6: while the agent is busy, the embedded composer renders both StopCircleButton and
        // SendCircleButton in the same row so the user can queue a follow-up by tap as well as Enter.
        let _guard = crate::services::test_guard();
        let p = ember_theme().palette;
        let stop_bg = format!("{:?}", compose_rust::schema::Modifier::Background(paint(p.red.strong)));
        let send_bg = format!("{:?}", compose_rust::schema::Modifier::Background(paint(p.ink)));
        let is_stop = |n: &Emitted| {
            n.mods.contains(&stop_bg)
                && n.mods.iter().any(|m| m == "Width(30.0)")
                && n.mods.iter().any(|m| m == "Height(30.0)")
                && n.props.iter().any(|prop| prop.starts_with("OnClick="))
        };
        let is_send = |n: &Emitted| {
            n.mods.contains(&send_bg)
                && n.mods.iter().any(|m| m == "Width(32.0)")
                && n.mods.iter().any(|m| m == "Height(32.0)")
                && n.props.iter().any(|prop| prop.starts_with("OnClick="))
                && (n.mods.iter().any(|m| m.contains(crate::ui::cr::a11y::SEND))
                    || n.props.iter().any(|prop| prop.contains(crate::ui::cr::a11y::SEND)))
        };

        // Busy session (SessionStatus::Running): both stop and send sit in the composer row.
        services().open_session("demo-s3");
        let busy_nodes = emitted(host_conversation_s3_embedded);
        let stop = busy_nodes.iter().find(|n| is_stop(n)).expect("StopCircleButton while busy");
        let send = busy_nodes.iter().find(|n| is_send(n)).expect("SendCircleButton while busy");
        assert!(stop.parent.is_some() && stop.parent == send.parent, "stop and send share the composer row");

        // Idle/finished session: send is shown and stop is not.
        services().open_session("demo-s2");
        let idle_nodes = emitted(host_conversation_s2_embedded);
        assert!(idle_nodes.iter().any(is_send), "SendCircleButton when idle");
        assert!(!idle_nodes.iter().any(is_stop), "no StopCircleButton when idle");
    }

    fn shell_call_item(seq: i64, call_id: &str, cmd: &str) -> TranscriptItem {
        TranscriptItem::ToolCall {
            seq,
            call_id: call_id.into(),
            name: "Bash".into(),
            input: serde_json::json!({ "command": cmd }),
            result: Some(ember_client::transcript::ToolOutput {
                output: String::new(),
                is_error: false,
            }),
        }
    }

    fn consecutive_shell_rows() {
        let expanded = remember(|| mutable_state_of(HashSet::<String>::new()));
        let answering = remember(|| mutable_state_of(HashSet::<String>::new()));
        let items = vec![
            shell_call_item(1, "s1", "cargo fmt --check"),
            shell_call_item(2, "s2", "cargo clippy"),
            shell_call_item(3, "s3", "cargo test --locked"),
        ];
        let grouped = group_shell_calls(&items);
        { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().fill_max_width(), content = move || {
            for item in grouped.iter() {
                TranscriptDisplayRow("demo-s1", "claude", item, &expanded, &answering, None, false);
            }
        }) };
    }

    #[test]
    fn fr_l5_consecutive_shell_calls_fold_into_collapsible_group_row() {
        use compose_rust::PropertyKind;
        use std::collections::HashMap;

        let _guard = crate::services::test_guard();
        let mut host = Host::new(|| Box::new(Recomposer::new(consecutive_shell_rows)));
        let batch = decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        let initial_texts = texts(&batch);
        assert!(
            initial_texts.iter().any(|t| t == "명령 3개 실행"),
            "missing grouped header in initial render: {initial_texts:?}"
        );
        for cmd in ["cargo fmt --check", "cargo clippy", "cargo test --locked"] {
            assert!(
                !initial_texts.iter().any(|t| t.contains(cmd)),
                "individual command {cmd:?} should be hidden while collapsed: {initial_texts:?}"
            );
        }

        // Locate the clickable ancestor of the "명령 3개 실행" label and click it.
        let mut parents: HashMap<u32, u32> = HashMap::new();
        let mut handlers: HashMap<u32, u64> = HashMap::new();
        let mut label_node: Option<u32> = None;
        for m in &batch {
            match m {
                Mutation::Insert { parent_id, node_id, .. } => {
                    parents.insert(*node_id, *parent_id);
                }
                Mutation::SetProp {
                    node_id,
                    property: PropertyKind::OnClick,
                    value: PropertyValue::Integer(h),
                } => {
                    handlers.insert(*node_id, *h as u64);
                }
                Mutation::SetProp {
                    node_id,
                    property: PropertyKind::Text,
                    value: PropertyValue::String(s),
                } if *s == "명령 3개 실행" => {
                    label_node = Some(*node_id);
                }
                _ => {}
            }
        }
        let mut cursor = label_node.expect("group label node");
        let (click_node, handler_id) = loop {
            if let Some(&h) = handlers.get(&cursor) {
                break (cursor, h);
            }
            cursor = *parents
                .get(&cursor)
                .expect("group label has a clickable ancestor");
        };

        let (bytes, _) = host
            .dispatch(HostEvent::new(
                click_node,
                handler_id,
                EventPayload::Clicked,
            ))
            .expect("click group row");
        let expanded_texts = texts(&decode_batch(bytes).expect("decode expanded"));
        for cmd in ["cargo fmt --check", "cargo clippy", "cargo test --locked"] {
            assert!(
                expanded_texts.iter().any(|t| t.contains(cmd)),
                "missing expanded command {cmd:?}: {expanded_texts:?}"
            );
        }
    }
}
