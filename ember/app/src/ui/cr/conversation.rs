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
    MutableState, current_window_size, launched_effect, mutable_state_of, remember,
    remember_with_key,
};
use compose_rust::ui::{self as cu, Modifier};
use compose_rust::{
    Alignment, ColorRole, IconRole, Paint, SpaceRole, TextAlign, TextOverflow, TypeRole,
};

use ember_client::transcript::{ApprovalState, TranscriptItem};
use ember_client::wire::{ApprovalDecision, MentionCandidate, SessionPatch, TurnOutcome};
use ember_client::LauncherStatus;

pub fn item_key(item: &TranscriptItem) -> String {
    crate::ui::conversation::item_key(item)
}

use crate::ide::IdeKind;
use crate::model::{self, Tone};
use crate::server::{CurrentComputer, IdeLaunch};
use crate::services::{run_shared, services};
use crate::ui::coexist::{SharedUi, shared_ui};
use crate::ui::Route;

use super::compat::{Badge, Banner, CodeText, MessageText, Spinner, StatusIndicator};
use super::components::{
    AppButton, ButtonKind, EmberCard, EmberField, FieldOptions, IconButton, Label, OptionSheet,
    Pill, PillTone, Txt,
};
use super::ko;
use super::launcher::{ConnectionBanner, export, patch_session};
use super::notice;
use super::shell::NavIcon;
use super::theme::*;

pub const ICON_SHIELD_SVG: &str = include_str!("../../../assets/nav/shield.svg");
pub const ICON_TERMINAL_SVG: &str = include_str!("../../../assets/nav/terminal.svg");
pub const ICON_SEND_SVG: &str = include_str!("../../../assets/nav/send.svg");

const POPUP_W: f32 = 280.0;

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
    let computer_menu = remember_with_key(sid.clone(), || mutable_state_of(false));
    let more_menu = remember_with_key(sid.clone(), || mutable_state_of(false));

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
        cu::Column()
            .alignment(Alignment::Center)
            .space_role(SpaceRole::Md)
            .modifier(
                Modifier::new()
                    .fill_max_width()
                    .fill_max_height()
                    .padding_role(SpaceRole::Md),
            )
            .content(|| {
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
            });
        return;
    };

    let title = record.title.clone();
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
    let computer_name = computer
        .get()
        .as_ref()
        .map(|c| c.computer.name.clone())
        .unwrap_or_else(|| "이 컴퓨터".into());
    let computers = {
        let _ = ui.live.launcher.get();
        s.client.read(|st| st.computers().to_vec())
    };
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
                t.pending_approvals().count(),
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
            None => (Vec::new(), Vec::new(), 0, false),
        })
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
    let remembered = ui
        .prefs
        .get()
        .ide_target
        .as_deref()
        .and_then(IdeKind::from_key);
    let mut ide_order: Vec<IdeKind> = IdeKind::ALL.to_vec();
    if let Some(k) = remembered {
        ide_order.retain(|x| *x != k);
        ide_order.insert(0, k);
    }
    let ide_now = ide.get();
    let ide_entries: Vec<(IdeKind, String)> = ide_order
        .iter()
        .map(|k| {
            let mut label = k.label().to_string();
            if Some(*k) == remembered {
                label.push_str(" (최근 사용)");
            }
            if let Some(Ok(l)) = &ide_now {
                if let Some(t) = l.target(k.key()) {
                    if !t.available {
                        if let Some(r) = &t.reason {
                            label = format!("{label}: {r}");
                        }
                    }
                }
            }
            (*k, label)
        })
        .collect();

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

    let pinned = record.pinned;
    let agent = record.agent.clone();

    cu::Box()
        .modifier(Modifier::new().fill_max_width().fill_max_height())
        .content(|| {
            cu::Column()
                .space_role(SpaceRole::Sm)
                .modifier(
                    Modifier::new()
                        .fill_max_width()
                        .fill_max_height()
                        .padding(if embedded { SP[2] } else { SP[3] }),
                )
                .content(|| {
                    // ---- header (hidden when embedded in SessionScreen SingleView, which has its own compact topbar) ----
                    if !embedded {
                        cu::Row()
                            .alignment(Alignment::CenterStart)
                            .space_role(SpaceRole::Sm)
                            .modifier(Modifier::new().fill_max_width())
                            .content(|| {
                                if compact {
                                    let u = ui.clone();
                                    IconButton(IconRole::Back, true, move || u.go(Route::Projects));
                                }
                                cu::Column()
                                    .modifier(Modifier::new().weight(1.0))
                                    .content(|| {
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
                                    });
                                StatusIndicator(status);
                                let more = more_menu.clone();
                                IconButton(IconRole::More, true, move || more.set(true));
                            });
                    }

                    if let Some(text) = ko::approve_all_banner(approve_all) {
                        Banner(text, Tone::Warning);
                    }

                    if let Some(text) = fork_label.clone() {
                        cu::Row()
                            .alignment(Alignment::CenterStart)
                            .space_role(SpaceRole::Sm)
                            .modifier(Modifier::new().fill_max_width())
                            .content(|| {
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
                            });
                    }

                    if !embedded {
                        // Computer and IDE row
                        cu::Row()
                            .alignment(Alignment::CenterStart)
                            .space_role(SpaceRole::Sm)
                            .modifier(Modifier::new().fill_max_width())
                            .content(|| {
                                Label(
                                    "컴퓨터",
                                    Fs::Xs,
                                    Weight::Regular,
                                    Some(paint(p.muted)),
                                    Some(1),
                                );
                                let cm = computer_menu.clone();
                                AppButton(
                                    &computer_name,
                                    None,
                                    ButtonKind::Default,
                                    true,
                                    !busy,
                                    false,
                                    move || cm.set(true),
                                );
                                cu::Spacer().modifier(Modifier::new().weight(1.0));
                                if pending > 0 {
                                    Badge(&format!("승인 대기 {pending}"), Tone::Warning);
                                }
                                let im = ide_menu.clone();
                                AppButton(
                                    "IDE 열기",
                                    Some(IconRole::Forward),
                                    ButtonKind::Primary,
                                    true,
                                    true,
                                    false,
                                    move || im.set(true),
                                );
                            });
                    }

                    ConnectionBanner();

                    if busy && !embedded {
                        cu::IndeterminateProgressIndicator()
                            .circular(false)
                            .modifier(Modifier::new().fill_max_width());
                    }

                    TeamPanel(&sid);

                    // ---- transcript ------------------------------------------------------------
                    cu::Box()
                        .modifier(Modifier::new().fill_max_width().weight(1.0))
                        .content(|| {
                            if count == 0 {
                                cu::Box()
                                    .alignment(Alignment::Center)
                                    .modifier(
                                        Modifier::new().fill_max_width().fill_max_height(),
                                    )
                                    .content(|| {
                                        let empty_msg = if status == LauncherStatus::Idle {
                                            "무엇을 작업할지 입력해 주세요."
                                        } else {
                                            "대화를 불러오는 중입니다\u{2026}"
                                        };
                                        Label(
                                            empty_msg,
                                            Fs::Md,
                                            Weight::Regular,
                                            Some(paint(p.muted)),
                                            None,
                                        );
                                    });
                            } else {
                                let items_row = items.clone();
                                let _keys_row = keys.clone();
                                let sid_row = sid.clone();
                                let agent_row = agent.clone();
                                let exp_row = expanded.clone();
                                let ans_row = answering.clone();
                                cu::ScrollColumn()
                                    .modifier(
                                        Modifier::new().fill_max_width().fill_max_height(),
                                    )
                                    .content(move || {
                                        cu::Column()
                                            .space_role(SpaceRole::Sm)
                                            .modifier(
                                                Modifier::new()
                                                    .fill_max_width()
                                                    .padding_role(SpaceRole::Sm),
                                            )
                                            .content(move || {
                                                for item in items_row.iter() {
                                                    TranscriptRow(
                                                        &sid_row,
                                                        &agent_row,
                                                        item,
                                                        &exp_row,
                                                        &ans_row,
                                                    );
                                                }
                                                cu::Spacer().modifier(Modifier::new().height(24.0));
                                            });
                                    });
                            }
                        });

                    // ---- queue (FR-L6) ---------------------------------------------------------
                    if !queued.is_empty() {
                        cu::Row()
                            .alignment(Alignment::CenterStart)
                            .space_role(SpaceRole::Sm)
                            .modifier(Modifier::new().fill_max_width())
                            .content(|| {
                                Badge(&format!("{}개 대기 중", queued.len()), Tone::Accent);
                                cu::Box().modifier(Modifier::new().weight(1.0)).content(|| {
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
                                });
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
                            });
                    }
                    if !queue_error.is_empty() {
                        cu::Row()
                            .alignment(Alignment::CenterStart)
                            .space_role(SpaceRole::Sm)
                            .modifier(Modifier::new().fill_max_width())
                            .content(|| {
                                cu::Box().modifier(Modifier::new().weight(1.0)).content(|| {
                                    Banner(
                                        &format!("전송하지 못했습니다: {queue_error}"),
                                        Tone::Error,
                                    );
                                });
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
                            });
                    }

                    // ---- mention suggestions (FR-T6) -------------------------------------------
                    if !suggestions.is_empty() {
                        EmberCard(false, true, None, Some(SP[1]), None, || {
                            cu::Column()
                                .space_role(SpaceRole::Xs)
                                .modifier(Modifier::new().fill_max_width())
                                .content(|| {
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
                                });
                        });
                    }
                    if !picked.is_empty() {
                        cu::Row()
                            .alignment(Alignment::CenterStart)
                            .space_role(SpaceRole::Sm)
                            .modifier(Modifier::new().fill_max_width())
                            .content(|| {
                                cu::Box().modifier(Modifier::new().weight(1.0)).content(|| {
                                    Label(
                                        &format!("멘션 대상: {}", picked.join(", ")),
                                        Fs::Xs,
                                        Weight::Regular,
                                        Some(paint(p.muted)),
                                        Some(1),
                                    );
                                });
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
                            });
                    }

                    // ---- composer --------------------------------------------------------------
                    let _gen = composer_generation.get();
                    if embedded {
                        cu::Column()
                            .space_role(SpaceRole::Sm)
                            .modifier(
                                Modifier::new()
                                    .fill_max_width()
                                    .background(paint(p.surface))
                                    .border(1.0, paint(p.line))
                                    .corner_radius(CORNER_XL)
                                    .padding(SP[2]),
                            )
                            .content(|| {
                                let send_sub = send.clone();
                                EmberField(FieldOptions {
                                    bare: true,
                                    fill_max_width: true,
                                    multiline: true,
                                    placeholder: if busy {
                                        "다음 턴에 보낼 메시지를 입력하세요".to_string()
                                    } else {
                                        "무엇이든 물어보세요. @@로 세션 멘션".to_string()
                                    },
                                    on_value_change: Some(Box::new(on_draft.clone())),
                                    on_submit: Some(Box::new(move |v| send_sub(v))),
                                    ..Default::default()
                                });
                                cu::Row()
                                    .alignment(Alignment::CenterStart)
                                    .space_role(SpaceRole::Xs)
                                    .modifier(Modifier::new().fill_max_width())
                                    .content(|| {
                                        cu::Box().modifier(Modifier::new().weight(1.0)).content(|| {
                                            super::session::ComposerChips(&sid);
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
                                    });
                            });
                    } else {
                        cu::Row()
                            .alignment(Alignment::CenterStart)
                            .space_role(SpaceRole::Xs)
                            .modifier(
                                Modifier::new()
                                    .fill_max_width()
                                    .background(paint(p.surface))
                                    .border(1.0, paint(p.line))
                                    .corner_radius(CORNER_XL)
                                    .padding(SP[1]),
                            )
                            .content(|| {
                                let send_sub = send.clone();
                                EmberField(FieldOptions {
                                    bare: true,
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
                            });
                    }
                });

            // ---- Popups (outside spaced Column) ------------------------------------------------
            // 1. More menu OptionSheet
            let more_labels = vec![
                if pinned {
                    "고정 해제".to_string()
                } else {
                    "고정".to_string()
                },
                "내보내기".to_string(),
                ko::FORK_MENU_LABEL.to_string(),
                ko::approve_all_menu_label(approve_all).to_string(),
            ];
            let more_dismiss = more_menu.clone();
            let sid_more = sid.clone();
            let title_more = title.clone();
            let ui_more = ui.clone();
            OptionSheet(
                more_menu.get(),
                &more_labels,
                None,
                POPUP_W,
                move || more_dismiss.set(false),
                move |idx| match idx {
                    0 => patch_session(
                        sid_more.clone(),
                        SessionPatch {
                            pinned: Some(!pinned),
                            ..Default::default()
                        },
                    ),
                    1 => export(sid_more.clone(), title_more.clone()),
                    2 => fork_from(&ui_more, sid_more.clone(), None),
                    3 => {
                        let id_ap = sid_more.clone();
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
            );

            // 2. Computer menu OptionSheet
            let comp_labels: Vec<String> = if computers.is_empty() {
                vec!["등록된 컴퓨터가 없습니다".to_string()]
            } else {
                computers
                    .iter()
                    .map(|c| {
                        format!(
                            "{} ({})",
                            c.name,
                            ko::reachability_label(c.reachability)
                        )
                    })
                    .collect()
            };
            let comp_dismiss = computer_menu.clone();
            let comps_for_pick = computers.clone();
            let sid_comp = sid.clone();
            let reload_comp = load.clone();
            OptionSheet(
                computer_menu.get(),
                &comp_labels,
                None,
                POPUP_W,
                move || comp_dismiss.set(false),
                move |idx| {
                    let Some(c) = comps_for_pick.get(idx) else {
                        return;
                    };
                    if demo_blocked_cr() {
                        return;
                    }
                    let cid = c.id.clone();
                    let s_id = sid_comp.clone();
                    let reload = reload_comp.clone();
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
                                reload();
                                notice::post(format!(
                                    "{} 컴퓨터로 전환했습니다",
                                    out.computer.name
                                ));
                            }
                            Err(e) => {
                                notice::post(format!("컴퓨터를 전환하지 못했습니다: {e}"))
                            }
                        },
                    );
                },
            );

            // 3. IDE menu OptionSheet (when not embedded)
            if !embedded {
                let ide_labels: Vec<String> =
                    ide_entries.iter().map(|(_, l)| l.clone()).collect();
                let ide_kinds: Vec<IdeKind> = ide_entries.iter().map(|(k, _)| *k).collect();
                let ide_dismiss = ide_menu.clone();
                let ui_ide = ui.clone();
                let launch_res = ide.get();
                OptionSheet(
                    ide_menu.get(),
                    &ide_labels,
                    Some(0),
                    POPUP_W,
                    move || ide_dismiss.set(false),
                    move |idx| {
                        let Some(&kind) = ide_kinds.get(idx) else {
                            return;
                        };
                        ui_ide.update_prefs(|p| p.ide_target = Some(kind.key().to_string()));
                        super::session::launch_ide_shared(&ui_ide, kind, launch_res.clone());
                    },
                );
            }
        });
}

/// One transcript entry (FR-L5..FR-L7, FR-S5).
pub fn TranscriptRow(
    session: &str,
    agent: &str,
    item: &TranscriptItem,
    expanded: &MutableState<HashSet<String>>,
    answering: &MutableState<HashSet<String>>,
) {
    let theme = ember_theme();
    let p = theme.palette;
    let sid = session.to_string();
    let agent_name = agent.to_string();

    match item.clone() {
        TranscriptItem::User { text, .. } => {
            cu::Box()
                .alignment(Alignment::CenterEnd)
                .modifier(Modifier::new().fill_max_width().padding_role(SpaceRole::Xs))
                .content(move || {
                    cu::Column()
                        .alignment(Alignment::CenterEnd)
                        .space_role(SpaceRole::Xs)
                        .content(move || {
                            Label(
                                "나",
                                Fs::Xs,
                                Weight::Regular,
                                Some(paint(p.muted)),
                                Some(1),
                            );
                            cu::Box()
                                .modifier(
                                    Modifier::new()
                                        .background(paint(p.ink))
                                        .corner_radius(CORNER_BUBBLE)
                                        .padding(SP[2] + 4.0),
                                )
                                .content(move || {
                                    MessageText(&text, Some(ColorRole::OnPrimary));
                                });
                        });
                });
        }
        TranscriptItem::Assistant {
            text, streaming, ..
        } => {
            cu::Box()
                .alignment(Alignment::CenterStart)
                .modifier(Modifier::new().fill_max_width().padding_role(SpaceRole::Xs))
                .content(move || {
                    cu::Column().space_role(SpaceRole::Xs).content(move || {
                        cu::Row()
                            .alignment(Alignment::CenterStart)
                            .space_role(SpaceRole::Xs)
                            .content(move || {
                                Label(
                                    &agent_name,
                                    Fs::Xs,
                                    Weight::Regular,
                                    Some(paint(p.muted)),
                                    Some(1),
                                );
                                if streaming {
                                    Spinner(10.0);
                                }
                            });
                        cu::Box()
                            .modifier(
                                Modifier::new()
                                    .background(paint(p.surface_2))
                                    .corner_radius(CORNER_BUBBLE)
                                    .padding(SP[2] + 4.0),
                            )
                            .content(move || {
                                let shown = if streaming {
                                    format!("{text}\u{2589}")
                                } else {
                                    text.clone()
                                };
                                MessageText(&shown, None);
                            });
                    });
                });
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
            let show_tail = has_result && !out_text.trim().is_empty();

            EmberCard(false, true, None, Some(SP[2]), None, move || {
                cu::Column()
                    .space_role(SpaceRole::Xs)
                    .modifier(Modifier::new().fill_max_width())
                    .content(move || {
                        cu::Row()
                            .alignment(Alignment::CenterStart)
                            .space_role(SpaceRole::Sm)
                            .modifier(Modifier::new().fill_max_width())
                            .content(move || {
                                if !has_result {
                                    Spinner(12.0);
                                } else {
                                    NavIcon(ICON_TERMINAL_SVG, p.muted.to_argb(), 16.0);
                                }
                                Label(&tool_name, Fs::Sm, Weight::Semibold, None, Some(1));
                                cu::Box().modifier(Modifier::new().weight(1.0)).content(
                                    move || {
                                        Label(
                                            &summary,
                                            Fs::Xs,
                                            Weight::Regular,
                                            Some(paint(p.muted)),
                                            Some(1),
                                        );
                                    },
                                );
                                Badge(state_text, tone);
                                AppButton(
                                    if open { "접기" } else { "자세히" },
                                    Some(if open {
                                        IconRole::Collapse
                                    } else {
                                        IconRole::More
                                    }),
                                    ButtonKind::Default,
                                    true,
                                    true,
                                    false,
                                    move || {
                                        exp_toggle.update(|set| {
                                            if !set.remove(&cid_toggle) {
                                                set.insert(cid_toggle.clone());
                                            }
                                        });
                                    },
                                );
                            });
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
                            CodeText(&out_text, Some(3), out_err);
                        }
                    });
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
            let (a1, a2, a3) = (answer.clone(), answer.clone(), answer);
            let border = if state == ApprovalState::Pending {
                Some(p.orange.strong)
            } else {
                None
            };
            EmberCard(false, true, None, Some(SP[2]), border, move || {
                cu::Column()
                    .space_role(SpaceRole::Xs)
                    .modifier(Modifier::new().fill_max_width())
                    .content(move || {
                        cu::Row()
                            .alignment(Alignment::CenterStart)
                            .space_role(SpaceRole::Sm)
                            .modifier(Modifier::new().fill_max_width())
                            .content(|| {
                                NavIcon(ICON_SHIELD_SVG, p.orange.strong.to_argb(), 16.0);
                                Label(
                                    &format!("{tool} 실행을 허용할까요?"),
                                    Fs::Md,
                                    Weight::Semibold,
                                    None,
                                    Some(1),
                                );
                                cu::Spacer().modifier(Modifier::new().weight(1.0));
                                if state == ApprovalState::Pending {
                                    Pill("대기 중", PillTone::Orange, true);
                                }
                            });
                        CodeText(&model::json_pretty(&input), Some(12), false);
                        match state {
                            ApprovalState::Pending => {
                                cu::Row()
                                    .alignment(Alignment::CenterStart)
                                    .space_role(SpaceRole::Sm)
                                    .modifier(Modifier::new().fill_max_width())
                                    .content(move || {
                                        let f1 = a1.clone();
                                        AppButton(
                                            "한 번 허용",
                                            None,
                                            ButtonKind::Primary,
                                            true,
                                            !in_flight,
                                            false,
                                            move || f1(ApprovalDecision::AllowOnce),
                                        );
                                        let f2 = a2.clone();
                                        AppButton(
                                            "항상 허용",
                                            None,
                                            ButtonKind::Default,
                                            true,
                                            !in_flight,
                                            false,
                                            move || f2(ApprovalDecision::AllowAlways),
                                        );
                                        let f3 = a3.clone();
                                        AppButton(
                                            "거부",
                                            None,
                                            ButtonKind::Destructive,
                                            true,
                                            !in_flight,
                                            false,
                                            move || f3(ApprovalDecision::Deny),
                                        );
                                        if in_flight {
                                            Spinner(14.0);
                                        }
                                    });
                            }
                            ApprovalState::Resolved(d) => {
                                let (txt, col) = match d {
                                    ApprovalDecision::AllowOnce => ("허용함", p.green.strong),
                                    ApprovalDecision::AllowAlways => ("항상 허용", p.green.strong),
                                    ApprovalDecision::Deny => ("거부함", p.red.strong),
                                };
                                Label(txt, Fs::Xs, Weight::Medium, Some(paint(col)), Some(1));
                            }
                            ApprovalState::Abandoned => {
                                Label(
                                    "턴이 끝나기 전에 응답하지 않았습니다",
                                    Fs::Xs,
                                    Weight::Regular,
                                    Some(paint(p.muted)),
                                    Some(1),
                                );
                            }
                        }
                    });
            });
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
            cu::Row()
                .alignment(Alignment::Center)
                .space_role(SpaceRole::Sm)
                .modifier(Modifier::new().fill_max_width().padding_role(SpaceRole::Xs))
                .content(move || {
                    cu::Box()
                        .modifier(Modifier::new().weight(1.0))
                        .content(|| {
                            cu::Divider();
                        });
                    Label(text, Fs::Xs, Weight::Regular, Some(paint(col)), Some(1));
                    if outcome == TurnOutcome::Completed {
                        let u_fork = ui.clone();
                        let s_fork = sid.clone();
                        AppButton(
                            ko::FORK_TURN_LABEL,
                            None,
                            ButtonKind::Default,
                            true,
                            can,
                            false,
                            move || fork_from(&u_fork, s_fork.clone(), Some(seq)),
                        );
                    }
                    cu::Box()
                        .modifier(Modifier::new().weight(1.0))
                        .content(|| {
                            cu::Divider();
                        });
                });
        }
        TranscriptItem::Error { message, .. } => {
            Banner(&message, Tone::Error);
        }
        TranscriptItem::Notice { text, .. } => {
            cu::Row()
                .alignment(Alignment::Center)
                .modifier(Modifier::new().fill_max_width().padding_role(SpaceRole::Xs))
                .content(move || {
                    Txt(
                        &text,
                        TypeRole::Caption,
                        Some(Paint::Role(ColorRole::OnSurfaceVariant)),
                        None,
                        None,
                        None,
                        Some(TextAlign::Center),
                    );
                });
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
        cu::Column()
            .space_role(SpaceRole::Xs)
            .modifier(Modifier::new().fill_max_width())
            .content(|| {
                let is_open = expanded.get();
                cu::Row()
                    .alignment(Alignment::CenterStart)
                    .space_role(SpaceRole::Sm)
                    .modifier(Modifier::new().fill_max_width())
                    .content(|| {
                        cu::Box().modifier(Modifier::new().weight(1.0)).content(|| {
                            Label(
                                &format!("팀 \u{00b7} 팀원 {active}명 \u{00b7} 열린 작업 {open}개"),
                                Fs::Md,
                                Weight::Medium,
                                None,
                                Some(1),
                            );
                        });
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
                    });
                if is_open {
                    for r in rows.iter().cloned() {
                        let u = ui.clone();
                        let tid = team_id.clone();
                        cu::Row()
                            .alignment(Alignment::CenterStart)
                            .space_role(SpaceRole::Sm)
                            .modifier(Modifier::new().fill_max_width())
                            .content(move || {
                                let name_label = if r.leader {
                                    format!("{} (리더)", r.name)
                                } else {
                                    r.name.clone()
                                };
                                Label(
                                    &name_label,
                                    Fs::Md,
                                    if r.this {
                                        Weight::Medium
                                    } else {
                                        Weight::Regular
                                    },
                                    None,
                                    Some(1),
                                );
                                let sub = if r.ended {
                                    format!("{} \u{00b7} 종료됨", r.agent)
                                } else {
                                    format!("{} \u{00b7} {}", r.agent, r.status)
                                };
                                cu::Box().modifier(Modifier::new().weight(1.0)).content(
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
                            });
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
            });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use compose_rust::protocol::{HostEvent, Mutation, PropertyValue, decode_batch};
    use compose_rust::{
        EventPayload, Host, Recomposer, WindowHeightClass, WindowSizeClass, composable,
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

    #[composable]
    fn host_conversation_s1() {
        ConversationView("demo-s1", false);
    }

    #[test]
    fn fr_l5_conversation_view_renders_demo_transcript_and_approval_in_compose_rust() {
        let _guard = crate::services::test_guard();
        services().open_session("demo-s1");
        compose_rust::window::reset_window_size();
        let mut host = Host::new(|| Box::new(Recomposer::new(host_conversation_s1)));
        let mut all = texts(&decode_batch(host.rebuild().expect("rebuild")).expect("decode"));
        let event = HostEvent {
            node_id: 0,
            handler_id: 0,
            payload: EventPayload::WindowSizeChanged {
                width_dp: 1440.0,
                height_dp: 900.0,
                class: WindowSizeClass::Expanded,
                height_class: WindowHeightClass::Medium,
            },
        };
        let (bytes, _) = host.dispatch(event).expect("dispatch");
        all.extend(texts(&decode_batch(bytes).expect("decode")));
        for want in ["포트 설정 추가", "한 번 허용", "항상 허용", "거부", "IDE 열기"] {
            assert!(
                all.iter().any(|t| t.contains(want)),
                "missing {want} in ConversationView: {all:?}"
            );
        }
        compose_rust::window::reset_window_size();
    }
}
