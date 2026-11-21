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

use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use sysinfo::{Components, Disks, Networks, ProcessesToUpdate, System, MINIMUM_CPU_UPDATE_INTERVAL};

use crate::proto::*;

pub const DEFAULT_TOP: usize = 20;
pub const MAX_TOP: usize = 200;

/// Why the GPU list is empty.
pub const GPU_UNAVAILABLE: &str =
    "GPU metrics are not collected: no verified GPU API on this node";

struct Sampler {
    sys: System,
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

/// Take one sample. Blocks (the first call sleeps ~200 ms); call it from the blocking pool.
pub fn sample(top: usize) -> MetricsSample {
    let top = top.min(MAX_TOP);
    let mut guard = SAMPLER.lock().unwrap_or_else(|e| e.into_inner());
    let sampler = guard.get_or_insert_with(|| {
        let mut sys = System::new();
        sys.refresh_cpu_usage();
        sys.refresh_processes(ProcessesToUpdate::All, true);
        std::thread::sleep(MINIMUM_CPU_UPDATE_INTERVAL);
        Sampler { sys }
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
    let all: Vec<ProcessMetrics> = sys
        .processes()
        .iter()
        .map(|(pid, p)| ProcessMetrics {
            pid: pid.as_u32(),
            name: p.name().to_string_lossy().into_owned(),
            cpu_percent: if p.cpu_usage().is_finite() { p.cpu_usage().max(0.0) } else { 0.0 },
            memory_bytes: p.memory(),
        })
        .collect();
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(s.processes.iter().all(|p| p.power_watts.is_none()), "no per-process energy API is verified");
        assert_eq!(s.power_reason.as_deref(), Some(POWER_UNAVAILABLE));
        assert!(s.sessions.is_empty(), "the node does not know sessions; the server fills them");
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
