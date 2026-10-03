//! The conversation view (FR-L5–FR-L7).
//!
//! - Header: title, project, agent, account, model, status, the session's current computer
//!   (with a switch menu, FR-X3) and "Open IDE" (Ember / VS Code / Gateway, FR-L7).
//! - Transcript in a `LazyColumn`, keyed by event sequence so a streaming reply growing at the
//!   bottom does not disturb the rows above: user and assistant messages, collapsible tool-call
//!   cards with their results, approval cards (allow once / always / deny), turn-ended and
//!   error rows.
//! - Composer: Enter sends, Shift+Enter is a new line (the renderer does this for a multiline
//!   field with a submit handler; IME composition never submits). While the agent is busy a
//!   message is queued and delivered at the next turn boundary (FR-L6), and an interrupt
//!   button shows.

use std::collections::HashSet;

use dioxus_compose::prelude::*;

use ember_client::transcript::{ApprovalState, TranscriptItem};
use ember_client::wire::{ApprovalDecision, TurnOutcome};
use ember_client::LauncherStatus;

use crate::ide::{open_external, plan, IdeAction, IdeKind};
use crate::model::{self, Tone};
use crate::server::{CurrentComputer, IdeLaunch};
use crate::services::{run, services};
use crate::ui::compat::{Badge, Banner, CodeText, MessageText, Spinner, StatusIndicator};
use crate::ui::launcher::{export, ConnectionBanner};
use crate::ui::{use_ui, Route};

/// A transcript row's stable key: its event sequence number.
fn item_key(item: &TranscriptItem) -> String {
    let seq = match item {
        TranscriptItem::User { seq, .. }
        | TranscriptItem::Assistant { seq, .. }
        | TranscriptItem::ToolCall { seq, .. }
        | TranscriptItem::Approval { seq, .. }
        | TranscriptItem::TurnEnded { seq, .. }
        | TranscriptItem::Error { seq, .. } => *seq,
    };
    seq.to_string()
}

#[component]
pub fn ConversationView(id: String) -> Element {
    let ui = use_ui();
    let window = use_window_size();
    let s = services();

    // ---- header data -------------------------------------------------------------------------
    let header = {
        let _ = *ui.live.launcher.read();
        s.client.read(|st| st.session(&id).map(|v| (v.record.clone(), v.status)))
    };

    // Fetched when the view opens (and after a computer switch).
    let computer = use_signal(|| None::<CurrentComputer>);
    let ide = use_signal(|| None::<Result<IdeLaunch, String>>);
    let load = {
        let id = id.clone();
        move || {
            let a = id.clone();
            run(
                async move { services().server.session_computer(&a).await.map_err(|e| e.to_string()) },
                move |r| {
                    let mut computer = computer;
                    match r {
                        Ok(c) => computer.set(Some(c)),
                        Err(e) => tracing::debug!("current computer: {e}"),
                    }
                },
            );
            let b = id.clone();
            run(
                async move { services().server.ide_targets(&b, None).await.map_err(|e| e.to_string()) },
                move |r| {
                    let mut ide = ide;
                    ide.set(Some(r));
                },
            );
        }
    };
    use_hook({
        let load = load.clone();
        move || load()
    });

    // ---- composer state ----------------------------------------------------------------------
    let mut draft = use_signal(String::new);
    // Changing this rebuilds the (uncontrolled) text field, which is how it is cleared.
    let mut composer_generation = use_signal(|| 0_u64);
    let expanded = use_signal(HashSet::<String>::new);
    let answering = use_signal(HashSet::<String>::new);
    let mut ide_menu = use_signal(|| false);
    let mut computer_menu = use_signal(|| false);
    let mut more_menu = use_signal(|| false);

    let send = use_callback({
        let id = id.clone();
        move |text: String| {
            let text = text.trim().to_string();
            if text.is_empty() {
                return;
            }
            let s = services();
            s.outbox.lock().unwrap().push(&id, text);
            s.bump_outbox();
            // Sends now if the agent is idle; otherwise it waits for the turn boundary.
            s.drive_outbox(&id);
            draft.set(String::new());
            composer_generation += 1;
        }
    });

    let Some((record, status)) = header else {
        return rsx! {
            Column {
                fill_max_width: true,
                fill_max_height: true,
                padding_role: SpaceRole::Md,
                space_role: SpaceRole::Md,
                alignment: Alignment::Center,
                arrangement: Arrangement::Center,
                Text { text: "This session is not on the main server.", type_role: TypeRole::Title }
                Text {
                    text: "It may have been deleted, or the list has not loaded yet.",
                    type_role: TypeRole::Body,
                    color: Paint::Role(ColorRole::OnSurfaceVariant),
                }
                Button { text: "Back to projects", icon: IconRole::Back, on_click: move |_| ui.go(Route::Projects) }
            }
        };
    };

    let title = ui.prefs.read().titles.get(&id).cloned().unwrap_or_else(|| record.title.clone());
    let account = {
        let sa = ui.live.session_accounts.read();
        let labels = model::account_labels(&ui.live.accounts.read());
        sa.get(&id).map(|a| labels.get(a).cloned().unwrap_or_else(|| a.clone()))
    };
    let busy = model::is_busy(status);
    let mut meta = vec![record.project.clone(), record.agent.clone()];
    meta.push(account.unwrap_or_else(|| "server login".into()));
    if let Some(m) = &record.model {
        meta.push(m.clone());
    }
    let meta = meta.join(" \u{00b7} ");
    let computer_name = computer
        .read()
        .as_ref()
        .map(|c| c.computer.name.clone())
        .unwrap_or_else(|| "Computer\u{2026}".into());
    let computers = {
        let _ = *ui.live.launcher.read();
        s.client.read(|st| st.computers().to_vec())
    };

    // ---- transcript ----------------------------------------------------------------------------
    let (count, keys, pending) = {
        let _ = *ui.live.transcripts.read();
        s.client.read(|st| match st.transcript(&id) {
            Some(t) => (t.items.len(), t.items.iter().map(item_key).collect::<Vec<_>>(), t.pending_approvals().count()),
            None => (0, Vec::new(), 0),
        })
    };

    // ---- outbox --------------------------------------------------------------------------------
    let (queued, queue_error) = {
        let _ = *ui.live.outbox.read();
        let o = s.outbox.lock().unwrap();
        (o.queued_texts(&id), o.error(&id).map(String::from).unwrap_or_default())
    };

    // ---- actions -------------------------------------------------------------------------------
    let open_ide = {
        move |kind: IdeKind| {
            ui.update_prefs(|p| p.ide_target = Some(kind.key().to_string()));
            let launch = ide.read().clone();
            let target = match &launch {
                None => {
                    show_message("Still asking the main server for IDE targets\u{2026}");
                    return;
                }
                Some(Err(e)) => {
                    show_message(format!("Could not get IDE targets: {e}"));
                    return;
                }
                Some(Ok(l)) => l.target(kind.key()).cloned(),
            };
            match plan(kind, target.as_ref()) {
                IdeAction::OpenUrl(url) => {
                    let label = kind.label();
                    run(
                        async move {
                            tokio::task::spawn_blocking(move || open_external(&url))
                                .await
                                .map_err(|e| e.to_string())
                                .and_then(|r| r.map_err(|e| e.to_string()))
                        },
                        move |r| match r {
                            Ok(()) => show_message(format!("Opening {label}\u{2026}")),
                            Err(e) => show_message(format!("Could not open {label}: {e}")),
                        },
                    );
                }
                IdeAction::EmberPlaceholder => {
                    // TODO(M8, editor core): open the Ember IDE window here.
                    Message::new("The Ember IDE arrives with the editor core (M8). Use VS Code or Gateway for now.")
                        .with_duration(MessageDuration::Long)
                        .show();
                }
                IdeAction::Unavailable(why) => {
                    Message::new(format!("{} is not available: {why}", kind.label()))
                        .with_duration(MessageDuration::Long)
                        .show();
                }
            }
        }
    };
    // The remembered target first (FR-L7: the choice is remembered per user).
    let remembered = ui.prefs.read().ide_target.as_deref().and_then(IdeKind::from_key);
    let mut ide_order: Vec<IdeKind> = IdeKind::ALL.to_vec();
    if let Some(k) = remembered {
        ide_order.retain(|x| *x != k);
        ide_order.insert(0, k);
    }
    let ide_reason = |kind: IdeKind| -> Option<String> {
        match &*ide.read() {
            Some(Ok(l)) => match l.target(kind.key()) {
                Some(t) if t.available => None,
                Some(t) => t.reason.clone(),
                None => Some("not offered by the server".into()),
            },
            _ => None,
        }
    };
    let ide_entries: Vec<(IdeKind, &'static str, String)> = ide_order
        .iter()
        .map(|k| {
            let mut label = k.label().to_string();
            if Some(*k) == remembered {
                label.push_str(" (last used)");
            }
            if let Some(r) = ide_reason(*k) {
                label = format!("{label}: {r}");
            }
            (*k, k.key(), label)
        })
        .collect();

    let switch_to = {
        let id = id.clone();
        let load = load.clone();
        move |computer_id: String| {
            let sid = id.clone();
            let reload = load.clone();
            run(
                async move { services().server.switch_computer(&sid, &computer_id).await.map_err(|e| e.to_string()) },
                move |r| match r {
                    Ok(out) => {
                        // The current computer and its IDE targets changed.
                        reload();
                        show_message(match (&out.notice, out.changed) {
                            (_, false) => format!("Already on {}", out.computer.name),
                            (Some(_), true) => format!("Switched to {}. The agent is told with your next message.", out.computer.name),
                            (None, true) => format!("Switched to {}", out.computer.name),
                        });
                    }
                    Err(e) => Message::new(format!("Could not switch computer: {e}")).with_duration(MessageDuration::Long).show(),
                },
            );
        }
    };

    let interrupt = {
        let id = id.clone();
        move |_: ()| {
            let sid = id.clone();
            run(
                async move { services().client.interrupt(&sid).await.map_err(|e| e.to_string()) },
                |r| {
                    if let Err(e) = r {
                        show_message(format!("Could not interrupt: {e}"));
                    }
                },
            );
        }
    };
    let clear_queue = {
        let id = id.clone();
        move |_: ()| {
            services().outbox.lock().unwrap().clear(&id);
            services().bump_outbox();
        }
    };
    let retry_queue = {
        let id = id.clone();
        move |_: ()| services().drive_outbox(&id)
    };
    let export_this = {
        let (id, title) = (id.clone(), title.clone());
        move |_: ()| {
            more_menu.set(false);
            export(id.clone(), title.clone());
        }
    };
    let pin_this = {
        let id = id.clone();
        move |_: ()| {
            more_menu.set(false);
            let id = id.clone();
            ui.update_prefs(move |p| p.toggle_pin(&id));
        }
    };
    let pinned = ui.prefs.read().pinned.contains(&id);
    let compact = window.is_compact();
    let live = ui.live;
    let id_items = id.clone();
    let agent = record.agent.clone();

    rsx! {
        Column {
            fill_max_width: true,
            fill_max_height: true,
            padding_role: SpaceRole::Sm,
            space_role: SpaceRole::Sm,

            // ---- header ------------------------------------------------------------------------
            Row {
                fill_max_width: true,
                alignment: Alignment::CenterStart,
                space_role: SpaceRole::Sm,
                if compact {
                    Button {
                        text: "",
                        icon: IconRole::Back,
                        variant: ButtonVariant::Text,
                        on_click: move |_| ui.go(Route::Projects),
                    }
                }
                Column {
                    weight: 1.0,
                    Text { text: title.clone(), type_role: TypeRole::Title, max_lines: 1, overflow: TextOverflow::Ellipsis }
                    Text {
                        text: meta.clone(),
                        type_role: TypeRole::Caption,
                        color: Paint::Role(ColorRole::OnSurfaceVariant),
                        max_lines: 1,
                        overflow: TextOverflow::Ellipsis,
                    }
                }
                StatusIndicator { status }
                Menu {
                    expanded: more_menu(),
                    on_dismiss: move |_| more_menu.set(false),
                    anchor: rsx! {
                        Button { text: "", icon: IconRole::More, variant: ButtonVariant::Text, on_click: move |_| more_menu.set(true) }
                    },
                    Button { text: if pinned { "Unpin" } else { "Pin" }, variant: ButtonVariant::Text, fill_max_width: true, on_click: pin_this }
                    Button { text: "Export", variant: ButtonVariant::Text, fill_max_width: true, on_click: export_this }
                }
            }
            // Computer and IDE, a row of their own (they do not fit beside the title on a phone).
            Row {
                fill_max_width: true,
                alignment: Alignment::CenterStart,
                space_role: SpaceRole::Sm,
                Text { text: "On", type_role: TypeRole::Caption, color: Paint::Role(ColorRole::OnSurfaceVariant) }
                Menu {
                    expanded: computer_menu(),
                    on_dismiss: move |_| computer_menu.set(false),
                    anchor: rsx! {
                        Button {
                            text: computer_name.clone(),
                            variant: ButtonVariant::Outlined,
                            enabled: !busy,
                            on_click: move |_| computer_menu.set(true),
                        }
                    },
                    if computers.is_empty() {
                        Text { text: "No computers registered", type_role: TypeRole::Caption }
                    }
                    for c in computers.iter().cloned() {
                        Button {
                            key: "{c.id}",
                            text: format!("{} ({})", c.name, model::reachability_label(c.reachability)),
                            variant: ButtonVariant::Text,
                            fill_max_width: true,
                            on_click: {
                                let switch_to = switch_to.clone();
                                let cid = c.id.clone();
                                move |_: ()| {
                                    computer_menu.set(false);
                                    switch_to(cid.clone());
                                }
                            },
                        }
                    }
                }
                Spacer { weight: 1.0 }
                if pending > 0 {
                    Badge { text: format!("{pending} approval{} waiting", if pending == 1 { "" } else { "s" }), tone: Tone::Warning }
                }
                // FR-L7: top right of the conversation view.
                Menu {
                    expanded: ide_menu(),
                    on_dismiss: move |_| ide_menu.set(false),
                    anchor: rsx! {
                        Button {
                            text: "Open IDE",
                            icon: IconRole::Forward,
                            variant: ButtonVariant::Filled,
                            on_click: move |_| ide_menu.set(true),
                        }
                    },
                    for (kind, kind_key, label) in ide_entries.iter().cloned() {
                        Button {
                            key: "{kind_key}",
                            text: label,
                            variant: ButtonVariant::Text,
                            fill_max_width: true,
                            on_click: {
                                                                move |_: ()| {
                                    ide_menu.set(false);
                                    open_ide(kind);
                                }
                            },
                        }
                    }
                }
            }
            ConnectionBanner {}
            if busy {
                ProgressIndicator { determinate: false, fill_max_width: true }
            }

            // ---- transcript --------------------------------------------------------------------
            dioxus_compose::Box {
                fill_max_width: true,
                weight: 1.0,
                // TODO(dioxus-compose): keep the list pinned to the bottom while a reply
                // streams in (no scroll-to API on LazyColumn yet).
                LazyColumn {
                    fill_max_width: true,
                    fill_max_height: true,
                    item_count: count,
                    key_of: move |i: usize| keys[i].clone(),
                    item: move |i: usize| {
                        // Read here so this list's scope redraws when any transcript changes.
                        let _ = *live.transcripts.read();
                        let item = services().client.read(|st| st.transcript(&id_items).and_then(|t| t.items.get(i).cloned()));
                        match item {
                            Some(item) => rsx! {
                                TranscriptRow { session: id_items.clone(), agent: agent.clone(), item, expanded, answering }
                            },
                            None => rsx! {},
                        }
                    },
                }
                if count == 0 {
                    Column {
                        fill_max_width: true,
                        fill_max_height: true,
                        alignment: Alignment::Center,
                        arrangement: Arrangement::Center,
                        Text {
                            text: if status == LauncherStatus::Idle { "Say what to work on." } else { "Loading the conversation\u{2026}" },
                            type_role: TypeRole::Body,
                            color: Paint::Role(ColorRole::OnSurfaceVariant),
                        }
                    }
                }
            }

            // ---- queue (FR-L6) -------------------------------------------------------------------
            if !queued.is_empty() {
                Row {
                    fill_max_width: true,
                    alignment: Alignment::CenterStart,
                    space_role: SpaceRole::Sm,
                    Badge { text: format!("{} queued", queued.len()), tone: Tone::Accent }
                    Text {
                        text: format!("Sent in order when the current turn ends. Next: {}", model::one_line(&queued[0], 60)),
                        type_role: TypeRole::Caption,
                        color: Paint::Role(ColorRole::OnSurfaceVariant),
                        weight: 1.0,
                        max_lines: 1,
                        overflow: TextOverflow::Ellipsis,
                    }
                    Button { text: "Clear", variant: ButtonVariant::Text, on_click: clear_queue }
                }
            }
            if !queue_error.is_empty() {
                Row {
                    fill_max_width: true,
                    alignment: Alignment::CenterStart,
                    space_role: SpaceRole::Sm,
                    dioxus_compose::Box { weight: 1.0, Banner { text: format!("Could not send: {queue_error}"), tone: Tone::Error } }
                    Button { text: "Retry", variant: ButtonVariant::Text, on_click: retry_queue }
                }
            }

            // ---- composer ----------------------------------------------------------------------
            Row {
                fill_max_width: true,
                material: MaterialRole::Regular,
                shape_role: ShapeRole::Large,
                padding_role: SpaceRole::Xs,
                space_role: SpaceRole::Xs,
                alignment: Alignment::CenterStart,
                for generation in [composer_generation()] {
                    TextField {
                        key: "composer-{generation}",
                        weight: 1.0,
                        multiline: true,
                        placeholder: if busy { "Queue a message for the next turn" } else { "Message" },
                        on_value_change: move |v: String| draft.set(v),
                        on_submit: move |v: String| send.call(v),
                    }
                }
                if busy {
                    Tooltip {
                        text: "Interrupt the running turn",
                        Button {
                            text: if compact { "" } else { "Stop" },
                            icon: IconRole::Close,
                            variant: ButtonVariant::Outlined,
                            on_click: interrupt,
                        }
                    }
                }
                Button {
                    text: "",
                    icon: IconRole::Send,
                    variant: ButtonVariant::Tonal,
                    shape_role: ShapeRole::Full,
                    on_click: move |_| send.call(draft()),
                }
            }
        }
    }
}

/// One transcript entry.
#[component]
fn TranscriptRow(
    session: String,
    agent: String,
    item: TranscriptItem,
    expanded: Signal<HashSet<String>>,
    answering: Signal<HashSet<String>>,
) -> Element {
    match item {
        TranscriptItem::User { text, .. } => rsx! {
            dioxus_compose::Box {
                fill_max_width: true,
                padding_role: SpaceRole::Xs,
                alignment: Alignment::CenterEnd,
                Column {
                    alignment: Alignment::CenterEnd,
                    space_role: SpaceRole::Xs,
                    Text { text: "You", type_role: TypeRole::Caption, color: Paint::Role(ColorRole::OnSurfaceVariant) }
                    Column {
                        background: Paint::Role(ColorRole::PrimaryContainer),
                        shape_role: ShapeRole::Large,
                        padding_role: SpaceRole::Md,
                        MessageText { text, ink: ColorRole::OnPrimaryContainer }
                    }
                }
            }
        },
        TranscriptItem::Assistant { text, streaming, .. } => rsx! {
            dioxus_compose::Box {
                fill_max_width: true,
                padding_role: SpaceRole::Xs,
                alignment: Alignment::CenterStart,
                Column {
                    space_role: SpaceRole::Xs,
                    Row {
                        space_role: SpaceRole::Xs,
                        alignment: Alignment::CenterStart,
                        Text { text: agent, type_role: TypeRole::Caption, color: Paint::Role(ColorRole::OnSurfaceVariant) }
                        if streaming {
                            Spinner { size: 10.0 }
                        }
                    }
                    Column {
                        background: Paint::Role(ColorRole::SurfaceContainer),
                        shape_role: ShapeRole::Large,
                        padding_role: SpaceRole::Md,
                        // A reply still arriving shows a caret, so an empty one does not look dead.
                        MessageText { text: if streaming { format!("{text}\u{2589}") } else { text } }
                    }
                }
            }
        },
        TranscriptItem::ToolCall { call_id, name, input, result, .. } => {
            let open = expanded.read().contains(&call_id);
            let summary = model::json_summary(&input, 100);
            let toggle = {
                let call_id = call_id.clone();
                move |_: ()| {
                    let mut e = expanded;
                    let mut set = e.write();
                    if !set.remove(&call_id) {
                        set.insert(call_id.clone());
                    }
                }
            };
            let (state_text, tone) = match &result {
                None => ("running", Tone::Accent),
                Some(r) if r.is_error => ("error", Tone::Error),
                Some(_) => ("done", Tone::Success),
            };
            let name = if name.is_empty() { "Tool".to_string() } else { name };
            let has_result = result.is_some();
            let (out_text, out_err) = result.as_ref().map(|r| (r.output.clone(), r.is_error)).unwrap_or_default();
            let show_tail = has_result && !out_text.trim().is_empty();
            rsx! {
                Card {
                    fill_max_width: true,
                    padding_role: SpaceRole::Sm,
                    Column {
                        fill_max_width: true,
                        space_role: SpaceRole::Xs,
                        Row {
                            fill_max_width: true,
                            alignment: Alignment::CenterStart,
                            space_role: SpaceRole::Sm,
                            if !has_result {
                                Spinner {}
                            }
                            Text { text: name, type_role: TypeRole::BodyStrong }
                            Text {
                                text: summary,
                                type_role: TypeRole::Caption,
                                color: Paint::Role(ColorRole::OnSurfaceVariant),
                                weight: 1.0,
                                max_lines: 1,
                                overflow: TextOverflow::Ellipsis,
                            }
                            Badge { text: state_text, tone }
                            Button {
                                text: if open { "Hide" } else { "Details" },
                                icon: if open { IconRole::Collapse } else { IconRole::More },
                                variant: ButtonVariant::Text,
                                on_click: toggle,
                            }
                        }
                        if open {
                            Text { text: "Input", type_role: TypeRole::Label }
                            CodeText { text: model::json_pretty(&input) }
                            if has_result {
                                Text { text: if out_err { "Error" } else { "Result" }, type_role: TypeRole::Label }
                                CodeText { text: out_text.clone(), error: out_err }
                            }
                        } else if show_tail {
                            CodeText { text: out_text.clone(), max_lines: 3, error: out_err }
                        }
                    }
                }
            }
        }
        TranscriptItem::Approval { approval_id, tool, input, state, .. } => {
            let in_flight = answering.read().contains(&approval_id);
            let answer = {
                let session = session.clone();
                let approval_id = approval_id.clone();
                move |decision: ApprovalDecision| {
                    let mut a = answering;
                    a.write().insert(approval_id.clone());
                    let (sid, aid) = (session.clone(), approval_id.clone());
                    let done = approval_id.clone();
                    run(
                        async move { services().client.answer(&sid, &aid, decision).await.map_err(|e| e.to_string()) },
                        move |r: Result<(), String>| {
                            let mut a = answering;
                            a.write().remove(&done);
                            if let Err(e) = r {
                                show_message(format!("Could not answer: {e}"));
                            }
                        },
                    );
                }
            };
            let (a1, a2, a3) = (answer.clone(), answer.clone(), answer);
            rsx! {
                Card {
                    fill_max_width: true,
                    padding_role: SpaceRole::Sm,
                    border_width: if state == ApprovalState::Pending { Some(2.0) } else { None },
                    border_color: Paint::Role(ColorRole::Tertiary),
                    Column {
                        fill_max_width: true,
                        space_role: SpaceRole::Xs,
                        Text { text: format!("{tool} wants approval"), type_role: TypeRole::BodyStrong }
                        CodeText { text: model::json_pretty(&input), max_lines: 12 }
                        match state {
                            ApprovalState::Pending => rsx! {
                                Row {
                                    fill_max_width: true,
                                    space_role: SpaceRole::Sm,
                                    alignment: Alignment::CenterStart,
                                    Button {
                                        text: "Allow once",
                                        variant: ButtonVariant::Filled,
                                        enabled: !in_flight,
                                        on_click: { let f = a1.clone(); move |_: ()| f(ApprovalDecision::AllowOnce) },
                                    }
                                    Button {
                                        text: "Always allow",
                                        variant: ButtonVariant::Tonal,
                                        enabled: !in_flight,
                                        on_click: { let f = a2.clone(); move |_: ()| f(ApprovalDecision::AllowAlways) },
                                    }
                                    Button {
                                        text: "Deny",
                                        variant: ButtonVariant::Outlined,
                                        color: Paint::Role(ColorRole::Error),
                                        enabled: !in_flight,
                                        on_click: { let f = a3.clone(); move |_: ()| f(ApprovalDecision::Deny) },
                                    }
                                    if in_flight {
                                        Spinner {}
                                    }
                                }
                            },
                            ApprovalState::Resolved(d) => rsx! {
                                Text {
                                    text: match d {
                                        ApprovalDecision::AllowOnce => "Allowed once",
                                        ApprovalDecision::AllowAlways => "Always allowed",
                                        ApprovalDecision::Deny => "Denied",
                                    },
                                    type_role: TypeRole::Caption,
                                    color: Paint::Role(if d == ApprovalDecision::Deny { ColorRole::Error } else { ColorRole::OnSurfaceVariant }),
                                }
                            },
                            ApprovalState::Abandoned => rsx! {
                                Text {
                                    text: "Not answered before the turn ended",
                                    type_role: TypeRole::Caption,
                                    color: Paint::Role(ColorRole::OnSurfaceVariant),
                                }
                            },
                        }
                    }
                }
            }
        }
        TranscriptItem::TurnEnded { outcome, .. } => {
            let (text, ink) = match outcome {
                TurnOutcome::Completed => ("Turn completed", ColorRole::OnSurfaceVariant),
                TurnOutcome::Interrupted => ("Interrupted", ColorRole::OnSurfaceVariant),
                TurnOutcome::Failed => ("Turn failed", ColorRole::Error),
            };
            rsx! {
                Row {
                    fill_max_width: true,
                    alignment: Alignment::Center,
                    space_role: SpaceRole::Sm,
                    padding_role: SpaceRole::Xs,
                    dioxus_compose::Box { weight: 1.0, Divider {} }
                    Text { text, type_role: TypeRole::Caption, color: Paint::Role(ink) }
                    dioxus_compose::Box { weight: 1.0, Divider {} }
                }
            }
        }
        TranscriptItem::Error { message, .. } => rsx! {
            Banner { text: message, tone: Tone::Error }
        },
    }
}
