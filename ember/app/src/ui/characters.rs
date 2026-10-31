//! Team character styles (FR-L22, FR-L24; `docs/DESIGN.md` section 7, INTENT D29).
//!
//! A member is drawn as a small character in one of six styles chosen per project. The drawing is
//! an SVG string generated from (style, name, harness, state, leader, scheme) and registered as an
//! SVG image once per distinct string (`register_owned` compares content, so the cache key is the
//! string itself). Everything here except [`Character`] and [`StylePicker`] is a pure function,
//! tested without a renderer. The port follows the mockup's `chars.js`.
//!
//! #166 workarounds: the toolkit has no animation, so a state shows as a static pose (eyes, mouth
//! and a small mark), not as motion. TODO(animation): breathe, blink and wave when it lands.
//!
//! TODO(server): the project's style is kept per device in prefs until the server stores it.

use std::fmt::Write as _;

use dioxus_compose::prelude::*;
use ember_client::LauncherStatus;
use serde::{Deserialize, Serialize};

use crate::ui::components::{AppButton, ButtonKind, Label};
use crate::ui::project::is_default_project;
use crate::ui::theme::*;
use crate::ui::use_ui;

// ---------------------------------------------------------------------------------------------
// Vocabulary
// ---------------------------------------------------------------------------------------------

/// The six team character styles. Names are [provisional] (D29).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CharStyle {
    #[default]
    Mongle,
    Malang,
    Crew,
    Pocket,
    Cream,
    Keyring,
}

impl CharStyle {
    pub const ALL: [CharStyle; 6] = [
        CharStyle::Mongle,
        CharStyle::Malang,
        CharStyle::Crew,
        CharStyle::Pocket,
        CharStyle::Cream,
        CharStyle::Keyring,
    ];

    pub fn label(self) -> &'static str {
        match self {
            CharStyle::Mongle => "몽글",
            CharStyle::Malang => "말랑",
            CharStyle::Crew => "크루",
            CharStyle::Pocket => "포켓",
            CharStyle::Cream => "크림",
            CharStyle::Keyring => "키링",
        }
    }

    pub fn blurb(self) -> &'static str {
        match self {
            CharStyle::Mongle => "눈, 입, 볼이 있는 기본 캐릭터입니다",
            CharStyle::Malang => "이름으로 정해지는 말랑한 블롭입니다",
            CharStyle::Crew => "짧은 팔다리와 역할 소품이 있습니다",
            CharStyle::Pocket => "소품 하나를 단 단순한 마스코트입니다",
            CharStyle::Cream => "크림색 몸에 하네스 색 옷을 입습니다",
            CharStyle::Keyring => "고리에 매달린 작은 참입니다",
        }
    }
}

/// What a character is doing; sets the pose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CharState {
    Run,
    Wait,
    Fail,
    Done,
    #[default]
    Idle,
}

impl CharState {
    pub const ALL: [CharState; 5] = [
        CharState::Run,
        CharState::Wait,
        CharState::Fail,
        CharState::Done,
        CharState::Idle,
    ];

    pub fn from_status(s: LauncherStatus) -> CharState {
        match s {
            LauncherStatus::Running => CharState::Run,
            LauncherStatus::WaitingForApproval => CharState::Wait,
            LauncherStatus::Failed => CharState::Fail,
            LauncherStatus::Finished | LauncherStatus::FinishedUnread => CharState::Done,
            LauncherStatus::Idle | LauncherStatus::Unknown => CharState::Idle,
        }
    }
}

/// The harness colour family (DESIGN section 7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    Claude,
    Codex,
    Gemini,
}

pub fn family_of(agent: &str) -> Family {
    let a = agent.to_lowercase();
    if a.contains("codex") || a.contains("gpt") || a.contains("openai") {
        Family::Codex
    } else if a.contains("gemini") || a.contains("agy") || a.contains("antigravity") {
        Family::Gemini
    } else {
        Family::Claude
    }
}

fn family_key(f: Family) -> &'static str {
    match f {
        Family::Claude => "claude",
        Family::Codex => "codex",
        Family::Gemini => "gemini",
    }
}

/// Hue range in degrees: Claude orange, Codex blue, Gemini sky.
pub fn hue_range(f: Family) -> (f32, f32) {
    match f {
        Family::Claude => (20.0, 34.0),
        Family::Codex => (218.0, 238.0),
        Family::Gemini => (196.0, 212.0),
    }
}

/// Saturation and lightness bands (percent); Codex is deeper and Gemini paler so they stay apart.
fn sat_range(f: Family) -> (f32, f32) {
    match f {
        Family::Claude => (48.0, 62.0),
        Family::Codex => (30.0, 42.0),
        Family::Gemini => (40.0, 54.0),
    }
}

fn light_range(f: Family) -> (f32, f32) {
    match f {
        Family::Claude => (60.0, 72.0),
        Family::Codex => (54.0, 66.0),
        Family::Gemini => (72.0, 82.0),
    }
}

/// Everything the drawing depends on.
#[derive(Debug, Clone, PartialEq)]
pub struct CharSpec {
    pub style: CharStyle,
    /// The seed: the member's name.
    pub name: String,
    pub agent: String,
    pub state: CharState,
    pub lead: bool,
    /// The member's role name (크루 props).
    pub role: Option<String>,
    pub dark: bool,
}

/// The leader badge rule (FR-L24): a leader outside 기본 프로젝트 only.
pub fn crown_shown(project_id: &str, leader: bool) -> bool {
    leader && !is_default_project(project_id)
}

/// The crown group's marker in the SVG.
pub const CROWN_ID: &str = "id=\"crown\"";

// ---------------------------------------------------------------------------------------------
// Seed
// ---------------------------------------------------------------------------------------------

/// FNV-1a over the UTF-8 bytes.
pub fn seed_hash(s: &str) -> u32 {
    let mut h: u32 = 2166136261;
    for b in s.bytes() {
        h ^= u32::from(b);
        h = h.wrapping_mul(16777619);
    }
    h
}

/// mulberry32.
struct Rng(u32);

impl Rng {
    fn next(&mut self) -> f32 {
        self.0 = self.0.wrapping_add(0x6D2B79F5);
        let mut t = self.0;
        t = (t ^ (t >> 15)).wrapping_mul(t | 1);
        t ^= t.wrapping_add((t ^ (t >> 7)).wrapping_mul(t | 61));
        let v = f64::from(t ^ (t >> 14)) / 4294967296.0;
        (v as f32).min(0.9999)
    }
}

const SHAPES: usize = 8;

#[derive(Clone)]
struct Traits {
    shape: usize,
    w: f32,
    h: f32,
    wob: f32,
    rot: f32,
    r2: [f32; 3],
    eye_w: f32,
    eye_h: f32,
    eye_gap: f32,
    eye_y: f32,
    gaze: f32,
    eye_round: bool,
    j: [f32; 12],
    k: f32,
    k2: f32,
}

fn traits(name: &str, fam: Family) -> Traits {
    let mut r = Rng(seed_hash(&format!("{name}|{}", family_key(fam))));
    let shape = (r.next() * SHAPES as f32) as usize;
    let w = 0.86 + r.next() * 0.24;
    let h = 0.84 + r.next() * 0.22;
    let wob = 0.04 + r.next() * 0.07;
    let rot = r.next() * 6.28;
    let r2 = [r.next(), r.next(), r.next()];
    let eye_w = 4.2 + r.next() * 2.6;
    let eye_h = 8.5 + r.next() * 6.0;
    let eye_gap = 8.5 + r.next() * 5.0;
    let eye_y = -2.0 + r.next() * 6.0;
    let gaze = (r.next() - 0.5) * 3.0;
    let eye_round = r.next() > 0.35;
    let mut j = [0.0; 12];
    for v in j.iter_mut() {
        *v = r.next();
    }
    let k = r.next();
    let k2 = r.next();
    Traits {
        shape,
        w,
        h,
        wob,
        rot,
        r2,
        eye_w,
        eye_h,
        eye_gap,
        eye_y,
        gaze,
        eye_round,
        j,
        k,
        k2,
    }
}

/// The seeded body hue, inside the harness family's range.
pub fn body_hue(name: &str, agent: &str) -> f32 {
    let fam = family_of(agent);
    let t = traits(name, fam);
    let (lo, hi) = hue_range(fam);
    lo + t.r2[0] * (hi - lo)
}

// ---------------------------------------------------------------------------------------------
// Colour
// ---------------------------------------------------------------------------------------------

fn hsl_hex(h: f32, s: f32, l: f32) -> String {
    let (s, l) = (s / 100.0, l / 100.0);
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = (h.rem_euclid(360.0)) / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r, g, b) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    let q = |v: f32| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    format!("#{:02x}{:02x}{:02x}", q(r), q(g), q(b))
}

struct Pal {
    fill: String,
    dk: String,
    acc: String,
    ink: &'static str,
}

fn palette(t: &Traits, fam: Family, dark: bool) -> Pal {
    let (h0, h1) = hue_range(fam);
    let (s0, s1) = sat_range(fam);
    let (l0, l1) = light_range(fam);
    let hue = h0 + t.r2[0] * (h1 - h0);
    let sat = s0 + t.r2[1] * (s1 - s0) - if dark { 4.0 } else { 0.0 };
    let lig = l0 + t.r2[2] * (l1 - l0) - if dark { 8.0 } else { 0.0 };
    let hs = |dl: f32, ds: f32| hsl_hex(hue, (sat + ds).max(0.0), (lig + dl).clamp(8.0, 96.0));
    Pal {
        fill: hs(0.0, 0.0),
        dk: hs(-16.0, 4.0),
        acc: hs(-24.0, 6.0),
        ink: if lig < 58.0 { "#fbfaf7" } else { "#26221f" },
    }
}

// ---------------------------------------------------------------------------------------------
// Parts
// ---------------------------------------------------------------------------------------------

fn f(v: f32) -> String {
    format!("{v:.2}")
}

fn radius(t: &Traits, a: f32) -> f32 {
    let (c, s) = (a.cos(), a.sin());
    match t.shape {
        2 => 1.0 / (c.abs().powi(4) + s.abs().powi(4)).powf(0.25) * 0.92,
        3 => 1.0 / ((c.abs() / 1.25).powi(4) + (s.abs() / 0.82).powf(2.2)).powf(1.0 / 3.2),
        4 => 0.9 + 0.1 * (5.0 * a + t.rot).cos(),
        5 => 1.0 + 0.32 * (-s).max(0.0).powi(4),
        6 => 0.94 + 0.14 * (a + 1.57 - 0.6 * (t.j[0] - 0.5)).cos().max(0.0).powi(24),
        7 => 1.0 / ((c.abs() / 1.1).powf(2.6) + (s.abs() / 0.9).powf(2.6)).powf(1.0 / 2.6),
        1 => 1.0 + 0.09 * (3.0 * a + t.rot).sin(),
        _ => 1.0,
    }
}

fn blob_path(t: &Traits, cx: f32, cy: f32, rad: f32) -> String {
    let n = if t.shape == 4 { 20 } else { 16 };
    let boxy = if t.shape == 2 { 0.4 } else { 1.0 };
    let pts: Vec<(f32, f32)> = (0..n)
        .map(|i| {
            let a = i as f32 / n as f32 * 6.2832 - 1.5708;
            let k = radius(t, a) * (1.0 + (t.j[i % 12] - 0.5) * t.wob * boxy);
            (cx + a.cos() * rad * t.w * k, cy + a.sin() * rad * t.h * k)
        })
        .collect();
    let mut d = format!("M{} {}", f(pts[0].0), f(pts[0].1));
    for i in 0..n {
        let p0 = pts[(i + n - 1) % n];
        let p1 = pts[i];
        let p2 = pts[(i + 1) % n];
        let p3 = pts[(i + 2) % n];
        let _ = write!(
            d,
            " C{} {} {} {} {} {}",
            f(p1.0 + (p2.0 - p0.0) / 6.0),
            f(p1.1 + (p2.1 - p0.1) / 6.0),
            f(p2.0 - (p3.0 - p1.0) / 6.0),
            f(p2.1 - (p3.1 - p1.1) / 6.0),
            f(p2.0),
            f(p2.1)
        );
    }
    d.push('Z');
    d
}

fn crown(cx: f32, top: f32) -> String {
    format!(
        "<g {CROWN_ID} transform=\"translate({} {})\"><path d=\"M-9 0 L-10 -10 L-4.5 -5 L0 -12 L4.5 -5 L10 -10 L9 0Z\" fill=\"#f2b84b\" stroke=\"#c98a1d\" stroke-width=\"1\" stroke-linejoin=\"round\"/></g>",
        f(cx),
        f(top - 1.0)
    )
}

/// A 크루 role prop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prop {
    Cap,
    Phones,
    Tie,
    Bow,
    Beret,
}

const ROLE_PROPS: [(&str, Prop); 11] = [
    ("리뷰어", Prop::Tie),
    ("검토", Prop::Tie),
    ("테스터", Prop::Phones),
    ("테스트", Prop::Phones),
    ("디자인", Prop::Cap),
    ("문서", Prop::Bow),
    ("카피", Prop::Bow),
    ("조사", Prop::Beret),
    ("빌드", Prop::Phones),
    ("배포", Prop::Cap),
    ("인프라", Prop::Cap),
];

/// The prop a role name wears, if it names one.
pub fn role_prop(role: &str) -> Option<Prop> {
    ROLE_PROPS
        .iter()
        .find(|(k, _)| role.contains(k))
        .map(|(_, p)| *p)
}

fn prop_svg(kind: Prop, cx: f32, top: f32, rad: f32, ink: &str, acc: &str) -> String {
    match kind {
        Prop::Cap => format!(
            "<g transform=\"translate({} {})\"><path d=\"M-13 0a13 10 0 0 1 26 0z\" fill=\"{acc}\"/><path d=\"M8 0h12\" stroke=\"{acc}\" stroke-width=\"3.2\" stroke-linecap=\"round\"/></g>",
            f(cx),
            f(top + 3.0)
        ),
        Prop::Phones => format!(
            "<path d=\"M{} {}a{} {} 0 0 1 {} 0\" fill=\"none\" stroke=\"{ink}\" stroke-width=\"2.6\" stroke-linecap=\"round\"/><rect x=\"{}\" y=\"{}\" width=\"7\" height=\"12\" rx=\"3.5\" fill=\"{acc}\"/><rect x=\"{}\" y=\"{}\" width=\"7\" height=\"12\" rx=\"3.5\" fill=\"{acc}\"/>",
            f(cx - rad * 0.78),
            f(top + rad * 0.55),
            f(rad * 0.78),
            f(rad * 0.62),
            f(rad * 1.56),
            f(cx - rad * 0.92),
            f(top + rad * 0.48),
            f(cx + rad * 0.92 - 7.0),
            f(top + rad * 0.48)
        ),
        Prop::Tie => format!(
            "<path d=\"M{} {}l-3.4 4 3.4 11 3.4-11z\" fill=\"{acc}\"/>",
            f(cx),
            f(top + rad * 1.32)
        ),
        Prop::Bow => format!(
            "<path d=\"M{x} {y}l-8-4.4v8.8zM{x} {y}l8-4.4v8.8z\" fill=\"{acc}\"/><circle cx=\"{x}\" cy=\"{y}\" r=\"2\" fill=\"{acc}\"/>",
            x = f(cx),
            y = f(top + rad * 1.36)
        ),
        Prop::Beret => format!(
            "<ellipse cx=\"{}\" cy=\"{}\" rx=\"13\" ry=\"5\" fill=\"{acc}\"/><circle cx=\"{}\" cy=\"{}\" r=\"1.8\" fill=\"{acc}\"/>",
            f(cx + 2.0),
            f(top + 2.0),
            f(cx + 2.0),
            f(top - 3.0)
        ),
    }
}

fn seeded_prop(t: &Traits) -> Prop {
    [Prop::Tie, Prop::Phones, Prop::Cap, Prop::Bow, Prop::Beret][(t.k * 5.0) as usize % 5]
}

fn eyes(t: &Traits, cx: f32, cy: f32, ink: &str, st: CharState, dot: bool) -> String {
    let mut g = t.eye_gap;
    let mut w = t.eye_w;
    let mut h = t.eye_h;
    let y = cy + t.eye_y;
    if dot {
        w = (t.eye_w).max(3.6);
        h = w;
        g = g.min(9.0);
    }
    match st {
        CharState::Fail => format!(
            "<path d=\"M{} {}h{}M{} {}h{}\" stroke=\"{ink}\" stroke-width=\"2.2\" stroke-linecap=\"round\"/>",
            f(cx - g - w / 2.0),
            f(y + 1.0),
            f(w + 2.0),
            f(cx + g - w / 2.0 - 2.0),
            f(y + 1.0),
            f(w + 2.0)
        ),
        CharState::Done => format!(
            "<path d=\"M{} {}q3.5-4.5 7 0M{} {}q3.5-4.5 7 0\" stroke=\"{ink}\" stroke-width=\"2.2\" fill=\"none\" stroke-linecap=\"round\"/>",
            f(cx - g - 3.5),
            f(y + 1.0),
            f(cx + g - 3.5),
            f(y + 1.0)
        ),
        _ => {
            let rr = if dot || t.eye_round { w / 2.0 } else { 1.4 };
            let mut s = String::new();
            for ex in [cx - g - w / 2.0 + t.gaze, cx + g - w / 2.0 + t.gaze] {
                let _ = write!(
                    s,
                    "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" rx=\"{}\" fill=\"{ink}\"/>",
                    f(ex),
                    f(y - h / 2.0),
                    f(w),
                    f(h),
                    f(rr)
                );
            }
            s
        }
    }
}

/// The small mark each state adds at the upper right: a static stand-in for motion.
fn state_mark(st: CharState, dark: bool) -> String {
    let blue = if dark { "#6d9be8" } else { "#3f74d4" };
    let sky = if dark { "#7f9fc9" } else { "#9dbde9" };
    let gold = "#f2b84b";
    match st {
        CharState::Run => format!(
            "<circle cx=\"78\" cy=\"22\" r=\"2.4\" fill=\"{blue}\"/><circle cx=\"85\" cy=\"16\" r=\"1.8\" fill=\"{blue}\"/>"
        ),
        CharState::Wait => {
            "<circle cx=\"83\" cy=\"34\" r=\"5\" fill=\"#b4b2ac\"/><path d=\"M77 46 L80 40\" stroke=\"#b4b2ac\" stroke-width=\"3\" stroke-linecap=\"round\"/>"
                .to_string()
        }
        CharState::Fail => format!(
            "<path d=\"M80 24c2 3 3 4.6 3 6a3 3 0 0 1-6 0c0-1.4 1-3 3-6z\" fill=\"{sky}\"/>"
        ),
        CharState::Done => format!(
            "<path d=\"M82 14 L83.6 18.4 L88 20 L83.6 21.6 L82 26 L80.4 21.6 L76 20 L80.4 18.4Z\" fill=\"{gold}\"/>"
        ),
        CharState::Idle => String::new(),
    }
}

fn mongle_body(kind: &str, c: &str) -> String {
    let d = match kind {
        "round" => "M50 24c16 0 27 11 27 28S66 80 50 80 23 69 23 52s11-28 27-28z",
        "bean" => {
            "M38 26c12-4 20 2 26 0 10-3 16 8 14 24-2 18-12 30-28 30S22 68 22 52c0-12 6-23 16-26z"
        }
        "egg" => "M50 20c14 0 25 16 25 34 0 16-11 26-25 26S25 70 25 54c0-18 11-34 25-34z",
        "squircle" => {
            "M34 26h32c8 0 12 4 12 12v28c0 8-4 12-12 12H34c-8 0-12-4-12-12V38c0-8 4-12 12-12z"
        }
        "pill" => "M26 46a24 24 0 0 1 48 0v12a24 24 0 0 1-48 0z",
        "drop" => "M50 20c9 13 26 25 26 40 0 13-11 20-26 20S24 73 24 60c0-15 17-27 26-40z",
        _ => "M50 20c4 13 12 21 27 27-15 5-23 14-27 31-4-17-12-26-27-31 15-6 23-14 27-27z",
    };
    format!("<path d=\"{d}\" fill=\"{c}\"/>")
}

fn mouth(st: CharState) -> &'static str {
    match st {
        CharState::Done => "M44 60q6 5 12 0",
        CharState::Run => "M45 61h10",
        CharState::Wait => "M47 60a3 3 0 0 0 6 0",
        CharState::Fail => "M44 63q6-5 12 0",
        CharState::Idle => "M45 60q5 3.5 10 0",
    }
}

// ---------------------------------------------------------------------------------------------
// The drawing
// ---------------------------------------------------------------------------------------------

/// The SVG of one character. Pure: the same spec gives the same string.
pub fn svg(spec: &CharSpec) -> String {
    let fam = family_of(&spec.agent);
    let mut t = traits(&spec.name, fam);
    let c = palette(&t, fam, spec.dark);
    let st = spec.state;
    let (cx, cy, rad) = (50.0_f32, 58.0_f32, 30.0_f32);
    let mut body = String::new();
    let face;
    let mut extra = String::new();
    let top;
    match spec.style {
        CharStyle::Malang => {
            let bp = blob_path(&t, cx, cy, rad);
            body.push_str(&format!("<path d=\"{bp}\" fill=\"{}\"/>", c.fill));
            body.push_str(&format!(
                "<ellipse cx=\"{}\" cy=\"{}\" rx=\"{}\" ry=\"{}\" fill=\"#ffffff\" opacity=\"0.18\"/>",
                f(cx - 6.0),
                f(cy - rad * t.h * 0.55),
                f(rad * t.w * 0.45),
                f(rad * t.h * 0.2)
            ));
            face = eyes(&t, cx, cy - 2.0, c.ink, st, false);
            top = cy - rad * t.h;
        }
        CharStyle::Mongle => {
            let kinds: [&str; 3] = match fam {
                Family::Claude => ["round", "bean", "egg"],
                Family::Codex => ["squircle", "pill", "squircle"],
                Family::Gemini => ["star", "drop", "star"],
            };
            let kind = kinds[(t.k * 3.0) as usize % 3];
            body.push_str(&mongle_body(kind, &c.fill));
            body.push_str("<path d=\"M33 60h4M63 60h4\" stroke=\"#ffffff\" stroke-opacity=\"0.4\" stroke-width=\"2.6\" stroke-linecap=\"round\"/>");
            let ey = match st {
                CharState::Done => "<path d=\"M37 49q4-4 8 0M55 49q4-4 8 0\" stroke=\"#26221f\" stroke-width=\"2.4\" fill=\"none\" stroke-linecap=\"round\"/>",
                CharState::Fail => "<path d=\"M37 50h7M56 50h7\" stroke=\"#26221f\" stroke-width=\"2.4\" stroke-linecap=\"round\"/>",
                _ => "<ellipse cx=\"41\" cy=\"49\" rx=\"3\" ry=\"4\" fill=\"#26221f\"/><ellipse cx=\"59\" cy=\"49\" rx=\"3\" ry=\"4\" fill=\"#26221f\"/>",
            };
            face = format!(
                "{ey}<path d=\"{}\" fill=\"none\" stroke=\"#26221f\" stroke-width=\"2.2\" stroke-linecap=\"round\"/>",
                mouth(st)
            );
            top = if kind == "star" { 22.0 } else { 24.0 };
        }
        CharStyle::Crew => {
            let bw = 40.0 + t.k * 10.0;
            let bh = 42.0 + t.k2 * 8.0;
            let bx = 50.0 - bw / 2.0;
            let by = 22.0;
            let br = 11.0 + t.k * 6.0;
            let leg_y = by + bh;
            let arm_y = by + bh * 0.55;
            let _ = write!(
                body,
                "<rect x=\"{}\" y=\"{}\" width=\"8\" height=\"14\" rx=\"4\" fill=\"{dk}\"/><rect x=\"{}\" y=\"{}\" width=\"8\" height=\"14\" rx=\"4\" fill=\"{dk}\"/>",
                f(50.0 - bw * 0.28 - 4.0),
                f(leg_y - 4.0),
                f(50.0 + bw * 0.28 - 4.0),
                f(leg_y - 4.0),
                dk = c.dk
            );
            // Arms: raised on wait and done, down otherwise.
            let up = matches!(st, CharState::Wait | CharState::Done);
            let ay = if up { by + bh * 0.2 } else { arm_y };
            let _ = write!(
                body,
                "<rect x=\"{}\" y=\"{}\" width=\"8\" height=\"16\" rx=\"4\" fill=\"{dk}\"/><rect x=\"{}\" y=\"{}\" width=\"8\" height=\"16\" rx=\"4\" fill=\"{dk}\"/>",
                f(bx - 7.0),
                f(arm_y),
                f(bx + bw - 1.0),
                f(ay),
                dk = c.dk
            );
            let _ = write!(
                body,
                "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" rx=\"{}\" fill=\"{}\"/><rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" rx=\"{}\" fill=\"#ffffff\" opacity=\"0.16\"/>",
                f(bx),
                f(by),
                f(bw),
                f(bh),
                f(br),
                c.fill,
                f(bx + 4.0),
                f(by + 3.0),
                f(bw - 8.0),
                f(bh * 0.32),
                f(br - 3.0)
            );
            t.eye_gap = 8.0;
            t.eye_w = 4.2;
            t.eye_h = 6.4;
            t.eye_y = 0.0;
            t.gaze = 0.0;
            t.eye_round = true;
            face = eyes(&t, 50.0, by + bh * 0.42, c.ink, st, false);
            let pk = spec
                .role
                .as_deref()
                .and_then(role_prop)
                .unwrap_or_else(|| seeded_prop(&t));
            if !(spec.lead && pk == Prop::Cap) {
                extra.push_str(&prop_svg(pk, 50.0, by, bw / 2.0 * 0.95, c.ink, &c.acc));
            }
            top = by;
        }
        CharStyle::Pocket => {
            let kd = (t.k * 4.0) as usize % 4;
            let d = match kd {
                0 => "M50 26 C54 26 82 76 80 80 C78 84 22 84 20 80 C18 76 46 26 50 26Z",
                1 => "M30 80 C14 80 14 58 28 56 C26 40 46 34 54 44 C64 34 82 44 76 58 C90 62 86 80 72 80Z",
                2 => "M38 46 C34 30 36 18 41 18 C46 18 46 34 46 44 L54 44 C54 34 54 18 59 18 C64 18 66 30 62 46 C76 52 78 82 50 82 C22 82 24 52 38 46Z",
                _ => "M50 24 C62 42 76 54 76 66 C76 80 64 86 50 86 C36 86 24 80 24 66 C24 54 38 42 50 24Z",
            };
            body.push_str(&format!("<path d=\"{d}\" fill=\"{}\"/>", c.fill));
            t.eye_gap = 8.0;
            t.eye_w = 4.2;
            t.gaze = 0.0;
            t.eye_y = 0.0;
            face = eyes(&t, 50.0, if kd == 0 { 66.0 } else { 62.0 }, c.ink, st, true);
            let dp = [Prop::Bow, Prop::Beret, Prop::Phones, Prop::Tie][kd];
            let pt = match kd {
                2 => 34.0,
                0 => 30.0,
                _ => 40.0,
            };
            extra.push_str(&prop_svg(dp, 50.0, pt, 24.0, c.ink, &c.acc));
            top = match kd {
                2 => 18.0,
                0 => 28.0,
                _ => 36.0,
            };
        }
        CharStyle::Cream => {
            let cream = if spec.dark { "#e6dcc8" } else { "#f4ecdc" };
            body.push_str(&format!(
                "<ellipse cx=\"50\" cy=\"58\" rx=\"29\" ry=\"27\" fill=\"{cream}\"/><path d=\"M24 70 C30 86 70 86 76 70 C70 76 30 76 24 70Z\" fill=\"{}\"/><path d=\"M42 72 L50 79 L58 72\" fill=\"none\" stroke=\"{}\" stroke-width=\"2\"/>",
                c.fill, c.dk
            ));
            t.eye_gap = 8.5;
            t.eye_w = 4.0;
            t.gaze = 0.0;
            t.eye_y = 0.0;
            let mut fc = eyes(&t, 50.0, 54.0, "#2a2522", st, true);
            if !matches!(st, CharState::Fail | CharState::Done) {
                fc.push_str("<path d=\"M46.5 63 q3.5 2.4 7 0\" stroke=\"#2a2522\" stroke-width=\"1.7\" fill=\"none\" stroke-linecap=\"round\"/>");
            }
            face = fc;
            top = 32.0;
        }
        CharStyle::Keyring => {
            let bp = blob_path(&t, cx, cy + 4.0, rad * 0.86);
            body.push_str(&format!("<path d=\"{bp}\" fill=\"{}\"/>", c.fill));
            face = eyes(&t, cx, cy + 2.0, c.ink, st, true);
            let btop = cy + 4.0 - rad * 0.86 * t.h;
            extra.push_str(&format!(
                "<circle cx=\"50\" cy=\"12\" r=\"5\" fill=\"none\" stroke=\"#b4b2ac\" stroke-width=\"2\"/><path d=\"M50 17 V{}\" stroke=\"#b4b2ac\" stroke-width=\"2\"/>",
                f(btop + 1.0)
            ));
            top = btop;
        }
    }
    extra.push_str(&state_mark(st, spec.dark));
    if spec.lead {
        if spec.style == CharStyle::Keyring {
            extra.push_str(&crown(62.0, top + 6.0));
        } else {
            extra.push_str(&crown(50.0, top));
        }
    }
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 100 100\" width=\"100\" height=\"100\"><ellipse cx=\"50\" cy=\"93\" rx=\"20\" ry=\"3\" fill=\"#000000\" opacity=\"0.08\"/>{body}{face}{extra}</svg>"
    )
}

// ---------------------------------------------------------------------------------------------
// Components
// ---------------------------------------------------------------------------------------------

/// A drawn character. `project` picks the project's style from prefs and decides the crown
/// (never in 기본 프로젝트); an empty project means the default style.
#[component]
pub fn Character(
    agent: String,
    leader: bool,
    size: f32,
    name: String,
    #[props(default)] project: String,
    #[props(default)] state: CharState,
    #[props(default)] role: Option<String>,
) -> Element {
    let theme = use_ember_theme();
    let ui = use_ui();
    let style = ui.prefs.read().character_style(&project);
    let spec = CharSpec {
        style,
        name,
        agent,
        state,
        lead: crown_shown(&project, leader),
        role,
        dark: theme.scheme == Scheme::Dark,
    };
    draw(&spec, size)
}

fn draw(spec: &CharSpec, size: f32) -> Element {
    // The renderer draws an SVG at its own width and height, so the pixel size goes in.
    let px = format!("{}", size.round() as i32);
    let doc = svg(spec).replacen(
        "width=\"100\" height=\"100\"",
        &format!("width=\"{px}\" height=\"{px}\""),
        1,
    );
    let id = dioxus_compose::asset::register_owned(AssetKind::Svg, doc.into_bytes());
    rsx! { Image { asset_id: id, width: size, height: size } }
}

/// The 팀 캐릭터 picker of the project settings sheet: six preview cards, three rows of two, so the Korean names are not clipped.
/// The choice is saved to prefs at once. TODO(server): save it on the project instead.
#[component]
pub fn StylePicker(project_id: String, seed_name: String) -> Element {
    let theme = use_ember_theme();
    let ui = use_ui();
    let p = theme.palette;
    let current = ui.prefs.read().character_style(&project_id);
    let dark = theme.scheme == Scheme::Dark;
    let rows: Vec<Vec<CharStyle>> = CharStyle::ALL.chunks(2).map(|c| c.to_vec()).collect();
    rsx! {
        Column {
            fill_max_width: true,
            space_role: SpaceRole::Sm,
            for row in rows {
                Row {
                    fill_max_width: true,
                    space_role: SpaceRole::Sm,
                    for style in row {
                        Column {
                            key: "{style.label()}",
                            weight: 1.0,
                            alignment: Alignment::TopCenter,
                            space_role: SpaceRole::Xs,
                            {draw(&CharSpec {
                                style,
                                name: seed_name.clone(),
                                agent: "claude".into(),
                                state: CharState::Idle,
                                lead: false,
                                role: Some("리뷰어".into()),
                                dark,
                            }, 56.0)}
                            AppButton {
                                text: style.label(),
                                small: true,
                                kind: if style == current { ButtonKind::Primary } else { ButtonKind::Default },
                                on_click: {
                                    let pid = project_id.clone();
                                    move |()| ui.update_prefs(|pr| pr.set_character_style(&pid, style))
                                },
                            }
                            Label { text: style.blurb(), fs: Fs::Xs2, color: paint(p.text_2), max_lines: 3 }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::project::DEFAULT_PROJECT_ID;

    fn spec(style: CharStyle, name: &str, agent: &str) -> CharSpec {
        CharSpec {
            style,
            name: name.into(),
            agent: agent.into(),
            state: CharState::Idle,
            lead: false,
            role: None,
            dark: false,
        }
    }

    // FR-L22: the same name always gives the same character.
    #[test]
    fn same_name_gives_the_same_svg() {
        for style in CharStyle::ALL {
            let a = svg(&spec(style, "ember-리더", "claude"));
            let b = svg(&spec(style, "ember-리더", "claude"));
            assert_eq!(a, b, "{style:?}");
        }
        assert_eq!(seed_hash("ember"), seed_hash("ember"));
        assert_ne!(seed_hash("ember"), seed_hash("embers"));
    }

    // FR-L22: well-formed output, for every style, state and scheme.
    #[test]
    fn svg_is_well_formed_for_every_style_and_state() {
        for style in CharStyle::ALL {
            for state in CharState::ALL {
                for dark in [false, true] {
                    for lead in [false, true] {
                        let mut s = spec(style, "리뷰어", "codex");
                        s.state = state;
                        s.dark = dark;
                        s.lead = lead;
                        s.role = Some("리뷰어".into());
                        let out = svg(&s);
                        assert!(out.starts_with("<svg"), "{style:?} {state:?}");
                        assert!(out.ends_with("</svg>"), "{style:?} {state:?}");
                        assert!(is_balanced(&out), "{style:?} {state:?}: {out}");
                        assert!(!out.contains('\u{2014}'));
                        assert!(!out.contains("NaN") && !out.contains("inf"));
                    }
                }
            }
        }
    }

    // Open tags are closed or self-closing, and `<` and `>` pair up.
    fn is_balanced(s: &str) -> bool {
        let mut stack: Vec<String> = Vec::new();
        let mut rest = s;
        while let Some(i) = rest.find('<') {
            let Some(j) = rest[i..].find('>') else {
                return false;
            };
            let tag = &rest[i + 1..i + j];
            rest = &rest[i + j + 1..];
            if let Some(name) = tag.strip_prefix('/') {
                if stack.pop().as_deref() != Some(name.trim()) {
                    return false;
                }
            } else if !tag.ends_with('/') {
                let name = tag.split_whitespace().next().unwrap_or("");
                stack.push(name.to_string());
            }
        }
        stack.is_empty() && !rest.contains('>')
    }

    // FR-L22: different seeds give different shapes in every style.
    #[test]
    fn different_names_give_different_drawings() {
        for style in CharStyle::ALL {
            let outs: std::collections::BTreeSet<String> =
                ["가", "나", "다", "ember-a", "ember-b", "qa", "docs"]
                    .iter()
                    .map(|n| svg(&spec(style, n, "claude")))
                    .collect();
            assert!(outs.len() >= 5, "{style:?} drew only {} looks", outs.len());
        }
    }

    // DESIGN section 7: the harness sets the colour family.
    #[test]
    fn harness_sets_the_hue_family() {
        assert_eq!(family_of("claude-code"), Family::Claude);
        assert_eq!(family_of("codex"), Family::Codex);
        assert_eq!(family_of("gemini"), Family::Gemini);
        assert_eq!(family_of("unknown"), Family::Claude);
        assert_eq!(hue_range(Family::Claude), (20.0, 34.0));
        assert_eq!(hue_range(Family::Codex), (218.0, 238.0));
        assert_eq!(hue_range(Family::Gemini), (196.0, 212.0));
        for name in ["a", "b", "c", "d", "e", "f", "g", "h"] {
            for (agent, fam) in [
                ("claude", Family::Claude),
                ("codex", Family::Codex),
                ("gemini", Family::Gemini),
            ] {
                let (lo, hi) = hue_range(fam);
                let h = body_hue(name, agent);
                assert!(h >= lo && h <= hi, "{name} {agent} {h}");
            }
        }
    }

    // Dark mode is slightly darker than light.
    #[test]
    fn dark_scheme_changes_the_colours() {
        let light = svg(&spec(CharStyle::Mongle, "x", "claude"));
        let mut d = spec(CharStyle::Mongle, "x", "claude");
        d.dark = true;
        assert_ne!(light, svg(&d));
    }

    // FR-L24: the crown only when lead, and never in 기본 프로젝트.
    #[test]
    fn crown_only_for_a_leader_outside_the_default_project() {
        assert!(crown_shown("ember", true));
        assert!(!crown_shown("ember", false));
        assert!(!crown_shown(DEFAULT_PROJECT_ID, true));
        for style in CharStyle::ALL {
            let plain = svg(&spec(style, "m", "claude"));
            let mut l = spec(style, "m", "claude");
            l.lead = true;
            assert!(!plain.contains(CROWN_ID), "{style:?}");
            assert!(svg(&l).contains(CROWN_ID), "{style:?}");
        }
    }

    // 크루 role props.
    #[test]
    fn crew_role_props_follow_the_role() {
        assert_eq!(role_prop("리뷰어"), Some(Prop::Tie));
        assert_eq!(role_prop("테스터"), Some(Prop::Phones));
        assert_eq!(role_prop("디자인"), Some(Prop::Cap));
        assert_eq!(role_prop("문서"), Some(Prop::Bow));
        assert_eq!(role_prop("조사"), Some(Prop::Beret));
        assert_eq!(role_prop("없는 역할"), None);
        let mut a = spec(CharStyle::Crew, "m", "claude");
        let mut b = a.clone();
        a.role = Some("리뷰어".into());
        b.role = Some("테스터".into());
        assert_ne!(svg(&a), svg(&b));
    }

    // States are poses: each state draws differently.
    #[test]
    fn each_state_has_its_own_pose() {
        for style in CharStyle::ALL {
            let outs: std::collections::BTreeSet<String> = CharState::ALL
                .iter()
                .map(|st| {
                    let mut s = spec(style, "m", "gemini");
                    s.state = *st;
                    svg(&s)
                })
                .collect();
            assert_eq!(outs.len(), CharState::ALL.len(), "{style:?}");
        }
    }

    #[test]
    fn launcher_status_maps_to_a_state() {
        use ember_client::LauncherStatus as S;
        assert_eq!(CharState::from_status(S::Running), CharState::Run);
        assert_eq!(
            CharState::from_status(S::WaitingForApproval),
            CharState::Wait
        );
        assert_eq!(CharState::from_status(S::Failed), CharState::Fail);
        assert_eq!(CharState::from_status(S::Finished), CharState::Done);
        assert_eq!(CharState::from_status(S::FinishedUnread), CharState::Done);
        assert_eq!(CharState::from_status(S::Idle), CharState::Idle);
    }

    #[test]
    fn six_styles_with_ids_and_korean_names() {
        let names: Vec<&str> = CharStyle::ALL.iter().map(|s| s.label()).collect();
        assert_eq!(names, ["몽글", "말랑", "크루", "포켓", "크림", "키링"]);
        assert_eq!(CharStyle::default(), CharStyle::Mongle);
        for s in CharStyle::ALL {
            assert!(!s.blurb().is_empty());
            assert!(!s.blurb().contains('\u{2014}'));
        }
    }

    #[test]
    fn style_serializes_as_a_lowercase_id() {
        assert_eq!(
            serde_json::to_string(&CharStyle::Keyring).unwrap(),
            "\"keyring\""
        );
        assert_eq!(
            serde_json::from_str::<CharStyle>("\"crew\"").unwrap(),
            CharStyle::Crew
        );
    }
}
