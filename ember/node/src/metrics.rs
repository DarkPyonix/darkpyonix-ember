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
        ProcessMetrics { pid, name: format!("p{pid}"), cpu_percent: cpu, memory_bytes: mem }
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
}
