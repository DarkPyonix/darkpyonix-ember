//! The computer monitor sheet (FR-L21; `docs/DESIGN.md` section 6.9).
//!
//! The server serves one metrics sample per request (FR-X7, `GET /api/computers/{id}/metrics`).
//! The sheet polls it while open and keeps a [`MonitorFeed`]: the last two samples (network
//! counters become rates by differencing them) and a short rolling series per card. A card
//! the node cannot fill says why; fans are not collected yet and say "준비 중입니다". The
//! functions below are pure so that rates, formats, sort and empty states are tested without a
//! window. The temperature meter shows the hottest sensor on a fixed 0 to 100 degree Celsius
//! scale (memory and disk meters use their percent).

use ember_client::api::{MemoryMetrics, MetricsSample};
use ember_client::state::Reachability;

use crate::ui::project::{Member, ProjectBox};

/// The metric cards of the monitor, in display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Metric {
    Cpu,
    Gpu,
    Memory,
    Temperature,
    Disk,
    Network,
    Fans,
}

impl Metric {
    pub const ALL: [Metric; 7] = [
        Metric::Cpu,
        Metric::Gpu,
        Metric::Memory,
        Metric::Temperature,
        Metric::Disk,
        Metric::Network,
        Metric::Fans,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Metric::Cpu => "CPU",
            Metric::Gpu => "GPU",
            Metric::Memory => "메모리",
            Metric::Temperature => "온도",
            Metric::Disk => "디스크",
            Metric::Network => "네트워크",
            Metric::Fans => "팬",
        }
    }
}

/// Where the live values come from. Nothing implements it with real data until ember node
/// reports them (FR-X7). `None` means "not reported", never an estimate.
pub trait ComputerMetrics {
    /// The latest samples of one metric, oldest first.
    fn series(&self, metric: Metric) -> Option<Vec<f32>>;
    /// The computer's process list.
    fn processes(&self) -> Option<Vec<ProcessRow>>;
}

/// The source used today: reports nothing.
pub struct NoMetrics;

impl ComputerMetrics for NoMetrics {
    fn series(&self, _metric: Metric) -> Option<Vec<f32>> {
        None
    }
    fn processes(&self) -> Option<Vec<ProcessRow>> {
        None
    }
}

/// Samples kept per graph: one minute at one sample a second.
pub const SERIES_CAP: usize = 60;

/// Fold new samples into a rolling series: append, drop non-finite values, keep the newest `cap`.
pub fn fold_samples(prev: &[f32], incoming: &[f32], cap: usize) -> Vec<f32> {
    let mut out: Vec<f32> = prev
        .iter()
        .chain(incoming.iter())
        .copied()
        .filter(|v| v.is_finite())
        .collect();
    if out.len() > cap {
        out.drain(..out.len() - cap);
    }
    out
}

/// One row of the process table.
#[derive(Debug, Clone, PartialEq)]
pub struct ProcessRow {
    pub pid: Option<u32>,
    pub name: String,
    /// CPU percent, when reported.
    pub cpu: Option<f32>,
    /// Resident memory in bytes, when reported.
    pub mem: Option<u64>,
    /// The session of a member's agent process (FR-L21: the row opens it).
    pub session: Option<String>,
    pub agent: String,
    pub leader: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Name,
    Pid,
    Cpu,
    Memory,
}

/// The table's sort, kept by the sheet across updates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SortState {
    pub key: SortKey,
    pub descending: bool,
}

impl Default for SortState {
    fn default() -> Self {
        SortState {
            key: SortKey::Cpu,
            descending: true,
        }
    }
}

/// A click on a header: the same key flips the direction; a new key starts descending for CPU
/// and memory and ascending for name and PID.
pub fn toggle_sort(state: SortState, key: SortKey) -> SortState {
    if state.key == key {
        SortState {
            key,
            descending: !state.descending,
        }
    } else {
        SortState {
            key,
            descending: matches!(key, SortKey::Cpu | SortKey::Memory),
        }
    }
}

/// Sort rows by the state. Rows without the value go last in either direction; ties break by
/// name, then PID, so rows do not jump between updates.
pub fn sort_rows(rows: &mut [ProcessRow], state: SortState) {
    use std::cmp::Ordering;
    // `None` goes last in both directions, so the direction applies to known values only.
    fn known<T: PartialOrd>(a: &Option<T>, b: &Option<T>, desc: bool) -> Ordering {
        match (a, b) {
            (Some(x), Some(y)) => {
                let o = x.partial_cmp(y).unwrap_or(Ordering::Equal);
                if desc {
                    o.reverse()
                } else {
                    o
                }
            }
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        }
    }
    let desc = state.descending;
    rows.sort_by(|a, b| {
        let primary = match state.key {
            SortKey::Name => {
                let o = a.name.cmp(&b.name);
                if desc {
                    o.reverse()
                } else {
                    o
                }
            }
            SortKey::Pid => known(&a.pid, &b.pid, desc),
            SortKey::Cpu => known(&a.cpu, &b.cpu, desc),
            SortKey::Memory => known(&a.mem, &b.mem, desc),
        };
        primary
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.pid.cmp(&b.pid))
    });
}

/// Members of the projects assigned to a computer, once each. TODO(FR-X7): the client does not
/// know which computer a session runs on, so this is every member of the assigned projects.
pub fn computer_members(projects: &[String], boxes: &[ProjectBox]) -> Vec<Member> {
    let mut out: Vec<Member> = Vec::new();
    for b in boxes.iter().filter(|b| projects.contains(&b.id)) {
        for m in &b.members {
            if !out.iter().any(|o| o.session_id == m.session_id) {
                out.push(m.clone());
            }
        }
    }
    out
}

/// Agent process rows for members; CPU, memory and PID are unknown until FR-X7.
pub fn agent_rows(members: &[Member]) -> Vec<ProcessRow> {
    members
        .iter()
        .map(|m| ProcessRow {
            pid: None,
            name: m.name.clone(),
            cpu: None,
            mem: None,
            session: Some(m.session_id.clone()),
            agent: m.agent.clone(),
            leader: m.leader,
        })
        .collect()
}

/// The line under the title: the online state, and for a computer that is not online the notice
/// and what is not known (the client keeps no last connection time yet).
pub fn offline_notice(r: Reachability) -> Option<&'static str> {
    match r {
        Reachability::Online => None,
        Reachability::Offline => Some("연결이 끊겼습니다"),
        Reachability::Unknown => Some("연결 상태를 알 수 없습니다"),
    }
}

/// Cards per row: 2 wide or medium, 1 on a phone. TODO(GAPS: 창 폭 레이아웃): 4 at 1280 and up.
pub fn card_columns(compact: bool) -> usize {
    if compact {
        1
    } else {
        2
    }
}

/// CPU percent text for a reported value; a missing one is a dash.
pub fn cpu_text(v: Option<f32>) -> String {
    v.map_or_else(|| "-".to_string(), |v| format!("{v:.1}%"))
}

/// Memory text in MB or GB for a reported value; a missing one is a dash.
pub fn mem_text(v: Option<u64>) -> String {
    match v {
        None => "-".to_string(),
        Some(b) if b >= 1 << 30 => format!("{:.1} GB", b as f64 / (1u64 << 30) as f64),
        Some(b) => format!("{} MB", b >> 20),
    }
}

// ---------------------------------------------------------------------------------------------
// Live feed (FR-X7, FR-L21)
// ---------------------------------------------------------------------------------------------

/// Seconds between two requests while the sheet is open.
pub const POLL_SECS: u64 = 3;
/// Processes asked for (`?top=`).
pub const TOP_PROCESSES: u32 = 30;
/// Samples kept per card graph: three minutes at one request every [`POLL_SECS`].
pub const FEED_CAP: usize = 60;

/// Bytes per second from two cumulative counters `dt_ms` apart. A counter that went down (the
/// interface was reset or the node restarted) gives 0, never a huge wrapped number.
pub fn rate_per_sec(prev: u64, cur: u64, dt_ms: u64) -> u64 {
    if dt_ms == 0 || cur < prev {
        return 0;
    }
    ((cur - prev) as u128 * 1000 / dt_ms as u128) as u64
}

/// Receive and send bytes per second over all interfaces present in both samples. `None` when
/// the samples are not in time order, so there is nothing to difference.
pub fn net_rates(prev: &MetricsSample, cur: &MetricsSample) -> Option<(u64, u64)> {
    if cur.at_ms <= prev.at_ms {
        return None;
    }
    let dt = cur.at_ms - prev.at_ms;
    let (mut rx, mut tx) = (0u64, 0u64);
    for n in &cur.network {
        if let Some(p) = prev.network.iter().find(|p| p.interface == n.interface) {
            rx = rx.saturating_add(rate_per_sec(p.rx_bytes, n.rx_bytes, dt));
            tx = tx.saturating_add(rate_per_sec(p.tx_bytes, n.tx_bytes, dt));
        }
    }
    Some((rx, tx))
}

/// A percent for display: one decimal and `%`; a missing or non-finite one is a dash.
pub fn percent_text(v: Option<f32>) -> String {
    match v {
        Some(v) if v.is_finite() => format!("{v:.1}%"),
        _ => "-".to_string(),
    }
}

/// A byte count with a binary unit (1024), one decimal from KB up.
pub fn bytes_text(b: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if b < 1024 {
        return format!("{b} B");
    }
    let mut v = b as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    format!("{v:.1} {}", UNITS[u])
}

/// A byte rate: [`bytes_text`] and `/s`.
pub fn rate_text(bytes_per_sec: u64) -> String {
    format!("{}/s", bytes_text(bytes_per_sec))
}

/// `part` of `whole` as a percent; 0 when `whole` is 0.
pub fn ratio_percent(part: u64, whole: u64) -> f32 {
    if whole == 0 {
        0.0
    } else {
        (part as f64 * 100.0 / whole as f64).min(100.0) as f32
    }
}

/// Used percent of a disk, from total and available bytes.
pub fn disk_used_percent(total: u64, available: u64) -> f32 {
    ratio_percent(total.saturating_sub(available), total)
}

/// Where the feed is, for the cards' empty text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedStatus {
    /// Nothing asked or answered yet.
    Waiting,
    /// The latest request succeeded.
    Live,
    /// The latest request failed; the last values (if any) are kept.
    Failed,
}

/// The last two samples, a rolling series per card and the last error.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MonitorFeed {
    latest: Option<MetricsSample>,
    prev: Option<MetricsSample>,
    error: Option<String>,
    cpu: Vec<f32>,
    memory: Vec<f32>,
    temperature: Vec<f32>,
    net_rx: Vec<f32>,
}

impl MonitorFeed {
    /// Take a sample. One that is not newer than the latest is dropped (a late answer).
    pub fn push(&mut self, sample: MetricsSample) {
        if self.latest.as_ref().is_some_and(|l| sample.at_ms <= l.at_ms) {
            self.error = None;
            return;
        }
        let rates = self.latest.as_ref().and_then(|l| net_rates(l, &sample));
        let cpu = sample.cpu.total_percent.into_iter().collect::<Vec<_>>();
        let mem = sample.memory.iter().map(|m| ratio_percent(m.used_bytes, m.total_bytes)).collect::<Vec<_>>();
        let temp = hottest(&sample).map(|t| t.celsius).into_iter().collect::<Vec<_>>();
        self.cpu = fold_samples(&self.cpu, &cpu, FEED_CAP);
        self.memory = fold_samples(&self.memory, &mem, FEED_CAP);
        self.temperature = fold_samples(&self.temperature, &temp, FEED_CAP);
        if let Some((rx, _)) = rates {
            self.net_rx = fold_samples(&self.net_rx, &[rx as f32], FEED_CAP);
        }
        self.prev = self.latest.take();
        self.latest = Some(sample);
        self.error = None;
    }

    /// A request failed. The last values stay so the sheet can show them dimmed.
    pub fn fail(&mut self, message: impl Into<String>) {
        self.error = Some(message.into());
    }

    pub fn status(&self) -> FeedStatus {
        if self.error.is_some() {
            FeedStatus::Failed
        } else if self.latest.is_some() {
            FeedStatus::Live
        } else {
            FeedStatus::Waiting
        }
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn latest(&self) -> Option<&MetricsSample> {
        self.latest.as_ref()
    }

    /// Network rates between the last two samples; `None` until there are two.
    pub fn rates(&self) -> Option<(u64, u64)> {
        net_rates(self.prev.as_ref()?, self.latest.as_ref()?)
    }
}

impl ComputerMetrics for MonitorFeed {
    fn series(&self, metric: Metric) -> Option<Vec<f32>> {
        let v = match metric {
            Metric::Cpu => &self.cpu,
            Metric::Memory => &self.memory,
            Metric::Temperature => &self.temperature,
            Metric::Network => &self.net_rx,
            _ => return None,
        };
        (!v.is_empty()).then(|| v.clone())
    }

    fn processes(&self) -> Option<Vec<ProcessRow>> {
        self.latest.as_ref().map(process_rows)
    }
}

fn hottest(s: &MetricsSample) -> Option<&ember_client::api::TempMetrics> {
    s.temperatures
        .iter()
        .filter(|t| t.celsius.is_finite())
        .max_by(|a, b| a.celsius.partial_cmp(&b.celsius).unwrap_or(std::cmp::Ordering::Equal))
}

/// The sample's processes as table rows. The client cannot tell an agent process from another
/// yet (FR-X7), so none is linked to a session.
pub fn process_rows(s: &MetricsSample) -> Vec<ProcessRow> {
    s.processes
        .iter()
        .map(|p| ProcessRow {
            pid: Some(p.pid),
            name: p.name.clone(),
            cpu: Some(p.cpu_percent),
            mem: Some(p.memory_bytes),
            session: None,
            agent: String::new(),
            leader: false,
        })
        .collect()
}

/// What one card shows.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CardView {
    /// The big value; a dash when nothing is known.
    pub headline: String,
    pub lines: Vec<String>,
    /// Bars, 0 to 100 each: the cores for CPU, the recent series for the others.
    pub bars: Vec<f32>,
}

fn memory_lines(m: &MemoryMetrics) -> Vec<String> {
    let mut lines = vec![format!("{} / {}", bytes_text(m.used_bytes), bytes_text(m.total_bytes))];
    if m.swap_total_bytes > 0 {
        lines.push(format!(
            "스왑 {} / {}",
            bytes_text(m.swap_used_bytes),
            bytes_text(m.swap_total_bytes)
        ));
    } else {
        lines.push("스왑 없음".to_string());
    }
    lines
}

/// The values of one card from the feed, or `None` when the feed has no sample (the card then
/// shows [`empty_text`]).
pub fn card_view(metric: Metric, feed: &MonitorFeed) -> Option<CardView> {
    if metric == Metric::Fans {
        return None;
    }
    let s = feed.latest()?;
    Some(match metric {
        Metric::Cpu => CardView {
            headline: percent_text(s.cpu.total_percent),
            lines: vec![format!("코어 {}개", s.cpu.cores.len())],
            bars: s.cpu.cores.clone(),
        },
        Metric::Memory => match &s.memory {
            Some(m) => CardView {
                headline: percent_text(Some(ratio_percent(m.used_bytes, m.total_bytes))),
                lines: memory_lines(m),
                bars: vec![],
            },
            None => CardView {
                headline: "-".into(),
                lines: vec!["이 컴퓨터가 메모리 값을 알려주지 않습니다".into()],
                bars: vec![],
            },
        },
        Metric::Disk => {
            let worst = s
                .disks
                .iter()
                .map(|d| disk_used_percent(d.total_bytes, d.available_bytes))
                .fold(None, |a: Option<f32>, v| Some(a.map_or(v, |a| a.max(v))));
            let mut lines: Vec<String> = s
                .disks
                .iter()
                .take(3)
                .map(|d| {
                    let used = d.total_bytes.saturating_sub(d.available_bytes);
                    if s.disks.len() == 1 {
                        format!("{} / {} 사용 중입니다", bytes_text(used), bytes_text(d.total_bytes))
                    } else {
                        format!(
                            "{} {} ({} / {})",
                            d.mount,
                            percent_text(Some(disk_used_percent(d.total_bytes, d.available_bytes))),
                            bytes_text(used),
                            bytes_text(d.total_bytes)
                        )
                    }
                })
                .collect();
            if s.disks.len() > 3 {
                lines.push(format!("외 {}개", s.disks.len() - 3));
            }
            if s.disks.is_empty() {
                lines.push("디스크를 찾지 못했습니다".into());
            }
            CardView { headline: percent_text(worst), lines, bars: vec![] }
        }
        Metric::Network => match feed.rates() {
            Some((rx, tx)) => CardView {
                headline: format!("\u{2193} {}", rate_text(rx)),
                lines: vec![format!("\u{2191} {}", rate_text(tx))],
                bars: feed.series(Metric::Network).filter(|b| b.len() >= 2).unwrap_or_default(),
            },
            None => CardView {
                headline: "-".into(),
                lines: vec!["속도를 계산하는 중입니다".into()],
                bars: vec![],
            },
        },
        Metric::Temperature => match hottest(s) {
            Some(t) => CardView {
                headline: format!("{:.1} \u{00B0}C", t.celsius),
                lines: s
                    .temperatures
                    .iter()
                    .take(3)
                    .map(|t| format!("{} {:.1} \u{00B0}C", t.label, t.celsius))
                    .collect(),
                bars: vec![],
            },
            None => CardView {
                headline: "-".into(),
                lines: vec!["이 컴퓨터가 온도를 알려주지 않습니다".into()],
                bars: vec![],
            },
        },
        Metric::Gpu => {
            if s.gpu.devices.is_empty() {
                CardView {
                    headline: "-".into(),
                    lines: vec![s
                        .gpu
                        .reason
                        .clone()
                        .filter(|r| !r.is_empty())
                        .unwrap_or_else(|| "GPU 값을 읽지 못했습니다".into())],
                    bars: vec![],
                }
            } else {
                let d = &s.gpu.devices[0];
                let mut lines = vec![d.name.clone()];
                if let (Some(u), Some(t)) = (d.memory_used_bytes, d.memory_total_bytes) {
                    lines.push(format!("{} / {}", bytes_text(u), bytes_text(t)));
                }
                CardView { headline: percent_text(d.utilization_percent), lines, bars: vec![] }
            }
        }
        Metric::Fans => return None,
    })
}

/// What a card says when [`card_view`] has nothing: the computer is offline, the first answer
/// is pending, the request failed, or (fans) the node does not collect it yet.
pub fn empty_text(metric: Metric, online: bool, status: FeedStatus) -> &'static str {
    if metric == Metric::Fans {
        "준비 중입니다"
    } else if !online {
        "연결이 끊겨 값을 읽을 수 없습니다"
    } else if status == FeedStatus::Failed {
        "지표를 읽지 못했습니다"
    } else {
        "불러오는 중입니다"
    }
}

/// The status card's line: the process count, or the reason there is none.
pub fn status_line(feed: &MonitorFeed, online: bool) -> String {
    match (feed.latest(), online) {
        (Some(s), true) => format!("프로세스 {}개 실행 중입니다", s.process_count),
        (_, false) => "연결이 끊겨 값을 읽을 수 없습니다".to_string(),
        (None, true) if feed.status() == FeedStatus::Failed => "지표를 읽지 못했습니다".to_string(),
        (None, true) => "불러오는 중입니다".to_string(),
    }
}

/// The note under the process table.
pub fn process_footer(feed: &MonitorFeed) -> String {
    match feed.latest() {
        Some(s) if !s.processes.is_empty() => format!(
            "CPU 사용량이 높은 상위 {}개 프로세스를 표시합니다. 전체는 {}개입니다.",
            s.processes.len(),
            s.process_count
        ),
        _ => "프로세스 목록은 팀원의 세션만 표시합니다.".to_string(),
    }
}

/// Bar height as a share of the card: 0 to 100 clamped; non-finite is 0. A series without a
/// natural 0 to 100 range (network) is scaled by its own maximum.
pub fn bar_fraction(v: f32, max: f32) -> f32 {
    if !v.is_finite() || max <= 0.0 {
        0.0
    } else {
        (v / max).clamp(0.0, 1.0)
    }
}

/// The fill of a card's meter, 0 to 1, for the cards that show one value against a track:
/// memory and disk by their percent, temperature on a 0 to 100 degree scale. `None` for the others.
pub fn meter_fraction(metric: Metric, feed: &MonitorFeed) -> Option<f32> {
    let s = feed.latest()?;
    let pct = match metric {
        Metric::Memory => s.memory.as_ref().map(|m| ratio_percent(m.used_bytes, m.total_bytes))?,
        Metric::Disk => s
            .disks
            .iter()
            .map(|d| disk_used_percent(d.total_bytes, d.available_bytes))
            .fold(None, |a: Option<f32>, v| Some(a.map_or(v, |a| a.max(v))))?,
        Metric::Temperature => hottest(s)?.celsius,
        _ => return None,
    };
    Some(bar_fraction(pct, 100.0))
}

/// The scale of a card's bars: 100 for percents, the series maximum for the network card.
pub fn bar_max(metric: Metric, bars: &[f32]) -> f32 {
    if metric == Metric::Network {
        bars.iter().copied().filter(|v| v.is_finite()).fold(1.0, f32::max)
    } else {
        100.0
    }
}

// ---------------------------------------------------------------------------------------------
// Status score and fan mode (FR-L21, #389; mockup monitor.js `score`, `scoreWord`, fan card)
// ---------------------------------------------------------------------------------------------

/// The main disk: the `/` mount, else the largest one. `None` without disks.
pub fn main_disk(s: &MetricsSample) -> Option<&ember_client::api::DiskMetrics> {
    s.disks
        .iter()
        .find(|d| d.mount == "/")
        .or_else(|| s.disks.iter().filter(|d| d.total_bytes > 0).max_by_key(|d| d.total_bytes))
}

/// The health score, 0 to 100, from one sample with the mockup's formula: 100, minus 0.25 per
/// CPU percent, minus 0.6 per memory percent above 60, minus 0.8 per degree above 70 on the
/// hottest sensor, minus 12 when the main disk has less than 15 percent free. A value the node
/// did not report takes nothing off; without a CPU total there is no score.
pub fn health_score(s: &MetricsSample) -> Option<u8> {
    let cpu = s.cpu.total_percent.filter(|v| v.is_finite())?;
    let mem = s.memory.as_ref().filter(|m| m.total_bytes > 0).map(|m| ratio_percent(m.used_bytes, m.total_bytes));
    let temp = hottest(s).map(|t| t.celsius);
    let low_disk = main_disk(s)
        .filter(|d| d.total_bytes > 0)
        .is_some_and(|d| (d.available_bytes as f64 / d.total_bytes as f64) < 0.15);
    let mut v = 100.0 - cpu.clamp(0.0, 100.0) * 0.25;
    if let Some(m) = mem {
        v -= (m - 60.0).max(0.0) * 0.6;
    }
    if let Some(t) = temp {
        v -= (t - 70.0).max(0.0) * 0.8;
    }
    if low_disk {
        v -= 12.0;
    }
    Some(v.clamp(0.0, 100.0).round() as u8)
}

/// The word beside the score (mockup `scoreWord`).
pub fn score_word(n: u8) -> &'static str {
    match n {
        85.. => "아주 좋습니다",
        70..=84 => "좋습니다",
        50..=69 => "바쁩니다",
        _ => "점검이 필요합니다",
    }
}

/// The ring's tone: green from 70, orange from 50, red below.
pub fn score_tone(n: u8) -> crate::ui::components::PillTone {
    use crate::ui::components::PillTone;
    match n {
        70.. => PillTone::Green,
        50..=69 => PillTone::Orange,
        _ => PillTone::Red,
    }
}

/// The line under the score: whether the main disk has room, and how much is free.
pub fn storage_line(s: &MetricsSample) -> Option<String> {
    let d = main_disk(s).filter(|d| d.total_bytes > 0)?;
    let low = (d.available_bytes as f64 / d.total_bytes as f64) < 0.15;
    let word = if low { "저장 공간이 부족합니다" } else { "저장 공간이 넉넉합니다" };
    Some(format!("{word} \u{00B7} 여유 {}", bytes_text(d.available_bytes)))
}

/// The RAM size badge of the status card, from the memory total.
pub fn ram_badge(s: &MetricsSample) -> Option<String> {
    s.memory.as_ref().filter(|m| m.total_bytes > 0).map(|m| bytes_text(m.total_bytes))
}

/// The fan modes of the mockup's fan card, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FanMode {
    Auto,
    Cool,
    Max,
}

impl FanMode {
    pub const ALL: [FanMode; 3] = [FanMode::Auto, FanMode::Cool, FanMode::Max];

    pub fn label(self) -> &'static str {
        match self {
            FanMode::Auto => "자동",
            FanMode::Cool => "냉각",
            FanMode::Max => "최대",
        }
    }
}

/// Whether the fan mode can be changed. The node neither reports fan speed nor sets a mode
/// (FR-X7), so the control is shown disabled. TODO(#389): enable when the node can.
pub const FAN_MODE_AVAILABLE: bool = false;

/// Why the fan mode control is disabled.
pub const FAN_MODE_REASON: &str = "이 컴퓨터의 팬 속도와 모드는 아직 읽거나 바꿀 수 없습니다";

#[cfg(test)]
mod tests {
    use super::*;
    use ember_client::LauncherStatus;

    fn row(name: &str, pid: Option<u32>, cpu: Option<f32>, mem: Option<u64>) -> ProcessRow {
        ProcessRow {
            pid,
            name: name.into(),
            cpu,
            mem,
            session: None,
            agent: "claude-code".into(),
            leader: false,
        }
    }

    fn member(id: &str, leader: bool) -> Member {
        Member {
            session_id: id.into(),
            name: format!("m-{id}"),
            task: String::new(),
            agent: "claude-code".into(),
            status: LauncherStatus::Running,
            leader,
            updated_at: 0,
        }
    }

    fn names(rows: &[ProcessRow]) -> Vec<&str> {
        rows.iter().map(|r| r.name.as_str()).collect()
    }

    #[test]
    fn fold_appends_and_keeps_the_newest() {
        assert_eq!(fold_samples(&[1.0, 2.0], &[3.0], 5), vec![1.0, 2.0, 3.0]);
        assert_eq!(
            fold_samples(&[1.0, 2.0, 3.0], &[4.0, 5.0], 3),
            vec![3.0, 4.0, 5.0]
        );
        assert_eq!(fold_samples(&[], &[1.0, 2.0, 3.0], 2), vec![2.0, 3.0]);
    }

    #[test]
    fn fold_drops_non_finite_and_handles_zero_cap() {
        assert_eq!(
            fold_samples(&[1.0], &[f32::NAN, f32::INFINITY, 2.0], 5),
            vec![1.0, 2.0]
        );
        assert!(fold_samples(&[1.0], &[2.0], 0).is_empty());
    }

    #[test]
    fn no_metrics_reports_nothing() {
        for m in Metric::ALL {
            assert!(NoMetrics.series(m).is_none());
        }
        assert!(NoMetrics.processes().is_none());
    }

    #[test]
    fn toggle_flips_the_same_key_and_resets_a_new_one() {
        let s = SortState::default();
        assert_eq!((s.key, s.descending), (SortKey::Cpu, true));
        let s = toggle_sort(s, SortKey::Cpu);
        assert!(!s.descending);
        let s = toggle_sort(s, SortKey::Name);
        assert_eq!((s.key, s.descending), (SortKey::Name, false));
        let s = toggle_sort(s, SortKey::Memory);
        assert_eq!((s.key, s.descending), (SortKey::Memory, true));
    }

    #[test]
    fn sort_by_cpu_descending_puts_unknown_last() {
        let mut rows = vec![
            row("a", Some(1), None, None),
            row("b", Some(2), Some(5.0), None),
            row("c", Some(3), Some(50.0), None),
        ];
        sort_rows(
            &mut rows,
            SortState {
                key: SortKey::Cpu,
                descending: true,
            },
        );
        assert_eq!(names(&rows), ["c", "b", "a"]);
        sort_rows(
            &mut rows,
            SortState {
                key: SortKey::Cpu,
                descending: false,
            },
        );
        assert_eq!(names(&rows), ["b", "c", "a"]);
    }

    #[test]
    fn sort_keeps_its_state_across_updates() {
        let state = toggle_sort(SortState::default(), SortKey::Name);
        let mut first = vec![row("b", None, None, None), row("a", None, None, None)];
        sort_rows(&mut first, state);
        let mut second = vec![
            row("c", None, None, None),
            row("a", None, None, None),
            row("b", None, None, None),
        ];
        sort_rows(&mut second, state);
        assert_eq!(names(&first), ["a", "b"]);
        assert_eq!(names(&second), ["a", "b", "c"]);
    }

    #[test]
    fn equal_values_break_ties_by_name_then_pid() {
        let mut rows = vec![
            row("x", Some(9), Some(1.0), Some(5)),
            row("x", Some(3), Some(1.0), Some(5)),
            row("a", Some(7), Some(1.0), Some(5)),
        ];
        sort_rows(
            &mut rows,
            SortState {
                key: SortKey::Memory,
                descending: true,
            },
        );
        assert_eq!(
            rows.iter().map(|r| r.pid).collect::<Vec<_>>(),
            [Some(7), Some(3), Some(9)]
        );
    }

    #[test]
    fn members_of_assigned_projects_appear_once() {
        let mk = |id: &str, ms: Vec<Member>| ProjectBox {
            id: id.into(),
            name: id.into(),
            members: ms,
            running: 0,
            waiting: 0,
            computers: vec![],
            top: None,
            repo: None,
            branch: None,
        };
        let boxes = vec![
            mk("p1", vec![member("s1", true), member("s2", false)]),
            mk("p2", vec![member("s2", false), member("s3", false)]),
            mk("p3", vec![member("s4", false)]),
        ];
        let got = computer_members(&["p1".into(), "p2".into()], &boxes);
        let ids: Vec<&str> = got.iter().map(|m| m.session_id.as_str()).collect();
        assert_eq!(ids, ["s1", "s2", "s3"]);
        assert!(computer_members(&[], &boxes).is_empty());
    }

    #[test]
    fn agent_rows_carry_session_agent_and_leader() {
        let rows = agent_rows(&[member("s1", true), member("s2", false)]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].session.as_deref(), Some("s1"));
        assert!(rows[0].leader && !rows[1].leader);
        assert_eq!(rows[0].name, "m-s1");
        assert_eq!(rows[0].pid, None);
        assert_eq!(rows[0].cpu, None);
    }

    // NFR-L4: notices in 합니다체.
    #[test]
    fn offline_notice_only_when_not_online() {
        assert_eq!(offline_notice(Reachability::Online), None);
        assert_eq!(
            offline_notice(Reachability::Offline),
            Some("연결이 끊겼습니다")
        );
        assert_eq!(
            offline_notice(Reachability::Unknown),
            Some("연결 상태를 알 수 없습니다")
        );
    }

    #[test]
    fn value_text_uses_a_dash_for_unknown() {
        assert_eq!(cpu_text(None), "-");
        assert_eq!(cpu_text(Some(12.34)), "12.3%");
        assert_eq!(mem_text(None), "-");
        assert_eq!(mem_text(Some(512 << 20)), "512 MB");
        assert_eq!(mem_text(Some(3 << 30)), "3.0 GB");
    }

    #[test]
    fn card_columns_by_width() {
        assert_eq!(card_columns(false), 2);
        assert_eq!(card_columns(true), 1);
    }

    // ---- FR-X7 / FR-L21 live feed ----

    use ember_client::api::{CpuMetrics, DiskMetrics, GpuReport, NetMetrics, ProcessMetrics, TempMetrics};

    fn sample(at_ms: u64, rx: u64, tx: u64) -> MetricsSample {
        MetricsSample {
            at_ms,
            cpu: CpuMetrics { total_percent: Some(25.0), cores: vec![10.0, 40.0] },
            memory: Some(MemoryMetrics {
                total_bytes: 16 << 30,
                used_bytes: 8 << 30,
                swap_total_bytes: 4 << 30,
                swap_used_bytes: 1 << 30,
            }),
            disks: vec![DiskMetrics { name: "d".into(), mount: "/".into(), total_bytes: 100, available_bytes: 40 }],
            network: vec![NetMetrics { interface: "en0".into(), rx_bytes: rx, tx_bytes: tx }],
            temperatures: vec![TempMetrics { label: "cpu".into(), celsius: 51.5 }],
            gpu: GpuReport { devices: vec![], reason: Some("GPU를 읽을 수 없습니다".into()) },
            processes: vec![ProcessMetrics { pid: 7, name: "node".into(), cpu_percent: 3.0, memory_bytes: 1 << 20 }],
            process_count: 321,
        }
    }

    #[test]
    fn fr_x7_rates_difference_two_counters() {
        assert_eq!(rate_per_sec(1000, 4000, 1000), 3000);
        assert_eq!(rate_per_sec(0, 1000, 2000), 500);
    }

    #[test]
    fn fr_x7_a_counter_reset_or_zero_interval_gives_zero() {
        assert_eq!(rate_per_sec(5000, 100, 1000), 0);
        assert_eq!(rate_per_sec(100, 5000, 0), 0);
    }

    #[test]
    fn fr_x7_net_rates_sum_interfaces_present_in_both_samples() {
        let a = sample(1000, 1000, 500);
        let mut b = sample(3000, 5000, 1500);
        b.network.push(NetMetrics { interface: "utun".into(), rx_bytes: 9_999_999, tx_bytes: 9_999_999 });
        assert_eq!(net_rates(&a, &b), Some((2000, 500)));
        assert_eq!(net_rates(&b, &a), None);
        let reset = sample(4000, 10, 10);
        assert_eq!(net_rates(&b, &reset), Some((0, 0)));
    }

    #[test]
    fn fr_l21_percent_and_byte_text() {
        assert_eq!(percent_text(Some(12.34)), "12.3%");
        assert_eq!(percent_text(None), "-");
        assert_eq!(percent_text(Some(f32::NAN)), "-");
        assert_eq!(bytes_text(512), "512 B");
        assert_eq!(bytes_text(1536), "1.5 KB");
        assert_eq!(bytes_text(3 << 30), "3.0 GB");
        assert_eq!(bytes_text(5 << 40), "5.0 TB");
        assert_eq!(rate_text(2 << 20), "2.0 MB/s");
        assert_eq!(disk_used_percent(100, 40), 60.0);
        assert_eq!(disk_used_percent(0, 0), 0.0);
    }

    #[test]
    fn fr_l21_feed_keeps_two_samples_and_a_rolling_series() {
        let mut f = MonitorFeed::default();
        assert_eq!(f.status(), FeedStatus::Waiting);
        f.push(sample(1000, 0, 0));
        assert_eq!(f.status(), FeedStatus::Live);
        assert_eq!(f.rates(), None);
        f.push(sample(2000, 1000, 100));
        assert_eq!(f.rates(), Some((1000, 100)));
        assert_eq!(f.series(Metric::Cpu), Some(vec![25.0, 25.0]));
        assert_eq!(f.series(Metric::Network), Some(vec![1000.0]));
        assert_eq!(f.series(Metric::Fans), None);
        for i in 0..(FEED_CAP as u64 + 10) {
            f.push(sample(3000 + i, 0, 0));
        }
        assert_eq!(f.series(Metric::Cpu).unwrap().len(), FEED_CAP);
    }

    #[test]
    fn fr_l21_a_late_sample_is_dropped() {
        let mut f = MonitorFeed::default();
        f.push(sample(2000, 0, 0));
        f.push(sample(1000, 0, 0));
        assert_eq!(f.latest().unwrap().at_ms, 2000);
    }

    #[test]
    fn fr_l21_a_failure_keeps_the_last_values_and_a_success_clears_it() {
        let mut f = MonitorFeed::default();
        f.fail("server answered 502");
        assert_eq!(f.status(), FeedStatus::Failed);
        assert!(card_view(Metric::Cpu, &f).is_none());
        f.push(sample(1000, 0, 0));
        f.fail("timeout");
        assert_eq!(f.status(), FeedStatus::Failed);
        assert!(card_view(Metric::Cpu, &f).is_some());
        f.push(sample(2000, 0, 0));
        assert_eq!(f.status(), FeedStatus::Live);
        assert_eq!(f.error(), None);
    }

    #[test]
    fn fr_l21_cards_show_live_values() {
        let mut f = MonitorFeed::default();
        f.push(sample(1000, 0, 0));
        f.push(sample(2000, 2048, 1024));
        let cpu = card_view(Metric::Cpu, &f).unwrap();
        assert_eq!(cpu.headline, "25.0%");
        assert_eq!(cpu.bars, vec![10.0, 40.0]);
        let mem = card_view(Metric::Memory, &f).unwrap();
        assert_eq!(mem.headline, "50.0%");
        assert_eq!(mem.lines, vec!["8.0 GB / 16.0 GB".to_string(), "스왑 1.0 GB / 4.0 GB".to_string()]);
        let disk = card_view(Metric::Disk, &f).unwrap();
        assert_eq!(disk.headline, "60.0%");
        assert_eq!(disk.lines, vec!["60 B / 100 B 사용 중입니다".to_string()]);
        assert!(!disk.lines[0].contains('%'));
        let net = card_view(Metric::Network, &f).unwrap();
        assert_eq!(net.headline, "\u{2193} 2.0 KB/s");
        assert_eq!(net.lines, vec!["\u{2191} 1.0 KB/s".to_string()]);
        let t = card_view(Metric::Temperature, &f).unwrap();
        assert_eq!(t.headline, "51.5 \u{00B0}C");
    }

    #[test]
    fn fr_l21_the_gpu_card_shows_the_server_reason_when_it_has_no_device() {
        let mut f = MonitorFeed::default();
        f.push(sample(1000, 0, 0));
        let gpu = card_view(Metric::Gpu, &f).unwrap();
        assert_eq!(gpu.headline, "-");
        assert_eq!(gpu.lines, vec!["GPU를 읽을 수 없습니다".to_string()]);
    }

    #[test]
    fn fr_l21_first_sample_has_no_network_rate_and_fans_stay_unbuilt() {
        let mut f = MonitorFeed::default();
        f.push(sample(1000, 0, 0));
        assert_eq!(card_view(Metric::Network, &f).unwrap().lines, vec!["속도를 계산하는 중입니다".to_string()]);
        assert!(card_view(Metric::Fans, &f).is_none());
        assert_eq!(empty_text(Metric::Fans, true, FeedStatus::Live), "준비 중입니다");
    }

    #[test]
    fn fr_l21_empty_states_for_offline_waiting_and_failed() {
        assert_eq!(empty_text(Metric::Cpu, false, FeedStatus::Waiting), "연결이 끊겨 값을 읽을 수 없습니다");
        assert_eq!(empty_text(Metric::Cpu, true, FeedStatus::Waiting), "불러오는 중입니다");
        assert_eq!(empty_text(Metric::Cpu, true, FeedStatus::Failed), "지표를 읽지 못했습니다");
        let mut f = MonitorFeed::default();
        assert_eq!(status_line(&f, true), "불러오는 중입니다");
        f.fail("x");
        assert_eq!(status_line(&f, true), "지표를 읽지 못했습니다");
        f.push(sample(1, 0, 0));
        assert_eq!(status_line(&f, true), "프로세스 321개 실행 중입니다");
        assert_eq!(status_line(&f, false), "연결이 끊겨 값을 읽을 수 없습니다");
    }

    #[test]
    fn fr_l21_live_processes_sort_by_cpu_or_memory() {
        let mut s = sample(1, 0, 0);
        s.processes = vec![
            ProcessMetrics { pid: 1, name: "a".into(), cpu_percent: 1.0, memory_bytes: 900 },
            ProcessMetrics { pid: 2, name: "b".into(), cpu_percent: 9.0, memory_bytes: 100 },
            ProcessMetrics { pid: 3, name: "c".into(), cpu_percent: 5.0, memory_bytes: 500 },
        ];
        let mut rows = process_rows(&s);
        sort_rows(&mut rows, SortState { key: SortKey::Cpu, descending: true });
        assert_eq!(names(&rows), ["b", "c", "a"]);
        sort_rows(&mut rows, SortState { key: SortKey::Memory, descending: true });
        assert_eq!(names(&rows), ["a", "c", "b"]);
        assert_eq!(rows[0].pid, Some(1));
        assert!(rows.iter().all(|r| r.session.is_none()));
    }

    #[test]
    fn fr_l21_bar_scale() {
        assert_eq!(bar_fraction(50.0, 100.0), 0.5);
        assert_eq!(bar_fraction(f32::NAN, 100.0), 0.0);
        assert_eq!(bar_fraction(500.0, 100.0), 1.0);
        assert_eq!(bar_max(Metric::Cpu, &[1.0]), 100.0);
        assert_eq!(bar_max(Metric::Network, &[10.0, 40.0]), 40.0);
    }

    #[test]
    fn fr_l21_meter_fill_equals_the_value_and_clamps() {
        let mut f = MonitorFeed::default();
        assert_eq!(meter_fraction(Metric::Memory, &f), None);
        f.push(sample(1000, 0, 0));
        assert_eq!(meter_fraction(Metric::Memory, &f), Some(0.5));
        assert_eq!(meter_fraction(Metric::Disk, &f), Some(0.6));
        assert!((meter_fraction(Metric::Temperature, &f).unwrap() - 0.515).abs() < 1e-4);
        assert_eq!(meter_fraction(Metric::Cpu, &f), None);
        assert_eq!(bar_fraction(-5.0, 100.0), 0.0);
        assert_eq!(bar_fraction(250.0, 100.0), 1.0);
        let mem = card_view(Metric::Memory, &f).unwrap();
        assert!(mem.bars.is_empty(), "a single sample must not draw a full-width bar");
    }

    #[test]
    fn fr_l21_several_mounts_keep_their_rows() {
        let mut f = MonitorFeed::default();
        let mut s = sample(1000, 0, 0);
        let mut d2 = s.disks[0].clone();
        d2.mount = "/data".into();
        s.disks.push(d2);
        f.push(s);
        let disk = card_view(Metric::Disk, &f).unwrap();
        assert_eq!(disk.lines.len(), 2);
        assert!(disk.lines[1].starts_with("/data 60.0%"));
    }
    #[test]
    fn fr_l21_health_score_follows_the_mockup_formula() {
        // 25 % CPU, 50 % memory, 51.5 C, 40 % free: only the CPU takes points off.
        let s = sample(1, 0, 0);
        assert_eq!(health_score(&s), Some(94));
        let mut busy = sample(1, 0, 0);
        busy.cpu.total_percent = Some(80.0);
        busy.memory = Some(MemoryMetrics { total_bytes: 100, used_bytes: 90, swap_total_bytes: 0, swap_used_bytes: 0 });
        busy.temperatures = vec![TempMetrics { label: "cpu".into(), celsius: 85.0 }];
        busy.disks = vec![DiskMetrics { name: "d".into(), mount: "/".into(), total_bytes: 100, available_bytes: 10 }];
        // 100 - 20 - 18 - 12 - 12 = 38
        assert_eq!(health_score(&busy), Some(38));
        assert_eq!(score_word(38), "점검이 필요합니다");
    }

    #[test]
    fn fr_l21_health_score_needs_a_cpu_total_and_skips_unknown_values() {
        let mut s = sample(1, 0, 0);
        s.cpu.total_percent = None;
        assert_eq!(health_score(&s), None);
        let mut bare = sample(1, 0, 0);
        bare.memory = None;
        bare.temperatures.clear();
        bare.disks.clear();
        assert_eq!(health_score(&bare), Some(94));
        assert_eq!(storage_line(&bare), None);
        assert_eq!(ram_badge(&bare), None);
    }

    #[test]
    fn fr_l21_health_words_and_tones_by_band() {
        use crate::ui::components::PillTone;
        assert_eq!(score_word(100), "아주 좋습니다");
        assert_eq!(score_word(85), "아주 좋습니다");
        assert_eq!(score_word(84), "좋습니다");
        assert_eq!(score_word(70), "좋습니다");
        assert_eq!(score_word(69), "바쁩니다");
        assert_eq!(score_word(50), "바쁩니다");
        assert_eq!(score_word(49), "점검이 필요합니다");
        assert_eq!(score_tone(70), PillTone::Green);
        assert_eq!(score_tone(69), PillTone::Orange);
        assert_eq!(score_tone(49), PillTone::Red);
    }

    #[test]
    fn fr_l21_health_storage_line_reads_the_main_disk() {
        let mut s = sample(1, 0, 0);
        s.disks = vec![
            DiskMetrics { name: "data".into(), mount: "/data".into(), total_bytes: 8 << 30, available_bytes: 1 << 30 },
            DiskMetrics { name: "root".into(), mount: "/".into(), total_bytes: 4 << 30, available_bytes: 2 << 30 },
        ];
        assert_eq!(storage_line(&s).as_deref(), Some("저장 공간이 넉넉합니다 \u{00B7} 여유 2.0 GB"));
        s.disks = vec![DiskMetrics { name: "c".into(), mount: "C:\\".into(), total_bytes: 10 << 30, available_bytes: 1 << 30 }];
        assert_eq!(storage_line(&s).as_deref(), Some("저장 공간이 부족합니다 \u{00B7} 여유 1.0 GB"));
        assert_eq!(ram_badge(&s).as_deref(), Some("16.0 GB"));
    }

    #[test]
    fn fr_l21_fan_modes_match_the_mockup_and_stay_disabled() {
        let labels: Vec<_> = FanMode::ALL.iter().map(|m| m.label()).collect();
        assert_eq!(labels, ["자동", "냉각", "최대"]);
        assert!(!FAN_MODE_AVAILABLE);
        assert!(FAN_MODE_REASON.ends_with("없습니다"));
    }
}
