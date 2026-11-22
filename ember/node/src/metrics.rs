//! Computer metrics for the monitor sheet (SPEC FR-X7, FR-L21).
//!
//! One call to [`sample`] returns the CPU per core, memory, disks, network counters,
//! temperatures and the busiest processes, read through the `sysinfo` crate (macOS and Linux).
//! A metric the OS does not expose is `None` or an empty list, never an estimate. The GPU is
//! reported empty with a reason: no GPU API is verified on this node yet, and the node does
//! not shell out to a tool on a guess.
//!
//! The sampler keeps one `sysinfo` state between calls, because CPU percent is the change
//! since the previous refresh. The first call primes it and waits the minimum interval.
//!
//! The chip (the CPU brand `sysinfo` reads) and the OS name and version (`/usr/bin/sw_vers` on
//! macOS, `/etc/os-release` on Linux, nothing elsewhere) are read once, when the sampler starts
//! (#389). Process power is read on macOS only, from `proc_pid_rusage` with the `rusage_info_v6`
//! flavor (`ri_energy_nj`, nanojoules, in the SDK header `sys/resource.h`). Power is a rate, so
//! the sampler keeps each pid's last reading and reports watts from the change between two
//! samples; a pid's first sample, a counter that went down, and a process the call is refused
//! for (another user's) stay `None`. Other OSes keep `None` and say why ([`POWER_UNAVAILABLE`]).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use sysinfo::{Components, Disks, Networks, ProcessesToUpdate, System, MINIMUM_CPU_UPDATE_INTERVAL};

use crate::proto::*;

pub const DEFAULT_TOP: usize = 20;
pub const MAX_TOP: usize = 200;

/// Why the GPU list is empty.
pub const GPU_UNAVAILABLE: &str =
    "GPU metrics are not collected: no verified GPU API on this node";

/// Why every process's power is empty on an OS with no per-process energy API.
pub const POWER_UNAVAILABLE: &str =
    "process power is not collected: no verified per-process energy API on this node";

/// Why no process has a power value yet on macOS: a rate needs two readings of a pid.
pub const POWER_WARMING_UP: &str =
    "process power needs two samples of a process; none has a rate yet";

/// Watts from two energy readings of one process: `prev_nj` (the earlier reading, if any),
/// `now_nj` and the nanoseconds between them. One nanojoule per nanosecond is one watt.
/// `None` for an unknown first reading, a counter that went down (a reset or a reused pid)
/// and no elapsed time.
pub fn watts_between(prev_nj: Option<u64>, now_nj: u64, elapsed_ns: u128) -> Option<f32> {
    let prev = prev_nj?;
    if now_nj < prev || elapsed_ns == 0 {
        return None;
    }
    let w = (now_nj - prev) as f64 / elapsed_ns as f64;
    w.is_finite().then_some(w as f32)
}

/// The energy counter of a process in nanojoules (`proc_pid_rusage`, `RUSAGE_INFO_V6`).
#[cfg(target_os = "macos")]
pub fn energy_nj(pid: u32) -> std::io::Result<u64> {
    // Field for field `struct rusage_info_v6` of the macOS SDK `sys/resource.h` (40 u64
    // before the energy counter at line 410, then 5 more and `ri_reserved[9]`). The `libc` crate stops at `rusage_info_v4`,
    // which has no energy counter.
    #[repr(C)]
    struct RusageInfoV6 {
        ri_uuid: [u8; 16],
        counters: [u64; 40],
        ri_energy_nj: u64,
        ri_penergy_nj: u64,
        tail: [u64; 5],
        ri_reserved: [u64; 9],
    }
    extern "C" {
        fn proc_pid_rusage(pid: libc::c_int, flavor: libc::c_int, buffer: *mut libc::c_void) -> libc::c_int;
    }
    const RUSAGE_INFO_V6: libc::c_int = 6;
    // SAFETY: an all-zero bit pattern is valid for this plain integer struct, and the call
    // writes at most one `rusage_info_v6` into the buffer we own.
    let mut info: RusageInfoV6 = unsafe { std::mem::zeroed() };
    let rc = unsafe { proc_pid_rusage(pid as libc::c_int, RUSAGE_INFO_V6, &mut info as *mut _ as *mut libc::c_void) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(info.ri_energy_nj)
}

struct Sampler {
    /// The last energy reading per pid (macOS), with when it was taken.
    energy: HashMap<u32, (u64, Instant)>,
    sys: System,
    chip: Option<String>,
    os_name: Option<String>,
    os_version: Option<String>,
}

/// A processor name with its runs of spaces folded to one (Intel brands pad with spaces);
/// `None` when nothing is left.
pub fn chip_name(brand: &str) -> Option<String> {
    let name = brand.split_whitespace().collect::<Vec<_>>().join(" ");
    (!name.is_empty()).then_some(name)
}

/// `NAME` and `VERSION_ID` (else `VERSION`) of an `os-release` file, unquoted; an empty value
/// is `None`.
pub fn os_release(text: &str) -> (Option<String>, Option<String>) {
    let field = |key: &str| {
        text.lines()
            .find_map(|l| l.trim().strip_prefix(key).and_then(|rest| rest.strip_prefix('=')))
            .map(|v| v.trim().trim_matches('"').trim_matches('\'').trim().to_string())
            .filter(|v| !v.is_empty())
    };
    (field("NAME"), field("VERSION_ID").or_else(|| field("VERSION")))
}

/// First line of `/usr/bin/sw_vers <arg>` (a fixed path, as `envinfo` runs it).
fn sw_vers(arg: &str) -> Option<String> {
    let out = std::process::Command::new("/usr/bin/sw_vers").arg(arg).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().next().unwrap_or("").trim().to_string();
    (!line.is_empty()).then_some(line)
}

/// The OS name and version of this computer, where the node knows how to read them.
fn os_about() -> (Option<String>, Option<String>) {
    if cfg!(target_os = "macos") {
        return (sw_vers("-productName"), sw_vers("-productVersion"));
    }
    if cfg!(target_os = "linux") {
        return std::fs::read_to_string("/etc/os-release")
            .or_else(|_| std::fs::read_to_string("/usr/lib/os-release"))
            .map(|t| os_release(&t))
            .unwrap_or((None, None));
    }
    (None, None)
}

static SAMPLER: Mutex<Option<Sampler>> = Mutex::new(None);

/// Clamp a percent into 0 to 100, mapping NaN to 0.
pub fn clamp_percent(v: f32) -> f32 {
    if v.is_nan() {
        0.0
    } else {
        v.clamp(0.0, 100.0)
    }
}

/// Busiest first (CPU, then memory, then PID for a stable order), cut to `top`.
pub fn rank_processes(mut all: Vec<ProcessMetrics>, top: usize) -> Vec<ProcessMetrics> {
    all.sort_by(|a, b| {
        b.cpu_percent
            .partial_cmp(&a.cpu_percent)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.memory_bytes.cmp(&a.memory_bytes))
            .then(a.pid.cmp(&b.pid))
    });
    all.truncate(top);
    all
}

/// The GPU part of a sample.
pub fn gpu_report() -> GpuReport {
    GpuReport { devices: Vec::new(), reason: Some(GPU_UNAVAILABLE.into()) }
}

#[cfg(any(test, target_os = "linux"))]
fn gpu_report_at(_drm: &std::path::Path) -> GpuReport {
    GpuReport { devices: Vec::new(), reason: Some(GPU_UNAVAILABLE.into()) }
}

/// Why power is empty, `None` when some process has a value.
fn power_reason(any_power: bool) -> Option<String> {
    if any_power {
        None
    } else if cfg!(target_os = "macos") {
        Some(POWER_WARMING_UP.into())
    } else {
        Some(POWER_UNAVAILABLE.into())
    }
}

/// Read one pid's energy, store it in `next` and return the watts since its reading in `prev`.
#[cfg(target_os = "macos")]
fn process_power(pid: u32, prev: &HashMap<u32, (u64, Instant)>, next: &mut HashMap<u32, (u64, Instant)>) -> Option<f32> {
    let nj = energy_nj(pid).ok()?;
    let now = Instant::now();
    next.insert(pid, (nj, now));
    let (p_nj, p_at) = prev.get(&pid).copied()?;
    watts_between(Some(p_nj), nj, now.duration_since(p_at).as_nanos())
}

#[cfg(not(target_os = "macos"))]
fn process_power(_pid: u32, _prev: &HashMap<u32, (u64, Instant)>, _next: &mut HashMap<u32, (u64, Instant)>) -> Option<f32> {
    None
}

/// Take one sample. Blocks (the first call sleeps ~200 ms); call it from the blocking pool.
pub fn sample(top: usize) -> MetricsSample {
    let top = top.min(MAX_TOP);
    let mut guard = SAMPLER.lock().unwrap_or_else(|e| e.into_inner());
    let sampler = guard.get_or_insert_with(|| {
        let mut sys = System::new();
        sys.refresh_cpu_usage();
        sys.refresh_processes(ProcessesToUpdate::All, true);
        std::thread::sleep(MINIMUM_CPU_UPDATE_INTERVAL);
        let chip = sys.cpus().first().and_then(|c| chip_name(c.brand()));
        let (os_name, os_version) = os_about();
        Sampler { energy: HashMap::new(), sys, chip, os_name, os_version }
    });
    let sys = &mut sampler.sys;
    sys.refresh_cpu_usage();
    sys.refresh_memory();
    sys.refresh_processes(ProcessesToUpdate::All, true);

    let cores: Vec<f32> = sys.cpus().iter().map(|c| clamp_percent(c.cpu_usage())).collect();
    let total_percent = if cores.is_empty() { None } else { Some(clamp_percent(sys.global_cpu_usage())) };
    let memory = (sys.total_memory() > 0).then(|| MemoryMetrics {
        total_bytes: sys.total_memory(),
        used_bytes: sys.used_memory(),
        swap_total_bytes: sys.total_swap(),
        swap_used_bytes: sys.used_swap(),
    });
    let disks = Disks::new_with_refreshed_list()
        .iter()
        .map(|d| DiskMetrics {
            name: d.name().to_string_lossy().into_owned(),
            mount: d.mount_point().to_string_lossy().into_owned(),
            total_bytes: d.total_space(),
            available_bytes: d.available_space(),
        })
        .collect();
    let network = Networks::new_with_refreshed_list()
        .iter()
        .map(|(name, d)| NetMetrics {
            interface: name.clone(),
            rx_bytes: d.total_received(),
            tx_bytes: d.total_transmitted(),
        })
        .collect();
    let temperatures = Components::new_with_refreshed_list()
        .iter()
        .filter_map(|c| c.temperature().map(|t| TempMetrics { label: c.label().to_string(), celsius: t }))
        .filter(|t| t.celsius.is_finite())
        .collect();
    let mut energy = HashMap::new();
    let all: Vec<ProcessMetrics> = sys
        .processes()
        .iter()
        .map(|(pid, p)| {
            let pid = pid.as_u32();
            ProcessMetrics {
                pid,
                name: p.name().to_string_lossy().into_owned(),
                cpu_percent: if p.cpu_usage().is_finite() { p.cpu_usage().max(0.0) } else { 0.0 },
                memory_bytes: p.memory(),
                power_watts: process_power(pid, &sampler.energy, &mut energy),
            }
        })
        .collect();
    // Pids that are gone drop out: only this sample's readings are kept.
    sampler.energy = energy;
    let any_power = all.iter().any(|p| p.power_watts.is_some());
    let process_count = all.len() as u32;
    MetricsSample {
        at_ms: SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0),
        cpu: CpuMetrics { total_percent, cores },
        memory,
        disks,
        network,
        temperatures,
        gpu: gpu_report(),
        processes: rank_processes(all, top),
        process_count,
        chip: sampler.chip.clone(),
        os_name: sampler.os_name.clone(),
        os_version: sampler.os_version.clone(),
        power_reason: power_reason(any_power),
        sessions: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr_x7_a_metrics_sample_reads_current_amdgpu_counters() {
        let drm = tempfile::tempdir().unwrap();
        let device = drm.path().join("card1/device");
        std::fs::create_dir_all(&device).unwrap();
        std::fs::write(device.join("vendor"), "0x1002\n").unwrap();
        std::fs::write(device.join("gpu_busy_percent"), "41\n").unwrap();
        std::fs::write(device.join("mem_info_vram_total"), "8388608\n").unwrap();
        std::fs::write(device.join("mem_info_vram_used"), "2097152\n").unwrap();

        let report = gpu_report_at(drm.path());
        assert!(report.reason.is_none());
        assert_eq!(report.devices.len(), 1);
        let gpu = &report.devices[0];
        assert_eq!(gpu.utilization_percent, Some(41.0));
        assert_eq!(gpu.memory_total_bytes, Some(8 * 1024 * 1024));
        assert_eq!(gpu.memory_used_bytes, Some(2 * 1024 * 1024));

        std::fs::write(device.join("gpu_busy_percent"), "55\n").unwrap();
        assert_eq!(gpu_report_at(drm.path()).devices[0].utilization_percent, Some(55.0));
    }

    #[test]
    fn fr_x7_unavailable_gpu_counters_remain_unknown() {
        let drm = tempfile::tempdir().unwrap();
        let device = drm.path().join("card0/device");
        std::fs::create_dir_all(&device).unwrap();
        std::fs::write(device.join("vendor"), "0x1002").unwrap();
        std::fs::write(device.join("gpu_busy_percent"), "101").unwrap();
        assert!(gpu_report_at(drm.path()).devices.is_empty());
        assert!(gpu_report_at(drm.path()).reason.is_some());

        std::fs::write(device.join("mem_info_vram_total"), "4096").unwrap();
        let report = gpu_report_at(drm.path());
        assert_eq!(report.devices.len(), 1);
        assert_eq!(report.devices[0].utilization_percent, None);
        assert_eq!(report.devices[0].memory_total_bytes, Some(4096));
        assert_eq!(report.devices[0].memory_used_bytes, None);
    }

    fn p(pid: u32, cpu: f32, mem: u64) -> ProcessMetrics {
        ProcessMetrics { pid, name: format!("p{pid}"), cpu_percent: cpu, memory_bytes: mem, power_watts: None }
    }

    #[test]
    fn fr_x7_rank_orders_by_cpu_then_memory_then_pid_and_cuts() {
        let r = rank_processes(vec![p(3, 1.0, 10), p(1, 5.0, 1), p(2, 1.0, 20), p(4, 1.0, 20)], 3);
        assert_eq!(r.iter().map(|x| x.pid).collect::<Vec<_>>(), vec![1, 2, 4]);
    }

    #[test]
    fn fr_x7_rank_survives_nan_and_zero_top() {
        assert!(rank_processes(vec![p(1, f32::NAN, 1), p(2, 1.0, 1)], 0).is_empty());
        assert_eq!(rank_processes(vec![p(1, f32::NAN, 1), p(2, 1.0, 1)], 5).len(), 2);
    }

    #[test]
    fn fr_x7_percent_is_clamped() {
        assert_eq!(clamp_percent(-3.0), 0.0);
        assert_eq!(clamp_percent(140.0), 100.0);
        assert_eq!(clamp_percent(f32::NAN), 0.0);
        assert_eq!(clamp_percent(42.5), 42.5);
    }

    #[test]
    fn fr_x7_gpu_is_empty_with_a_reason_never_invented() {
        let g = gpu_report();
        assert!(g.devices.is_empty());
        assert!(g.reason.as_deref().is_some_and(|r| !r.is_empty()));
    }

    #[test]
    fn fr_x7_sample_reads_this_computer() {
        let s = sample(5);
        assert!(!s.cpu.cores.is_empty(), "a computer has at least one core");
        assert!(s.cpu.cores.iter().all(|c| (0.0..=100.0).contains(c)));
        let m = s.memory.expect("memory is exposed on macOS and Linux");
        assert!(m.total_bytes > 0 && m.used_bytes <= m.total_bytes);
        assert!(s.processes.len() <= 5);
        assert!(s.process_count as usize >= s.processes.len());
        assert!(s.processes.iter().any(|x| x.pid == std::process::id()) || s.process_count > 5);
        assert!(s.at_ms > 0);
    }

    // ---- #389: chip, OS and power ----

    #[test]
    fn fr_x7_os_release_gives_name_and_version() {
        let text = "PRETTY_NAME=\"Ubuntu 24.04.1 LTS\"\nNAME=\"Ubuntu\"\nVERSION_ID=\"24.04\"\nVERSION=\"24.04.1 LTS (Noble Numbat)\"\nID=ubuntu\n";
        assert_eq!(os_release(text), (Some("Ubuntu".into()), Some("24.04".into())));
        // No VERSION_ID (a rolling release): VERSION, else nothing.
        assert_eq!(os_release("NAME=Arch Linux\nVERSION=rolling\n"), (Some("Arch Linux".into()), Some("rolling".into())));
        assert_eq!(os_release("NAME='Alpine Linux'\n"), (Some("Alpine Linux".into()), None));
        // PRETTY_NAME and VERSION_CODENAME are not NAME or VERSION.
        assert_eq!(os_release("PRETTY_NAME=x\nVERSION_CODENAME=noble\n"), (None, None));
        assert_eq!(os_release("NAME=\"\"\n"), (None, None));
    }

    #[test]
    fn fr_x7_chip_name_is_trimmed_and_empty_is_none() {
        assert_eq!(chip_name("  Apple M2 Pro "), Some("Apple M2 Pro".into()));
        assert_eq!(chip_name("Intel(R) Core(TM) i7-9750H CPU   @ 2.60GHz"), Some("Intel(R) Core(TM) i7-9750H CPU @ 2.60GHz".into()));
        assert_eq!(chip_name("   "), None);
        assert_eq!(chip_name(""), None);
    }

    #[test]
    fn fr_x7_sample_reports_chip_and_os_and_never_invents_power() {
        let s = sample(5);
        if let Some(chip) = &s.chip {
            assert!(!chip.trim().is_empty() && chip.trim() == chip, "{chip:?}");
        }
        if cfg!(any(target_os = "macos", target_os = "linux")) {
            assert!(s.os_name.as_deref().is_some_and(|n| !n.is_empty()), "{:?}", s.os_name);
        }
        // Power is never invented. Off macOS no process has a value and the reason is the
        // fixed one. On macOS a value is a finite, non-negative rate, and the reason is set
        // exactly when no process has one (the first sample of a pid has none).
        if cfg!(target_os = "macos") {
            assert!(s.processes.iter().all(|p| p.power_watts.is_none_or(|w| w.is_finite() && w >= 0.0)));
            let any = s.processes.iter().any(|p| p.power_watts.is_some());
            if s.power_reason.is_none() {
                assert!(any || s.process_count > 5, "no reason is given only when power is reported");
            } else {
                assert_eq!(s.power_reason.as_deref(), Some(POWER_WARMING_UP));
            }
        } else {
            assert!(s.processes.iter().all(|p| p.power_watts.is_none()), "no per-process energy API is verified");
            assert_eq!(s.power_reason.as_deref(), Some(POWER_UNAVAILABLE));
        }
        assert!(s.sessions.is_empty(), "the node does not know sessions; the server fills them");
    }

    #[test]
    fn fr_x7_watts_are_the_energy_change_over_the_time() {
        // 2_000_000_000 nJ in 1 s is 2 W.
        assert_eq!(watts_between(Some(1_000), 2_000_001_000, 1_000_000_000), Some(2.0));
        assert_eq!(watts_between(Some(5), 5, 10), Some(0.0));
        // An unknown first reading, a counter that went down and no time are not a rate.
        assert_eq!(watts_between(None, 5, 10), None);
        assert_eq!(watts_between(Some(10), 9, 10), None);
        assert_eq!(watts_between(Some(1), 2, 0), None);
    }

    #[test]
    fn fr_x7_power_reason_is_set_only_when_no_power_is_reported() {
        assert_eq!(power_reason(true), None);
        let why = power_reason(false).expect("a reason when nothing is reported");
        assert!(why == POWER_WARMING_UP || why == POWER_UNAVAILABLE);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn fr_x7_macos_reads_this_process_energy_and_a_busy_loop_has_a_rate() {
        let pid = std::process::id();
        let first = energy_nj(pid).expect("proc_pid_rusage on the current process");
        let at = Instant::now();
        let mut x = 0u64;
        while at.elapsed() < std::time::Duration::from_millis(300) {
            x = std::hint::black_box(x.wrapping_add(1));
        }
        let second = energy_nj(pid).expect("a second reading");
        let w = watts_between(Some(first), second, at.elapsed().as_nanos()).expect("two readings give a rate");
        assert!(w >= 0.0 && w.is_finite(), "{w}");
        // A pid that does not exist is an error, not a value.
        assert!(energy_nj(u32::MAX >> 1).is_err());
    }

    #[test]
    fn fr_x7_an_older_sample_without_the_new_fields_still_decodes() {
        let v = serde_json::json!({
            "at_ms": 1, "cpu": {"total_percent": null, "cores": []}, "memory": null, "disks": [],
            "network": [], "temperatures": [], "gpu": {"devices": [], "reason": null},
            "processes": [{"pid": 1, "name": "a", "cpu_percent": 0.0, "memory_bytes": 0}], "process_count": 1
        });
        let s: MetricsSample = serde_json::from_value(v).unwrap();
        assert_eq!((s.chip, s.os_name, s.os_version, s.power_reason), (None, None, None, None));
        assert!(s.sessions.is_empty());
        assert_eq!(s.processes[0].power_watts, None);
    }
}
