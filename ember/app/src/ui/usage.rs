//! Token usage as the screens show it (FR-U7, FR-L11, FR-L20, FR-L18, #332): the pure model
//! behind the Home token tile, the 에이전트 usage section and the Session 오늘 토큰 tile.
//!
//! The server (`GET /api/usage/summary`, `GET /api/sessions/{id}/usage`) sums the tokens the
//! harnesses reported. Three rules hold everywhere:
//!
//! - **unknown is not zero**: a session whose harness reported nothing is "알 수 없음", and a
//!   period with no report says so instead of showing 0;
//! - **cost is unknown**: the SPEC has no price table and the server sends `cost_usd: null`, so
//!   no screen shows a dollar figure;
//! - **a day is a UTC day**: wherever one is named the text says "UTC 기준".

use ember_client::api::{SessionUsage, UsageDayRow, UsageSummary};

pub const UNKNOWN: &str = "알 수 없음";
pub const UTC_NOTE: &str = "UTC 기준";
pub const COST_UNKNOWN: &str = "비용은 알 수 없습니다";
pub const LOADING: &str = "불러오는 중입니다";
pub const FAILED: &str = "불러오지 못했습니다";
pub const NO_REPORT: &str = "보고된 사용량이 없습니다";

/// A value fetched from the server: not yet, failed, or here.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Load<T> {
    #[default]
    Loading,
    Failed(String),
    Ready(T),
}

/// "1.2M", "3.4K" or the plain number below 1000.
pub fn format_compact(n: u64) -> String {
    if n < 1_000 {
        return n.to_string();
    }
    // Round to one decimal in tenths, so 999_960 becomes 1.0M and not 1000.0K.
    let tenths_k = (n + 50) / 100;
    if tenths_k < 10_000 {
        return format!("{}.{}K", tenths_k / 10, tenths_k % 10);
    }
    let tenths_m = (n + 50_000) / 100_000;
    format!("{}.{}M", tenths_m / 10, tenths_m % 10)
}

/// "11월 6일 (UTC 기준)" for `2026-11-06`; the input itself when it is not a date.
pub fn day_label(day: &str) -> String {
    let mut it = day.split('-');
    match (it.next(), it.next().and_then(|m| m.parse::<u32>().ok()), it.next().and_then(|d| d.parse::<u32>().ok())) {
        (Some(_), Some(m), Some(d)) if (1..=12).contains(&m) && (1..=31).contains(&d) => {
            format!("{m}월 {d}일 ({UTC_NOTE})")
        }
        _ => format!("{day} ({UTC_NOTE})"),
    }
}

/// The harness name for a server agent kind.
pub fn agent_label(kind: &str) -> String {
    match kind {
        "claude-code" => "Claude Code".to_string(),
        "codex" => "Codex".to_string(),
        "antigravity" => "agy".to_string(),
        other => other.to_string(),
    }
}

/// A token count, or "알 수 없음" when the harness reported none. Never 0 for unknown.
pub fn token_value(reported: bool, total: u64) -> String {
    if reported {
        format_compact(total)
    } else {
        UNKNOWN.to_string()
    }
}

/// One session's tokens: (value, sub line). The cost is always unknown.
pub fn session_usage_text(u: &SessionUsage) -> (String, String) {
    if !u.reported {
        return (UNKNOWN.to_string(), "하네스가 토큰 수를 알려 주지 않았습니다".to_string());
    }
    (format_compact(u.total_tokens), format!("{}턴 · {COST_UNKNOWN}", u.turns))
}

/// Tokens reported on `today` (UTC), or `None` when nothing was reported that day.
pub fn today_tokens(s: &UsageSummary, today: &str) -> Option<u64> {
    s.days.iter().find(|d| d.day == today && d.turns > 0).map(|d| d.total_tokens)
}

/// A tile with one figure and a line under it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenTile {
    pub value: String,
    pub sub: String,
}

/// The 오늘 토큰 tile of Home and of the Session status view: today's tokens (UTC day).
pub fn today_tile(load: &Load<UsageSummary>, today: &str) -> TokenTile {
    let (value, sub) = match load {
        Load::Loading => ("-".to_string(), LOADING.to_string()),
        Load::Failed(_) => ("-".to_string(), FAILED.to_string()),
        Load::Ready(s) => match today_tokens(s, today) {
            Some(t) => (format_compact(t), format!("{UTC_NOTE} · {COST_UNKNOWN}")),
            None => ("-".to_string(), NO_REPORT.to_string()),
        },
    };
    TokenTile { value, sub }
}

/// One bar of the usage chart: tokens of the three harness families on one UTC day.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DayBar {
    pub day: String,
    pub claude: u64,
    pub codex: u64,
    pub other: u64,
}

impl DayBar {
    pub fn total(&self) -> u64 {
        self.claude + self.codex + self.other
    }
}

fn bar_of(d: &UsageDayRow) -> DayBar {
    let sum = |kind: &str| d.by_agent.iter().filter(|a| a.agent == kind).map(|a| a.total_tokens).sum::<u64>();
    let (claude, codex) = (sum("claude-code"), sum("codex"));
    DayBar { day: d.day.clone(), claude, codex, other: d.total_tokens.saturating_sub(claude + codex) }
}

/// What the 에이전트 사용량 section draws.
#[derive(Debug, Clone, PartialEq)]
pub struct SectionModel {
    /// A state line instead of figures: loading, failed or nothing reported.
    pub notice: Option<String>,
    /// Today's tokens (UTC), "-" when there are none to show.
    pub today: String,
    /// Under the figure: the UTC day, the window total and its daily mean.
    pub today_sub: String,
    /// (agent kind, tokens) per harness over the window, most tokens first.
    pub harness: Vec<(String, String)>,
    pub bars: Vec<DayBar>,
    pub input: String,
    pub output: String,
    /// Raw counts behind `input` and `output`, for the split bar.
    pub input_n: u64,
    pub output_n: u64,
    /// Shares of input and output in the window, whole percent, "-" when there are none.
    pub input_share: String,
    pub output_share: String,
    pub cost: String,
}

/// The section for the last `window` UTC days of `load` (7 or 14).
pub fn section_model(load: &Load<UsageSummary>, today: &str, window: usize) -> SectionModel {
    let blank = |notice: &str| SectionModel {
        notice: Some(notice.to_string()),
        today: "-".to_string(),
        today_sub: day_label(today),
        harness: Vec::new(),
        bars: Vec::new(),
        input: "-".to_string(),
        output: "-".to_string(),
        input_n: 0,
        output_n: 0,
        input_share: "-".to_string(),
        output_share: "-".to_string(),
        cost: COST_UNKNOWN.to_string(),
    };
    let s = match load {
        Load::Loading => return blank(LOADING),
        Load::Failed(_) => return blank(FAILED),
        Load::Ready(s) => s,
    };
    let start = s.days.len().saturating_sub(window.max(1));
    let rows = &s.days[start..];
    let turns: u64 = rows.iter().map(|d| d.turns).sum();
    if turns == 0 {
        return blank(NO_REPORT);
    }
    let total: u64 = rows.iter().map(|d| d.total_tokens).sum();
    let input: u64 = rows.iter().map(|d| d.input_tokens).sum();
    let output: u64 = rows.iter().map(|d| d.output_tokens).sum();
    let mut agents: Vec<(String, u64)> = Vec::new();
    for a in rows.iter().flat_map(|d| d.by_agent.iter()) {
        match agents.iter_mut().find(|(k, _)| *k == a.agent) {
            Some((_, t)) => *t += a.total_tokens,
            None => agents.push((a.agent.clone(), a.total_tokens)),
        }
    }
    agents.sort_by(|a, b| b.1.cmp(&a.1));
    let share = |n: u64| if input + output == 0 { "-".to_string() } else { format!("{}%", (n * 100 + (input + output) / 2) / (input + output)) };
    let mean = total / rows.len().max(1) as u64;
    let today_value = today_tokens(s, today);
    SectionModel {
        notice: None,
        today: today_value.map_or_else(|| "-".to_string(), format_compact),
        today_sub: format!(
            "{} · 최근 {}일 {} · 하루 평균 {}",
            day_label(today),
            rows.len(),
            format_compact(total),
            format_compact(mean)
        ),
        harness: agents.into_iter().map(|(k, t)| (k, format_compact(t))).collect(),
        bars: rows.iter().map(bar_of).collect(),
        input: format_compact(input),
        output: format_compact(output),
        input_n: input,
        output_n: output,
        input_share: share(input),
        output_share: share(output),
        cost: COST_UNKNOWN.to_string(),
    }
}

/// The Home trend tile's total over the last `window` UTC days across every harness, or `None`
/// until the summary is here or when nothing was reported.
pub fn window_total(load: &Load<UsageSummary>, window: usize) -> Option<u64> {
    let Load::Ready(s) = load else { return None };
    let start = s.days.len().saturating_sub(window.max(1));
    let rows = &s.days[start..];
    (rows.iter().map(|d| d.turns).sum::<u64>() > 0).then(|| rows.iter().map(|d| d.total_tokens).sum())
}

/// The Home cost card (FR-L11, #509) shows a stat only when the server sent its data.
pub const NO_DATA: &str = "데이터 없음";
/// The card opens on 14 days, as the mockup does.
pub const DEFAULT_WINDOW: usize = 14;

fn window_rows(load: &Load<UsageSummary>, window: usize) -> &[UsageDayRow] {
    let Load::Ready(s) = load else { return &[] };
    let start = s.days.len().saturating_sub(window.max(1));
    &s.days[start..]
}

/// Sum of the days' `cost_usd` in the window, or `None` when no day carries a cost (today the
/// server sends `null` everywhere, so this is `None`; no price is invented).
pub fn period_cost(load: &Load<UsageSummary>, window: usize) -> Option<f64> {
    let costs: Vec<f64> = window_rows(load, window).iter().filter_map(|d| d.cost_usd).collect();
    (!costs.is_empty()).then(|| costs.iter().sum())
}

/// Sessions that reported usage in the window (sum of the daily counts), or `None` when zero.
pub fn window_sessions(load: &Load<UsageSummary>, window: usize) -> Option<u64> {
    let n: u64 = window_rows(load, window).iter().map(|d| d.sessions).sum();
    (n > 0).then_some(n)
}

/// Cost per session: the cost divided by the sessions, `None` unless both exist.
pub fn cost_per_job(cost: Option<f64>, sessions: Option<u64>) -> Option<f64> {
    match (cost, sessions) {
        (Some(c), Some(n)) if n > 0 => Some(c / n as f64),
        _ => None,
    }
}

pub fn format_usd(v: f64) -> String {
    format!("${v:.2}")
}

/// The four stats of the card, `(label, value)`: 기간 비용, 토큰, 작업당 비용, 캐시 적중.
/// 캐시 적중 is always [`NO_DATA`]: the `usage` event carries input and output tokens only.
pub fn cost_stats(load: &Load<UsageSummary>, window: usize) -> [(&'static str, String); 4] {
    let cost = period_cost(load, window);
    let per_job = cost_per_job(cost, window_sessions(load, window));
    let usd = |v: Option<f64>| v.map(format_usd).unwrap_or_else(|| NO_DATA.to_string());
    [
        ("기간 비용", usd(cost)),
        ("토큰", window_total(load, window).map(format_compact).unwrap_or_else(|| NO_DATA.to_string())),
        ("작업당 비용", usd(per_job)),
        ("캐시 적중", NO_DATA.to_string()),
    ]
}

/// What the bars are: tokens per UTC day. The cost line of the mockup is named only when cost
/// data exists.
pub fn cost_legend(load: &Load<UsageSummary>, window: usize) -> String {
    if period_cost(load, window).is_some() {
        "막대: 일별 토큰 · 선: 비용".to_string()
    } else {
        "막대: 일별 토큰 (비용 데이터 없음)".to_string()
    }
}

/// Tokens per UTC day for the last `window` days, oldest first, as `(YYYY-MM-DD, tokens)`.
/// Empty until the summary is here.
pub fn trend_days(load: &Load<UsageSummary>, window: usize) -> Vec<(String, u64)> {
    let Load::Ready(s) = load else { return Vec::new() };
    let start = s.days.len().saturating_sub(window.max(1));
    s.days[start..].iter().map(|d| (d.day.clone(), d.total_tokens)).collect()
}

/// The line under the Home trend: the window total, or why there is none.
pub fn trend_line(load: &Load<UsageSummary>, window: usize) -> String {
    match load {
        Load::Loading => LOADING.to_string(),
        Load::Failed(_) => FAILED.to_string(),
        Load::Ready(_) => match window_total(load, window) {
            Some(t) => format!("최근 {window}일 토큰 합계 {} ({UTC_NOTE})", format_compact(t)),
            None => NO_REPORT.to_string(),
        },
    }
}

/// Which vendor colour a harness gets in the Home stack: Claude orange, Codex blue, Gemini sky,
/// every other harness a neutral grey (#360).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vendor {
    Claude,
    Codex,
    Gemini,
    Other,
}

impl Vendor {
    pub fn of(kind: &str) -> Vendor {
        match kind {
            "claude-code" => Vendor::Claude,
            "codex" => Vendor::Codex,
            "gemini" => Vendor::Gemini,
            _ => Vendor::Other,
        }
    }

    fn rank(self) -> u8 {
        self as u8
    }
}

/// One harness's slice of a day's stack.
#[derive(Debug, Clone, PartialEq)]
pub struct StackSeg {
    pub agent: String,
    pub vendor: Vendor,
    pub tokens: u64,
    /// Share of the day's stack, 0..=1. The shares of one day sum to 1.
    pub fraction: f32,
}

/// One UTC day of the Home stack: segments bottom to top in legend order.
#[derive(Debug, Clone, PartialEq)]
pub struct StackDay {
    pub day: String,
    pub weekday: &'static str,
    /// Sum of the reported segments. 0 means nothing was reported that day.
    pub total: u64,
    pub segs: Vec<StackSeg>,
}

/// A harness that appears in the window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegendEntry {
    pub agent: String,
    pub label: String,
    pub vendor: Vendor,
}

/// What the Home 하네스별 사용량 tile draws (FR-L23, #360).
#[derive(Debug, Clone, PartialEq)]
pub struct HomeStack {
    /// A state line instead of a chart: loading, failed or nothing reported.
    pub notice: Option<String>,
    /// "오늘 1.2M 토큰", or why there is no figure.
    pub today: String,
    /// "7일 평균 4.0K", over the days that reported.
    pub average: String,
    pub average_tokens: u64,
    pub days: Vec<StackDay>,
    pub legend: Vec<LegendEntry>,
    /// Shown under the weekday axis: the days are UTC days.
    pub axis_caption: &'static str,
    /// The legend text of the dashed average line.
    pub average_legend: &'static str,
    /// A 다시 시도 button is shown (the load failed).
    pub retry: bool,
}

pub const UTC_CAPTION: &str = "(UTC 기준)";
pub const AVERAGE_LEGEND: &str = "7일 평균";
pub const RETRY: &str = "다시 시도";

/// Mon..Sun name of a `YYYY-MM-DD` UTC day, "" when it is not a date.
pub fn weekday_name(day: &str) -> &'static str {
    let mut it = day.split('-').map(|p| p.parse::<i64>().ok());
    let (Some(Some(y)), Some(Some(m)), Some(Some(d))) = (it.next(), it.next(), it.next()) else { return "" };
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return "";
    }
    // Days since 1970-01-01 (Thursday), civil-from-days inverse.
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    ["일", "월", "화", "수", "목", "금", "토"][(days + 4).rem_euclid(7) as usize]
}

fn reported(a: &ember_client::api::AgentUsageRow) -> bool {
    a.turns > 0 || a.total_tokens > 0
}

/// The stack for the last `window` UTC days. A harness that reported nothing on a day is left
/// out of that day (unknown is not zero); the legend lists the harnesses that reported in the
/// window, Claude, Codex, Gemini, then the others by name, so its order never jumps.
pub fn home_stack(load: &Load<UsageSummary>, window: usize) -> HomeStack {
    let blank = |notice: &str| HomeStack {
        notice: Some(notice.to_string()),
        today: format!("오늘 {UNKNOWN}"),
        average: "-".to_string(),
        average_tokens: 0,
        days: Vec::new(),
        legend: Vec::new(),
        axis_caption: UTC_CAPTION,
        average_legend: AVERAGE_LEGEND,
        retry: false,
    };
    let s = match load {
        Load::Loading => return blank(LOADING),
        Load::Failed(_) => return HomeStack { retry: true, ..blank(FAILED) },
        Load::Ready(s) => s,
    };
    let start = s.days.len().saturating_sub(window.max(1));
    let rows = &s.days[start..];
    let mut agents: Vec<String> = Vec::new();
    for a in rows.iter().flat_map(|d| d.by_agent.iter()).filter(|a| reported(a)) {
        if !agents.contains(&a.agent) {
            agents.push(a.agent.clone());
        }
    }
    agents.sort_by(|a, b| (Vendor::of(a).rank(), a).cmp(&(Vendor::of(b).rank(), b)));
    let days: Vec<StackDay> = rows
        .iter()
        .map(|d| {
            let mut segs: Vec<StackSeg> = Vec::new();
            for kind in &agents {
                let tokens: u64 = d.by_agent.iter().filter(|a| a.agent == *kind && reported(a)).map(|a| a.total_tokens).sum();
                if d.by_agent.iter().any(|a| a.agent == *kind && reported(a)) && tokens > 0 {
                    segs.push(StackSeg { agent: kind.clone(), vendor: Vendor::of(kind), tokens, fraction: 0.0 });
                }
            }
            let total: u64 = segs.iter().map(|g| g.tokens).sum();
            for g in &mut segs {
                g.fraction = g.tokens as f32 / total as f32;
            }
            StackDay { day: d.day.clone(), weekday: weekday_name(&d.day), total, segs }
        })
        .collect();
    let reported_days: Vec<u64> = days.iter().filter(|d| d.total > 0).map(|d| d.total).collect();
    if reported_days.is_empty() {
        return blank(NO_REPORT);
    }
    let average_tokens = reported_days.iter().sum::<u64>() / reported_days.len() as u64;
    let today = match days.last() {
        Some(d) if d.total > 0 => format!("오늘 {} 토큰", format_compact(d.total)),
        _ => format!("오늘 {NO_REPORT}"),
    };
    HomeStack {
        notice: None,
        today,
        average: format!("{}일 평균 {}", rows.len(), format_compact(average_tokens)),
        average_tokens,
        days,
        legend: agents
            .into_iter()
            .map(|k| LegendEntry { label: agent_label(&k), vendor: Vendor::of(&k), agent: k })
            .collect(),
        axis_caption: UTC_CAPTION,
        average_legend: AVERAGE_LEGEND,
        retry: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ember_client::api::AgentUsageRow;

    fn day(d: &str, claude: u64, codex: u64, turns: u64) -> UsageDayRow {
        let row = |k: &str, t: u64| AgentUsageRow { agent: k.into(), input_tokens: t * 3 / 4, output_tokens: t / 4, total_tokens: t, turns, cost_usd: None };
        UsageDayRow {
            day: d.into(),
            input_tokens: (claude + codex) * 3 / 4,
            output_tokens: (claude + codex) / 4,
            total_tokens: claude + codex,
            turns,
            by_agent: vec![row("claude-code", claude), row("codex", codex)],
            ..Default::default()
        }
    }

    fn summary(days: Vec<UsageDayRow>) -> Load<UsageSummary> {
        Load::Ready(UsageSummary { days, ..Default::default() })
    }

    #[test]
    fn fr_l11_cost_card_without_cost_says_no_data() {
        let l = summary(vec![day("2026-11-05", 100, 0, 2)]);
        let st = cost_stats(&l, 14);
        assert_eq!(st[0].1, NO_DATA);
        assert_eq!(st[2].1, NO_DATA);
        assert_eq!(st[3].1, NO_DATA);
        assert_eq!(st[1].1, "100");
        assert!(cost_legend(&l, 14).contains("비용 데이터 없음"));
        assert_eq!(cost_stats(&Load::Loading, 14)[1].1, NO_DATA);
    }

    #[test]
    fn fr_l11_cost_card_sums_cost_and_divides_by_sessions() {
        let mut a = day("2026-11-04", 10, 0, 1);
        a.cost_usd = Some(1.0);
        a.sessions = 1;
        let mut b = day("2026-11-05", 10, 0, 1);
        b.cost_usd = Some(2.0);
        b.sessions = 2;
        let l = summary(vec![a, b]);
        assert_eq!(period_cost(&l, 14), Some(3.0));
        assert_eq!(window_sessions(&l, 14), Some(3));
        assert_eq!(cost_per_job(Some(3.0), Some(3)), Some(1.0));
        assert_eq!(cost_per_job(None, Some(3)), None);
        assert_eq!(cost_per_job(Some(3.0), None), None);
        assert_eq!(cost_stats(&l, 14)[0].1, "$3.00");
        assert_eq!(period_cost(&l, 1), Some(2.0));
        assert_eq!(DEFAULT_WINDOW, 14);
    }

    #[test]
    fn fr_u7_compact_numbers() {
        assert_eq!(format_compact(0), "0");
        assert_eq!(format_compact(999), "999");
        assert_eq!(format_compact(1_000), "1.0K");
        assert_eq!(format_compact(3_400), "3.4K");
        assert_eq!(format_compact(1_234_567), "1.2M");
        assert_eq!(format_compact(999_960), "1.0M");
        assert_eq!(format_compact(15_200_000), "15.2M");
    }

    #[test]
    fn fr_u7_unknown_is_not_zero() {
        assert_eq!(token_value(false, 0), "알 수 없음");
        assert_eq!(token_value(true, 0), "0");
        let u = SessionUsage { session_id: "s".into(), reported: false, ..Default::default() };
        assert_eq!(session_usage_text(&u).0, UNKNOWN);
        let u = SessionUsage { session_id: "s".into(), reported: true, total_tokens: 3_400, turns: 2, ..Default::default() };
        let (v, sub) = session_usage_text(&u);
        assert_eq!(v, "3.4K");
        assert!(sub.contains("비용은 알 수 없습니다") && !sub.contains('$'));
    }

    #[test]
    fn fr_u7_day_label_says_utc() {
        assert_eq!(day_label("2026-11-06"), "11월 6일 (UTC 기준)");
        assert_eq!(day_label("nonsense"), "nonsense (UTC 기준)");
    }

    #[test]
    fn fr_l18_today_tile_states() {
        assert_eq!(today_tile(&Load::Loading, "2026-11-06").sub, LOADING);
        assert_eq!(today_tile(&Load::Failed("x".into()), "2026-11-06").sub, FAILED);
        // Nothing reported today: not "0".
        let none = summary(vec![day("2026-11-06", 0, 0, 0)]);
        assert_eq!(today_tile(&none, "2026-11-06"), TokenTile { value: "-".into(), sub: NO_REPORT.into() });
        let some = summary(vec![day("2026-11-05", 5, 5, 1), day("2026-11-06", 1_000_000, 234_567, 4)]);
        let t = today_tile(&some, "2026-11-06");
        assert_eq!(t.value, "1.2M");
        assert!(t.sub.contains("UTC 기준") && !t.sub.contains('$'));
        // Yesterday's tokens are not today's.
        assert_eq!(today_tile(&some, "2026-11-07").value, "-");
    }

    #[test]
    fn fr_u7_empty_summary_says_nothing_was_reported() {
        let m = section_model(&Load::Ready(UsageSummary::default()), "2026-11-06", 7);
        assert_eq!(m.notice.as_deref(), Some(NO_REPORT));
        assert!(m.bars.is_empty() && m.harness.is_empty());
        assert_eq!(m.cost, COST_UNKNOWN);
    }

    #[test]
    fn fr_u7_failure_and_loading_carry_no_figures() {
        let m = section_model(&Load::Failed("boom".into()), "2026-11-06", 7);
        assert_eq!(m.notice.as_deref(), Some(FAILED));
        assert_eq!(m.today, "-");
        assert_eq!(section_model(&Load::Loading, "2026-11-06", 7).notice.as_deref(), Some(LOADING));
        assert_eq!(window_total(&Load::Loading, 7), None);
    }

    #[test]
    fn fr_l20_section_windows_harnesses_and_shares() {
        let days: Vec<_> = (1..=10).map(|i| day(&format!("2026-11-{i:02}"), 1_000, 3_000, 1)).collect();
        let l = summary(days);
        let m = section_model(&l, "2026-11-10", 7);
        assert_eq!(m.bars.len(), 7);
        assert_eq!(m.bars[0].day, "2026-11-04");
        assert_eq!(m.bars[0].total(), 4_000);
        assert_eq!(m.harness[0], ("codex".to_string(), "21.0K".to_string()));
        assert_eq!(m.harness[1], ("claude-code".to_string(), "7.0K".to_string()));
        assert_eq!((m.input_share.as_str(), m.output_share.as_str()), ("75%", "25%"));
        assert!(m.today_sub.contains("UTC 기준") && m.today_sub.contains("하루 평균 4.0K"));
        assert_eq!(window_total(&l, 7), Some(28_000));
        assert_eq!(window_total(&l, 14), Some(40_000));
    }

    #[test]
    fn fr_l11_trend_line_and_days() {
        let l = summary(vec![day("2026-11-05", 1_000, 0, 1), day("2026-11-06", 2_000, 0, 1)]);
        assert_eq!(trend_days(&l, 7).len(), 2);
        assert_eq!(trend_days(&l, 1), vec![("2026-11-06".to_string(), 2_000)]);
        assert_eq!(trend_line(&l, 7), "최근 7일 토큰 합계 3.0K (UTC 기준)");
        assert_eq!(trend_line(&summary(vec![]), 7), NO_REPORT);
        assert_eq!(trend_line(&Load::Failed("x".into()), 7), FAILED);
        assert!(trend_days(&Load::Loading, 7).is_empty());
    }

    #[test]
    fn fr_u7_other_harnesses_fall_in_other() {
        let mut d = day("2026-11-06", 100, 100, 1);
        d.total_tokens += 50;
        d.by_agent.push(AgentUsageRow { agent: "antigravity".into(), total_tokens: 50, ..Default::default() });
        assert_eq!(bar_of(&d).other, 50);
        assert_eq!(agent_label("claude-code"), "Claude Code");
        assert_eq!(agent_label("omp"), "omp");
    }

    #[test]
    fn fr_u7_agent_names_map_to_harness_labels() {
        assert_eq!(agent_label("claude-code"), "Claude Code");
        assert_eq!(agent_label("codex"), "Codex");
        assert_eq!(agent_label("antigravity"), "agy");
        assert_eq!(agent_label("scripted"), "scripted");
        assert_eq!(agent_label("my-acp-agent"), "my-acp-agent");
    }

    fn agent_row(k: &str, t: u64, turns: u64) -> AgentUsageRow {
        AgentUsageRow { agent: k.into(), total_tokens: t, turns, ..Default::default() }
    }

    #[test]
    fn fr_l23_stack_fractions_sum_to_the_day_total() {
        let l = summary(vec![day("2026-11-05", 3_000, 1_000, 2), day("2026-11-06", 1_000_000, 234_567, 4)]);
        let m = home_stack(&l, 7);
        assert!(m.notice.is_none());
        for d in &m.days {
            assert_eq!(d.segs.iter().map(|g| g.tokens).sum::<u64>(), d.total);
            assert!((d.segs.iter().map(|g| g.fraction).sum::<f32>() - 1.0).abs() < 1e-4);
        }
        assert_eq!(m.days[0].segs[0].fraction, 0.75);
        assert_eq!(m.today, "오늘 1.2M 토큰");
    }

    #[test]
    fn fr_l23_unreported_harness_is_omitted_not_zero() {
        let mut d1 = day("2026-11-05", 100, 0, 1);
        d1.by_agent.push(agent_row("antigravity", 0, 0));
        let m = home_stack(&summary(vec![d1, day("2026-11-06", 100, 100, 1)]), 7);
        assert_eq!(m.days[0].segs.len(), 1);
        assert_eq!(m.days[0].segs[0].agent, "claude-code");
        assert!(m.legend.iter().all(|e| e.agent != "antigravity"));
        // A day with no report has no segments, and the average skips it.
        let m = home_stack(&summary(vec![day("2026-11-05", 0, 0, 0), day("2026-11-06", 400, 0, 1)]), 7);
        assert!(m.days[0].segs.is_empty() && m.days[0].total == 0);
        assert_eq!(m.average_tokens, 400);
    }

    #[test]
    fn fr_l23_legend_order_is_stable_and_vendor_coloured() {
        let mut a = day("2026-11-05", 1, 1, 1);
        a.by_agent.insert(0, agent_row("zeta", 5, 1));
        a.by_agent.push(agent_row("gemini", 5, 1));
        a.by_agent.push(agent_row("antigravity", 5, 1));
        let mut b = day("2026-11-06", 1, 1, 1);
        b.by_agent.reverse();
        let m1 = home_stack(&summary(vec![a.clone(), b.clone()]), 7);
        let m2 = home_stack(&summary(vec![b, a]), 7);
        let order: Vec<&str> = m1.legend.iter().map(|e| e.agent.as_str()).collect();
        assert_eq!(order, ["claude-code", "codex", "gemini", "antigravity", "zeta"]);
        assert_eq!(m1.legend.iter().map(|e| &e.agent).collect::<Vec<_>>(), m2.legend.iter().map(|e| &e.agent).collect::<Vec<_>>());
        assert_eq!(m1.legend[3].label, "agy");
        let v: Vec<Vendor> = m1.legend.iter().map(|e| e.vendor).collect();
        assert_eq!(v, [Vendor::Claude, Vendor::Codex, Vendor::Gemini, Vendor::Other, Vendor::Other]);
    }

    #[test]
    fn fr_l23_stack_states_loading_failed_empty() {
        assert_eq!(home_stack(&Load::Loading, 7).notice.as_deref(), Some(LOADING));
        assert_eq!(home_stack(&Load::Failed("x".into()), 7).notice.as_deref(), Some(FAILED));
        assert!(home_stack(&Load::Failed("x".into()), 7).retry);
        assert!(!home_stack(&Load::Loading, 7).retry);
        assert!(!home_stack(&summary(vec![day("2026-11-06", 1, 1, 1)]), 7).retry);
        let e = home_stack(&summary(vec![]), 7);
        assert_eq!(e.notice.as_deref(), Some(NO_REPORT));
        assert!(e.days.is_empty() && e.legend.is_empty());
        assert!(!e.today.contains("0 토큰"));
    }

    #[test]
    fn fr_u7_stack_average_text_and_window() {
        let days: Vec<_> = (1..=9).map(|i| day(&format!("2026-11-{i:02}"), 1_000, 3_000, 1)).collect();
        let m = home_stack(&summary(days), 7);
        assert_eq!(m.days.len(), 7);
        assert_eq!(m.average, "7일 평균 4.0K");
        assert_eq!(m.days[0].day, "2026-11-03");
    }

    #[test]
    fn fr_l23_stack_captions_name_utc_and_average() {
        let m = home_stack(&summary(vec![day("2026-11-06", 1, 1, 1)]), 7);
        assert_eq!(m.axis_caption, "(UTC 기준)");
        assert_eq!(m.average_legend, "7일 평균");
        assert_eq!(RETRY, "다시 시도");
    }

    #[test]
    fn fr_u7_weekday_names_from_utc_days() {
        assert_eq!(weekday_name("2025-10-25"), "토");
        assert_eq!(weekday_name("2026-11-06"), "금");
        assert_eq!(weekday_name("1970-01-01"), "목");
        assert_eq!(weekday_name("nope"), "");
    }
}
