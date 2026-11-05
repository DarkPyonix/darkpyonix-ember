#![cfg(all(feature = "bench", feature = "letify"))]

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};

struct Listener(Child);

impl Drop for Listener {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn nfr_n1_bench_binary_json_output_format() {
    let binary = env!("CARGO_BIN_EXE_ember-transport-bench");
    let mut listener = Listener(
        Command::new(binary)
            .args(["listen", "--relay", "none"])
            .env("EMBER_TRANSPORT_BACKEND", "letify")
            .stdout(Stdio::piped())
            .spawn()
            .expect("start listener"),
    );
    let mut address = String::new();
    BufReader::new(listener.0.stdout.take().unwrap())
        .read_line(&mut address)
        .expect("read listener address");
    let output = Command::new(binary)
        .args(["dial", address.trim(), "--relay", "none", "--pings", "3", "--bytes", "4096", "--no-tcp"])
        .env("EMBER_TRANSPORT_BACKEND", "letify")
        .output()
        .expect("run benchmark");
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON report");
    for field in ["setup_ms", "rtt_p50_ms", "rtt_p95_ms", "throughput_mib_s"] {
        assert!(report[field].as_f64().is_some_and(|n| n.is_finite() && n >= 0.0), "invalid {field}: {report}");
    }
    let history = report["path_history"].as_array().expect("path_history array");
    assert!(!history.is_empty());
    assert!(history.iter().all(|entry| entry["t_ms"].is_number() && entry["path"].is_string()));
}
