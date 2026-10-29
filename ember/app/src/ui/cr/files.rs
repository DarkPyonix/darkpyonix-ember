//! compose-rust version of the Files screen (#220 phase 5; FR-L27, FR-L27.1 to FR-L27.6,
//! `docs/DESIGN.md` section 6.8): file transfer between the user's devices (D24, FR-F).
//!
//! The decisions (devices, the transfer state machine, sizes and speeds, offers, the send
//! blockers, the KPIs) are `ui::files`' plain functions and the [`TransferService`] trait, used
//! as they are. The transfer feature has no server or node API yet (FR-F is Draft), so the
//! screen shows the user's real computers as devices and "준비 중입니다" for the rest;
//! `EMBER_CR_FILES_SAMPLE=1` fills it with sample data so every section can be reviewed.
//!
//! #166 workarounds carry over: no drag and drop and no file picker (the send sheet picks the
//! receiver and then says the file choice is not ready), and tiles carry a small button.

#![allow(non_snake_case)]

use compose_rust::runtime::{mutable_state_of, remember, MutableState};
use compose_rust::ui as cu;
use compose_rust::ui::Modifier;
use compose_rust::{Alignment, ButtonVariant, IconRole, ShapeRole};

use ember_client::state::Computer;

pub use crate::ui::files::{
    answer_offer, build_model, connected_count, empty_text, format_bytes, grid, kpis, offer_line,
    path_label, percent, progress_line, send_blocker, sendable_devices, transfer_service,
    user_actions, visible_offers, Answer, Device, DeviceKind, Direction, FilesKpi, FilesModel,
    HistoryEntry, HistoryResult, Offer, PathKind, Presence, ReceivedFile, SendDraft, SendTarget,
    Transfer, TransferAction, TransferService, TransferState, FILE_PICKER_AVAILABLE,
    FILE_PICK_UNAVAILABLE, LOCK_LINE, UNAVAILABLE,
};
use super::characters::{CharState, Character};
use super::components::{AppButton, ButtonKind, Clickable, EmberCard, EmberSheetSized, FilterChip, Label, Pill, PillTone};
use super::home::{fit_label, text_px, Cols, Part};
use super::notice;
use super::project::{Header, ThemeButton};
use super::shell::NavIcon;
use super::theme::*;
use crate::services::services;
use crate::ui::coexist::{shared_ui, SharedUi};
use crate::ui::home::{crew, CrewMember};

macro_rules! line_icon {
    ($d:literal) => {
        concat!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d=""#,
            $d,
            r#""/></svg>"#
        )
    };
}

// Heroicons outline paths, as in the mockup's `FL_IC`.
const ICON_PHONE: &str = line_icon!("M10.5 1.5H8.25A2.25 2.25 0 0 0 6 3.75v16.5a2.25 2.25 0 0 0 2.25 2.25h7.5A2.25 2.25 0 0 0 18 20.25V3.75a2.25 2.25 0 0 0-2.25-2.25H13.5m-3 0V3h3V1.5m-3 0h3m-3 18.75h3");
const ICON_TABLET: &str = line_icon!("M10.5 19.5h3m-6.75 2.25h10.5a2.25 2.25 0 0 0 2.25-2.25v-15a2.25 2.25 0 0 0-2.25-2.25H6.75A2.25 2.25 0 0 0 4.5 4.5v15a2.25 2.25 0 0 0 2.25 2.25Z");
const ICON_DESKTOP: &str = line_icon!("M9 17.25v1.007a3 3 0 0 1-.879 2.122L7.5 21h9l-.621-.621A3 3 0 0 1 15 18.257V17.25m6-12V15a2.25 2.25 0 0 1-2.25 2.25H5.25A2.25 2.25 0 0 1 3 15V5.25m18 0A2.25 2.25 0 0 0 18.75 3H5.25A2.25 2.25 0 0 0 3 5.25m18 0V12a2.25 2.25 0 0 1-2.25 2.25H5.25A2.25 2.25 0 0 1 3 12V5.25");
const ICON_LAPTOP: &str = line_icon!("M4.5 15V6.75A1.5 1.5 0 0 1 6 5.25h12a1.5 1.5 0 0 1 1.5 1.5V15M2.25 15h19.5l-.6 1.8a1.5 1.5 0 0 1-1.42 1.02H4.27a1.5 1.5 0 0 1-1.42-1.02L2.25 15Z");
const ICON_SERVER: &str = line_icon!("M21.75 17.25v-.228a4.5 4.5 0 0 0-.12-1.03l-2.268-9.64a3.375 3.375 0 0 0-3.285-2.602H7.923a3.375 3.375 0 0 0-3.285 2.602l-2.268 9.64a4.5 4.5 0 0 0-.12 1.03v.228m19.5 0a3 3 0 0 1-3 3H5.25a3 3 0 0 1-3-3m19.5 0a3 3 0 0 0-3-3H5.25a3 3 0 0 0-3 3m16.5 0h.008v.008h-.008v-.008Zm-3 0h.008v.008h-.008v-.008Z");
const ICON_DOC: &str = line_icon!("M19.5 14.25v-2.625a3.375 3.375 0 0 0-3.375-3.375h-1.5A1.125 1.125 0 0 1 13.5 7.125v-1.5a3.375 3.375 0 0 0-3.375-3.375H8.25m0 12.75h7.5m-7.5 3H12M10.5 2.25H5.625c-.621 0-1.125.504-1.125 1.125v17.25c0 .621.504 1.125 1.125 1.125h12.75c.621 0 1.125-.504 1.125-1.125V11.25a9 9 0 0 0-9-9Z");
const ICON_UP: &str = line_icon!("M12 19.5v-15m0 0l-6 6m6-6l6 6");
const ICON_DOWN: &str = line_icon!("M12 4.5v15m0 0l6-6m-6 6l-6-6");
const ICON_LOCK: &str = line_icon!("M16.5 10.5V6.75a4.5 4.5 0 1 0-9 0v3.75m-.75 11.25h10.5a2.25 2.25 0 0 0 2.25-2.25v-6.75a2.25 2.25 0 0 0-2.25-2.25H6.75a2.25 2.25 0 0 0-2.25 2.25v6.75a2.25 2.25 0 0 0 2.25 2.25Z");
const ICON_CLOCK: &str = line_icon!("M12 7v5l3 2M21 12a9 9 0 11-18 0 9 9 0 0118 0z");
const ICON_SWAP: &str = include_str!("../../../assets/nav/arrows-right-left.svg");

// ---------------------------------------------------------------------------------------------
// Layout decisions and pure helpers
// ---------------------------------------------------------------------------------------------

/// Device tile columns: 4, 2 up to 640 px (`.fl-devgrid`).
pub fn device_cols(width_dp: f32) -> usize {
    if width_dp > 640.0 {
        4
    } else {
        2
    }
}

/// Transfers beside the received files above 1100 px (`.fl-cols`).
pub fn side_by_side(width_dp: f32) -> bool {
    width_dp > 1100.0
}

/// The history filter (`.fl-hfilter`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryFilter {
    All,
    Sent,
    Received,
    Problems,
}

impl HistoryFilter {
    pub const ALL: [HistoryFilter; 4] = [HistoryFilter::All, HistoryFilter::Sent, HistoryFilter::Received, HistoryFilter::Problems];

    pub fn label(self) -> &'static str {
        match self {
            HistoryFilter::All => "전체",
            HistoryFilter::Sent => "보냄",
            HistoryFilter::Received => "받음",
            HistoryFilter::Problems => "실패와 거절",
        }
    }

    pub fn matches(self, h: &HistoryEntry) -> bool {
        let done = h.result == HistoryResult::Done;
        match self {
            HistoryFilter::All => true,
            HistoryFilter::Sent => done && h.direction == Direction::Send,
            HistoryFilter::Received => done && h.direction == Direction::Receive,
            HistoryFilter::Problems => !done,
        }
    }
}

/// The glyph of a device kind.
fn kind_icon(k: DeviceKind) -> &'static str {
    match k {
        DeviceKind::Phone => ICON_PHONE,
        DeviceKind::Tablet => ICON_TABLET,
        DeviceKind::Laptop => ICON_LAPTOP,
        DeviceKind::Server => ICON_SERVER,
        DeviceKind::Desktop | DeviceKind::Computer => ICON_DESKTOP,
    }
}

fn presence_color(p: Presence, pal: &Palette) -> compose_rust::Color {
    match p {
        Presence::Online => pal.green.strong,
        Presence::Sleeping => pal.orange.strong,
        Presence::Offline | Presence::Unknown => pal.grey.strong,
    }
}

fn presence_tone(p: Presence) -> PillTone {
    match p {
        Presence::Online => PillTone::Green,
        Presence::Sleeping => PillTone::Orange,
        Presence::Offline | Presence::Unknown => PillTone::Grey,
    }
}

fn path_tone(p: PathKind) -> PillTone {
    match p {
        PathKind::Nearby => PillTone::Green,
        PathKind::Direct => PillTone::Blue,
        PathKind::Relay => PillTone::Grey,
    }
}

/// Sample data for `EMBER_CR_FILES_SAMPLE=1`, so the sections the service does not fill yet can
/// be reviewed against the mockup. Never shown otherwise.
pub fn sample_model() -> (FilesModel, Vec<Offer>) {
    let dev = |id: &str, name: &str, kind, presence, path, ms| Device { id: id.into(), name: name.into(), kind, presence, path, latency_ms: ms };
    let devices = vec![
        dev("d1", "macbook-pro", DeviceKind::Laptop, Presence::Online, Some(PathKind::Nearby), Some(3)),
        dev("d2", "BREW의 iPhone", DeviceKind::Phone, Presence::Online, Some(PathKind::Nearby), Some(6)),
        dev("d3", "ws-linux-01", DeviceKind::Server, Presence::Online, Some(PathKind::Direct), Some(18)),
        dev("d4", "iPad Pro", DeviceKind::Tablet, Presence::Online, Some(PathKind::Relay), Some(41)),
        dev("d5", "Pixel 10", DeviceKind::Phone, Presence::Sleeping, None, None),
        dev("d6", "win-desktop", DeviceKind::Desktop, Presence::Offline, None, None),
        dev("d7", "gpu-box", DeviceKind::Server, Presence::Unknown, None, None),
    ];
    let transfers = vec![
        Transfer { id: "t1".into(), name: "design-assets-v8.zip".into(), device: "macbook-pro".into(), direction: Direction::Send, total: 820 << 20, done: 560 << 20, bytes_per_sec: Some(54 << 20), state: TransferState::Sending },
        Transfer { id: "t2".into(), name: "IMG_2041.mov".into(), device: "BREW의 iPhone".into(), direction: Direction::Receive, total: 1_200 << 20, done: 300 << 20, bytes_per_sec: None, state: TransferState::Paused },
    ];
    let received = vec![
        ReceivedFile { name: "screenshot-0925.png".into(), size: 2 << 20, from: "BREW의 iPhone".into() },
        ReceivedFile { name: "notes.pdf".into(), size: 640 << 10, from: "iPad Pro".into() },
    ];
    let history = vec![
        HistoryEntry { name: "release-notes.md".into(), device: "ws-linux-01".into(), direction: Direction::Send, size: 12 << 10, result: HistoryResult::Done, when: "오늘 09:41".into() },
        HistoryEntry { name: "dump.sql".into(), device: "iPad Pro".into(), direction: Direction::Receive, size: 48 << 20, result: HistoryResult::Failed, when: "어제 22:10".into() },
        HistoryEntry { name: "photo.heic".into(), device: "Pixel 10".into(), direction: Direction::Send, size: 4 << 20, result: HistoryResult::Declined, when: "어제 18:02".into() },
    ];
    let offers = vec![Offer { id: "o1".into(), from: "BREW의 iPhone".into(), files: vec!["IMG_2042.heic".into(), "IMG_2043.heic".into()], total: 9 << 20 }];
    (FilesModel { devices, transfers, received, history, ready: true }, offers)
}

// ---------------------------------------------------------------------------------------------
// The screen
// ---------------------------------------------------------------------------------------------

/// `EMBER_CR_FILES_SHEET=send` opens the send sheet at the start, for review captures.
#[derive(Clone)]
struct State {
    sheet: MutableState<Option<SendDraft>>,
    answered: MutableState<Vec<String>>,
    filter: MutableState<HistoryFilter>,
}

impl State {
    fn new() -> State {
        State {
            sheet: mutable_state_of((std::env::var("EMBER_CR_FILES_SHEET").as_deref() == Ok("send")).then(SendDraft::default)),
            answered: mutable_state_of(Vec::new()),
            filter: mutable_state_of(HistoryFilter::All),
        }
    }
}

/// The screen with the service's data, or the sample with `EMBER_CR_FILES_SAMPLE=1`.
pub fn FilesScreen() {
    let ui = shared_ui().clone();
    // The service has no change signal yet: redraw with the launcher state.
    let _ = ui.live.launcher.get();
    let svc = transfer_service();
    if std::env::var("EMBER_CR_FILES_SAMPLE").as_deref() == Ok("1") {
        let (model, offers) = sample_model();
        let members = services().client.read(|s| crew(s, 4));
        return FilesScreenWith(&model, offers, members);
    }
    let (computers, members): (Vec<Computer>, Vec<CrewMember>) =
        services().client.read(|s| (s.computers().to_vec(), crew(s, 4)));
    FilesScreenWith(&build_model(&computers, svc), svc.pending_offers(), members);
}

pub fn FilesScreenWith(model: &FilesModel, pending: Vec<Offer>, members: Vec<CrewMember>) {
    let ui = shared_ui().clone();
    let st = remember(State::new);
    let window = compose_rust::runtime::current_window_size();
    let w = window.width_dp;
    let compact = w <= 560.0;
    let pad = page_pad(w);
    let cells = kpi_cells(model);
    let offers = visible_offers(&pending, &st.answered.get());
    let ready = model.ready;
    let skip = Skip::from_env(EST_HEAD + EST_DEVICES + EST_AGENTS + EST_TRAY + EST_TRANSFERS + EST_HISTORY, window.height_dp);

    cu::Box().modifier(Modifier::new().fill_max_width().fill_max_height()).content(|| {
        cu::ScrollColumn().modifier(Modifier::new().fill_max_width().fill_max_height()).content(|| {
            cu::Column().spacing(SP[4]).modifier(Modifier::new().fill_max_width().padding(pad)).content(|| {
                if skip.show(EST_HEAD) {
                    // On a phone the title sits below the menu button's row.
                    if compact {
                        cu::Spacer().modifier(Modifier::new().height(PHONE_MENU_ROW));
                    }
                    Header("파일", "연결된 모든 장비 사이에서 파일을 주고받습니다. 휴대폰과 태블릿도 포함됩니다.", compact, || {
                        ThemeButton(&ui);
                        let s = st.sheet.clone();
                        let open = move || s.set(Some(SendDraft::default()));
                        if compact {
                            // A round ink button, icon only (`.lbl{display:none}`).
                            let p = ember_theme().palette;
                            cu::Button("")
                                .icon(IconRole::Send)
                                .variant(ButtonVariant::Text)
                                .color(paint(p.ink_text))
                                .modifier(Modifier::new().size(H_BTN, H_BTN).background(paint(p.ink)).shape_role(ShapeRole::Full))
                                .on_click(open);
                        } else {
                            AppButton("보내기", Some(IconRole::Send), ButtonKind::Primary, false, true, false, open);
                        }
                    });
                    Cols(
                        if w > 1279.0 { 4 } else { 2 },
                        SP[4],
                        cells.iter().map(|c| -> Part { Box::new(move || KpiView(c)) }).collect(),
                    );
                }
                Stage(model, &members, &st, &ui, w, &skip);
                if skip.show(EST_TRANSFERS) {
                    let transfers = || TransfersCard(model, ready);
                    let received = || ReceivedCard(model, ready);
                    if side_by_side(w) {
                        cu::Row().spacing(SP[4]).modifier(Modifier::new().fill_max_width()).content(|| {
                            cu::Column().modifier(Modifier::new().weight(1.1)).content(transfers);
                            cu::Column().modifier(Modifier::new().weight(1.0)).content(received);
                        });
                    } else {
                        transfers();
                        received();
                    }
                }
                if skip.show(EST_HISTORY) {
                    HistoryCard(model, &st, ready, w);
                }
            });
        });
        // The incoming offer floats over the page, at the top right (above the tab bar on a phone).
        if let Some(offer) = offers.first() {
            OfferCard(offer, &pending, &model.devices, &st, w);
        }
        SendSheet(&st, &model.devices, &members, ready, w);
    });
}

/// The page padding: 16 on a phone, 24 up to 1279, 32 above.
pub fn page_pad(width_dp: f32) -> f32 {
    if width_dp <= 560.0 {
        SP[3]
    } else if width_dp <= 1279.0 {
        SP[5]
    } else {
        SP[6]
    }
}

/// The room of the shell's menu button row at the top of a phone.
const PHONE_MENU_ROW: f32 = 44.0;

/// `EMBER_CR_SCROLL=<dp|bottom|agents|tray|transfers|history>` for review captures: the toolkit
/// cannot scroll a column from code (TODO(compose-rust#385)), so the blocks that would have
/// scrolled out, by estimated height, are left out.
struct Skip(std::cell::Cell<f32>);

const EST_HEAD: f32 = 230.0;
const EST_DEVICES: f32 = 470.0;
const EST_AGENTS: f32 = 250.0;
const EST_TRAY: f32 = 130.0;
const EST_TRANSFERS: f32 = 330.0;
const EST_HISTORY: f32 = 400.0;

impl Skip {
    fn from_env(total_est: f32, window_h: f32) -> Skip {
        let v = match std::env::var("EMBER_CR_SCROLL").ok().as_deref() {
            Some("bottom") => (total_est - window_h).max(0.0),
            Some("agents") => EST_HEAD + EST_DEVICES,
            Some("tray") => EST_HEAD + EST_DEVICES + EST_AGENTS,
            Some("transfers") => EST_HEAD + EST_DEVICES + EST_AGENTS + EST_TRAY,
            Some("history") => EST_HEAD + EST_DEVICES + EST_AGENTS + EST_TRAY + EST_TRANSFERS,
            Some(n) => n.parse::<f32>().unwrap_or(0.0),
            None => 0.0,
        };
        Skip(std::cell::Cell::new(v))
    }

    /// Whether a block of about `est` dp is still in view.
    fn show(&self, est: f32) -> bool {
        let left = self.0.get();
        if left >= est {
            self.0.set(left - est);
            false
        } else {
            self.0.set(0.0);
            true
        }
    }
}

/// One KPI of the strip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KpiCell {
    pub label: &'static str,
    pub value: String,
    pub unit: &'static str,
    /// A pill beside the value (`전체 7`, `멈춤 1`).
    pub pill: Option<(String, PillTone)>,
    /// A meter under the value, in percent (the direct connection ratio).
    pub meter: Option<u32>,
    pub available: bool,
}

/// The four KPIs as the mockup draws them. What the service cannot say is "준비 중입니다".
pub fn kpi_cells(m: &FilesModel) -> [KpiCell; 4] {
    let base = kpis(m);
    let unavailable = |label| KpiCell { label, value: UNAVAILABLE.to_string(), unit: "", pill: None, meter: None, available: false };
    let running = m.transfers.iter().filter(|t| matches!(t.state, TransferState::Sending | TransferState::Queued)).count();
    let stopped = m.transfers.iter().filter(|t| matches!(t.state, TransferState::Paused | TransferState::Failed)).count();
    let active = if m.ready {
        KpiCell {
            label: "지금 전송 중",
            value: running.to_string(),
            unit: "",
            pill: Some(if stopped > 0 { (format!("멈춤 {stopped}"), PillTone::Blue) } else { ("모두 진행".to_string(), PillTone::Grey) }),
            meter: None,
            available: true,
        }
    } else {
        unavailable("지금 전송 중")
    };
    let direct = match base[3].value.strip_suffix('%').and_then(|v| v.parse::<u32>().ok()).filter(|_| base[3].available) {
        Some(p) => KpiCell { label: "직접 연결 비율", value: p.to_string(), unit: "%", pill: None, meter: Some(p), available: true },
        None => unavailable("직접 연결 비율"),
    };
    [
        KpiCell {
            label: "연결된 장비",
            value: connected_count(&m.devices).to_string(),
            unit: "",
            pill: Some((format!("전체 {}", m.devices.len()), PillTone::Green)),
            meter: None,
            available: true,
        },
        active,
        unavailable("이번 주 보낸 용량"),
        direct,
    ]
}

/// The row weights of a tile grid: equal, except that a last row of three tiles in four columns
/// gives its last tile two, one of two tiles gives its last three, and an odd last tile of two
/// columns takes both (`.fl-devgrid>.fl-dev:last-child:nth-child(...)`).
pub fn grid_weights(n: usize, cols: usize) -> Vec<Vec<f32>> {
    let cols = cols.max(1);
    let mut rows: Vec<Vec<f32>> = Vec::new();
    let mut left = n;
    while left > 0 {
        let k = left.min(cols);
        let mut row = vec![1.0; k];
        if k < cols {
            let extra = (cols - k) as f32;
            if let Some(last) = row.last_mut() {
                *last += extra;
            }
        }
        rows.push(row);
        left -= k;
    }
    rows
}

/// A grid of tiles by [`grid_weights`].
fn TileGrid<'a>(cols: usize, gap: f32, tiles: Vec<Part<'a>>) {
    let weights = grid_weights(tiles.len(), cols);
    cu::Column().spacing(gap).modifier(Modifier::new().fill_max_width()).content(|| {
        let mut it = tiles.into_iter();
        for row in weights {
            cu::Row().spacing(gap).modifier(Modifier::new().fill_max_width()).content(|| {
                for wt in row {
                    if let Some(tile) = it.next() {
                        cu::Column().modifier(Modifier::new().weight(wt)).content(tile);
                    }
                }
            });
        }
    });
}

/// A KPI: the label, the number with its small unit and pill, and the green meter.
fn KpiView(c: &KpiCell) {
    let p = ember_theme().palette;
    cu::Column().spacing(SP[0]).modifier(Modifier::new().fill_max_width()).content(|| {
        Label(c.label, Fs::Md, Weight::Regular, Some(paint(p.muted)), Some(1));
        if c.available {
            cu::Row().spacing(SP[1]).alignment(Alignment::CenterStart).content(|| {
                cu::Row().alignment(Alignment::CenterStart).content(|| {
                    Label(&c.value, Fs::Xl2, Weight::Regular, None, Some(1));
                    if !c.unit.is_empty() {
                        Label(c.unit, Fs::Md, Weight::Medium, Some(paint(p.muted)), Some(1));
                    }
                });
                if let Some((text, tone)) = &c.pill {
                    Pill(text, *tone, true);
                }
            });
            if let Some(pct) = c.meter {
                // `.fl-meter`: a 6 px track with the green fill.
                cu::Row().modifier(Modifier::new().fill_max_width().height(6.0).background(paint(p.track)).shape_role(ShapeRole::Full)).content(|| {
                    cu::Spacer().modifier(Modifier::new().weight((pct as f32).max(0.5)).height(6.0).background(paint(p.green.strong)).shape_role(ShapeRole::Full));
                    cu::Spacer().modifier(Modifier::new().weight((100 - pct.min(100)) as f32 + 0.001));
                });
            }
        } else {
            Label(&c.value, Fs::Md, Weight::Regular, Some(paint(p.muted)), Some(1));
        }
    });
}

/// 장비: my devices, the agents and the tray, in one card (`.fl-stage`).
fn Stage(model: &FilesModel, members: &[CrewMember], st: &State, ui: &SharedUi, w: f32, skip: &Skip) {
    let p = ember_theme().palette;
    let ready = model.ready;
    let (devices, agents, tray) = (skip.show(EST_DEVICES), skip.show(EST_AGENTS), skip.show(EST_TRAY));
    if !(devices || agents || tray) {
        return;
    }
    super::components::EmberCard(false, true, None, None, None, || {
        cu::Column().spacing(SP[3]).modifier(Modifier::new().fill_max_width()).content(|| {
            let cols = device_cols(w);
            if devices {
                super::home::CardHead(ICON_DESKTOP, "장비", || {});
                if model.devices.is_empty() {
                    Label(empty_text(crate::ui::files::Section::Devices, ready), Fs::Md, Weight::Regular, Some(paint(p.muted)), None);
                }
                TileGrid(
                    cols,
                    SP[2],
                    model
                        .devices
                        .iter()
                        .map(|d| -> Part {
                            let s = st.sheet.clone();
                            Box::new(move || {
                                DeviceTile(d, move |id| {
                                    s.set(Some(SendDraft { target: Some(SendTarget::Device(id)), files: Vec::new() }))
                                })
                            })
                        })
                        .collect(),
                );
            }
            if agents {
                // The title and its hint share a line, the hint wrapping below on a phone.
                cu::Row().spacing(SP[2]).alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width()).content(|| {
                    Label("에이전트에게 보내기", Fs::Lg, Weight::Semibold, None, Some(1));
                    if w > 900.0 {
                        Label("팀원이 일하는 컴퓨터의 작업 폴더로 보내고 그 대화에 첨부합니다", Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                    }
                });
                if w <= 900.0 {
                    Label("팀원이 일하는 컴퓨터의 작업 폴더로 보내고 그 대화에 첨부합니다", Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(2));
                }
                if members.is_empty() {
                    Label(empty_text(crate::ui::files::Section::Agents, ready), Fs::Md, Weight::Regular, Some(paint(p.muted)), None);
                }
                TileGrid(
                    cols,
                    SP[2],
                    members
                        .iter()
                        .map(|m| -> Part {
                            let s = st.sheet.clone();
                            Box::new(move || {
                                AgentTile(m, ui, move |id| {
                                    s.set(Some(SendDraft { target: Some(SendTarget::Agent(id)), files: Vec::new() }))
                                })
                            })
                        })
                        .collect(),
                );
            }
            if tray {
                // `.fl-tray`: the files to send. TODO(#166, compose-rust): no file picker or drag
                // and drop yet, so the tray is empty and its button says so.
                cu::Spacer().modifier(Modifier::new().fill_max_width().height(1.0).background(paint(p.line)));
                cu::Row().spacing(SP[2]).alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width()).content(|| {
                    Label("보낼 파일", Fs::Md, Weight::Semibold, None, Some(1));
                    Label(if w > 560.0 { "장비를 누르면 받는 곳을 고르고 파일을 보냅니다." } else { "장비를 누르면 보낼 파일을 고릅니다." }, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(2));
                });
                AppButton("파일 고르기", Some(IconRole::Add), ButtonKind::Default, true, true, false, || notice::post(FILE_PICK_UNAVAILABLE));
            }
        });
    });
}

/// "근처", "직접 연결" or "릴레이".
pub fn path_name(p: PathKind) -> &'static str {
    match p {
        PathKind::Nearby => "근처",
        PathKind::Direct => "직접 연결",
        PathKind::Relay => "릴레이",
    }
}

/// `.fl-dev`: a device card: the kind glyph with its presence dot, the name, the kind, and the
/// path pill with the latency beside it. The whole tile sends to the device (it opens the send
/// sheet), as in the mockup. TODO(compose-rust#372): a pointer-up `Clickable`.
fn DeviceTile(d: &Device, on_send: impl FnMut(String) + 'static) {
    let p = ember_theme().palette;
    let id = d.id.clone();
    let mut on_send = on_send;
    let m = Modifier::new().fill_max_width().background(paint(p.surface)).border(1.0, paint(p.line)).corner_radius(CORNER_LG).padding(SP[3]);
    let offline = matches!(d.presence, Presence::Offline | Presence::Unknown);
    Clickable(if offline { m.alpha(0.62) } else { m }, Alignment::TopStart, move || on_send(id.clone()), || {
        cu::Column().spacing(2.0).modifier(Modifier::new().fill_max_width()).content(|| {
            cu::Box().modifier(Modifier::new().size(48.0, 48.0)).content(|| {
                cu::Box()
                    .alignment(Alignment::Center)
                    .modifier(Modifier::new().size(48.0, 48.0).background(paint(p.surface_2)).border(1.0, paint(p.line)).shape_role(ShapeRole::Full))
                    .content(|| NavIcon(kind_icon(d.kind), p.text.to_argb(), 24.0));
                cu::Box().alignment(Alignment::BottomEnd).modifier(Modifier::new().size(48.0, 48.0)).content(|| {
                    cu::Spacer().modifier(
                        Modifier::new().size(12.0, 12.0).background(paint(presence_color(d.presence, p))).border(2.0, paint(p.surface)).shape_role(ShapeRole::Full),
                    );
                });
            });
            cu::Spacer().modifier(Modifier::new().height(SP[0]));
            Label(&fit_label(&d.name, 150.0, Fs::Md.size()), Fs::Md, Weight::Semibold, None, Some(1));
            Label(d.kind.label(), Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
            cu::Spacer().modifier(Modifier::new().height(SP[0]));
            // The foot: the path pill and the latency as plain text; sleeping, offline and
            // unknown devices show their state.
            cu::Row().spacing(SP[0]).alignment(Alignment::CenterStart).content(|| match (d.presence, d.path) {
                (Presence::Online, Some(path)) => {
                    Pill(path_name(path), path_tone(path), true);
                    if let Some(ms) = d.latency_ms {
                        Label(&format!("{ms} ms"), Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                    }
                }
                (presence, _) => Pill(presence.label(), presence_tone(presence), true),
            });
        });
    });
}

/// An agent's tile: its character, name, computer and task, and its status. The tile sends to the
/// computer the agent works on. TODO(compose-rust#372): a pointer-up `Clickable`.
fn AgentTile(m: &CrewMember, ui: &SharedUi, on_send: impl FnMut(String) + 'static) {
    let p = ember_theme().palette;
    let mut on_send = on_send;
    let send_id = m.id.clone();
    let _ = ui;
    let name = crate::ui::project::member_name(None, &m.project, &m.agent, &m.title);
    let state = if m.waiting { CharState::Wait } else { CharState::Run };
    Clickable(
        Modifier::new().fill_max_width().background(paint(p.surface)).border(1.0, paint(p.line)).corner_radius(CORNER_LG).padding(SP[3]),
        Alignment::TopStart,
        move || on_send(send_id.clone()),
        || {
            cu::Column().spacing(2.0).modifier(Modifier::new().fill_max_width()).content(|| {
                Character(&m.agent, false, 34.0, &name, &m.project, state, None);
                Label(&fit_label(&name, 150.0, Fs::Md.size()), Fs::Md, Weight::Semibold, None, Some(1));
                // TODO(server): which computer a session runs on is not in the client's state.
                Label(&format!("컴퓨터 정보는 {UNAVAILABLE}"), Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(2));
                Label(&fit_label(&m.title, 150.0, Fs::Xs.size()), Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                cu::Spacer().modifier(Modifier::new().height(SP[0]));
                if m.waiting {
                    Pill("승인 대기", PillTone::Orange, true);
                } else {
                    Pill("실행 중", PillTone::Blue, true);
                }
            });
        },
    );
}

/// Where received files land. TODO(FR-F8): the real inbox comes from the transfer settings.
pub const DEFAULT_INBOX: &str = "~/Downloads/Ember";

/// The incoming offer (FR-F3) as the mockup's floating card (`.fl-offer`): the sender's icon and
/// name, the files, where they will be saved, the encryption note, and equal 거절 and 받기.
fn OfferCard(offer: &Offer, pending: &[Offer], devices: &[Device], st: &State, w: f32) {
    let p = ember_theme().palette;
    let compact = w <= 560.0;
    let sender = devices.iter().find(|d| d.name == offer.from);
    let card_w = (w - 2.0 * SP[3]).min(360.0);
    let route = sender.and_then(|d| d.path).map(path_name);
    let card = || {
        cu::Box()
            .modifier(Modifier::new().width(card_w).background(paint(p.surface)).border(1.0, paint(p.line)).corner_radius(CORNER_LG).padding(SP[3]))
            .content(|| {
                cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(|| {
                    cu::Row().spacing(SP[2]).alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width()).content(|| {
                        cu::Box()
                            .alignment(Alignment::Center)
                            .modifier(Modifier::new().size(40.0, 40.0).background(paint(p.surface_2)).border(1.0, paint(p.line)).shape_role(ShapeRole::Full))
                            .content(|| NavIcon(kind_icon(sender.map(|d| d.kind).unwrap_or(DeviceKind::Computer)), p.text.to_argb(), 20.0));
                        cu::Column().modifier(Modifier::new().weight(1.0)).content(|| {
                            Label(&fit_label(&offer.from, card_w - 100.0, Fs::Md.size()), Fs::Md, Weight::Semibold, None, Some(1));
                            Label(&format!("파일 {}개, {}를 보내려고 합니다", offer.files.len(), format_bytes(offer.total)), Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(2));
                        });
                    });
                    cu::Row().spacing(SP[1]).alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width()).content(|| {
                        for _ in offer.files.iter().take(3) {
                            FileChip(ICON_DOC, PillTone::Blue);
                        }
                        Label(&fit_label(&offer.files.join(", "), card_w - 150.0, Fs::Xs.size()), Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                    });
                    Label(&format!("저장 위치 {DEFAULT_INBOX}"), Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                    cu::Row().spacing(4.0).alignment(Alignment::CenterStart).content(|| {
                        NavIcon(ICON_LOCK, p.muted.to_argb(), 12.0);
                        let text = match route {
                            Some(r) => format!("종단 간 암호화 \u{b7} {r}"),
                            None => "종단 간 암호화".to_string(),
                        };
                        Label(&text, Fs::Xs2, Weight::Regular, Some(paint(p.muted)), Some(1));
                    });
                    cu::Row().spacing(SP[1]).modifier(Modifier::new().fill_max_width()).content(|| {
                        for (text, kind, answer) in [("거절", ButtonKind::Default, Answer::Decline), ("받기", ButtonKind::Primary, Answer::Accept)] {
                            let (id, pending, answered) = (offer.id.clone(), pending.to_vec(), st.answered.clone());
                            cu::Column().modifier(Modifier::new().weight(1.0)).content(|| {
                                AppButton(text, None, kind, false, true, true, move || {
                                    let mut list = answered.get_untracked();
                                    answer_offer(transfer_service(), &pending, &mut list, &id, answer);
                                    answered.set(list);
                                });
                            });
                        }
                    });
                });
            });
    };
    if compact {
        cu::Box().alignment(Alignment::BottomCenter).modifier(Modifier::new().fill_max_width().fill_max_height()).content(|| {
            cu::Column().modifier(Modifier::new().padding(SP[3])).content(|| {
                card();
                // Above the floating tab bar.
                cu::Spacer().modifier(Modifier::new().height(TABBAR_H + TABBAR_BOTTOM_GAP));
            });
        });
    } else {
        cu::Box().alignment(Alignment::TopEnd).modifier(Modifier::new().fill_max_width().fill_max_height().padding(SP[5])).content(card);
    }
}

// --- transfers, received, history -------------------------------------------------------------

/// A 32 px file-type chip (`.fl-ft`).
fn FileChip(icon: &'static str, tone: PillTone) {
    let p = ember_theme().palette;
    let (fg, bg) = super::components::pill_colors(tone, p);
    cu::Box()
        .alignment(Alignment::Center)
        .modifier(Modifier::new().size(32.0, 32.0).background(paint(bg)).corner_radius(CORNER_SM))
        .content(|| NavIcon(icon, fg.to_argb(), 16.0));
}

fn TransfersCard(model: &FilesModel, ready: bool) {
    let p = ember_theme().palette;
    EmberCard(false, true, None, None, None, || {
        cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(|| {
            super::home::CardHead(ICON_SWAP, "전송 중", || {
                Pill(&model.transfers.len().to_string(), PillTone::Grey, false);
            });
            if model.transfers.is_empty() {
                Label(empty_text(crate::ui::files::Section::Transfers, ready), Fs::Md, Weight::Regular, Some(paint(p.muted)), None);
            }
            for (i, t) in model.transfers.iter().enumerate() {
                if i > 0 {
                    cu::Spacer().modifier(Modifier::new().fill_max_width().height(1.0).background(paint(p.line)));
                }
                TransferRow(t);
            }
            cu::Row().spacing(SP[0]).alignment(Alignment::CenterStart).content(|| {
                NavIcon(ICON_LOCK, p.muted.to_argb(), 12.0);
                Label(LOCK_LINE, Fs::Xs2, Weight::Regular, Some(paint(p.muted)), Some(2));
            });
        });
    });
}

fn TransferRow(t: &Transfer) {
    let p = ember_theme().palette;
    let svc = transfer_service();
    let dir = match t.direction {
        Direction::Send => "보내는 중",
        Direction::Receive => "받는 중",
    };
    cu::Column().spacing(SP[1]).modifier(Modifier::new().fill_max_width()).content(|| {
        cu::Row().spacing(SP[2]).alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width()).content(|| {
            FileChip(ICON_DOC, PillTone::Grey);
            cu::Column().modifier(Modifier::new().weight(1.0)).content(|| {
                Label(&t.name, Fs::Md, Weight::Medium, None, Some(1));
                cu::Row().spacing(4.0).alignment(Alignment::CenterStart).content(|| {
                    NavIcon(if t.direction == Direction::Send { ICON_UP } else { ICON_DOWN }, p.muted.to_argb(), 12.0);
                    Label(&format!("{dir} \u{b7} {}", t.device), Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                });
            });
            cu::Row().alignment(Alignment::CenterStart).content(|| {
                Label(&percent(t).to_string(), Fs::Xl2, Weight::Regular, None, Some(1));
                Label("%", Fs::Sm, Weight::Regular, Some(paint(p.muted)), Some(1));
            });
        });
        // TODO(compose-rust#377): the bar's stroke and track colours are the toolkit's.
        cu::LinearProgressIndicator(percent(t) as f32 / 100.0).modifier(Modifier::new().fill_max_width());
        Label(&format!("{} / {} \u{b7} {}", format_bytes(t.done), format_bytes(t.total), progress_line(t)), Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(2));
        cu::Row().spacing(SP[0]).content(|| {
            for action in user_actions(t.state).iter().copied() {
                let id = t.id.clone();
                AppButton(action.label(), None, if action == TransferAction::Cancel { ButtonKind::Destructive } else { ButtonKind::Default }, true, true, false, move || {
                    svc.control(&id, action)
                });
            }
        });
    });
}

fn ReceivedCard(model: &FilesModel, ready: bool) {
    let p = ember_theme().palette;
    EmberCard(false, true, None, None, None, || {
        cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(|| {
            super::home::CardHead(ICON_DOWN, "받은 파일", || {});
            if model.received.is_empty() {
                Label(empty_text(crate::ui::files::Section::Received, ready), Fs::Md, Weight::Regular, Some(paint(p.muted)), None);
            }
            for f in model.received.iter() {
                cu::Row().spacing(SP[2]).alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width()).content(|| {
                    FileChip(ICON_DOC, PillTone::Blue);
                    cu::Column().modifier(Modifier::new().weight(1.0)).content(|| {
                        Label(&fit_label(&f.name, 260.0, Fs::Sm.size()), Fs::Sm, Weight::Medium, None, Some(1));
                        Label(&format!("{} \u{b7} {}", format_bytes(f.size), f.from), Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                    });
                });
            }
        });
    });
}

fn HistoryCard(model: &FilesModel, st: &State, ready: bool, w: f32) {
    let p = ember_theme().palette;
    let filter = st.filter.get();
    let rows: Vec<&HistoryEntry> = model.history.iter().filter(|h| filter.matches(h)).collect();
    EmberCard(false, true, None, None, None, || {
        cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width()).content(|| {
            super::home::CardHead(ICON_CLOCK, "전송 기록", || {});
            cu::ScrollRow().modifier(Modifier::new().fill_max_width().height(H_CTL + SP[0])).content(|| {
                for f in HistoryFilter::ALL {
                    let s = st.filter.clone();
                    FilterChip(f.label(), f == filter, move || s.set(f));
                    cu::Spacer().modifier(Modifier::new().width(SP[0]));
                }
            });
            if rows.is_empty() {
                let text = if model.history.is_empty() { empty_text(crate::ui::files::Section::History, ready) } else { "조건에 맞는 전송 기록이 없습니다" };
                Label(text, Fs::Md, Weight::Regular, Some(paint(p.muted)), None);
            }
            Cols(
                if w > 900.0 { 2 } else { 1 },
                SP[4],
                rows.into_iter().map(|h| -> Part { Box::new(move || HistoryRow(h)) }).collect(),
            );
        });
    });
}

fn HistoryRow(h: &HistoryEntry) {
    let p = ember_theme().palette;
    let failed = h.result != HistoryResult::Done;
    let tone = if failed { PillTone::Red } else { PillTone::Grey };
    let (label, pill) = match (h.result, h.direction) {
        (HistoryResult::Done, Direction::Send) => ("보냄", PillTone::Green),
        (HistoryResult::Done, Direction::Receive) => ("받음", PillTone::Blue),
        (HistoryResult::Declined, _) | (HistoryResult::Cancelled, _) => (h.result.label(), PillTone::Orange),
        (HistoryResult::Failed, _) => (h.result.label(), PillTone::Red),
    };
    let who = match h.direction {
        Direction::Send => "받는 곳",
        Direction::Receive => "보낸 곳",
    };
    cu::Row().spacing(SP[2]).alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width()).content(|| {
        FileChip(if h.direction == Direction::Send { ICON_UP } else { ICON_DOWN }, tone);
        cu::Column().modifier(Modifier::new().weight(1.0)).content(|| {
            Label(&fit_label(&h.name, 220.0, Fs::Sm.size()), Fs::Sm, Weight::Medium, None, Some(1));
            Label(&format!("{who} {}", h.device), Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
            Label(&h.when, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
        });
        cu::Column().alignment(Alignment::CenterEnd).spacing(2.0).content(|| {
            Label(&format_bytes(h.size), Fs::Md, Weight::Regular, None, Some(1));
            Pill(label, pill, true);
        });
    });
}

// --- the send sheet ---------------------------------------------------------------------------

/// A receiver chip of the send sheet (`.fl-tg`): an outlined pill with the device glyph or the
/// agent's character; the selected one takes the `ink` border, an offline one is dimmed.
/// TODO(compose-rust#372): a pointer-up `Clickable`.
fn TargetChip(label: &str, icon: TargetIcon, selected: bool, dim: bool, on_click: impl FnMut() + 'static) {
    let p = ember_theme().palette;
    let fill = if selected { p.sel } else { p.surface };
    let border = if selected { p.ink } else { p.line };
    let m = Modifier::new().height(H_CTL).background(paint(fill)).border(1.0, paint(border)).shape_role(ShapeRole::Full);
    Clickable(if dim { m.alpha(0.55) } else { m }, Alignment::CenterStart, on_click, || {
        cu::Row().alignment(Alignment::CenterStart).content(|| {
            cu::Spacer().modifier(Modifier::new().width(12.0));
            match &icon {
                TargetIcon::Kind(k) => NavIcon(kind_icon(*k), p.text_2.to_argb(), 16.0),
                TargetIcon::Agent(m) => {
                    let name = crate::ui::project::member_name(None, &m.project, &m.agent, &m.title);
                    Character(&m.agent, false, 22.0, &name, &m.project, if m.waiting { CharState::Wait } else { CharState::Run }, None);
                }
            }
            cu::Spacer().modifier(Modifier::new().width(SP[1]));
            Label(label, Fs::Sm, Weight::Regular, Some(paint(if selected { p.text } else { p.text_2 })), Some(1));
            cu::Spacer().modifier(Modifier::new().width(12.0));
        });
    });
}

enum TargetIcon {
    Kind(DeviceKind),
    Agent(CrewMember),
}

/// A chip's width: 12 padding, the 16 px glyph (22 for a character), 8, the text, 12 padding.
pub fn target_chip_width(label: &str, glyph: f32) -> f32 {
    12.0 + glyph + 8.0 + text_px(label, Fs::Sm.size()) + 12.0
}

/// The send sheet: pick the receiver (every device and every agent), then the files (FR-L27.5).
fn SendSheet(st: &State, devices: &[Device], members: &[CrewMember], ready: bool, w: f32) {
    let p = ember_theme().palette;
    let draft = st.sheet.get();
    let open = draft.is_some();
    let d = draft.unwrap_or_default();
    let mut blocker = send_blocker(&d, FILE_PICKER_AVAILABLE, ready);
    let target_device = match &d.target {
        Some(SendTarget::Device(id)) => Some(id.clone()),
        _ => None,
    };
    let offline_target = target_device
        .as_deref()
        .and_then(|id| devices.iter().find(|x| x.id == id))
        .is_some_and(|x| x.presence != Presence::Online);
    if blocker.is_none() && offline_target {
        blocker = Some("이 장비는 지금 연결되어 있지 않습니다");
    }
    let can_send = blocker.is_none() && target_device.is_some();
    let sheet_w = (w - 32.0).clamp(280.0, 620.0);
    let inner = sheet_w - 2.0 * SP[4];
    let close = {
        let s = st.sheet.clone();
        move || s.set(None)
    };
    // The receivers flow left to right and wrap: devices first, then agents.
    let mut chips: Vec<(String, TargetIcon, bool, bool, SendTarget)> = Vec::new();
    for dev in devices {
        let sel = target_device.as_deref() == Some(dev.id.as_str());
        chips.push((dev.name.clone(), TargetIcon::Kind(dev.kind), sel, dev.presence != Presence::Online, SendTarget::Device(dev.id.clone())));
    }
    for m in members {
        let name = crate::ui::project::member_name(None, &m.project, &m.agent, &m.title);
        let sel = matches!(&d.target, Some(SendTarget::Agent(id)) if *id == m.id);
        chips.push((name, TargetIcon::Agent(m.clone()), sel, false, SendTarget::Agent(m.id.clone())));
    }
    let widths: Vec<f32> = chips
        .iter()
        .map(|(l, icon, ..)| target_chip_width(&fit_label(l, 160.0, Fs::Sm.size()), if matches!(icon, TargetIcon::Agent(_)) { 22.0 } else { 16.0 }))
        .collect();
    let lines = super::project::wrap_lines(&widths, SP[1], inner);
    EmberSheetSized("파일 보내기", open, sheet_w, close.clone(), None, || {
        cu::Column().spacing(SP[2]).modifier(Modifier::new().fill_max_width().padding(SP[4])).content(|| {
            Label("받는 곳", Fs::Sm, Weight::Regular, Some(paint(p.muted)), None);
            if chips.is_empty() {
                Label("보낼 수 있는 장비가 없습니다", Fs::Sm, Weight::Regular, Some(paint(p.muted)), None);
            }
            for line in lines.iter() {
                cu::Row().spacing(SP[1]).modifier(Modifier::new().fill_max_width()).content(|| {
                    for &i in line {
                        let (label, icon, sel, dim, target) = &chips[i];
                        let (s, base) = (st.sheet.clone(), d.clone());
                        let target = target.clone();
                        let shown = fit_label(label, 160.0, Fs::Sm.size());
                        let icon = match icon {
                            TargetIcon::Kind(k) => TargetIcon::Kind(*k),
                            TargetIcon::Agent(m) => TargetIcon::Agent(m.clone()),
                        };
                        TargetChip(&shown, icon, *sel, *dim, move || {
                            let mut next = base.clone();
                            next.target = Some(target.clone());
                            s.set(Some(next));
                        });
                    }
                });
            }
            Label("파일", Fs::Sm, Weight::Regular, Some(paint(p.muted)), None);
            // TODO(compose-rust): a file picker and drag and drop; until then the files stay empty.
            Label(&format!("{}개 선택됨", d.files.len()), Fs::Sm, Weight::Regular, None, None);
            if let Some(text) = blocker {
                Label(text, Fs::Sm, Weight::Regular, Some(paint(p.muted)), Some(2));
            }
        });
        // The footer: the encryption note at the left, the send button at the right.
        cu::Row().spacing(SP[2]).alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width().padding(SP[4])).content(|| {
            NavIcon(ICON_LOCK, p.muted.to_argb(), 12.0);
            Label("종단 간 암호화", Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
            cu::Spacer().modifier(Modifier::new().weight(1.0));
            let (files, s) = (d.files.clone(), st.sheet.clone());
            let target = target_device.clone();
            AppButton("보내기", Some(IconRole::Send), ButtonKind::Primary, true, can_send, false, move || {
                if let Some(id) = target.as_deref() {
                    if transfer_service().send(id, files.clone()).is_ok() {
                        s.set(None);
                    }
                }
            });
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use compose_rust::protocol::{decode_batch, HostEvent, Mutation, PropertyValue};
    use compose_rust::{Host, Recomposer};

    #[test]
    fn fr_l27_the_device_grid_and_columns_follow_the_mockup_breakpoints() {
        assert_eq!((device_cols(1440.0), device_cols(768.0), device_cols(390.0)), (4, 4, 2));
        assert_eq!((page_pad(1440.0), page_pad(1024.0), page_pad(768.0), page_pad(390.0)), (32.0, 24.0, 24.0, 16.0));
        // Seven tiles in four columns: the last tile of the second row takes two columns.
        assert_eq!(grid_weights(7, 4), vec![vec![1.0; 4], vec![1.0, 1.0, 2.0]]);
        assert_eq!(grid_weights(6, 4), vec![vec![1.0; 4], vec![1.0, 3.0]]);
        assert_eq!(grid_weights(5, 2), vec![vec![1.0, 1.0], vec![1.0, 1.0], vec![2.0]]);
        assert_eq!(grid_weights(4, 4), vec![vec![1.0; 4]]);
        assert!(grid_weights(0, 4).is_empty());
        assert!(side_by_side(1440.0) && !side_by_side(1024.0));
    }

    #[test]
    fn fr_l27_the_history_filter_separates_sent_received_and_problems() {
        let (m, _) = sample_model();
        let n = |f: HistoryFilter| m.history.iter().filter(|h| f.matches(h)).count();
        assert_eq!((n(HistoryFilter::All), n(HistoryFilter::Sent), n(HistoryFilter::Received), n(HistoryFilter::Problems)), (3, 1, 0, 2));
    }

    #[test]
    fn fr_l27_the_kpis_carry_pills_units_and_the_meter() {
        let (m, _) = sample_model();
        let k = kpi_cells(&m);
        assert_eq!((k[0].value.as_str(), k[0].pill.clone()), ("4", Some(("전체 7".to_string(), PillTone::Green))));
        assert_eq!(k[1].pill.as_ref().map(|p| p.0.as_str()), Some("멈춤 1"));
        assert!(!k[2].available);
        assert_eq!((k[3].unit, k[3].meter), ("%", Some(75)));
        // No service data: only the connected count is known.
        let none = FilesModel { ready: false, ..m };
        let k = kpi_cells(&none);
        assert!(k[0].available && !k[1].available && !k[3].available);
    }

    #[test]
    fn capture_scroll_skips_whole_blocks() {
        let s = Skip(std::cell::Cell::new(700.0));
        assert!(!s.show(230.0));
        assert!(!s.show(470.0));
        assert!(s.show(250.0));
    }

    #[test]
    fn fr_l27_a_target_chip_grows_with_its_label() {
        assert!(target_chip_width("macbook-pro", 16.0) > target_chip_width("gpu", 16.0));
        assert!(target_chip_width("gpu", 22.0) > target_chip_width("gpu", 16.0));
    }

    #[test]
    fn fr_l27_the_sample_fills_every_section() {
        let (m, offers) = sample_model();
        assert!(m.ready && !m.devices.is_empty() && !m.transfers.is_empty() && !m.received.is_empty());
        assert_eq!(offers.len(), 1);
        assert_eq!(connected_count(&m.devices), 4);
    }

    fn texts(batch: &[Mutation<'_>]) -> Vec<String> {
        batch
            .iter()
            .filter_map(|m| match m {
                Mutation::SetProp { value: PropertyValue::String(s), .. } => Some(s.to_string()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn fr_l27_the_files_screen_shows_the_sections_and_says_what_is_not_ready() {
        use compose_rust::{EventPayload, WindowHeightClass, WindowSizeClass};
        let _guard = crate::services::test_guard();
        compose_rust::window::reset_window_size();
        let mut host = Host::new(|| Box::new(Recomposer::new(FilesScreen)));
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
        let has = |want: &str| all.iter().any(|t| t.contains(want));
        for want in ["파일", "장비", "에이전트에게 보내기", "전송 중", "받은 파일", "전송 기록", UNAVAILABLE] {
            assert!(has(want), "missing {want}: {all:?}");
        }
        compose_rust::window::reset_window_size();
    }
}
