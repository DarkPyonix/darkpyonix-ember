//! The add-computer sheet of the Project screen (FR-C1 / FR-C2, D39, #490). It lists the
//! account's hub devices read-only and adds a new computer with a one-time enrol code: a name,
//! the code and the server's command, the poll while the other machine redeems it, the confirm
//! card (the node's reported name and the observed address as plain text, with warnings), and
//! the outcome. The decisions are in `ui::add_computer` and `ui::enrol_flow`. The code lives in
//! this sheet's state only until it closes: it is never logged or saved.

#![allow(non_snake_case)]

use compose_rust::runtime::{delay, launched_effect, mutable_state_of, MutableState};
use compose_rust::ui as cu;
use compose_rust::ui::Modifier;
use compose_rust::{TypeRole};
use std::time::Duration;

use super::components::{copy_notice, AppButton, ButtonKind, EmberField, EmberSheet, FieldOptions, Label, Pill, SheetFooter, COPY_LABEL};
use super::notice;
use super::theme::*;
use crate::model::now_ms;
use crate::services::{run_shared, services};
use crate::ui::add_computer::*;
use crate::ui::enrol_flow::{self as flow, CommandView, ConfirmRows, EnrolView, RunGuard, Stage};

/// What the sheet keeps about the code it minted. No `Debug`: it holds the code.
#[derive(Clone)]
struct Run {
    id: String,
    code: String,
    expires_ms: i64,
    command: CommandView,
    server_fp: Option<String>,
}

/// State equality for the toolkit's change check; the code is compared, never printed.
impl PartialEq for Run {
    fn eq(&self, o: &Self) -> bool {
        self.id == o.id && self.code == o.code && self.expires_ms == o.expires_ms && self.server_fp == o.server_fp
    }
}

#[derive(Clone)]
pub struct AddUi {
    pub open: MutableState<bool>,
    devices: MutableState<DevicesState>,
    /// `None` shows the device list; otherwise the step of the code flow.
    view: MutableState<Option<EnrolView>>,
    run: MutableState<Option<Run>>,
    /// Which run and mint answers the sheet may still take.
    guard: MutableState<RunGuard>,
    name: MutableState<String>,
    /// The name field of the confirm card (empty keeps the name chosen at mint).
    confirm_name: MutableState<String>,
    /// What the owner typed from the node's screen (peer-only node); never prefilled.
    typed_fp: MutableState<String>,
    busy: MutableState<bool>,
    failure: MutableState<Option<String>>,
    /// The owner confirmed; the node has not taken its credential yet.
    confirmed: MutableState<bool>,
    /// The confirm call has not answered yet: closing must not cancel the request.
    confirm_in_flight: MutableState<bool>,
    /// A confirm failed without an answer: it may have been accepted, so closing does not cancel.
    confirm_unknown: MutableState<bool>,
    /// The runs whose confirms were in flight at close; their answers are still reported once.
    detached_confirms: MutableState<Vec<String>>,
    armed_at: MutableState<i64>,
    /// Ticks once a second while a view counts down.
    tick: MutableState<i64>,
    /// A debug review capture: sample state, nothing calls the server.
    review: bool,
}

impl AddUi {
    pub fn new() -> AddUi {
        AddUi {
            open: mutable_state_of(false),
            devices: mutable_state_of(DevicesState::Loading),
            view: mutable_state_of(None),
            run: mutable_state_of(None),
            guard: mutable_state_of(RunGuard::default()),
            name: mutable_state_of(String::new()),
            confirm_name: mutable_state_of(String::new()),
            typed_fp: mutable_state_of(String::new()),
            busy: mutable_state_of(false),
            failure: mutable_state_of(None),
            confirmed: mutable_state_of(false),
            confirm_in_flight: mutable_state_of(false),
            confirm_unknown: mutable_state_of(false),
            detached_confirms: mutable_state_of(Vec::new()),
            armed_at: mutable_state_of(0),
            tick: mutable_state_of(0),
            review: false,
        }
    }

    /// The debug review hook (`EMBER_CR_PROJECT_SHEET=add-computer|enrol-name|enrol-waiting|enrol-confirm|enrol-confirm-peer|enrol-joined`).
    pub fn for_review() -> AddUi {
        let sheet = std::env::var("EMBER_CR_PROJECT_SHEET").ok();
        let debug = cfg!(debug_assertions);
        let mode = review_mode(sheet.as_deref(), debug);
        let stage = flow::review_stage(sheet.as_deref(), debug);
        let mut ui = AddUi::new();
        if mode == ReviewMode::Off && stage.is_none() {
            return ui;
        }
        ui.review = true;
        ui.open.set(true);
        ui.devices.set(DevicesState::Ready(demo_devices()));
        if let Some(stage) = stage {
            ui.show_demo(stage);
        }
        ui
    }

    /// The sample view of a stage: no server call.
    fn show_demo(&self, stage: Stage) {
        self.run.set(Some(Run {
            id: "demo".to_string(),
            code: flow::DEMO_CODE.to_string(),
            expires_ms: now_ms() + 9 * 60_000 + 41_000,
            command: CommandView::Command { text: flow::DEMO_COMMAND.to_string(), plain_http: true },
            server_fp: Some("12ab 34cd 56ef 7890".to_string()),
        }));
        self.armed_at.set(0);
        if stage == Stage::Name {
            self.run.set(None);
        }
        if stage == Stage::Joined {
            self.name.set("연구실 컴퓨터".to_string());
        }
        self.view.set(Some(flow::demo_view(stage, now_ms())));
    }

    /// The 컴퓨터 추가 chip: open the sheet and read the devices.
    pub fn show(&self) {
        self.open.set(true);
        self.with_guard(|g| g.open());
        if self.review {
            return;
        }
        load_devices(self.devices.clone());
    }

    fn sample(&self) -> bool {
        self.review || services().demo.is_some()
    }

    fn reset_flow(&self) {
        self.view.set(None);
        self.run.set(None);
        self.with_guard(|g| g.clear());
        self.name.set(String::new());
        self.confirm_name.set(String::new());
        self.typed_fp.set(String::new());
        self.busy.set(false);
        self.failure.set(None);
        self.confirmed.set(false);
        self.confirm_in_flight.set(false);
        self.confirm_unknown.set(false);
    }

    fn with_guard<R>(&self, f: impl FnOnce(&mut RunGuard) -> R) -> R {
        let mut g = self.guard.get_untracked();
        let r = f(&mut g);
        self.guard.set(g);
        r
    }

    /// True while `id` is the run on screen. An answer for an older run (the sheet was closed and
    /// a new code minted while the call was in flight) must change nothing.
    fn is_current(&self, id: &str) -> bool {
        self.with_guard(|g| g.is_current(id))
    }

    /// Feeds one event to the reducer and carries out its effects. The order decisions are in
    /// `flow::reduce`; this only copies state in and out.
    fn step(&self, ev: flow::Event) {
        let mut st = flow::SheetState {
            guard: self.guard.get_untracked(),
            view: self.view.get_untracked(),
            confirmed: self.confirmed.get_untracked(),
            confirm_in_flight: self.confirm_in_flight.get_untracked(),
            confirm_unknown: self.confirm_unknown.get_untracked(),
            detached_confirms: self.detached_confirms.get_untracked(),
            sample: self.sample(),
        };
        let effects = flow::reduce(&mut st, ev);
        self.guard.set(st.guard);
        self.confirmed.set(st.confirmed);
        self.confirm_in_flight.set(st.confirm_in_flight);
        self.confirm_unknown.set(st.confirm_unknown);
        self.detached_confirms.set(st.detached_confirms);
        for e in effects {
            match e {
                flow::Effect::Cancel(id) => cancel_on_server(id),
                flow::Effect::ArmConfirm => self.armed_at.set(now_ms()),
                flow::Effect::RefreshComputers => refresh_computers(),
                flow::Effect::Notice(t) => notice::post(t),
                flow::Effect::ClearTypedFingerprint => self.typed_fp.set(String::new()),
                flow::Effect::ClearSecrets => {
                    self.run.set(None);
                    self.typed_fp.set(String::new());
                }
                flow::Effect::SetView(v) => self.view.set(Some(v)),
            }
        }
    }

    /// The clock passed a waiting code's time: it ends as expired now, not only in the renderer.
    fn expire_if_due(&self, now: i64) {
        self.step(flow::Event::Tick { now_ms: now });
    }

    /// Closing drops the code. A code nobody redeemed is cancelled on the server; one the owner
    /// already confirmed (or is confirming) is left to finish.
    fn leave(&self) {
        self.step(flow::Event::Leave);
        self.reset_flow();
        self.open.set(false);
    }
}

/// Cancels an enrolment; a failure is told, because the request then lives until it expires.
fn cancel_on_server(id: String) {
    run_shared(async move { services().client.api().enrol_cancel(&id).await }, |r| {
        if r.is_err() {
            notice::post(flow::CANCEL_FAILED.to_string());
        }
    });
}

/// Reads the devices; the demo serves samples and calls nothing.
fn load_devices(state: MutableState<DevicesState>) {
    if services().demo.is_some() {
        state.set(DevicesState::Ready(demo_devices()));
        return;
    }
    state.set(DevicesState::Loading);
    run_shared(async move { services().client.api().hub_devices().await }, move |r| state.set(devices_state(r)));
}

/// Mints the code for `name`; the demo shows a sample and calls nothing.
fn mint(ui: AddUi, name: String) {
    if ui.sample() {
        ui.show_demo(Stage::Waiting);
        return;
    }
    ui.busy.set(true);
    ui.failure.set(None);
    let generation = ui.with_guard(|g| g.begin_mint());
    run_shared(async move { services().client.api().enrol_mint(&name).await }, move |r| {
        if let Ok(m) = &r {
            // A late code (the sheet closed or started over) is cancelled by the reducer.
            ui.step(flow::Event::MintAnswered { generation, id: m.id.clone() });
        }
        match r {
            Ok(m) if !ui.is_current(&m.id) => {}
            _ if !ui.with_guard(|g| g.mint_current(generation)) => {}
            Ok(m) => {
                ui.busy.set(false);
                ui.run.set(Some(Run {
                    id: m.id.clone(),
                    code: m.code.clone(),
                    expires_ms: m.expires_ms,
                    command: flow::command_view(&m),
                    server_fp: flow::server_fingerprint_line(&m),
                }));
                ui.view.set(Some(EnrolView::WaitingForNode { code: m.code.clone(), expires_ms: m.expires_ms }));
            }
            Err(e) => {
                ui.busy.set(false);
                ui.failure.set(Some(flow::mint_error_text(&e).to_string()));
            }
        }
    });
}

/// One status poll, on the runtime (the HTTP client needs it).
async fn fetch_status(id: String) -> Result<ember_client::api::EnrolStatus, ember_client::api::EnrolCallError> {
    let s = services();
    let job = s.rt.spawn(async move { services().client.api().enrol_status(&id).await });
    match job.await {
        Ok(r) => r,
        Err(_) => Err(ember_client::api::EnrolCallError::Off),
    }
}

/// Puts a poll answer into the sheet. A new confirm card arms the 600 ms delay; a join refreshes
/// the computer list.
fn apply_poll(ui: &AddUi, run: &Run, r: Result<ember_client::api::EnrolStatus, ember_client::api::EnrolCallError>) {
    if !ui.is_current(&run.id) {
        return;
    }
    let cur = ui.view.get_untracked();
    let Some(cur) = cur else { return };
    let next = match r {
        Ok(st) => flow::view_from_status(&st, &run.code, run.expires_ms),
        Err(e) => flow::view_after_poll_error(&cur, &e),
    };
    // The code and the command are secrets: the reducer drops them when the enrolment ends.
    ui.step(flow::Event::PollAnswered { id: run.id.clone(), view: next });
}

fn refresh_computers() {
    run_shared(
        async move {
            if let Ok(list) = services().server.computers(false).await {
                let merged = services().client.read(|st| crate::model::merge_computers(st.computers(), &list));
                services().client.apply(ember_client::state::Input::ComputersLoaded(merged));
            }
        },
        |_| {},
    );
}

fn confirm(ui: AddUi, run: Run, rows: ConfirmRows) {
    if ui.sample() {
        ui.view.set(Some(EnrolView::Joined));
        return;
    }
    let name = flow::confirm_name(&ui.confirm_name.get_untracked(), &rows.chosen_name);
    ui.busy.set(true);
    ui.failure.set(None);
    ui.confirm_in_flight.set(true);
    ui.confirm_unknown.set(false);
    let id = run.id.clone();
    let expect = flow::expect_fingerprint(&rows, &ui.typed_fp.get_untracked());
    run_shared(async move { services().client.api().enrol_confirm(&run.id, &name, expect.as_deref()).await }, move |r| {
        // The answer is always routed through the reducer: after the sheet closed it still
        // reports the outcome (notice, list refresh) and changes no view.
        let current = ui.is_current(&id);
        if current {
            ui.busy.set(false);
        }
        match r {
            Ok(_) => ui.step(flow::Event::ConfirmAnswered { id: id.clone(), ok: true }),
            Err(e) if flow::confirm_is_unknown(&e) => {
                ui.step(flow::Event::ConfirmUnknown { id: id.clone() });
                if current {
                    ui.failure.set(Some(flow::CONFIRM_UNKNOWN.to_string()));
                }
            }
            Err(e) => {
                ui.step(flow::Event::ConfirmAnswered { id: id.clone(), ok: false });
                if current {
                    match flow::view_after_confirm_error(&e) {
                        Some(end) => ui.step(flow::Event::PollAnswered { id: id.clone(), view: end }),
                        None => ui.failure.set(Some(flow::confirm_error_text(&e).to_string())),
                    }
                }
            }
        }
    });
}

fn deny(ui: AddUi, run: Run) {
    if ui.sample() {
        ui.view.set(Some(EnrolView::Denied));
        return;
    }
    ui.busy.set(true);
    ui.failure.set(None);
    let id = run.id.clone();
    run_shared(async move { services().client.api().enrol_deny(&run.id).await }, move |r| {
        if !ui.is_current(&id) {
            return;
        }
        ui.busy.set(false);
        ui.step(flow::Event::DenyAnswered { id: id.clone(), ok: r.is_ok() });
        if let Err(e) = r {
            match flow::view_after_confirm_error(&e) {
                Some(end) => ui.step(flow::Event::PollAnswered { id: id.clone(), view: end }),
                None => ui.failure.set(Some(flow::confirm_error_text(&e).to_string())),
            }
        }
    });
}

pub fn AddComputerSheet(ui: &AddUi) {
    let p = ember_theme().palette;
    let close = {
        let u = ui.clone();
        move || u.leave()
    };
    // Poll while a code waits or a request awaits the owner; the effect ends with the view.
    let sample = ui.sample();
    let polling = !sample && ui.view.get().as_ref().is_some_and(flow::should_poll);
    let run_id = ui.run.get().map(|r| r.id).unwrap_or_default();
    {
        let u = ui.clone();
        launched_effect((run_id, polling), move || async move {
            if !polling {
                return;
            }
            loop {
                let Some(run) = u.run.get_untracked() else { return };
                let r = fetch_status(run.id.clone()).await;
                apply_poll(&u, &run, r);
                if !u.view.get_untracked().as_ref().is_some_and(flow::should_poll) {
                    return;
                }
                delay(Duration::from_millis(flow::POLL_EVERY_MS)).await;
            }
        });
    }
    {
        let tick = ui.tick.clone();
        let u = ui.clone();
        let counting = polling || (sample && ui.view.get().as_ref().is_some_and(flow::should_poll));
        launched_effect(counting, move || async move {
            while counting {
                tick.set(now_ms());
                u.expire_if_due(now_ms());
                delay(Duration::from_secs(1)).await;
            }
        });
    }
    let w = compose_rust::runtime::current_window_size();
    let body_h = super::components::sheet_body_max_height(w.width_dp, w.height_dp, false, 60.0).min(700.0);
    EmberSheet("컴퓨터 추가", ui.open.get(), close.clone(), None, || {
        cu::ScrollColumn().modifier(Modifier::new().fill_max_width().height(body_h)).content(|| {
            cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width().padding(SP[4])).content(|| {
                let muted = Some(paint(p.muted));
                if sample {
                    Label(flow::DEMO_LINE, Fs::Xs, Weight::Regular, muted, None);
                }
                match ui.view.get() {
                    None => ListBody(ui, sample),
                    Some(EnrolView::Naming) => NamingBody(ui),
                    Some(EnrolView::WaitingForNode { code, expires_ms }) => WaitingBody(ui, &code, expires_ms, sample),
                    Some(EnrolView::AwaitingConfirm { rows }) => ConfirmBody(ui, &rows),
                    Some(EnrolView::Joined) => Outcome(ui, flow::JOINED_LINE, false),
                    Some(EnrolView::Denied) => Outcome(ui, flow::DENIED_LINE, false),
                    Some(EnrolView::Expired) => Outcome(ui, flow::EXPIRED_LINE, true),
                    Some(EnrolView::Failed(m)) => Outcome(ui, &m, true),
                }
            });
        });
        SheetFooter(|| {
            AppButton("닫기", None, ButtonKind::Default, true, true, false, close.clone());
        });
    });
}

fn ListBody(ui: &AddUi, sample: bool) {
    let p = ember_theme().palette;
    let muted = Some(paint(p.muted));
    let u = ui.clone();
    AppButton(flow::NEW_COMPUTER, None, ButtonKind::Primary, true, true, false, move || {
        u.failure.set(None);
        u.view.set(Some(EnrolView::Naming));
    });
    match ui.devices.get() {
        DevicesState::Loading => Label("기기를 불러오는 중입니다", Fs::Md, Weight::Regular, muted, None),
        DevicesState::HubOff => Label(HUB_OFF, Fs::Md, Weight::Regular, muted, None),
        DevicesState::Failed(m) => Label(&m, Fs::Md, Weight::Regular, Some(paint(p.red.strong)), None),
        DevicesState::Ready(list) if list.is_empty() => Label(NO_DEVICES, Fs::Md, Weight::Regular, muted, None),
        DevicesState::Ready(list) => {
            for r in device_rows(&list, sample) {
                DeviceRowView(&r);
            }
        }
    }
    Label(ENROL_NOTE, Fs::Xs, Weight::Regular, muted, Some(3));
}

fn NamingBody(ui: &AddUi) {
    let p = ember_theme().palette;
    let muted = Some(paint(p.muted));
    Label(flow::NAME_LABEL, Fs::Xs, Weight::Medium, Some(paint(p.text_2)), Some(1));
    let (n, f) = (ui.name.clone(), ui.failure.clone());
    EmberField(FieldOptions {
        fill_max_width: true,
        placeholder: "예: 연구실 컴퓨터".to_string(),
        text: Some(ui.name.get_untracked()),
        on_value_change: Some(Box::new(move |v| {
            f.set(None);
            n.set(v)
        })),
        ..Default::default()
    });
    Label(flow::NAME_HELP, Fs::Xs, Weight::Regular, muted, Some(3));
    let typed = ui.name.get();
    let valid = flow::validate_name(&typed);
    if let (Err(e), false) = (valid.clone(), typed.trim().is_empty()) {
        Label(e, Fs::Xs, Weight::Regular, Some(paint(p.red.strong)), None);
    }
    FailureLine(ui);
    let busy = ui.busy.get();
    cu::Row().spacing(SP[1]).content(|| {
        let u = ui.clone();
        AppButton("취소", None, ButtonKind::Default, true, !busy, false, move || u.reset_flow());
        let (u, name) = (ui.clone(), typed.clone());
        AppButton(if busy { "만드는 중" } else { flow::MAKE_CODE }, None, ButtonKind::Primary, true, valid.is_ok() && !busy, false, move || {
            if let Ok(n) = flow::validate_name(&name) {
                mint(u.clone(), n);
            }
        });
    });
}

fn FailureLine(ui: &AddUi) {
    let p = ember_theme().palette;
    if let Some(m) = ui.failure.get() {
        Label(&m, Fs::Xs, Weight::Regular, Some(paint(p.red.strong)), Some(3));
    }
}

fn WaitingBody(ui: &AddUi, code: &str, expires_ms: i64, sample: bool) {
    let p = ember_theme().palette;
    let muted = Some(paint(p.muted));
    let _ = ui.tick.get();
    let left = flow::remaining_secs(expires_ms, now_ms());
    Label(flow::WAITING_LINE, Fs::Sm, Weight::Regular, None, Some(3));
    if sample {
        Pill("예시", super::components::PillTone::Grey, true);
    }
    // Past its time the code and the command leave the screen at once, before the next poll.
    if left == 0 {
        Label(flow::EXPIRED_LINE, Fs::Sm, Weight::Medium, Some(paint(p.red.strong)), Some(3));
        return;
    }
    // The code, large and in the mono face, and a 복사 button that puts it on the clipboard
    // through the toolkit (compose-rust v0.0.7, #471).
    let w = compose_rust::runtime::current_window_size();
    let avail = w.width_dp.min(760.0) - 2.0 * SP[4];
    super::components::MonoLabel(code, flow::code_font_size(code.chars().count(), avail), Weight::Semibold, Some(paint(p.text)));
    {
        let code = code.to_string();
        AppButton(COPY_LABEL, None, ButtonKind::Default, true, true, false, move || {
            notice::post(copy_notice(compose_rust::clipboard::set_text(&code).is_ok()));
        });
    }

    Label(flow::CODE_ONCE, Fs::Xs, Weight::Regular, muted, Some(2));
    Label(&flow::countdown_text(left), Fs::Sm, Weight::Medium, Some(paint(if left == 0 { p.red.strong } else { p.text_2 })), Some(1));
    if let Some(run) = ui.run.get() {
        match &run.command {
            CommandView::Command { text, plain_http } => {
                match flow::stdin_form(text, &run.code) {
                    Some(stdin) => {
                        super::components::MonoLabel(&stdin, Fs::Xs, Weight::Regular, Some(paint(p.text)));
                        Label(flow::STDIN_FORM_NOTE, Fs::Xs, Weight::Regular, muted, Some(3));
                        Label(flow::ARG_LABEL, Fs::Xs, Weight::Medium, muted, Some(1));
                        super::components::MonoLabel(text, Fs::Xs, Weight::Regular, muted);
                        Label(flow::ARG_WARNING, Fs::Xs, Weight::Regular, muted, Some(3));
                    }
                    None => {
                        super::components::MonoLabel(text, Fs::Xs, Weight::Regular, Some(paint(p.text)));
                        Label(flow::STDIN_NOTE, Fs::Xs, Weight::Regular, muted, Some(3));
                    }
                }
                Label(flow::NODE_CMD_NOTE, Fs::Xs, Weight::Regular, muted, Some(3));
                Label(flow::ZSH_QUOTE_NOTE, Fs::Xs, Weight::Regular, muted, Some(2));
                if *plain_http {
                    Label(flow::HTTP_WARNING, Fs::Xs, Weight::Regular, Some(paint(p.red.strong)), Some(3));
                }
            }
            CommandView::Missing => Label(flow::NO_COMMAND, Fs::Sm, Weight::Regular, muted, Some(3)),
        }
        if let Some(fp) = &run.server_fp {
            KV(flow::SERVER_FINGERPRINT_LABEL, fp);
        }
    }
}

fn ConfirmBody(ui: &AddUi, rows: &ConfirmRows) {
    let p = ember_theme().palette;
    let muted = Some(paint(p.muted));
    let confirmed = ui.confirmed.get();
    Label(flow::CONFIRM_QUESTION, Fs::Sm, Weight::Medium, None, Some(4));
    Label(flow::CONFIRM_CHECK, Fs::Xs, Weight::Regular, muted, Some(3));
    KV(flow::REPORTED_NAME, &rows.reported_name);
    if !rows.system.is_empty() {
        KV("시스템", &rows.system);
    }
    KV(flow::OBSERVED_ADDR, &rows.observed_addr);
    if let Some(a) = &rows.reported_addr {
        KV(flow::REPORTED_ADDR, a);
    }
    let peer_only = flow::peer_only(rows);
    if flow::peer_id_unused(rows) {
        Label(flow::PEER_ID_UNUSED, Fs::Xs, Weight::Regular, muted, Some(3));
    }
    if let Some(a) = rows.proposed_addr.as_ref().filter(|_| !flow::hide_proposed(rows)) {
        KV(flow::PROPOSED_ADDR, a);
        Label(flow::NOTE_PROPOSED, Fs::Xs, Weight::Regular, muted, Some(3));
    }
    if peer_only {
        Label(flow::PEER_ONLY_NOTE, Fs::Xs, Weight::Medium, Some(paint(p.red.strong)), Some(4));
        if let Some(fp) = &rows.fingerprint {
            Label(flow::NODE_FINGERPRINT_PEER, Fs::Xs, Weight::Medium, Some(paint(p.text_2)), Some(1));
            Label(fp, Fs::Xs, Weight::Regular, muted, Some(1));
        }
        Label(flow::TYPED_FINGERPRINT_LABEL, Fs::Xs, Weight::Medium, Some(paint(p.text_2)), Some(1));
        let (t, f) = (ui.typed_fp.clone(), ui.failure.clone());
        EmberField(FieldOptions {
            fill_max_width: true,
            placeholder: "0000 0000 0000 0000".to_string(),
            text: Some(ui.typed_fp.get()),
            enabled: Some(!confirmed),
            on_value_change: Some(Box::new(move |v| {
                f.set(None);
                t.set(v)
            })),
            ..Default::default()
        });
        Label(flow::TYPED_FINGERPRINT_HELP, Fs::Xs, Weight::Regular, muted, Some(3));
    } else if let Some(fp) = &rows.fingerprint {
        KV(flow::NODE_FINGERPRINT, fp);
    }
    // Warnings follow the name the owner is typing.
    let typed = ui.confirm_name.get();
    let chosen = flow::confirm_name(&typed, &rows.chosen_name);
    let warns = flow::warning_rows(rows, &chosen);
    for w in &warns {
        Label(w.text(), Fs::Xs, Weight::Medium, Some(paint(p.red.strong)), Some(3));
    }
    if !warns.is_empty() {
        Label(flow::WARN_INFORMS, Fs::Xs, Weight::Regular, muted, Some(2));
    }
    Label(flow::NAME_LABEL, Fs::Xs, Weight::Medium, Some(paint(p.text_2)), Some(1));
    let (n, f) = (ui.confirm_name.clone(), ui.failure.clone());
    EmberField(FieldOptions {
        fill_max_width: true,
        placeholder: rows.chosen_name.clone(),
        text: Some(typed.clone()),
        enabled: Some(!confirmed),
        on_value_change: Some(Box::new(move |v| {
            f.set(None);
            n.set(v)
        })),
        ..Default::default()
    });
    if confirmed {
        Label(flow::CONFIRMED_WAIT, Fs::Sm, Weight::Medium, None, Some(3));
    }
    if let Some(h) = flow::fingerprint_hint(rows, &ui.typed_fp.get()).filter(|_| !confirmed) {
        Label(h, Fs::Xs, Weight::Regular, muted, Some(3));
    }
    FailureLine(ui);
    let busy = ui.busy.get();
    let Some(run) = ui.run.get() else { return };
    cu::Row().spacing(SP[1]).content(|| {
        let (u, r) = (ui.clone(), run.clone());
        AppButton(flow::DENY_LABEL, None, ButtonKind::Default, true, !busy && !confirmed, false, move || deny(u.clone(), r.clone()));
        let (u, r, rows) = (ui.clone(), run.clone(), rows.clone());
        let ok = !busy && !confirmed && flow::validate_name(&chosen).is_ok() && flow::fingerprint_ok(&rows, &ui.typed_fp.get());
        AppButton(if busy { "추가하는 중" } else { "확인하고 추가" }, None, ButtonKind::Primary, true, ok, false, move || {
            if !flow::confirm_counts(u.armed_at.get_untracked(), now_ms()) {
                return;
            }
            confirm(u.clone(), r.clone(), rows.clone());
        });
    });
}

fn Outcome(ui: &AddUi, line: &str, again: bool) {
    let p = ember_theme().palette;
    Label(line, Fs::Md, Weight::Medium, None, Some(4));
    if line == flow::JOINED_LINE {
        let typed = ui.confirm_name.get();
        let name = if typed.trim().is_empty() { ui.name.get() } else { typed };
        let name = Some(name.trim().to_string()).filter(|n| !n.is_empty());
        if let Some(n) = name {
            Label(&n, Fs::Lg, Weight::Semibold, None, Some(1));
        }
        Label(flow::JOINED_NEXT, Fs::Sm, Weight::Regular, Some(paint(p.muted)), Some(3));
    }
    if again {
        let u = ui.clone();
        AppButton("새 코드 만들기", None, ButtonKind::Default, true, true, false, move || {
            u.reset_flow();
            u.view.set(Some(EnrolView::Naming));
        });
    }
    let _ = p;
}

fn KV(k: &str, v: &str) {
    let p = ember_theme().palette;
    cu::Row().spacing(SP[2]).content(|| {
        Label(k, Fs::Xs, Weight::Medium, Some(paint(p.text_2)), Some(2));
        Label(v, Fs::Sm, Weight::Regular, None, Some(2));
    });
}

fn DeviceRowView(r: &DeviceRow) {
    let p = ember_theme().palette;
    let m = Modifier::new().fill_max_width().background(paint(p.surface)).border(1.0, paint(p.line)).corner_radius(CORNER_MD);
    cu::Column().spacing(SP[2]).modifier(m.padding(SP[3])).content(|| {
        cu::Row().spacing(SP[2]).content(|| {
            cu::Column().modifier(Modifier::new().weight(1.0)).content(|| {
                Label(&r.name, Fs::Sm, Weight::Semibold, None, Some(1));
                Label(&r.kind, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
            });
            if r.sample {
                Pill("예시", super::components::PillTone::Grey, true);
            }
            Pill(r.pill.0, r.pill.1.into(), true);
            Pill(r.online.0, r.online.1.into(), true);
        });
    });
}
