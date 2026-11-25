//! The editor server settings (SPEC `FR-W7`, `FR-W7a`, INTENT D56): the "에디터 서버" section of
//! the settings sheet, where the owner edits the server's record (`GET`/`PUT
//! /api/editor/settings`), and the same fields on a computer's page, each inherited from the
//! server ("서버 설정 따르기") or overridden for that node, with the node's last apply report
//! (`GET`/`PUT /api/computers/{id}/editor/settings`).
//!
//! Only the owner edits (both PUTs are owner routes, `FR-S12`): on this server's own door that is
//! the app holding the owner key, over the transport a device with scope `full`, and the demo.
//! Everyone else sees one line saying so; the computer's page still shows them the report.
//! The pure parts (who may edit, the inherit toggles, the merged values, the report lines) are
//! here beside the composables and tested without a server.

#![allow(non_snake_case)]

use compose_rust::runtime::{launched_effect, mutable_state_of, remember, MutableState};
use compose_rust::ui::Modifier;
use compose_rust::{Alignment, TypeRole};

use ember_client::api::Whoami;

use super::components::{
    AppButton, ButtonKind, Choice, ChoiceButton, ChoiceRow, EmberField, FieldOptions, InsetPanel, Label, Pill, PillTone,
    Txt,
};
use super::notice;
use super::theme::*;
use crate::owner_claim::ClaimState;
use crate::server::{
    editor_error_text, ApplyReport, ComputerEditorSettings, EditorDefaultIde, EditorExtension, EditorOverrides,
    EditorServerKind, EditorSettings, ServerError, APPLY_FIELDS, DEFAULT_EDITOR_THEME,
};
use crate::services::{run_shared, try_services};
use crate::ui::coexist::shared_ui;

// ---- text ----------------------------------------------------------------------------------

pub const TITLE: &str = "에디터 서버";
pub const SUBTITLE: &str = "모든 컴퓨터의 에디터 서버, 미리 설치할 확장, 테마와 IDE 기본값을 정합니다. 컴퓨터마다 따로 바꿀 수 있습니다.";
pub const OWNER_ONLY: &str = "에디터 서버 설정은 서버 소유자만 바꿀 수 있습니다.";
pub const LOADING: &str = "에디터 설정을 불러오는 중입니다";
pub const KIND_LABEL: &str = "서버 종류";
pub const SCRIPT_LABEL: &str = "스크립트";
pub const SCRIPT_HINT: &str = "저장하면 각 컴퓨터에서 사용자 권한으로 한 번 실행됩니다. 실행하기 전에 내용을 확인하세요.";
pub const LICENSE_TOGGLE: &str = "VS Code Server 라이선스에 동의합니다";
pub const LICENSE_URL: &str = "https://code.visualstudio.com/license/server";
pub const LICENSE_HINT: &str = "VS Code를 고르려면 아래 라이선스에 동의해야 합니다.";
pub const EXTENSIONS_LABEL: &str = "미리 설치할 확장";
pub const EXTENSIONS_EMPTY: &str = "미리 설치할 확장이 없습니다.";
pub const EXTENSION_PLACEHOLDER: &str = "publisher.name";
pub const EXTENSION_ADD: &str = "추가";
pub const EXTENSION_REMOVE: &str = "빼기";
pub const VSCODE_EXTENSIONS_NOTE: &str = "VS Code의 확장은 사용자가 직접 설치합니다.";
pub const THEME_LABEL: &str = "테마";
pub const DEFAULT_IDE_LABEL: &str = "IDE 기본값";
pub const DEFAULT_IDE_HINT: &str = "IDE를 고른 적 없는 프로젝트에서 IDE 열기가 이 IDE를 엽니다.";
pub const SAVE: &str = "저장";
pub const SAVED: &str = "저장했습니다. 각 컴퓨터에 적용합니다.";
pub const SCRIPT_REQUIRED: &str = "직접 스크립트를 고르면 스크립트 내용이 필요합니다.";
pub const LICENSE_REQUIRED: &str = "VS Code를 고르려면 VS Code Server 라이선스에 동의해야 합니다.";
pub const INHERIT: &str = "서버 설정 따르기";
pub const REPORT_LABEL: &str = "마지막 적용";
pub const NO_REPORT: &str = "아직 적용한 결과가 없습니다.";
pub const APPLIED: &str = "적용됨";
pub const NOT_APPLIED: &str = "적용하지 못했습니다";
pub const UNREACHABLE: &str = "연결할 수 없음";
pub const UNREACHABLE_NOTE: &str = "컴퓨터에 연결할 수 없어 마지막으로 받은 결과를 보여 줍니다.";

/// The line under the licence toggle once it is accepted: who and on which day (UTC), as the
/// server recorded them. `None` while it is not accepted or the server recorded neither (a tick
/// not saved yet).
pub fn license_line(rec: &EditorSettings) -> Option<String> {
    if !rec.vscode_license_accepted {
        return None;
    }
    let by = rec.vscode_license_accepted_by.as_deref().map(str::trim).filter(|b| !b.is_empty());
    let day = rec.vscode_license_accepted_at.map(crate::demo::day_string);
    match (by, day) {
        (Some(by), Some(day)) => Some(format!("{by}님이 {day}에 동의함")),
        (Some(by), None) => Some(format!("{by}님이 동의함")),
        (None, Some(day)) => Some(format!("{day}에 동의함")),
        (None, None) => None,
    }
}

/// Whether this app may edit the editor settings: the demo; over the transport a device with
/// scope `full`; on this server's own door, while the owner key is held.
pub fn can_edit(demo: bool, claim: &ClaimState, who: Option<&Whoami>) -> bool {
    if demo {
        return true;
    }
    match who {
        Some(w) if w.door == "transport" => w.scope == "full",
        _ => matches!(claim, ClaimState::Held | ClaimState::PendingOther { .. }),
    }
}

fn is_demo() -> bool {
    try_services().is_none_or(|s| s.demo.is_some())
}

/// The record's checks the server also makes, so the owner sees them before the PUT.
pub fn check_record(rec: &EditorSettings) -> Result<(), &'static str> {
    match rec.editor_server {
        EditorServerKind::ManualScript if rec.script.as_deref().is_none_or(|s| s.trim().is_empty()) => Err(SCRIPT_REQUIRED),
        EditorServerKind::Vscode if !rec.vscode_license_accepted => Err(LICENSE_REQUIRED),
        _ => Ok(()),
    }
}

/// `publisher.name`: two non-empty parts of letters, digits, `-` and `_`. The server checks the
/// id again (`extension_spec`).
pub fn valid_extension_id(id: &str) -> bool {
    let mut parts = id.split('.');
    let ok = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    matches!((parts.next(), parts.next(), parts.next()), (Some(a), Some(b), None) if ok(a) && ok(b))
}

/// `list` with `id` added once (ids compare without case, as the gallery does).
pub fn with_extension(list: &[EditorExtension], id: &str) -> Vec<EditorExtension> {
    let id = id.trim();
    let mut out = list.to_vec();
    if valid_extension_id(id) && !out.iter().any(|e| e.id.eq_ignore_ascii_case(id)) {
        out.push(EditorExtension { id: id.to_string(), version: None });
    }
    out
}

/// How an extension reads in the list: `id` or `id@version`.
pub fn extension_text(e: &EditorExtension) -> String {
    match e.version.as_deref().filter(|v| !v.is_empty()) {
        Some(v) => format!("{}@{v}", e.id),
        None => e.id.clone(),
    }
}

// ---- inherit and override (FR-W7a) ----------------------------------------------------------

/// The node's settings with `o`'s present fields over the server's record `server`, the
/// effective record the computer applies.
pub fn merged(server: &EditorSettings, o: &EditorOverrides) -> EditorSettings {
    let mut e = server.clone();
    if let Some(v) = o.editor_server {
        e.editor_server = v;
    }
    if let Some(v) = &o.script {
        e.script = Some(v.clone());
    }
    if let Some(v) = o.vscode_license_accepted {
        e.vscode_license_accepted = v;
    }
    if let Some(v) = &o.extensions {
        e.extensions = v.clone();
    }
    if let Some(v) = &o.theme {
        e.theme = v.clone();
    }
    if let Some(v) = o.default_ide {
        e.default_ide = v;
    }
    if let Some(v) = &o.settings {
        e.settings = v.clone();
    }
    e
}

/// The groups of fields a computer's page inherits or overrides one at a time. The editor
/// server goes with its script and licence, which only mean something beside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverrideField {
    EditorServer,
    Extensions,
    Theme,
    DefaultIde,
}

impl OverrideField {
    pub const ALL: [OverrideField; 4] =
        [OverrideField::EditorServer, OverrideField::Extensions, OverrideField::Theme, OverrideField::DefaultIde];

    pub fn label(self) -> &'static str {
        match self {
            OverrideField::EditorServer => TITLE,
            OverrideField::Extensions => EXTENSIONS_LABEL,
            OverrideField::Theme => THEME_LABEL,
            OverrideField::DefaultIde => DEFAULT_IDE_LABEL,
        }
    }

    /// True when the node sets this group itself.
    pub fn overridden(self, o: &EditorOverrides) -> bool {
        match self {
            OverrideField::EditorServer => {
                o.editor_server.is_some() || o.script.is_some() || o.vscode_license_accepted.is_some()
            }
            OverrideField::Extensions => o.extensions.is_some(),
            OverrideField::Theme => o.theme.is_some(),
            OverrideField::DefaultIde => o.default_ide.is_some(),
        }
    }

    /// Inherit the group again (`inherit`), or start overriding it from `current`, the value
    /// the node has now, so turning the toggle off changes nothing until a value is edited.
    pub fn set_inherit(self, o: &mut EditorOverrides, inherit: bool, current: &EditorSettings) {
        match (self, inherit) {
            (OverrideField::EditorServer, true) => {
                o.editor_server = None;
                o.script = None;
                o.vscode_license_accepted = None;
            }
            (OverrideField::EditorServer, false) => {
                o.editor_server = Some(current.editor_server);
                o.script = current.script.clone();
                o.vscode_license_accepted = Some(current.vscode_license_accepted);
            }
            (OverrideField::Extensions, true) => o.extensions = None,
            (OverrideField::Extensions, false) => o.extensions = Some(current.extensions.clone()),
            (OverrideField::Theme, true) => o.theme = None,
            (OverrideField::Theme, false) => o.theme = Some(current.theme.clone()),
            (OverrideField::DefaultIde, true) => o.default_ide = None,
            (OverrideField::DefaultIde, false) => o.default_ide = Some(current.default_ide),
        }
    }

    /// The inherited value as one line, for a group that follows the server.
    pub fn value_text(self, e: &EditorSettings) -> String {
        match self {
            OverrideField::EditorServer => e.editor_server.label().to_string(),
            OverrideField::Extensions if e.extensions.is_empty() => EXTENSIONS_EMPTY.to_string(),
            OverrideField::Extensions => e.extensions.iter().map(extension_text).collect::<Vec<_>>().join(", "),
            OverrideField::Theme => e.theme.clone(),
            OverrideField::DefaultIde => e.default_ide.label().to_string(),
        }
    }
}

/// One edit of a field, made to the server's record or to a node's overrides (where it makes
/// the field present, so overridden).
#[derive(Debug, Clone, PartialEq)]
pub enum FieldChange {
    Kind(EditorServerKind),
    Script(String),
    License(bool),
    Extensions(Vec<EditorExtension>),
    Theme(String),
    DefaultIde(EditorDefaultIde),
}

impl FieldChange {
    pub fn apply_server(self, d: &mut EditorSettings) {
        match self {
            FieldChange::Kind(k) => d.editor_server = k,
            FieldChange::Script(s) => d.script = Some(s),
            FieldChange::License(b) => d.vscode_license_accepted = b,
            FieldChange::Extensions(v) => d.extensions = v,
            FieldChange::Theme(t) => d.theme = t,
            FieldChange::DefaultIde(i) => d.default_ide = i,
        }
    }

    pub fn apply_node(self, o: &mut EditorOverrides) {
        match self {
            FieldChange::Kind(k) => o.editor_server = Some(k),
            FieldChange::Script(s) => o.script = Some(s),
            FieldChange::License(b) => o.vscode_license_accepted = Some(b),
            FieldChange::Extensions(v) => o.extensions = Some(v),
            FieldChange::Theme(t) => o.theme = Some(t),
            FieldChange::DefaultIde(i) => o.default_ide = Some(i),
        }
    }
}

/// Where the shared field controls write: the server's draft record or a node's draft
/// overrides.
#[derive(Clone, Copy)]
enum Draft {
    Server(MutableState<EditorSettings>),
    Node(MutableState<EditorOverrides>),
}

impl Draft {
    fn set(self, change: FieldChange) {
        match self {
            Draft::Server(s) => s.update(move |d| change.apply_server(d)),
            Draft::Node(s) => s.update(move |o| change.apply_node(o)),
        }
    }
}

/// The record as it is sent: an empty script is none, an empty theme the default.
pub fn record_to_send(d: &EditorSettings) -> EditorSettings {
    let mut d = d.clone();
    d.script = d.script.filter(|s| !s.trim().is_empty());
    if d.theme.trim().is_empty() {
        d.theme = DEFAULT_EDITOR_THEME.to_string();
    }
    d
}

// ---- the apply report -----------------------------------------------------------------------

/// One line of the report: the field's name, "적용됨" or the reason, and whether it applied
/// (the warning tone when not).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportRow {
    pub label: &'static str,
    pub text: String,
    pub applied: bool,
}

fn report_label(key: &str) -> &'static str {
    match key {
        "editor_server" => TITLE,
        "extensions" => EXTENSIONS_LABEL,
        "theme" => THEME_LABEL,
        _ => "기타 설정",
    }
}

/// The report's fields in [`APPLY_FIELDS`] order; a field the report leaves out has no line.
pub fn report_rows(report: Option<&ApplyReport>) -> Vec<ReportRow> {
    let Some(r) = report else { return Vec::new() };
    APPLY_FIELDS
        .iter()
        .filter_map(|key| {
            let f = r.fields.get(*key)?;
            let text = if f.applied {
                APPLIED.to_string()
            } else {
                f.reason.clone().filter(|t| !t.trim().is_empty()).unwrap_or_else(|| NOT_APPLIED.to_string())
            };
            Some(ReportRow { label: report_label(key), text, applied: f.applied })
        })
        .collect()
}

/// "마지막 적용: <day>" when the report has a time.
pub fn report_time(report: Option<&ApplyReport>) -> Option<String> {
    report?.applied_at.map(|ms| format!("{REPORT_LABEL}: {}", crate::demo::day_string(ms)))
}

/// The demo's computer: nothing overridden, the defaults, every field applied.
pub fn demo_computer() -> ComputerEditorSettings {
    let mut fields = std::collections::BTreeMap::new();
    for key in APPLY_FIELDS {
        fields.insert(key.to_string(), crate::server::FieldApply { applied: true, reason: None });
    }
    ComputerEditorSettings {
        overrides: EditorOverrides::default(),
        effective: EditorSettings::default(),
        applied: Some(ApplyReport { applied_at: Some(crate::demo::now_ms()), fields }),
        reachable: true,
    }
}

// ---- loading --------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Load<T> {
    Loading,
    Ready(T),
    Failed(String),
}

/// The text for a failed call; an owner refusal also opens the claim again ([`notice::post`]).
fn failure(e: &ServerError) -> String {
    if e.is_owner_required() {
        notice::post(e.to_string());
    }
    editor_error_text(e)
}

fn load_who(who: MutableState<Option<Whoami>>) {
    if is_demo() {
        return;
    }
    run_shared(async move { crate::services::services().whoami().await }, move |w| who.set(w));
}

/// The record as the section starts: the demo's defaults at once (it has no server), else
/// loading.
fn initial_server() -> Load<EditorSettings> {
    if is_demo() { Load::Ready(EditorSettings::default()) } else { Load::Loading }
}

fn load_server(rec: MutableState<Load<EditorSettings>>, draft: MutableState<EditorSettings>) {
    if is_demo() {
        return;
    }
    run_shared(async move { crate::services::services().server.editor_settings().await }, move |r| match r {
        Ok(v) => {
            draft.set(v.clone());
            rec.set(Load::Ready(v));
        }
        Err(e) => rec.set(Load::Failed(editor_error_text(&e))),
    });
}

fn save_server(
    rec: MutableState<Load<EditorSettings>>,
    draft: MutableState<EditorSettings>,
    error: MutableState<Option<String>>,
    saved: MutableState<bool>,
) {
    let body = record_to_send(&draft.get_untracked());
    saved.set(false);
    if let Err(t) = check_record(&body) {
        error.set(Some(t.to_string()));
        return;
    }
    error.set(None);
    if is_demo() {
        draft.set(body.clone());
        rec.set(Load::Ready(body));
        saved.set(true);
        return;
    }
    run_shared(async move { crate::services::services().server.put_editor_settings(&body).await }, move |r| match r {
        Ok(v) => {
            draft.set(v.clone());
            rec.set(Load::Ready(v));
            saved.set(true);
        }
        Err(e) => error.set(Some(failure(&e))),
    });
}

// ---- the settings sheet's section -----------------------------------------------------------

/// The "에디터 서버" section of the settings sheet: the form for the owner, one line for
/// everyone else.
pub fn EditorServerSection() {
    let ui = shared_ui().clone();
    let who = remember(|| mutable_state_of(None::<Whoami>));
    launched_effect(("editor-who",), move || async move { load_who(who) });
    let owner = can_edit(is_demo(), &ui.owner_claim.get().state, who.get().as_ref());
    EditorServerSectionAs(owner);
}

/// [`EditorServerSection`] with who may edit already decided.
pub fn EditorServerSectionAs(owner: bool) {
    let p = ember_theme().palette;
    let rec = remember(|| mutable_state_of(initial_server()));
    let draft = remember(|| mutable_state_of(EditorSettings::default()));
    let new_ext = remember(|| mutable_state_of(String::new()));
    let error = remember(|| mutable_state_of(None::<String>));
    let saved = remember(|| mutable_state_of(false));
    launched_effect(("editor-settings", owner), move || async move {
        if owner {
            load_server(rec, draft);
        }
    });

    if !owner {
        Label(OWNER_ONLY, Fs::Sm, Weight::Regular, Some(paint(p.muted)), Some(2));
        return;
    }
    match rec.get() {
        Load::Loading => Label(LOADING, Fs::Sm, Weight::Regular, Some(paint(p.muted)), Some(1)),
        Load::Failed(t) => Label(&t, Fs::Sm, Weight::Regular, Some(paint(p.red.strong)), Some(3)),
        Load::Ready(_) => {
            let d = draft.get();
            let sink = Draft::Server(draft);
            InsetPanel(true, || {
                { use ::compose_rust::ui::Column; Column!(spacing = SP[2], modifier = Modifier::new().fill_max_width(), {
                    ServerFields(&d, sink, new_ext, true);
                }) };
            });
            AppButton(SAVE, None, ButtonKind::Primary, true, true, false, move || save_server(rec, draft, error, saved));
            if let Some(t) = error.get() {
                Label(&t, Fs::Xs, Weight::Regular, Some(paint(p.red.strong)), Some(4));
            }
            if saved.get() {
                Label(SAVED, Fs::Xs, Weight::Regular, Some(paint(p.text_2)), Some(2));
            }
        }
    }
}

/// Every field of the record, writing to `sink`. `enabled: false` draws them for reading.
fn ServerFields(d: &EditorSettings, sink: Draft, new_ext: MutableState<String>, enabled: bool) {
    KindFields(d, sink, enabled);
    ExtensionFields(d, sink, new_ext, enabled);
    ThemeField(d, sink, enabled);
    DefaultIdeField(d, sink, enabled);
}

fn FieldTitle(text: &str) {
    let p = ember_theme().palette;
    Label(text, Fs::Xs, Weight::Medium, Some(paint(p.text_2)), Some(1));
}

fn Hint(text: &str) {
    let p = ember_theme().palette;
    Label(text, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(3));
}

/// The server type as three choices, the script (multiline, shown before it is saved) and the
/// VS Code licence with its address as text.
fn KindFields(d: &EditorSettings, sink: Draft, enabled: bool) {
    let p = ember_theme().palette;
    FieldTitle(KIND_LABEL);
    ChoiceRow(|| {
        for k in EditorServerKind::ALL {
            ChoiceButton(k.label(), Choice::Radio, d.editor_server == k, true, enabled, move || sink.set(FieldChange::Kind(k)));
        }
    });
    if d.editor_server == EditorServerKind::ManualScript {
        FieldTitle(SCRIPT_LABEL);
        EmberField(FieldOptions {
            fill_max_width: true,
            multiline: true,
            height: Some(120.0),
            type_role: Some(TypeRole::Mono),
            placeholder: "#!/bin/sh".to_string(),
            text: Some(d.script.clone().unwrap_or_default()),
            enabled: Some(enabled),
            on_value_change: Some(Box::new(move |v| sink.set(FieldChange::Script(v)))),
            ..Default::default()
        });
        Hint(SCRIPT_HINT);
    }
    let accepted = d.vscode_license_accepted;
    ChoiceButton(LICENSE_TOGGLE, Choice::Toggle, accepted, true, enabled, move || sink.set(FieldChange::License(!accepted)));
    Hint(LICENSE_HINT);
    Txt(LICENSE_URL, TypeRole::Mono, Some(paint(p.text)), Some(1), None, None, None);
    if let Some(line) = license_line(d) {
        Label(&line, Fs::Xs, Weight::Regular, Some(paint(p.text_2)), Some(1));
    }
}

/// The extension list: each id with 빼기, and a field with 추가.
fn ExtensionFields(d: &EditorSettings, sink: Draft, new_ext: MutableState<String>, enabled: bool) {
    let p = ember_theme().palette;
    FieldTitle(EXTENSIONS_LABEL);
    if d.editor_server == EditorServerKind::Vscode && !d.extensions.is_empty() {
        Label(VSCODE_EXTENSIONS_NOTE, Fs::Xs, Weight::Regular, Some(paint(p.orange.strong)), Some(2));
    }
    if d.extensions.is_empty() {
        Hint(EXTENSIONS_EMPTY);
    }
    for e in &d.extensions {
        let rest: Vec<EditorExtension> = d.extensions.iter().filter(|x| x.id != e.id).cloned().collect();
        { use ::compose_rust::ui::Row; Row!(spacing = SP[1], alignment = Alignment::CenterStart, modifier = Modifier::new().fill_max_width(), {
            { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().weight(1.0), {
                Txt(&extension_text(e), TypeRole::Mono, Some(paint(p.text)), Some(1), None, None, None);
            }) };
            AppButton(EXTENSION_REMOVE, None, ButtonKind::Default, true, enabled, false, move || {
                sink.set(FieldChange::Extensions(rest.clone()))
            });
        }) };
    }
    if enabled {
        let typed = new_ext.get();
        let list = d.extensions.clone();
        let generation = list.len();
        { use ::compose_rust::ui::Row; Row!(spacing = SP[1], alignment = Alignment::CenterStart, modifier = Modifier::new().fill_max_width(), {
            // The field keeps what was typed (it is uncontrolled); a new list length gives it a
            // new identity, so it starts empty after 추가.
            compose_rust::runtime::key(generation, || {
                EmberField(FieldOptions {
                    weight: Some(1.0),
                    placeholder: EXTENSION_PLACEHOLDER.to_string(),
                    text: Some(typed.clone()),
                    type_role: Some(TypeRole::Mono),
                    on_value_change: Some(Box::new(move |v| new_ext.set(v))),
                    ..Default::default()
                })
            });
            AppButton(EXTENSION_ADD, None, ButtonKind::Default, true, valid_extension_id(typed.trim()), false, move || {
                let id = new_ext.get_untracked();
                sink.set(FieldChange::Extensions(with_extension(&list, &id)));
                new_ext.set(String::new());
            });
        }) };
    }
}

/// The colour theme (`workbench.colorTheme`, FR-W6), the default shown as the placeholder.
fn ThemeField(d: &EditorSettings, sink: Draft, enabled: bool) {
    FieldTitle(THEME_LABEL);
    EmberField(FieldOptions {
        fill_max_width: true,
        placeholder: DEFAULT_EDITOR_THEME.to_string(),
        text: Some(d.theme.clone()),
        enabled: Some(enabled),
        on_value_change: Some(Box::new(move |v| sink.set(FieldChange::Theme(v)))),
        ..Default::default()
    });
    Hint(&format!("기본값: {DEFAULT_EDITOR_THEME}"));
}

/// The default IDE (FR-W7b) as three choices.
fn DefaultIdeField(d: &EditorSettings, sink: Draft, enabled: bool) {
    FieldTitle(DEFAULT_IDE_LABEL);
    ChoiceRow(|| {
        for i in EditorDefaultIde::ALL {
            ChoiceButton(i.label(), Choice::Radio, d.default_ide == i, true, enabled, move || sink.set(FieldChange::DefaultIde(i)));
        }
    });
    Hint(DEFAULT_IDE_HINT);
}

// ---- a computer's page ----------------------------------------------------------------------

/// A computer's settings as its page starts: the demo's at once, else loading.
fn initial_computer() -> Load<ComputerEditorSettings> {
    if is_demo() { Load::Ready(demo_computer()) } else { Load::Loading }
}

fn load_computer(
    id: String,
    server: MutableState<Option<EditorSettings>>,
    node: MutableState<Load<ComputerEditorSettings>>,
    draft: MutableState<EditorOverrides>,
) {
    if is_demo() {
        return;
    }
    run_shared(async move { crate::services::services().server.editor_settings().await }, move |r| {
        if let Ok(v) = r {
            server.set(Some(v));
        }
    });
    run_shared(async move { crate::services::services().server.computer_editor_settings(&id).await }, move |r| match r {
        Ok(v) => {
            draft.set(v.overrides.clone());
            node.set(Load::Ready(v));
        }
        Err(e) => node.set(Load::Failed(editor_error_text(&e))),
    });
}

fn save_computer(
    id: String,
    node: MutableState<Load<ComputerEditorSettings>>,
    draft: MutableState<EditorOverrides>,
    error: MutableState<Option<String>>,
    saved: MutableState<bool>,
    server: EditorSettings,
) {
    let body = draft.get_untracked();
    saved.set(false);
    if let Err(t) = check_record(&merged(&server, &body)) {
        error.set(Some(t.to_string()));
        return;
    }
    error.set(None);
    if is_demo() {
        let mut c = demo_computer();
        c.effective = merged(&server, &body);
        c.overrides = body;
        node.set(Load::Ready(c));
        saved.set(true);
        return;
    }
    run_shared(
        async move { crate::services::services().server.put_computer_editor_settings(&id, &body).await },
        move |r| match r {
            Ok(v) => {
                draft.set(v.overrides.clone());
                node.set(Load::Ready(v));
                saved.set(true);
            }
            Err(e) => error.set(Some(failure(&e))),
        },
    );
}

/// The editor settings on a registered computer's page: the last apply report per field (and
/// "연결할 수 없음" when the node could not be asked), then each group with "서버 설정 따르기",
/// on by default; turning it off edits the node's own value.
pub fn ComputerEditorSection(computer: &str) {
    let ui = shared_ui().clone();
    let who = remember(|| mutable_state_of(None::<Whoami>));
    launched_effect(("computer-editor-who",), move || async move { load_who(who) });
    let owner = can_edit(is_demo(), &ui.owner_claim.get().state, who.get().as_ref());
    ComputerEditorSectionAs(computer, owner);
}

/// [`ComputerEditorSection`] with who may edit already decided.
pub fn ComputerEditorSectionAs(computer: &str, owner: bool) {
    let p = ember_theme().palette;
    let id = computer.to_string();
    let server = remember(|| mutable_state_of(is_demo().then(EditorSettings::default)));
    let node = remember(|| mutable_state_of(initial_computer()));
    let draft = remember(|| mutable_state_of(EditorOverrides::default()));
    let new_ext = remember(|| mutable_state_of(String::new()));
    let error = remember(|| mutable_state_of(None::<String>));
    let saved = remember(|| mutable_state_of(false));
    {
        let id = id.clone();
        launched_effect(("computer-editor", id.clone()), move || {
            let id = id.clone();
            async move { load_computer(id, server, node, draft) }
        });
    }

    { use ::compose_rust::ui::Column; Column!(spacing = SP[1], modifier = Modifier::new().fill_max_width(), {
        { use ::compose_rust::ui::Row; Row!(spacing = SP[1], alignment = Alignment::CenterStart, {
            Label(TITLE, Fs::Md, Weight::Semibold, Some(paint(p.text)), Some(1));
            if matches!(node.get(), Load::Ready(ref c) if !c.reachable) {
                Pill(UNREACHABLE, PillTone::Orange, true);
            }
        }) };
        match node.get() {
            Load::Loading => Label(LOADING, Fs::Sm, Weight::Regular, Some(paint(p.muted)), Some(1)),
            Load::Failed(t) => Label(&t, Fs::Sm, Weight::Regular, Some(paint(p.red.strong)), Some(3)),
            Load::Ready(c) => {
                ReportPanel(&c);
                // The server's own values, for the groups that inherit them; the effective record
                // until the server's record arrives.
                let base = server.get().unwrap_or_else(|| c.effective.clone());
                let o = draft.get();
                let shown = merged(&base, &o);
                for field in OverrideField::ALL {
                    let inherit = !field.overridden(&o);
                    { use ::compose_rust::ui::Row; Row!(spacing = SP[1], alignment = Alignment::CenterStart, modifier = Modifier::new().fill_max_width(), {
                        { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().weight(1.0), {
                            Label(field.label(), Fs::Sm, Weight::Medium, Some(paint(p.text)), Some(1));
                        }) };
                        let current = shown.clone();
                        ChoiceButton(INHERIT, Choice::Toggle, inherit, true, owner, move || {
                            draft.update(|d| field.set_inherit(d, !inherit, &current));
                            saved.set(false);
                        });
                    }) };
                    if inherit {
                        Label(&field.value_text(&base), Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(2));
                    } else {
                        let sink = Draft::Node(draft);
                        InsetPanel(true, || {
                            { use ::compose_rust::ui::Column; Column!(spacing = SP[1], modifier = Modifier::new().fill_max_width(), {
                                match field {
                                    OverrideField::EditorServer => KindFields(&shown, sink, owner),
                                    OverrideField::Extensions => ExtensionFields(&shown, sink, new_ext, owner),
                                    OverrideField::Theme => ThemeField(&shown, sink, owner),
                                    OverrideField::DefaultIde => DefaultIdeField(&shown, sink, owner),
                                }
                            }) };
                        });
                    }
                }
                if owner {
                    let (id, base) = (id.clone(), base.clone());
                    AppButton(SAVE, None, ButtonKind::Primary, true, true, false, move || {
                        save_computer(id.clone(), node, draft, error, saved, base.clone())
                    });
                } else {
                    Label(OWNER_ONLY, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(2));
                }
                if let Some(t) = error.get() {
                    Label(&t, Fs::Xs, Weight::Regular, Some(paint(p.red.strong)), Some(4));
                }
                if saved.get() {
                    Label(SAVED, Fs::Xs, Weight::Regular, Some(paint(p.text_2)), Some(2));
                }
            }
        }
    }) };
}

/// The last apply report: one line per field, the reason in the warning tone.
fn ReportPanel(c: &ComputerEditorSettings) {
    let p = ember_theme().palette;
    let rows = report_rows(c.applied.as_ref());
    let time = report_time(c.applied.as_ref());
    InsetPanel(true, || {
        { use ::compose_rust::ui::Column; Column!(spacing = 4.0, modifier = Modifier::new().fill_max_width(), {
            Label(time.as_deref().unwrap_or(REPORT_LABEL), Fs::Xs, Weight::Medium, Some(paint(p.text_2)), Some(1));
            if !c.reachable {
                Label(UNREACHABLE_NOTE, Fs::Xs, Weight::Regular, Some(paint(p.orange.strong)), Some(2));
            }
            if rows.is_empty() {
                Label(NO_REPORT, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
            }
            for row in &rows {
                { use ::compose_rust::ui::Row; Row!(spacing = SP[1], alignment = Alignment::TopStart, modifier = Modifier::new().fill_max_width(), {
                    Label(row.label, Fs::Xs, Weight::Medium, Some(paint(p.text)), Some(1));
                    { use ::compose_rust::ui::Column; Column!(modifier = Modifier::new().weight(1.0), {
                        let tone = if row.applied { p.text_2 } else { p.orange.strong };
                        Label(&row.text, Fs::Xs, Weight::Regular, Some(paint(tone)), Some(4));
                    }) };
                }) };
            }
        }) };
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::FieldApply;
    use compose_rust::protocol::{decode_batch, Mutation, PropertyValue};
    use compose_rust::{Host, Recomposer};

    fn texts(mutations: &[Mutation<'_>]) -> Vec<String> {
        mutations
            .iter()
            .filter_map(|m| match m {
                Mutation::SetProp { value: PropertyValue::String(s), .. } => Some(s.to_string()),
                Mutation::SetText { text, .. } => Some(text.to_string()),
                _ => None,
            })
            .collect()
    }

    fn whoami(door: &str, scope: &str) -> Whoami {
        Whoami { door: door.into(), peer_id: Some("p".into()), scope: scope.into() }
    }

    /// SPEC FR-W7 (owner only, FR-S12): the demo edits; on this server's door only while the owner
    /// key is held; over the transport only a device with scope `full`.
    #[test]
    fn fr_w7_only_the_owner_edits_the_editor_settings() {
        assert!(can_edit(true, &ClaimState::Off, None));
        assert!(can_edit(false, &ClaimState::Held, None));
        assert!(can_edit(false, &ClaimState::PendingOther { claims: vec![] }, None));
        for not_held in [ClaimState::Off, ClaimState::Connecting, ClaimState::NeedCode { holders: 0 }, ClaimState::Lost { retry_in_ms: 1 }] {
            assert!(!can_edit(false, &not_held, None), "{not_held:?}");
        }
        assert!(can_edit(false, &ClaimState::Off, Some(&whoami("transport", "full"))));
        assert!(!can_edit(false, &ClaimState::Held, Some(&whoami("transport", "chat_only"))));
        assert!(!can_edit(false, &ClaimState::Off, Some(&whoami("local", "full"))), "the local door needs the key");
    }

    /// SPEC FR-W7: the record's own checks before the PUT, the extension list, and what is sent.
    #[test]
    fn fr_w7_record_checks_extensions_and_body() {
        let mut d = EditorSettings::default();
        assert_eq!(check_record(&d), Ok(()));
        d.editor_server = EditorServerKind::ManualScript;
        d.script = Some("  ".into());
        assert_eq!(check_record(&d), Err(SCRIPT_REQUIRED));
        d.script = Some("#!/bin/sh\nexec serve\n".into());
        assert_eq!(check_record(&d), Ok(()));
        d.editor_server = EditorServerKind::Vscode;
        assert_eq!(check_record(&d), Err(LICENSE_REQUIRED));
        d.vscode_license_accepted = true;
        assert_eq!(check_record(&d), Ok(()));

        assert!(valid_extension_id("rust-lang.rust-analyzer"));
        for bad in ["", "a", "a.", ".b", "a.b.c", "a b.c", "a/b.c"] {
            assert!(!valid_extension_id(bad), "{bad}");
        }
        let one = with_extension(&[], " a.b ");
        assert_eq!(one, vec![EditorExtension { id: "a.b".into(), version: None }]);
        assert_eq!(with_extension(&one, "A.B"), one, "once");
        assert_eq!(with_extension(&one, "bad"), one);
        assert_eq!(extension_text(&EditorExtension { id: "a.b".into(), version: Some("1.2.3".into()) }), "a.b@1.2.3");

        let sent = record_to_send(&EditorSettings { script: Some(" ".into()), theme: " ".into(), ..Default::default() });
        assert_eq!(sent.script, None);
        assert_eq!(sent.theme, DEFAULT_EDITOR_THEME);
    }

    /// SPEC FR-W7 (contract addition): under the licence toggle, who accepted and when, as the
    /// server recorded them.
    #[test]
    fn fr_w7_the_licence_line_names_who_accepted_and_when() {
        let mut d = EditorSettings {
            vscode_license_accepted_by: Some("brew".into()),
            vscode_license_accepted_at: Some(1_795_559_536_184),
            ..Default::default()
        };
        assert_eq!(license_line(&d), None, "not accepted");
        d.vscode_license_accepted = true;
        let day = crate::demo::day_string(1_795_559_536_184);
        assert_eq!(license_line(&d), Some(format!("brew님이 {day}에 동의함")));
        d.vscode_license_accepted_by = None;
        assert_eq!(license_line(&d), Some(format!("{day}에 동의함")));
        d.vscode_license_accepted_at = None;
        assert_eq!(license_line(&d), None, "a tick not saved yet");
    }

    fn compose_section(owner: bool, d: Option<EditorSettings>) -> Vec<String> {
        let mut host = Host::new(move || {
            let d = d.clone();
            Box::new(Recomposer::new(move || {
                if let Some(d) = &d {
                    // A draft drawn directly, as the form draws it once the record has loaded.
                    let draft = remember(|| mutable_state_of(d.clone()));
                    let new_ext = remember(|| mutable_state_of(String::new()));
                    let rec = draft.get();
                    ServerFields(&rec, Draft::Server(draft), new_ext, owner);
                } else {
                    EditorServerSectionAs(owner);
                }
            }))
        });
        texts(&decode_batch(host.rebuild().expect("rebuild")).expect("decode"))
    }

    /// SPEC FR-W7 (owner-only UI): a non-owner sees one line and no field; the owner's form has
    /// the three server types, the licence and its address, the extensions, the theme with its
    /// default, the default IDE and 저장.
    #[test]
    fn fr_w7_the_settings_section_is_the_owners_form() {
        let _guard = crate::services::test_guard();
        let other = compose_section(false, None);
        assert!(other.iter().any(|t| t == OWNER_ONLY), "{other:?}");
        for gone in [KIND_LABEL, LICENSE_TOGGLE, SAVE, "Code OSE"] {
            assert!(!other.iter().any(|t| t == gone), "{gone} is not shown to a non-owner: {other:?}");
        }

        let d = EditorSettings {
            editor_server: EditorServerKind::ManualScript,
            script: Some("#!/bin/sh\nexec serve\n".into()),
            vscode_license_accepted: true,
            vscode_license_accepted_by: Some("brew".into()),
            vscode_license_accepted_at: Some(1_795_559_536_184),
            extensions: vec![EditorExtension { id: "rust-lang.rust-analyzer".into(), version: None }],
            ..Default::default()
        };
        let all = compose_section(true, Some(d.clone()));
        let day = crate::demo::day_string(1_795_559_536_184);
        for want in [
            KIND_LABEL,
            "Code OSE",
            "VS Code",
            "직접 스크립트",
            SCRIPT_LABEL,
            "#!/bin/sh\nexec serve\n",
            SCRIPT_HINT,
            LICENSE_TOGGLE,
            LICENSE_URL,
            &format!("brew님이 {day}에 동의함"),
            EXTENSIONS_LABEL,
            "rust-lang.rust-analyzer",
            EXTENSION_REMOVE,
            EXTENSION_ADD,
            THEME_LABEL,
            DEFAULT_EDITOR_THEME,
            DEFAULT_IDE_LABEL,
            "Ember 에디터",
            "Gateway",
        ] {
            assert!(all.iter().any(|t| t == want), "missing {want}: {all:?}");
        }
        // The script field shows only for the manual script.
        let ose = compose_section(true, Some(EditorSettings::default()));
        assert!(!ose.iter().any(|t| t == SCRIPT_LABEL), "{ose:?}");
        assert!(ose.iter().any(|t| t == LICENSE_URL));
    }

    /// SPEC FR-W7 ("the defaults above hold on a fresh server"): the demo (the owner, no server)
    /// shows the fresh defaults with 저장.
    #[test]
    fn fr_w7_the_demo_section_shows_the_defaults() {
        let _guard = crate::services::test_guard();
        let all = compose_section(true, None);
        for want in [SAVE, "Code OSE", DEFAULT_EDITOR_THEME, "Ember 에디터", EXTENSIONS_EMPTY] {
            assert!(all.iter().any(|t| t == want), "missing {want}: {all:?}");
        }
        assert!(!all.iter().any(|t| t == LOADING), "{all:?}");
    }

    /// SPEC FR-W7a: a group inherits until its toggle is turned off; turning it off keeps the
    /// current value as the node's own; edits make the field present; the merged record puts the
    /// node's present fields over the server's.
    #[test]
    fn fr_w7_inherit_toggles_and_node_values() {
        let server = EditorSettings { default_ide: EditorDefaultIde::Vscode, theme: "Server".into(), ..Default::default() };
        let mut o = EditorOverrides::default();
        for f in OverrideField::ALL {
            assert!(!f.overridden(&o), "{f:?} inherits by default");
        }
        assert_eq!(merged(&server, &o), server);
        OverrideField::DefaultIde.set_inherit(&mut o, false, &server);
        assert_eq!(o.default_ide, Some(EditorDefaultIde::Vscode), "off keeps the current value");
        assert!(OverrideField::DefaultIde.overridden(&o));
        FieldChange::DefaultIde(EditorDefaultIde::Ember).apply_node(&mut o);
        assert_eq!(merged(&server, &o).default_ide, EditorDefaultIde::Ember);
        assert_eq!(merged(&server, &o).theme, "Server", "the other fields still follow the server");
        // The server's value changes: an inheriting field follows it, an overridden one does not.
        let later = EditorSettings { theme: "Later".into(), default_ide: EditorDefaultIde::Gateway, ..server.clone() };
        assert_eq!(merged(&later, &o).theme, "Later");
        assert_eq!(merged(&later, &o).default_ide, EditorDefaultIde::Ember);
        OverrideField::DefaultIde.set_inherit(&mut o, true, &server);
        assert_eq!(o, EditorOverrides::default());
        assert_eq!(serde_json::to_value(&o).unwrap(), serde_json::json!({}));

        OverrideField::EditorServer.set_inherit(&mut o, false, &server);
        assert!(o.editor_server.is_some() && o.vscode_license_accepted.is_some());
        FieldChange::Kind(EditorServerKind::Vscode).apply_node(&mut o);
        assert_eq!(check_record(&merged(&server, &o)), Err(LICENSE_REQUIRED));
        OverrideField::EditorServer.set_inherit(&mut o, true, &server);
        assert!(!OverrideField::EditorServer.overridden(&o));

        let mut d = EditorSettings::default();
        FieldChange::Theme("T".into()).apply_server(&mut d);
        FieldChange::Extensions(vec![EditorExtension { id: "a.b".into(), version: None }]).apply_server(&mut d);
        assert_eq!((d.theme.as_str(), d.extensions.len()), ("T", 1));
        assert_eq!(OverrideField::Extensions.value_text(&EditorSettings::default()), EXTENSIONS_EMPTY);
        assert_eq!(OverrideField::Extensions.value_text(&d), "a.b");
    }

    /// SPEC FR-W7a acceptance ("a failed apply shows on the computer's page with the reason"):
    /// each field reads 적용됨 or its reason, in the report's order.
    #[test]
    fn fr_w7_the_report_reads_per_field() {
        assert!(report_rows(None).is_empty());
        let mut fields = std::collections::BTreeMap::new();
        fields.insert("theme".to_string(), FieldApply { applied: true, reason: None });
        fields.insert("editor_server".to_string(), FieldApply { applied: false, reason: Some("DarkPyonix 매니저가 설치되어 있지 않습니다".into()) });
        fields.insert("extensions".to_string(), FieldApply { applied: false, reason: None });
        let r = ApplyReport { applied_at: Some(1_795_559_536_184), fields };
        let rows = report_rows(Some(&r));
        assert_eq!(
            rows,
            vec![
                ReportRow { label: TITLE, text: "DarkPyonix 매니저가 설치되어 있지 않습니다".into(), applied: false },
                ReportRow { label: EXTENSIONS_LABEL, text: NOT_APPLIED.into(), applied: false },
                ReportRow { label: THEME_LABEL, text: APPLIED.into(), applied: true },
            ]
        );
        assert_eq!(report_time(Some(&r)), Some(format!("{REPORT_LABEL}: {}", crate::demo::day_string(1_795_559_536_184))));
    }

    /// SPEC FR-W7a: the computer's page shows the reason of a failed field and "연결할 수 없음"
    /// when the node could not be asked; in the demo the page lists every group with its
    /// inherit toggle on.
    #[test]
    fn fr_w7_the_computer_page_shows_the_report_and_the_toggles() {
        let _guard = crate::services::test_guard();
        let mut c = demo_computer();
        c.reachable = false;
        c.applied.as_mut().unwrap().fields.insert(
            "extensions".into(),
            FieldApply { applied: false, reason: Some("VS Code의 확장은 사용자가 직접 설치합니다".into()) },
        );
        let mut host = Host::new(move || {
            let c = c.clone();
            Box::new(Recomposer::new(move || ReportPanel(&c)))
        });
        let all = texts(&decode_batch(host.rebuild().expect("rebuild")).expect("decode"));
        for want in [UNREACHABLE_NOTE, "VS Code의 확장은 사용자가 직접 설치합니다", APPLIED, TITLE, THEME_LABEL] {
            assert!(all.iter().any(|t| t == want), "missing {want}: {all:?}");
        }

        let mut host = Host::new(|| Box::new(Recomposer::new(|| ComputerEditorSectionAs("c1", true))));
        let all = texts(&decode_batch(host.rebuild().expect("rebuild")).expect("decode"));
        assert!(all.iter().any(|t| t == TITLE), "{all:?}");
        assert_eq!(all.iter().filter(|t| *t == INHERIT).count(), OverrideField::ALL.len(), "{all:?}");
        for f in OverrideField::ALL {
            assert!(all.iter().any(|t| t == f.label()), "missing {}: {all:?}", f.label());
        }
        // Inheriting groups show the server's values, not their controls.
        assert!(all.iter().any(|t| t == "Code OSE") && !all.iter().any(|t| t == KIND_LABEL), "{all:?}");
        assert!(all.iter().any(|t| t == SAVE));
        assert!(!all.iter().any(|t| t == UNREACHABLE), "the demo computer is reachable");

        // A non-owner sees the report and the values, and no 저장.
        let mut host = Host::new(|| Box::new(Recomposer::new(|| ComputerEditorSectionAs("c1", false))));
        let other = texts(&decode_batch(host.rebuild().expect("rebuild")).expect("decode"));
        assert!(other.iter().any(|t| t == OWNER_ONLY) && !other.iter().any(|t| t == SAVE), "{other:?}");
    }

    /// SPEC FR-W7a: turning a group's toggle off on the computer's page shows its controls with
    /// the node's value.
    #[test]
    fn fr_w7_turning_inherit_off_edits_the_nodes_value() {
        let _guard = crate::services::test_guard();
        let mut host = Host::new(|| {
            Box::new(Recomposer::new(|| {
                let draft = remember(|| {
                    let mut o = EditorOverrides::default();
                    OverrideField::Theme.set_inherit(&mut o, false, &EditorSettings::default());
                    o.theme = Some("Node Theme".into());
                    mutable_state_of(o)
                });
                let o = draft.get();
                let shown = merged(&EditorSettings::default(), &o);
                ThemeField(&shown, Draft::Node(draft), true);
            }))
        });
        let all = texts(&decode_batch(host.rebuild().expect("rebuild")).expect("decode"));
        assert!(all.iter().any(|t| t == THEME_LABEL) && all.iter().any(|t| t == "Node Theme"), "{all:?}");
    }
}
