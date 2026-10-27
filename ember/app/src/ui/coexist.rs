//! Coexistence between `compose-rust` (`#[composable]`, [`Recomposer`]) and
//! `dioxus-compose` (`rsx!`, [`DioxusRuntime`]) during the UI migration (#220, INTENT D9).
//!
//! Base screens (`shell`, `home`, `project`, `session`, `conversation`, `channels`, `routines`,
//! `vcs`, `agents`, `files`, `monitor`, `characters`, `search`, `launcher`, `new_session`)
//! compose directly on `compose-rust`, while the IDE screens (`editor`, `workbench`) stay on
//! `dioxus-compose`. While individual screens are migrated one PR at a time (Phases 1–7), both
//! runtimes share UI and live state through [`SharedUi`] and [`SharedLive`], and share a single
//! window through [`CoexistingRuntime`].

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::pin::pin;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};
use std::task::{Context, Poll};

use compose_rust::protocol::{decode_batch, HostEvent, Mutation, ProtocolError};
use compose_rust::runtime::{
    composable, mutable_state_of, remember, remember_node_size, MutableState,
};
use compose_rust::ui as cu;
use compose_rust::{Batch, Recomposer, Runtime};
use dioxus_compose::host::DioxusRuntime;
use dioxus_compose::prelude::*;
use dioxus_compose::renderer::ComposeRenderer;
use ember_client::wire::{DailyUsage, DetectedAgent};

use crate::bridge::Live;
use crate::model::{self, RowContext};
use crate::prefs::Prefs;
use crate::server::{Account, ChatGptAccount};
use crate::services::{save_prefs, services, try_services};
use crate::ui::editor::EditorTarget;
use crate::ui::shell::NavMenu;
use crate::ui::theme::ThemePref;
use crate::ui::{initial_menu, Route};

// ---------------------------------------------------------------------------------------------
// Shared live and UI state (`compose-rust` MutableState + Dioxus SyncSignal bridge)
// ---------------------------------------------------------------------------------------------

/// Live client and server state observed by `compose-rust` composables (#220).
///
/// Every field is a [`MutableState`], so it is `Clone + Send + Sync + 'static` and can be
/// written from `tokio` worker threads in [`crate::bridge`] or read inside any
/// `#[composable]` function.
#[derive(Clone)]
pub struct SharedLive {
    /// Projects, session records/status, computers, connection.
    pub launcher: MutableState<u64>,
    /// Any transcript.
    pub transcripts: MutableState<u64>,
    /// The message outbox (queued-message indicator).
    pub outbox: MutableState<u64>,
    /// Any team (members, tasks: FR-T7).
    pub teams: MutableState<u64>,
    pub accounts: MutableState<Vec<Account>>,
    /// ChatGPT plan accounts (FR-U4); empty when sign-in is off.
    pub chatgpt: MutableState<Vec<ChatGptAccount>>,
    pub agents: MutableState<Vec<DetectedAgent>>,
    /// Tokens per account per UTC day, last 14 days (FR-U7); empty until loaded.
    pub usage: MutableState<Vec<DailyUsage>>,
}

impl Default for SharedLive {
    fn default() -> Self {
        Self::new()
    }
}

impl SharedLive {
    pub fn new() -> Self {
        Self {
            launcher: mutable_state_of(0),
            transcripts: mutable_state_of(0),
            outbox: mutable_state_of(0),
            teams: mutable_state_of(0),
            accounts: mutable_state_of(Vec::new()),
            chatgpt: mutable_state_of(Vec::new()),
            agents: mutable_state_of(Vec::new()),
            usage: mutable_state_of(Vec::new()),
        }
    }

    pub fn bump(state: &MutableState<u64>) {
        state.update(|v| *v += 1);
    }

    pub fn bump_all(&self) {
        Self::bump(&self.launcher);
        Self::bump(&self.transcripts);
        Self::bump(&self.outbox);
        Self::bump(&self.teams);
    }
}

/// Dioxus `SyncSignal` mirrors registered when a Dioxus `VirtualDom` mounts [`crate::ui::app`],
/// so mutations made through [`SharedUi`] from a `compose-rust` composable immediately wake
/// any mounted Dioxus screen as well.
#[derive(Clone, Copy)]
pub struct DioxusMirrors {
    pub route: SyncSignal<Route>,
    pub prefs: SyncSignal<Prefs>,
    pub selected_project: SyncSignal<Option<String>>,
    pub new_session: SyncSignal<Option<String>>,
    pub show_archived: SyncSignal<bool>,
    pub menu: SyncSignal<NavMenu>,
    pub editor: SyncSignal<Option<EditorTarget>>,
    pub theme_pref: SyncSignal<ThemePref>,
}

/// UI-wide navigation, preference and dialog state observed by `compose-rust` composables
/// and synchronized with Dioxus [`crate::ui::Ui`] (#220).
#[derive(Clone)]
pub struct SharedUi {
    pub live: SharedLive,
    pub route: MutableState<Route>,
    pub prefs: MutableState<Prefs>,
    /// The project selected on the main screen.
    pub selected_project: MutableState<Option<String>>,
    /// `Some(project)` while the new-session dialog is open (empty: no preset).
    pub new_session: MutableState<Option<String>>,
    pub show_archived: MutableState<bool>,
    /// The menu chosen in the shell (FR-L10). A cold start lands on Home (D26).
    pub menu: MutableState<NavMenu>,
    /// `Some` while the Ember 에디터 screen is open (FR-L31); it replaces the shell.
    pub editor: MutableState<Option<EditorTarget>>,
    /// System / Light / Dark choice (FR-L28).
    pub theme_pref: MutableState<ThemePref>,
    mirrors: Arc<OnceLock<DioxusMirrors>>,
}

impl SharedUi {
    pub fn new(initial_prefs: Prefs) -> Self {
        let last_project = initial_prefs.last_project.clone();
        let theme = initial_prefs.theme;
        Self {
            live: SharedLive::new(),
            route: mutable_state_of(Route::Projects),
            prefs: mutable_state_of(initial_prefs),
            selected_project: mutable_state_of(last_project),
            new_session: mutable_state_of(None),
            show_archived: mutable_state_of(false),
            menu: mutable_state_of(initial_menu()),
            editor: mutable_state_of(None),
            theme_pref: mutable_state_of(theme),
            mirrors: Arc::new(OnceLock::new()),
        }
    }

    /// Register the Dioxus `SyncSignal` mirrors from the root Dioxus component.
    pub fn register_mirrors(&self, mirrors: DioxusMirrors) {
        let _ = self.mirrors.set(mirrors);
    }

    pub fn mirrors(&self) -> Option<DioxusMirrors> {
        self.mirrors.get().copied()
    }

    /// Navigate, opening/closing the conversation in the client as the route enters/leaves it
    /// (FR-S6, FR-L2), and keeping both `MutableState`s and Dioxus signals in step.
    pub fn go(&self, to: Route) {
        let from = self.route.get_untracked();
        if from == to {
            return;
        }
        if let Some(s) = try_services() {
            if let Route::Conversation(old) = &from {
                s.close_session(old);
            }
            if let Route::Conversation(id) = &to {
                s.open_session(id);
                let project = s
                    .client
                    .read(|st| st.session(id).map(|v| v.record.project.clone()));
                if let Some(p) = project {
                    self.set_selected_project(Some(p));
                }
            }
        }
        let next_menu = match &to {
            Route::Projects | Route::Search => NavMenu::Projects,
            Route::Conversation(_) => NavMenu::Sessions,
        };
        self.set_menu(next_menu);
        self.route.set(to.clone());
        if let Some(mut m) = self.mirrors() {
            m.route.set(to);
        }
    }

    pub fn open_conversation(&self, id: &str) {
        self.go(Route::Conversation(id.to_string()));
    }

    pub fn set_menu(&self, menu: NavMenu) {
        self.menu.set(menu);
        if let Some(mut m) = self.mirrors() {
            m.menu.set(menu);
        }
    }

    pub fn set_selected_project(&self, project: Option<String>) {
        self.selected_project.set(project.clone());
        if let Some(mut m) = self.mirrors() {
            m.selected_project.set(project);
        }
    }

    pub fn set_show_archived(&self, show: bool) {
        self.show_archived.set(show);
        if let Some(mut m) = self.mirrors() {
            m.show_archived.set(show);
        }
    }

    pub fn set_theme_pref(&self, pref: ThemePref) {
        self.theme_pref.set(pref);
        if let Some(mut m) = self.mirrors() {
            m.theme_pref.set(pref);
        }
        self.update_prefs(|p| p.theme = pref);
    }

    /// Change per-device prefs and persist them when `Services` is installed.
    pub fn update_prefs(&self, f: impl FnOnce(&mut Prefs)) {
        let mut snapshot = self.prefs.get_untracked();
        f(&mut snapshot);
        self.prefs.set(snapshot.clone());
        if let Some(mut m) = self.mirrors() {
            m.prefs.set(snapshot.clone());
        }
        if try_services().is_some() {
            save_prefs(snapshot);
        }
    }

    /// Open the Ember 에디터 for a project (FR-L7, FR-L31).
    pub fn open_editor(&self, project: String, folder: String) {
        let target = Some(EditorTarget { project, folder });
        self.editor.set(target.clone());
        if let Some(mut m) = self.mirrors() {
            m.editor.set(target);
        }
    }

    pub fn close_editor(&self) {
        self.editor.set(None);
        if let Some(mut m) = self.mirrors() {
            m.editor.set(None);
        }
    }

    pub fn open_new_session(&self, project: Option<String>) {
        let val = Some(project.unwrap_or_default());
        self.new_session.set(val.clone());
        if let Some(mut m) = self.mirrors() {
            m.new_session.set(val);
        }
    }

    pub fn close_new_session(&self) {
        self.new_session.set(None);
        if let Some(mut m) = self.mirrors() {
            m.new_session.set(None);
        }
    }

    /// Read the live data a session row needs (subscribing the calling composable to it,
    /// previews included) and build rows with `f`.
    pub fn with_rows<R>(&self, f: impl FnOnce(&ember_client::State, &RowContext<'_>) -> R) -> R {
        let _ = self.live.transcripts.get();
        self.with_rows_no_previews(f)
    }

    /// Like [`SharedUi::with_rows`], without redrawing on transcript changes.
    pub fn with_rows_no_previews<R>(
        &self,
        f: impl FnOnce(&ember_client::State, &RowContext<'_>) -> R,
    ) -> R {
        let _ = self.live.launcher.get();
        let accounts = self.live.accounts.get();
        let labels = model::account_labels(&accounts);
        let cx = RowContext {
            account_labels: &labels,
        };
        services().client.read(|s| f(s, &cx))
    }
}

/// Reach the shared UI state from any `compose-rust` composable or handler.
pub fn shared_ui() -> &'static SharedUi {
    &services().ui
}

/// Synchronize Dioxus signals into [`SharedUi`] and wire [`Live`] when [`crate::ui::app`] runs.
pub fn use_shared_mirrors(
    live: Live,
    route: SyncSignal<Route>,
    prefs: SyncSignal<Prefs>,
    selected_project: SyncSignal<Option<String>>,
    new_session: SyncSignal<Option<String>>,
    show_archived: SyncSignal<bool>,
    menu: SyncSignal<NavMenu>,
    editor: SyncSignal<Option<EditorTarget>>,
    theme_pref: SyncSignal<ThemePref>,
) {
    use_hook(move || {
        let Some(s) = try_services() else { return };
        s.ui.register_mirrors(DioxusMirrors {
            route,
            prefs,
            selected_project,
            new_session,
            show_archived,
            menu,
            editor,
            theme_pref,
        });
        let _ = live;
    });
}

// ---------------------------------------------------------------------------------------------
// Embedded Dioxus slot inside a `compose-rust` composable tree (Phase 2 bridge)
// ---------------------------------------------------------------------------------------------

/// First node id reserved for Dioxus islands embedded inside a `compose-rust` tree.
/// `compose-rust` numbers its own nodes from 1 upwards, and `HtmlView` uses `0x8000_0000`,
/// so `0x2000_0000..0x4000_0000` never collides with either.
const EMBED_NODE_BASE: u32 = 0x2000_0000;
const EMBED_NODE_SPAN: u32 = 0x0001_0000;
const EMBED_HANDLER_BASE: u64 = 1 << 44;
const EMBED_HANDLER_SPAN: u64 = 1 << 24;
const EMBED_SLOTS: u32 = 1024;

struct EmbeddedDioxus {
    dom: VirtualDom,
    renderer: ComposeRenderer,
    slot: u32,
    built: bool,
    root: Option<u32>,
    attached_to: Option<u32>,
    dirty: bool,
}

#[derive(Default)]
struct EmbedRegistry {
    islands: HashMap<u32, EmbeddedDioxus>,
    next_slot: u32,
    free_slots: Vec<u32>,
    /// Nodes to remove on the next frame because their composable slot was disposed.
    removed_roots: Vec<u32>,
}

impl EmbedRegistry {
    fn take_slot(&mut self) -> Option<u32> {
        if let Some(slot) = self.free_slots.pop() {
            return Some(slot);
        }
        (self.next_slot < EMBED_SLOTS).then(|| {
            let slot = self.next_slot;
            self.next_slot += 1;
            slot
        })
    }
}

thread_local! {
    static EMBED_REGISTRY: RefCell<EmbedRegistry> = RefCell::new(EmbedRegistry::default());
}

fn register_embedded_dioxus(token: u32, app: fn() -> Element) {
    EMBED_REGISTRY.with_borrow_mut(|reg| {
        if reg.islands.contains_key(&token) {
            return;
        }
        let Some(slot) = reg.take_slot() else { return };
        let first_node = EMBED_NODE_BASE + slot * EMBED_NODE_SPAN;
        let first_handler = EMBED_HANDLER_BASE + u64::from(slot) * EMBED_HANDLER_SPAN;
        reg.islands.insert(
            token,
            EmbeddedDioxus {
                dom: VirtualDom::new(app),
                renderer: ComposeRenderer::with_id_bases(first_node, first_handler),
                slot,
                built: false,
                root: None,
                attached_to: None,
                dirty: true,
            },
        );
    });
}

fn unregister_embedded_dioxus(token: u32) {
    let _ = EMBED_REGISTRY.try_with(|reg| {
        if let Ok(mut reg) = reg.try_borrow_mut()
            && let Some(island) = reg.islands.remove(&token)
        {
            reg.free_slots.push(island.slot);
            if let Some(root) = island.root {
                reg.removed_roots.push(root);
            }
        }
    });
}

/// RAII handle kept in a composable's `remember` slot so its embedded `VirtualDom` is dropped
/// as soon as the composable slot or its `Recomposer` is dropped (never left in TLS until
/// thread exit, when `dioxus_core`'s runtime TLS may already be destroyed).
struct EmbeddedHandle {
    token: u32,
}

impl EmbeddedHandle {
    fn new(token: u32, app: fn() -> Element) -> Rc<Self> {
        register_embedded_dioxus(token, app);
        Rc::new(Self { token })
    }
}

impl Drop for EmbeddedHandle {
    fn drop(&mut self) {
        unregister_embedded_dioxus(self.token);
    }
}

/// Mounts a Dioxus `rsx!` component inside a `compose-rust` composable container (#220).
///
/// Used while `shell.rs` is in `compose-rust` (Phase 2) and individual child screens are still
/// being migrated (Phases 3–7). The composable emits a container `Box` with `modifier` and
/// attaches the Dioxus `VirtualDom`'s root node under that container node in the same batch.
#[composable]
pub fn DioxusSlot(app: fn() -> Element, modifier: cu::Modifier) {
    let size = remember_node_size();
    let token = size.token();
    let token_u32 = token as u32;
    let _handle = remember(move || EmbeddedHandle::new(token_u32, app));
    cu::Box()
        .modifier(modifier.observe_size(token))
        .content(|| {});
}

fn flush_embedded_dioxus(recomposer: &mut Recomposer) {
    EMBED_REGISTRY.with_borrow_mut(|reg| {
        if reg.islands.is_empty() && reg.removed_roots.is_empty() {
            return;
        }
        for root in reg.removed_roots.drain(..) {
            recomposer.batch_mut().write(Mutation::Remove { node_id: root });
        }
        if reg.islands.is_empty() {
            return;
        }
        // Map each observed size_token to the container node_id in the Recomposer.
        let tokens: Vec<u32> = reg.islands.keys().copied().collect();
        let mut token_to_node = HashMap::new();
        // Node ids in Recomposer count up from 1; scan active nodes for matching size_tokens.
        for node_id in 1..4096 {
            if let Some(tok) = recomposer.size_token(node_id)
                && tokens.contains(&tok)
            {
                token_to_node.insert(tok, node_id);
                if token_to_node.len() == tokens.len() {
                    break;
                }
            }
        }
        let batch = recomposer.batch_mut();
        for (&token, island) in reg.islands.iter_mut() {
            let Some(&host_node) = token_to_node.get(&token) else {
                continue;
            };
            island.renderer.begin_frame();
            if !island.built {
                island.dom.rebuild(&mut island.renderer);
                island.built = true;
                island.dirty = false;
            } else if island.dirty {
                island.dom.render_immediate(&mut island.renderer);
                island.dirty = false;
            }
            let mut current_root = island.root;
            if let Ok(bytes) = island.renderer.finish_frame()
                && let Ok(mutations) = decode_batch(bytes)
            {
                for m in mutations {
                    match &m {
                        Mutation::Insert {
                            parent_id: 0,
                            node_id,
                            index,
                        } => {
                            if Some(*node_id) != island.root
                                && let Some(old_root) = island.root
                            {
                                batch.write(Mutation::Remove { node_id: old_root });
                            }
                            current_root = Some(*node_id);
                            island.attached_to = Some(host_node);
                            batch.write(Mutation::Insert {
                                parent_id: host_node,
                                node_id: *node_id,
                                index: *index,
                            });
                        }
                        Mutation::Move {
                            parent_id: 0,
                            node_id,
                            index,
                        } => {
                            batch.write(Mutation::Move {
                                parent_id: host_node,
                                node_id: *node_id,
                                index: *index,
                            });
                        }
                        Mutation::Remove { node_id } if Some(*node_id) == current_root => {
                            current_root = None;
                            island.attached_to = None;
                            batch.write(m);
                        }
                        _ => batch.write(m),
                    }
                }
            }
            island.root = current_root;
            if island.attached_to != Some(host_node)
                && let Some(root) = island.root
            {
                batch.write(Mutation::Insert {
                    parent_id: host_node,
                    node_id: root,
                    index: 0,
                });
                island.attached_to = Some(host_node);
            }
        }
    });
}

fn route_embedded_event(
    recomposer: &mut Recomposer,
    event: &HostEvent<'_>,
) -> Option<Result<i64, ProtocolError>> {
    if event.handler_id < EMBED_HANDLER_BASE {
        return None;
    }
    EMBED_REGISTRY.with_borrow_mut(|reg| {
        let island = reg
            .islands
            .values_mut()
            .find(|i| i.renderer.handler(event.handler_id).is_some())?;
        let res = deliver_embedded_event(&mut island.dom, &island.renderer, event);
        island.dirty = true;
        Some(res)
    })
    .map(|res| {
        flush_embedded_dioxus(recomposer);
        res
    })
}

fn poll_embedded_dioxus(context: &mut Context<'_>) -> bool {
    EMBED_REGISTRY.with_borrow_mut(|reg| {
        let mut ready = !reg.removed_roots.is_empty();
        for island in reg.islands.values_mut() {
            let work = island.dom.wait_for_work();
            let mut work = pin!(work);
            if work.as_mut().poll(context).is_ready() {
                island.dirty = true;
                ready = true;
            }
        }
        ready
    })
}

fn deliver_embedded_event(
    dom: &mut VirtualDom,
    renderer: &ComposeRenderer,
    event: &HostEvent<'_>,
) -> Result<i64, ProtocolError> {
    use compose_rust::schema::EventPayload;
    use dioxus_core::Event;

    let Some((element, node_id, name)) = renderer.handler(event.handler_id) else {
        return Err(ProtocolError::InvalidValueKind(0));
    };
    if event.node_id != node_id {
        return Err(ProtocolError::InvalidValueKind(0));
    }
    let pointer_handler = matches!(name, "pointer" | "onpointer");
    if pointer_handler != matches!(event.payload, EventPayload::Pointer { .. }) {
        return Err(ProtocolError::InvalidValueKind(0));
    }
    let mut key_event = None;
    let mut pointer_event = None;
    let link = renderer.link_value(event.handler_id);
    let event_data = match event.payload {
        EventPayload::Clicked if link.is_some() => {
            Event::new(Rc::new(link.unwrap_or_default()), false).into_any()
        }
        EventPayload::Clicked | EventPayload::FocusLost | EventPayload::FilesEntered => {
            Event::new(Rc::new(()), true).into_any()
        }
        EventPayload::TextChanged(text) | EventPayload::TextSubmitted(text) => {
            Event::new(Rc::new(text.to_owned()), true).into_any()
        }
        EventPayload::KeyDown {
            key,
            shift_key,
            ctrl_key,
            alt_key,
            meta_key,
        } => {
            let value = KeyEvent::new(key, shift_key, ctrl_key, alt_key, meta_key);
            key_event = Some(value.clone());
            Event::new(Rc::new(value), true).into_any()
        }
        EventPayload::Pointer {
            kind,
            pointer_id,
            pointer_type,
            x,
            y,
            buttons,
            button,
            modifiers,
            click_count,
            delta_x,
            delta_y,
            delta_mode,
            timestamp_nanos,
        } => {
            let value = PointerEvent::new(
                kind,
                pointer_id,
                pointer_type,
                x,
                y,
                buttons,
                button,
                modifiers,
                click_count,
                delta_x,
                delta_y,
                delta_mode,
                timestamp_nanos,
            );
            pointer_event = Some(value.clone());
            Event::new(Rc::new(value), true).into_any()
        }
        EventPayload::FilesDropped(paths) => {
            Event::new(Rc::new(FileDrop::new(paths)), true).into_any()
        }
        EventPayload::RangeRequested { start, count } => {
            Event::new(Rc::new(RangeRequest::new(start, count)), true).into_any()
        }
        EventPayload::ValueChanged(value) => Event::new(Rc::new(value), true).into_any(),
        _ => return Err(ProtocolError::InvalidValueKind(0)),
    };
    dom.runtime().handle_event(name, event_data, element);
    Ok(i64::from(
        key_event.is_some_and(|e| e.consumed()) || pointer_event.is_some_and(|e| e.consumed()),
    ))
}

// ---------------------------------------------------------------------------------------------
// Top-level runtime switching (`CoexistingRuntime`: `Recomposer` <-> `DioxusRuntime`)
// ---------------------------------------------------------------------------------------------

/// Which top-level runtime owns the window right now (#220, INTENT D9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveSurface {
    /// Base UI on `compose-rust` ([`Recomposer`]).
    Compose,
    /// IDE or unmigrated full-window surface on `dioxus-compose` ([`DioxusRuntime`]).
    Dioxus,
}

/// A [`compose_rust::Runtime`] that hosts both a `compose-rust` [`Recomposer`] (for base screens)
/// and a `dioxus-compose` [`DioxusRuntime`] (for the IDE / unmigrated screens) in the same
/// window, switching cleanly whenever the selector changes (#220).
///
/// When the active surface changes between [`ActiveSurface::Compose`] and
/// [`ActiveSurface::Dioxus`], `CoexistingRuntime` removes every top-level node (`parent_id == 0`)
/// that the outgoing runtime had placed on screen and rebuilds the incoming runtime into the
/// shared [`Batch`]. While [`ActiveSurface::Compose`] is active, it also drives any
/// [`DioxusSlot`] islands embedded inside the `compose-rust` tree.
pub struct CoexistingRuntime {
    batch: Batch,
    select: Box<dyn Fn() -> ActiveSurface>,
    make_compose: Box<dyn Fn() -> Recomposer>,
    make_dioxus: Box<dyn Fn() -> DioxusRuntime>,
    active: ActiveSurface,
    compose: Option<Recomposer>,
    dioxus: Option<DioxusRuntime>,
    root_nodes: Vec<u32>,
    /// Observed changes to `select` wake the Host via `poll_work`.
    surface_state: Option<MutableState<ActiveSurface>>,
}

impl CoexistingRuntime {
    pub fn new(
        select: impl Fn() -> ActiveSurface + 'static,
        make_compose: impl Fn() -> Recomposer + 'static,
        make_dioxus: impl Fn() -> DioxusRuntime + 'static,
    ) -> Self {
        let initial = select();
        Self {
            batch: Batch::new(),
            select: Box::new(select),
            make_compose: Box::new(make_compose),
            make_dioxus: Box::new(make_dioxus),
            active: initial,
            compose: None,
            dioxus: None,
            root_nodes: Vec::new(),
            surface_state: None,
        }
    }

    /// Build a `CoexistingRuntime` driven by a [`MutableState<ActiveSurface>`].
    pub fn with_state(
        state: MutableState<ActiveSurface>,
        compose_root: fn(),
        dioxus_root: fn() -> Element,
    ) -> Self {
        let s = state.clone();
        let mut rt = Self::new(
            move || s.get_untracked(),
            move || Recomposer::new(compose_root),
            move || DioxusRuntime::new(dioxus_root),
        );
        rt.surface_state = Some(state);
        rt
    }

    /// Track top-level root insertions (`parent_id == 0`) and removals emitted by the inner
    /// runtime, and copy its records into `self.batch`.
    fn drain_inner_batch(inner_batch: &mut Batch, out_batch: &mut Batch, roots: &mut Vec<u32>) {
        let Ok(bytes) = inner_batch.finish() else {
            return;
        };
        let Ok(mutations) = decode_batch(bytes) else {
            return;
        };
        let mut root_set: HashSet<u32> = roots.iter().copied().collect();
        for m in mutations {
            match &m {
                Mutation::Insert {
                    parent_id: 0,
                    node_id,
                    ..
                } => {
                    if root_set.insert(*node_id) {
                        roots.push(*node_id);
                    }
                }
                Mutation::Remove { node_id } => {
                    if root_set.remove(node_id) {
                        roots.retain(|id| id != node_id);
                    }
                }
                _ => {}
            }
            out_batch.write(m);
        }
    }

    fn rebuild_active(&mut self) {
        for old_root in self.root_nodes.drain(..) {
            self.batch.write(Mutation::Remove { node_id: old_root });
        }
        match self.active {
            ActiveSurface::Compose => {
                self.dioxus = None;
                let mut recomposer = (self.make_compose)();
                recomposer.batch_mut().begin();
                recomposer.rebuild();
                flush_embedded_dioxus(&mut recomposer);
                Self::drain_inner_batch(
                    recomposer.batch_mut(),
                    &mut self.batch,
                    &mut self.root_nodes,
                );
                self.compose = Some(recomposer);
            }
            ActiveSurface::Dioxus => {
                self.compose = None;
                let mut dioxus = (self.make_dioxus)();
                dioxus.batch_mut().begin();
                dioxus.rebuild();
                Self::drain_inner_batch(
                    dioxus.batch_mut(),
                    &mut self.batch,
                    &mut self.root_nodes,
                );
                self.dioxus = Some(dioxus);
            }
        }
    }
}

impl Runtime for CoexistingRuntime {
    fn batch(&self) -> &Batch {
        &self.batch
    }

    fn batch_mut(&mut self) -> &mut Batch {
        &mut self.batch
    }

    fn rebuild(&mut self) {
        self.active = (self.select)();
        self.rebuild_active();
    }

    fn render(&mut self, frame_time_nanos: Option<u64>) {
        let desired = (self.select)();
        if desired != self.active {
            self.active = desired;
            self.rebuild_active();
            return;
        }
        match self.active {
            ActiveSurface::Compose => {
                if let Some(recomposer) = &mut self.compose {
                    recomposer.batch_mut().begin();
                    recomposer.render(frame_time_nanos);
                    flush_embedded_dioxus(recomposer);
                    Self::drain_inner_batch(
                        recomposer.batch_mut(),
                        &mut self.batch,
                        &mut self.root_nodes,
                    );
                } else {
                    self.rebuild_active();
                }
            }
            ActiveSurface::Dioxus => {
                if let Some(dioxus) = &mut self.dioxus {
                    dioxus.batch_mut().begin();
                    dioxus.render(frame_time_nanos);
                    Self::drain_inner_batch(
                        dioxus.batch_mut(),
                        &mut self.batch,
                        &mut self.root_nodes,
                    );
                } else {
                    self.rebuild_active();
                }
            }
        }
    }

    fn handle_event(&mut self, event: &HostEvent<'_>) -> Result<i64, ProtocolError> {
        match self.active {
            ActiveSurface::Compose => {
                let Some(recomposer) = &mut self.compose else {
                    return Err(ProtocolError::InvalidValueKind(0));
                };
                if let Some(res) = route_embedded_event(recomposer, event) {
                    return res;
                }
                recomposer.handle_event(event)
            }
            ActiveSurface::Dioxus => {
                let Some(dioxus) = &mut self.dioxus else {
                    return Err(ProtocolError::InvalidValueKind(0));
                };
                dioxus.handle_event(event)
            }
        }
    }

    fn size_token(&self, node_id: u32) -> Option<u32> {
        match self.active {
            ActiveSurface::Compose => self.compose.as_ref()?.size_token(node_id),
            ActiveSurface::Dioxus => self.dioxus.as_ref()?.size_token(node_id),
        }
    }

    fn poll_work(&mut self, context: &mut Context<'_>) -> Poll<()> {
        if (self.select)() != self.active {
            return Poll::Ready(());
        }
        match self.active {
            ActiveSurface::Compose => {
                let mut ready = poll_embedded_dioxus(context);
                if let Some(recomposer) = &mut self.compose
                    && Runtime::poll_work(recomposer, context).is_ready()
                {
                    ready = true;
                }
                if ready {
                    Poll::Ready(())
                } else {
                    Poll::Pending
                }
            }
            ActiveSurface::Dioxus => match &mut self.dioxus {
                Some(dioxus) => dioxus.poll_work(context),
                None => Poll::Pending,
            },
        }
    }

    fn run_in_context(&mut self, action: &mut dyn FnMut()) {
        match self.active {
            ActiveSurface::Compose => {
                if let Some(recomposer) = &mut self.compose {
                    recomposer.run_in_context(action);
                } else {
                    action();
                }
            }
            ActiveSurface::Dioxus => {
                if let Some(dioxus) = &mut self.dioxus {
                    dioxus.run_in_context(action);
                } else {
                    action();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use compose_rust::schema::{EventPayload, PropertyKind, WidgetKind};
    use compose_rust::Host;

    #[test]
    fn shared_ui_tracks_route_editor_and_theme_transitions() {
        let ui = SharedUi::new(Prefs::default());
        assert_eq!(ui.menu.get_untracked(), NavMenu::Home);
        assert_eq!(ui.route.get_untracked(), Route::Projects);
        assert_eq!(ui.editor.get_untracked(), None);
        assert_eq!(ui.theme_pref.get_untracked(), ThemePref::System);

        ui.open_conversation("sess-42");
        assert_eq!(
            ui.route.get_untracked(),
            Route::Conversation("sess-42".to_string())
        );
        assert_eq!(ui.menu.get_untracked(), NavMenu::Sessions);

        ui.go(Route::Search);
        assert_eq!(ui.route.get_untracked(), Route::Search);
        assert_eq!(ui.menu.get_untracked(), NavMenu::Projects);

        ui.open_editor("ember".to_string(), "/workspace/ember".to_string());
        assert_eq!(
            ui.editor.get_untracked(),
            Some(EditorTarget {
                project: "ember".to_string(),
                folder: "/workspace/ember".to_string(),
            })
        );
        ui.close_editor();
        assert_eq!(ui.editor.get_untracked(), None);

        ui.set_theme_pref(ThemePref::Dark);
        assert_eq!(ui.theme_pref.get_untracked(), ThemePref::Dark);
        assert_eq!(ui.prefs.get_untracked().theme, ThemePref::Dark);
    }

    fn has_text(mutations: &[Mutation<'_>], expected: &str) -> bool {
        mutations.iter().any(|m| {
            matches!(
                m,
                Mutation::SetProp {
                    property: PropertyKind::Text,
                    value: compose_rust::protocol::PropertyValue::String(s),
                    ..
                } if *s == expected
            ) || matches!(m, Mutation::SetText { text, .. } if *text == expected)
                || matches!(m, Mutation::AppendText { text, .. } if *text == expected)
        })
    }

    #[test]
    fn shared_live_bumps_recompose_compose_rust_tree() {
        let live = SharedLive::new();
        let launcher = live.launcher.clone();

        let mut host = Host::new(move || {
            let launcher = launcher.clone();
            Box::new(Recomposer::new(move || {
                let rev = launcher.get();
                cu::Text(format!("rev:{rev}"));
            }))
        });

        let initial = decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        assert!(has_text(&initial, "rev:0"), "initial batch: {initial:?}");

        SharedLive::bump(&live.launcher);
        let updated = decode_batch(host.render_frame(16_000_000).expect("render")).expect("decode");
        assert!(
            has_text(&updated, "rev:1") || has_text(&updated, "1"),
            "updated batch: {updated:?}"
        );
    }

    #[composable]
    fn sample_compose_screen() {
        cu::Column().content(|| {
            cu::Text("compose-base-screen");
            cu::Button("compose-btn").on_click(|| {});
        });
    }

    fn sample_dioxus_screen() -> Element {
        rsx! {
            Column {
                Text { text: "dioxus-ide-screen" }
                Button {
                    text: "dioxus-btn",
                    on_click: move |_| {},
                }
            }
        }
    }

    #[test]
    fn coexisting_runtime_switches_between_compose_and_dioxus_cleanly() {
        let surface = mutable_state_of(ActiveSurface::Compose);
        let s = surface.clone();
        let mut host = Host::new(move || {
            Box::new(CoexistingRuntime::with_state(
                s.clone(),
                sample_compose_screen,
                sample_dioxus_screen,
            ))
        });

        // 1. Starts on compose-rust base screen.
        let batch_1 = decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        let compose_root = batch_1
            .iter()
            .find_map(|m| match m {
                Mutation::Insert {
                    parent_id: 0,
                    node_id,
                    ..
                } => Some(*node_id),
                _ => None,
            })
            .expect("compose root node inserted under parent 0");
        assert!(
            has_text(&batch_1, "compose-base-screen"),
            "batch_1: {batch_1:?}"
        );

        // 2. Switch to dioxus-compose IDE screen: old root is removed and new root is inserted.
        surface.set(ActiveSurface::Dioxus);
        let batch_2 = decode_batch(host.render_frame(16_000_000).expect("render")).expect("decode");
        assert!(
            batch_2.iter().any(|m| matches!(
                m,
                Mutation::Remove { node_id } if *node_id == compose_root
            )),
            "switching to Dioxus must remove outgoing Compose root node {compose_root}: {batch_2:?}"
        );
        assert!(
            has_text(&batch_2, "dioxus-ide-screen"),
            "batch_2: {batch_2:?}"
        );

        // 3. Switch back to compose-rust base screen: Dioxus root is removed and Compose rebuilds.
        surface.set(ActiveSurface::Compose);
        let batch_3 = decode_batch(host.render_frame(32_000_000).expect("render")).expect("decode");
        assert!(batch_3.iter().any(|m| matches!(m, Mutation::Remove { .. })));
        assert!(
            has_text(&batch_3, "compose-base-screen"),
            "batch_3: {batch_3:?}"
        );
    }

    fn embedded_child_app() -> Element {
        let mut clicks = use_signal(|| 0);
        rsx! {
            Button {
                text: "embedded-{clicks}",
                on_click: move |_| clicks += 1,
            }
        }
    }

    #[composable]
    fn compose_shell_with_dioxus_slot() {
        cu::Column().content(|| {
            cu::Text("compose-shell-header");
            DioxusSlot(embedded_child_app, cu::Modifier::new().fill_max_width());
        });
    }

    #[test]
    fn dioxus_slot_embeds_inside_compose_tree_and_routes_events() {
        let surface = mutable_state_of(ActiveSurface::Compose);
        let s = surface.clone();
        let mut host = Host::new(move || {
            Box::new(CoexistingRuntime::with_state(
                s.clone(),
                compose_shell_with_dioxus_slot,
                sample_dioxus_screen,
            ))
        });

        // Initial rebuild runs the composable, registers the DioxusSlot effect, and flushes
        // the embedded Dioxus tree under the observed container node in the same batch.
        let b1 = decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        assert!(has_text(&b1, "compose-shell-header"), "b1: {b1:?}");
        let (btn_node, btn_handler) = b1
            .iter()
            .find_map(|m| match m {
                Mutation::SetProp {
                    node_id,
                    property: PropertyKind::OnClick,
                    value: compose_rust::protocol::PropertyValue::Integer(h),
                } if *node_id >= EMBED_NODE_BASE => Some((*node_id, *h as u64)),
                _ => None,
            })
            .expect("embedded Dioxus button handler registered in EMBED_NODE_BASE range");

        assert!(b1.iter().any(|m| matches!(
            m,
            Mutation::Create {
                node_id,
                widget: WidgetKind::Button,
            } if *node_id == btn_node
        )));

        // Dispatch a click to the embedded Dioxus button and verify it updates to "embedded-1".
        let (b3_bytes, _) = host
            .dispatch(HostEvent {
                node_id: btn_node,
                handler_id: btn_handler,
                payload: EventPayload::Clicked,
            })
            .expect("dispatch click to embedded Dioxus slot");
        let b3 = decode_batch(b3_bytes).expect("decode");
        assert!(has_text(&b3, "embedded-1"), "b3: {b3:?}");
    }
}
