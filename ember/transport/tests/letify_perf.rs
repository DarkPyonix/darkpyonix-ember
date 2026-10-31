//! `NFR-N1` measurement on loopback for the letify-link backend: bulk throughput against a raw
//! TCP transfer, and the latency of small messages while a bulk transfer runs on the same
//! connection. Not run by default (it moves hundreds of MiB). Run with
//!
//! ```text
//! cargo test --release --features letify --test letify_perf -- --ignored --nocapture
//! ```
//!
//! `LETIFY_PERF_MIB` sets the bulk size (default 256). The numbers are printed on stderr as
//! `perf <name> <value> <unit>` lines to paste into the pull request and the SPEC.
#![cfg(feature = "letify")]

mod common;

use std::time::{Duration, Instant};

use common::letify_pair;
use ember_transport::{BiStream, Connection};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// First byte of a stream: `E` echoes until end of stream, `S` sinks and answers the byte count.
fn serve(transport: &ember_transport::Transport, service: &str) {
    let mut listener = transport.listen(service).unwrap();
    tokio::spawn(async move {
        while let Some(conn) = listener.accept().await {
            tokio::spawn(async move {
                while let Ok(stream) = conn.accept_bi().await {
                    tokio::spawn(handle(stream));
                }
            });
        }
    });
}

async fn handle(stream: BiStream) {
    let (mut w, mut r) = stream.into_split();
    let mut mode = [0u8; 1];
    if r.read_exact(&mut mode).await.is_err() {
        return;
    }
    match mode[0] {
        b'E' => {
            let mut buf = [0u8; 32];
            while r.read_exact(&mut buf).await.is_ok() {
                if w.write_all(&buf).await.is_err() {
                    return;
                }
            }
        }
        b'S' => {
            let mut total = 0u64;
            let mut buf = vec![0u8; 256 << 10];
            loop {
                match r.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => total += n as u64,
                }
            }
            let _ = w.write_all(&total.to_be_bytes()).await;
            let _ = w.shutdown().await;
        }
        _ => {}
    }
}

/// Sends `bytes` on one stream and returns MiB/s once the receiver reported the full count.
async fn bulk(conn: &Connection, bytes: u64, bulk_class: bool) -> f64 {
    let stream = if bulk_class { conn.open_bi_bulk().await } else { conn.open_bi().await }.unwrap();
    let (mut w, mut r) = stream.into_split();
    let started = Instant::now();
    w.write_all(b"S").await.unwrap();
    let chunk = vec![0xabu8; 64 << 10];
    let mut sent = 0u64;
    while sent < bytes {
        w.write_all(&chunk).await.unwrap();
        sent += chunk.len() as u64;
    }
    w.shutdown().await.unwrap();
    let mut count = [0u8; 8];
    r.read_exact(&mut count).await.unwrap();
    assert_eq!(u64::from_be_bytes(count), sent);
    sent as f64 / (1 << 20) as f64 / started.elapsed().as_secs_f64()
}

async fn raw_tcp(bytes: u64) -> f64 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let sink = tokio::spawn(async move {
        let (mut s, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 256 << 10];
        let mut total = 0u64;
        loop {
            match s.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => total += n as u64,
            }
        }
        total
    });
    let mut s = TcpStream::connect(addr).await.unwrap();
    s.set_nodelay(true).unwrap();
    let chunk = vec![0xabu8; 64 << 10];
    let started = Instant::now();
    let mut sent = 0u64;
    while sent < bytes {
        s.write_all(&chunk).await.unwrap();
        sent += chunk.len() as u64;
    }
    s.shutdown().await.unwrap();
    assert_eq!(sink.await.unwrap(), sent);
    sent as f64 / (1 << 20) as f64 / started.elapsed().as_secs_f64()
}

/// Round trips of a 32 byte message on one echo stream.
async fn pings(conn: &Connection, n: usize) -> Vec<Duration> {
    let stream = conn.open_bi().await.unwrap();
    let (mut w, mut r) = stream.into_split();
    w.write_all(b"E").await.unwrap();
    let (msg, mut back) = ([7u8; 32], [0u8; 32]);
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let t = Instant::now();
        w.write_all(&msg).await.unwrap();
        r.read_exact(&mut back).await.unwrap();
        out.push(t.elapsed());
    }
    out
}

fn percentile(sorted: &[Duration], p: f64) -> Duration {
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

fn report(name: &str, mut samples: Vec<Duration>) {
    samples.sort();
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    eprintln!("perf {name} p50 {:.3} ms", ms(percentile(&samples, 0.50)));
    eprintln!("perf {name} p95 {:.3} ms", ms(percentile(&samples, 0.95)));
    eprintln!("perf {name} p99 {:.3} ms", ms(percentile(&samples, 0.99)));
    eprintln!("perf {name} max {:.3} ms", ms(*samples.last().unwrap()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "measurement: see the module documentation"]
async fn nfr_n1_letify_loopback_throughput_and_small_message_latency_under_bulk() {
    let mib: u64 = std::env::var("LETIFY_PERF_MIB").ok().and_then(|v| v.parse().ok()).unwrap_or(256);
    let bytes = mib << 20;
    let (a, b, _dir) = letify_pair().await;
    serve(&b, "perf");
    let conn = a.connect(b.peer_id(), "perf").await.unwrap();

    // Warm up and baseline.
    let _ = pings(&conn, 50).await;
    let raw = raw_tcp(bytes).await;
    eprintln!("perf raw_tcp_loopback_throughput {raw:.1} MiB/s");

    let one = bulk(&conn, bytes, true).await;
    eprintln!("perf letify_one_stream_throughput {one:.1} MiB/s");
    eprintln!("perf letify_vs_raw_tcp {:.1} %", one / raw * 100.0);

    // Four bulk streams at once.
    let started = Instant::now();
    let many = futures::future::join_all((0..4).map(|_| bulk(&conn, bytes / 4, true))).await;
    let total = bytes as f64 / (1 << 20) as f64 / started.elapsed().as_secs_f64();
    eprintln!("perf letify_four_streams_throughput {total:.1} MiB/s (per stream {many:.1?})");

    // Small messages on an idle connection.
    report("small_message_rtt_idle", pings(&conn, 1000).await);

    // Small messages while a bulk transfer runs on the same connection: the transfer on a
    // bulk-class stream, then on an interactive one for comparison.
    for (label, bulk_class) in [("bulk_class", true), ("interactive_class", false)] {
        let c = conn.clone();
        let transfer = tokio::spawn(async move { bulk(&c, bytes, bulk_class).await });
        tokio::time::sleep(Duration::from_millis(300)).await;
        let samples = pings(&conn, 500).await;
        report(&format!("small_message_rtt_under_{label}_bulk"), samples);
        let rate = transfer.await.unwrap();
        eprintln!("perf bulk_throughput_while_pinging_{label} {rate:.1} MiB/s");
    }
}
