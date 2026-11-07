//! The 새 채널 sheet (FR-T10, FR-L12, #499): the `+` button of the channel list opens a form for
//! the name, the project and the project's agent sessions that join the channel.
//!
//! The decisions (name rules, members subset, hint, error sentences) are
//! `ui::channels::NewChannelDraft` and its functions. This file draws the sheet and sends
//! `Api::create_channel`. In demo mode and in review captures there is no channel list to write
//! to, so the button tells why instead of opening a form that cannot create.

#![allow(non_snake_case)]

use compose_rust::runtime::{mutable_state_of, remember, MutableState};
use compose_rust::ui as cu;
use compose_rust::ui::Modifier;
use compose_rust::{Alignment, ButtonVariant, Color, ShapeRole};

use super::channels::Feed;
use super::components::{AppButton, ButtonKind, EmberField, EmberSheet, FieldOptions, FilterChip, Label, SheetFooter};
use super::notice;
use super::theme::*;
use crate::ui::channels::{
    agent_choices, create_error_text, default_new_project, AgentChoice, NewChannelDraft, Room, DEMO_NO_CREATE,
};
use crate::ui::project::{display_name, CloseAction, SettingsGuard, DISCARD_QUESTION};

/// What the sheet holds between recompositions.
#[derive(Clone)]
pub struct NewChannelState {
    pub open: MutableState<bool>,
    draft: MutableState<NewChannelDraft>,
    /// Bumped to draw a fresh name field (the field is uncontrolled).
    generation: MutableState<u64>,
    busy: MutableState<bool>,
    /// The last failure, a fixed sentence.
    failure: MutableState<Option<String>>,
    confirm: MutableState<bool>,
    /// A review window (debug builds, `EMBER_CR_NEWCHANNEL=1`): sample choices, 만들기 never sends.
    review: bool,
}

impl NewChannelState {
    pub fn new() -> NewChannelState {
        remember(|| {
            let review = review_hook();
            let draft = if review {
                let (projects, _) = review_sources();
                let mut d = NewChannelDraft::new(projects.first().map(String::as_str).unwrap_or(""));
                d.name = "리뷰 채널 (예시)".to_string();
                d
            } else {
                NewChannelDraft::default()
            };
            NewChannelState {
                open: mutable_state_of(review),
                draft: mutable_state_of(draft),
                generation: mutable_state_of(0),
                busy: mutable_state_of(false),
                failure: mutable_state_of(None),
                confirm: mutable_state_of(false),
                review,
            }
        })
    }

    /// Starts an empty draft on the filtered project, else the first.
    fn start(&self, filter: Option<&str>) {
        let projects = project_names();
        self.draft.set(NewChannelDraft::new(&default_new_project(filter, &projects)));
        self.generation.set(self.generation.get_untracked() + 1);
        self.failure.set(None);
        self.confirm.set(false);
        self.open.set(true);
    }

    fn close(&self) {
        self.open.set(false);
        self.confirm.set(false);
    }
}

/// The review hook (debug builds only): `EMBER_CR_NEWCHANNEL=1` with `EMBER_CR_ROUTE=channels`.
fn review_hook() -> bool {
    crate::ui::channels::new_channel_review_hook(
        std::env::var("EMBER_CR_NEWCHANNEL").ok().as_deref(),
        std::env::var("EMBER_CR_ROUTE").ok().as_deref(),
        cfg!(debug_assertions),
    )
}

/// Review sample choices: the sample rooms' projects and their agent members.
fn review_sources() -> (Vec<String>, Vec<AgentChoice>) {
    let mut projects: Vec<String> = Vec::new();
    let mut agents: Vec<AgentChoice> = Vec::new();
    for r in super::channels::sample_rooms() {
        if !projects.contains(&r.project) {
            projects.push(r.project.clone());
        }
        for m in r.members.iter().filter(|m| m.is_agent) {
            if !agents.iter().any(|a| a.id == m.id && a.project == r.project) {
                agents.push(AgentChoice { id: m.id.clone(), project: r.project.clone(), label: m.name.clone(), harness: m.harness.clone() });
            }
        }
    }
    (projects, agents)
}

fn project_names() -> Vec<String> {
    crate::services::try_services()
        .map(|s| s.client.read(|st| st.projects().into_iter().map(|p| p.name).collect()))
        .unwrap_or_default()
}

/// The sessions of every project, for the member chips.
fn all_sessions() -> Vec<ember_client::wire::SessionRecord> {
    crate::services::try_services()
        .map(|s| {
            s.client.read(|st| {
                st.projects()
                    .iter()
                    .flat_map(|p| p.sessions.iter())
                    .filter_map(|id| st.session(id).map(|v| v.record.clone()))
                    .collect()
            })
        })
        .unwrap_or_default()
}

/// The round `+` of the list header. `live` is false in demo mode and for review captures: a tap
/// then says why nothing is created. The overlay button gives it the accessible name and focus
/// (TODO(compose-rust#425, #429): remove when Button takes content or a semantics modifier exists).
pub fn NewChannelButton(st: &NewChannelState, live: bool, filter: Option<String>) {
    let p = ember_theme().palette;
    let st = st.clone();
    let label = if live { "새 채널".to_string() } else { format!("새 채널, {DEMO_NO_CREATE}") };
    let alpha = if live { 1.0 } else { 0.4 };
    cu::Box()
        .alignment(Alignment::Center)
        .modifier(
            Modifier::new()
                .size(H_CTL, H_CTL)
                .background(paint(p.surface))
                .border(1.0, paint(p.line))
                .shape_role(ShapeRole::Full)
                .alpha(alpha),
        )
        .content(|| {
            super::shell::NavIcon(super::shell::ICON_PLUS_SVG, p.text_2.to_argb(), 16.0);
            cu::Button(&label)
                .variant(ButtonVariant::Text)
                .color(compose_rust::Paint::Literal(Color::argb(0)))
                .modifier(Modifier::new().fill_max_size())
                .on_click(move || {
                    if live {
                        st.start(filter.as_deref());
                    } else {
                        notice::post(DEMO_NO_CREATE);
                    }
                });
        });
}

/// Sends the draft. On success the list is read again, the new room opens and the sheet closes;
/// a failure is told in a fixed sentence and the draft stays.
fn create(st: &NewChannelState, feed: &Feed, open: &MutableState<Option<String>>, rooms: &[Room], agents: &[AgentChoice]) {
    if st.busy.get_untracked() {
        return;
    }
    let Some(body) = st.draft.get_untracked().to_new_channel(rooms, agents) else { return };
    st.busy.set(true);
    st.failure.set(None);
    let (st, feed, open) = (st.clone(), feed.clone(), open.clone());
    crate::services::run_shared(
        async move {
            let api = crate::services::services().client.api();
            let made = api.create_channel(&body).await?;
            // A failed re-read does not undo the create; the new room is added by hand then.
            let list = api.channels(None).await.ok();
            Ok::<_, ember_client::api::ApiError>((made, list))
        },
        move |r| {
            st.busy.set(false);
            match r {
                Ok((made, list)) => {
                    let old = feed.rooms.get_untracked();
                    match list {
                        Some(list) => feed.rooms.set(crate::ui::channels::rooms_from(&list, &old)),
                        None => feed.rooms.update(|v| v.push(crate::ui::channels::room_from(&made))),
                    }
                    if let Some(d) = crate::services::try_services().map(|s| s.ui.detail_open.clone()) {
                        d.set(true);
                    }
                    open.set(Some(made.id));
                    st.close();
                }
                Err(e) => {
                    tracing::warn!("create channel failed: {e}");
                    st.failure.set(Some(create_error_text(e.status(), e.code()).to_string()));
                }
            }
        },
    );
}

/// The sheet. `rooms` are the existing channels (a taken name is caught here), `feed` is `None`
/// when nothing can be created.
pub fn NewChannelSheet(st: &NewChannelState, rooms: &[Room], feed: Option<Feed>, open: &MutableState<Option<String>>) {
    let p = ember_theme().palette;
    let draft = st.draft.get();
    let busy = st.busy.get();
    let request_close = {
        let st = st.clone();
        move || match SettingsGuard::new(st.busy.get_untracked(), !st.draft.get_untracked().is_dirty()).on_close() {
            CloseAction::Ignore => {}
            CloseAction::Confirm => st.confirm.set(true),
            CloseAction::Close => st.close(),
        }
    };
    let review = st.review;
    let (projects, agents) = if review {
        let (ps, all) = review_sources();
        (ps, all.into_iter().filter(|a| a.project == draft.project).collect::<Vec<_>>())
    } else {
        (project_names(), agent_choices(&all_sessions(), &draft.project))
    };
    let problem = draft.problem(rooms);
    let valid = problem.is_none() && (feed.is_some() || review) && !busy;
    let failure = st.failure.get();
    let sheet_open = st.open.get();

    EmberSheet("새 채널", sheet_open, request_close.clone(), None, || {
        cu::Column().modifier(Modifier::new().fill_max_width()).content(|| {
            cu::Column().spacing(SP[3]).modifier(Modifier::new().fill_max_width().padding(SP[4])).content(|| {
                // Name.
                cu::Column().spacing(4.0).modifier(Modifier::new().fill_max_width()).content(|| {
                    Label("이름", Fs::Xs, Weight::Medium, Some(paint(p.text_2)), Some(1));
                    let _gen = st.generation.get();
                    let d = st.draft.clone();
                    // TODO(compose-rust#374): the field is uncontrolled.
                    EmberField(FieldOptions {
                        fill_max_width: true,
                        placeholder: "채널 이름".to_string(),
                        text: Some(draft.name.clone()),
                        enabled: Some(!busy),
                        on_value_change: Some(Box::new(move |v| {
                            let mut x = d.get_untracked();
                            x.name = v;
                            d.set(x);
                        })),
                        ..Default::default()
                    });
                    // An empty name is not an error yet; the 만들기 button simply waits.
                    if let Some(e) = problem.filter(|_| !draft.name.trim().is_empty()) {
                        Label(e.text(), Fs::Sm, Weight::Regular, Some(paint(p.red.strong)), None);
                    }
                });
                // Project.
                cu::Column().spacing(4.0).modifier(Modifier::new().fill_max_width()).content(|| {
                    Label("프로젝트", Fs::Xs, Weight::Medium, Some(paint(p.text_2)), Some(1));
                    if projects.is_empty() {
                        Label("프로젝트가 없습니다", Fs::Sm, Weight::Regular, Some(paint(p.muted)), None);
                    }
                    cu::ScrollRow().modifier(Modifier::new().fill_max_width().height(H_CTL + SP[0])).content(|| {
                        for name in projects.iter() {
                            let (d, n) = (st.draft.clone(), name.clone());
                            FilterChip(&display_name(name), *name == draft.project, move || {
                                let mut x = d.get_untracked();
                                x.set_project(&n);
                                d.set(x);
                            });
                            cu::Spacer().modifier(Modifier::new().width(SP[0]));
                        }
                    });
                });
                // Members.
                cu::Column().spacing(4.0).modifier(Modifier::new().fill_max_width()).content(|| {
                    Label("멤버 (에이전트)", Fs::Xs, Weight::Medium, Some(paint(p.text_2)), Some(1));
                    if agents.is_empty() {
                        Label("이 프로젝트에는 에이전트 세션이 없습니다", Fs::Sm, Weight::Regular, Some(paint(p.muted)), None);
                    }
                    cu::ScrollRow().modifier(Modifier::new().fill_max_width().height(H_CTL + SP[0])).content(|| {
                        for a in agents.iter() {
                            let (d, id) = (st.draft.clone(), a.id.clone());
                            FilterChip(&a.label, draft.has_member(&a.id), move || {
                                let mut x = d.get_untracked();
                                x.toggle_member(&id);
                                d.set(x);
                            });
                            cu::Spacer().modifier(Modifier::new().width(SP[0]));
                        }
                    });
                    if let Some(h) = draft.members_hint() {
                        Label(h, Fs::Sm, Weight::Regular, Some(paint(p.muted)), None);
                    }
                });
                if let Some(f) = failure.as_ref() {
                    Label(f, Fs::Sm, Weight::Regular, Some(paint(p.red.strong)), None);
                }
                if feed.is_none() && !review {
                    Label(DEMO_NO_CREATE, Fs::Sm, Weight::Regular, Some(paint(p.muted)), None);
                }
            });
        });
        SheetFooter(|| {
            AppButton("취소", None, ButtonKind::Default, true, !busy, false, request_close.clone());
            let (s2, f2, o2) = (st.clone(), feed.clone(), open.clone());
            let (rooms2, agents2) = (rooms.to_vec(), agents.clone());
            AppButton(if busy { "만드는 중" } else { "만들기" }, None, ButtonKind::Primary, true, valid, false, move || {
                if s2.review {
                    notice::post(DEMO_NO_CREATE);
                } else if let Some(f) = &f2 {
                    create(&s2, f, &o2, &rooms2, &agents2);
                }
            });
        });
    });
    // The unsaved-changes question (FR-L26 pattern): 닫기 leaves, 계속 편집 goes back.
    {
        let keep = {
            let c = st.confirm.clone();
            move || c.set(false)
        };
        EmberSheet("변경 사항", st.confirm.get() && sheet_open, keep.clone(), None, || {
            cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width().padding(SP[4])).content(|| {
                Label(DISCARD_QUESTION, Fs::Md, Weight::Regular, None, Some(3));
            });
            SheetFooter(|| {
                AppButton("계속 편집", None, ButtonKind::Default, true, true, false, keep.clone());
                let s = st.clone();
                AppButton("닫기", None, ButtonKind::Primary, true, true, false, move || s.close());
            });
        });
    }
}
