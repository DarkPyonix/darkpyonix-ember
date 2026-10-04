//! `ember-transport-bench`: NFR-N1 measurement harness (feature `bench`).
//!
//! Two processes, typically on two machines of the network matrix:
//!
//! ```text
//! # peer B (prints one JSON line: its address; copy it to peer A)
//! ember-transport-bench listen [--relay URL|none|default] [--key PATH] [--public-lookup]
//!
//! # peer A
//! ember-transport-bench dial '<json line from listen>' [--pings 200] [--bytes 33554432]
//!     [--direct-timeout-ms 10000] [--relay ...] [--tcp-host IP] [--no-tcp]
//! ```
//!
//! `dial` prints one JSON object: connection setup time to first echoed byte, time until a
//! direct path is selected, RTT p50/p95 over N pings on one stream, throughput of a fixed
//! one-way transfer, every path change with its timestamp, and the same RTT/throughput over a
//! raw TCP connection to the listener as the baseline NFR-N1 compares against.
//!
//! Protocol on each stream (and each baseline TCP connection): first byte selects the mode.
//! `E` = echo everything until EOF; `S` = sink until EOF, then reply the byte count (u64 BE).

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ember_transport::{PathState, PeerAddr, RelayConfig, SecretKey, Transport, TransportConfig};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const SERVICE: &str = "ember/bench/0";
const PING_SIZE: usize = 32;
const CHUNK: usize = 64 * 1024;

#[derive(Serialize, Deserialize)]
struct ListenInfo {
    #[serde(flatten)]
    addr: PeerAddr,
    tcp_port: Option<u16>,
}

struct Args {
    rest: Vec<String>,
}

impl Args {
    fn flag(&mut self, name: &str) -> bool {
        if let Some(i) = self.rest.iter().position(|a| a == name) {
            self.rest.remove(i);
            true
        } else {
            false
        }
    }

    fn value(&mut self, name: &str) -> Option<String> {
        let i = self.rest.iter().position(|a| a == name)?;
        self.rest.remove(i);
        (i < self.rest.len()).then(|| self.rest.remove(i))
    }

    fn num(&mut self, name: &str, default: u64) -> u64 {
        self.value(name).map(|v| v.parse().expect("numeric argument")).unwrap_or(default)
    }
}

fn usage() -> ! {
    eprintln!(
        "usage:\n  ember-transport-bench listen [--relay URL|none|default] [--key PATH] [--public-lookup]\n  \
         ember-transport-bench dial '<listen json>' [--pings N] [--bytes N] [--direct-timeout-ms N] \
         [--relay ...] [--tcp-host IP] [--no-tcp]"
    );
    std::process::exit(2)
}

async fn bind(args: &mut Args) -> Transport {
    let key = match args.value("--key") {
        Some(path) => SecretKey::load_or_generate(path).expect("key file"),
        None => SecretKey::generate(),
    };
    let mut config = TransportConfig::from_env(key);
    if let Some(relay) = args.value("--relay") {
        config = config.relay(RelayConfig::parse(&relay));
    }
    if args.flag("--public-lookup") {
        config = config.public_lookup(true);
    }
    Transport::bind(config).await.expect("bind transport")
}

#[tokio::main]
async fn main() {
    let mut argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.is_empty() {
        usage();
    }
    let mode = argv.remove(0);
    let mut args = Args { rest: argv };
    match mode.as_str() {
        "listen" => listen(&mut args).await,
        "dial" => dial(&mut args).await,
        _ => usage(),
    }
}

// ---------------------------------------------------------------------------------------------
// listen
// ---------------------------------------------------------------------------------------------

async fn listen(args: &mut Args) {
    let transport = bind(args).await;
    let mut listener = transport.listen(SERVICE).expect("listen");
    let tcp = TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0))
        .await
        .ok();

    // Give the endpoint a moment to learn its addresses (and reach its relay, if any).
    let online = transport.wait_online(Duration::from_secs(5)).await;
    let mut addr = transport.local_addr();
    let deadline = Instant::now() + Duration::from_secs(3);
    while addr.direct.is_empty() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
        addr = transport.local_addr();
    }
    let tcp_port = tcp.as_ref().and_then(|l| l.local_addr().ok()).map(|a| a.port());
    let info = ListenInfo { addr, tcp_port };
    println!("{}", serde_json::to_string(&info).unwrap());
    eprintln!("listening as {} (relay online: {online}); ctrl-c to stop", transport.peer_id());

    if let Some(tcp) = tcp {
        tokio::spawn(async move {
            while let Ok((sock, _)) = tcp.accept().await {
                sock.set_nodelay(true).ok();
                tokio::spawn(async move {
                    let (r, w) = sock.into_split();
                    serve_stream(r, w).await;
                });
            }
        });
    }

    while let Some(conn) = listener.accept().await {
        eprintln!("connection from {} path={}", conn.peer(), conn.path_state());
        tokio::spawn(async move {
            while let Ok(stream) = conn.accept_bi().await {
                tokio::spawn(async move {
                    let (w, r) = stream.into_split();
                    serve_stream(r, w).await;
                });
            }
        });
    }
}

async fn serve_stream<R, W>(mut r: R, mut w: W)
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut cmd = [0u8; 1];
    if r.read_exact(&mut cmd).await.is_err() {
        return;
    }
    match cmd[0] {
        b'E' => {
            tokio::io::copy(&mut r, &mut w).await.ok();
        }
        b'S' => {
            let n = tokio::io::copy(&mut r, &mut tokio::io::sink()).await.unwrap_or(0);
            w.write_all(&n.to_be_bytes()).await.ok();
        }
        _ => {}
    }
    w.shutdown().await.ok();
}

// ---------------------------------------------------------------------------------------------
// dial
// ---------------------------------------------------------------------------------------------

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn rtt_summary(mut samples: Vec<Duration>) -> serde_json::Value {
    if samples.is_empty() {
        return serde_json::Value::Null;
    }
    samples.sort();
    let pct = |p: f64| {
        let i = ((samples.len() as f64 - 1.0) * p).round() as usize;
        ms(samples[i])
    };
    json!({
        "n": samples.len(),
        "min_ms": ms(samples[0]),
        "p50_ms": pct(0.50),
        "p95_ms": pct(0.95),
        "max_ms": ms(*samples.last().unwrap()),
    })
}

/// Sends `PING_SIZE`-byte pings on an echo stream and times each round trip.
async fn pings<R, W>(r: &mut R, w: &mut W, n: u64) -> std::io::Result<Vec<Duration>>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let msg = [0x5au8; PING_SIZE];
    let mut buf = [0u8; PING_SIZE];
    let mut out = Vec::with_capacity(n as usize);
    for _ in 0..n {
        let t = Instant::now();
        w.write_all(&msg).await?;
        w.flush().await?;
        r.read_exact(&mut buf).await?;
        out.push(t.elapsed());
    }
    Ok(out)
}

/// Sends `bytes` after an `S` command and waits for the byte-count reply. Returns Mbit/s.
async fn throughput<R, W>(r: &mut R, w: &mut W, bytes: u64) -> std::io::Result<serde_json::Value>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let chunk = vec![0xa5u8; CHUNK];
    let t = Instant::now();
    w.write_all(b"S").await?;
    let mut left = bytes;
    while left > 0 {
        let n = left.min(CHUNK as u64) as usize;
        w.write_all(&chunk[..n]).await?;
        left -= n as u64;
    }
    w.shutdown().await?;
    let mut count = [0u8; 8];
    r.read_exact(&mut count).await?;
    let elapsed = t.elapsed();
    let received = u64::from_be_bytes(count);
    Ok(json!({
        "bytes": received,
        "seconds": elapsed.as_secs_f64(),
        "mbit_per_s": received as f64 * 8.0 / elapsed.as_secs_f64() / 1e6,
    }))
}

async fn dial(args: &mut Args) {
    let pings_n = args.num("--pings", 200);
    let bytes = args.num("--bytes", 32 * 1024 * 1024);
    let direct_timeout = Duration::from_millis(args.num("--direct-timeout-ms", 10_000));
    let no_tcp = args.flag("--no-tcp");
    let tcp_host = args.value("--tcp-host");
    let transport = bind(args).await;
    let Some(info) = args.rest.first() else { usage() };
    let info: ListenInfo = serde_json::from_str(info).expect("listen JSON");
    let peer_addr = info.addr.clone();

    // 1. Setup: connect, open a stream, first echoed byte.
    let t0 = Instant::now();
    let conn = transport.connect(peer_addr.clone(), SERVICE).await.expect("connect");
    let connected = t0.elapsed();
    let mut echo = conn.open_bi().await.expect("open stream");
    echo.write_all(b"E\x01").await.expect("write");
    let mut one = [0u8; 1];
    echo.read_exact(&mut one).await.expect("first byte");
    let first_byte = t0.elapsed();
    let initial_path = conn.path_state();

    // Path-change log, from now until the end of the run.
    let changes: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(vec![json!({
        "t_ms": ms(first_byte), "path": initial_path.to_string(),
    })]));
    let mut watch = conn.watch_path();
    {
        let changes = changes.clone();
        let mut watch = watch.clone();
        tokio::spawn(async move {
            while watch.changed().await.is_ok() {
                let p = watch.borrow_and_update().to_string();
                changes.lock().unwrap().push(json!({ "t_ms": ms(t0.elapsed()), "path": p }));
            }
        });
    }

    // 2. Time to a direct path.
    let time_to_direct = if initial_path.is_direct() {
        Some(first_byte)
    } else {
        let waited = tokio::time::timeout(direct_timeout, watch.wait_for(PathState::is_direct)).await;
        match waited {
            Ok(Ok(_)) => Some(t0.elapsed()),
            _ => None,
        }
    };

    // 3. RTT over the transport.
    let (mut w, mut r) = echo.into_split();
    let rtts = pings(&mut r, &mut w, pings_n).await.expect("pings");
    w.shutdown().await.ok();
    let path_during_pings = conn.path_state();

    // 4. Throughput over the transport.
    let (mut tw, mut tr) = conn.open_bi().await.expect("open stream").into_split();
    let tput = throughput(&mut tr, &mut tw, bytes).await.expect("throughput");

    // 5. Raw TCP baseline to the same host.
    let tcp = if no_tcp {
        serde_json::Value::Null
    } else {
        match tcp_baseline(&info, &peer_addr, tcp_host, pings_n, bytes).await {
            Ok(v) => v,
            Err(e) => json!({ "error": e.to_string() }),
        }
    };

    let report = json!({
        "local_peer": transport.peer_id().to_string(),
        "remote_peer": peer_addr.peer.to_string(),
        "connect_ms": ms(connected),
        "setup_first_byte_ms": ms(first_byte),
        "initial_path": initial_path.to_string(),
        "time_to_direct_ms": time_to_direct.map(ms),
        "path_during_pings": path_during_pings.to_string(),
        "rtt": rtt_summary(rtts),
        "throughput": tput,
        "path_changes": changes.lock().unwrap().clone(),
        "tcp_baseline": tcp,
        "note": "single run; NFR-N1 needs 20 runs per network pair",
    });
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
    conn.close(0, "bench done");
    transport.close().await;
}

async fn tcp_baseline(
    info: &ListenInfo,
    peer: &PeerAddr,
    host: Option<String>,
    pings_n: u64,
    bytes: u64,
) -> std::io::Result<serde_json::Value> {
    let Some(port) = info.tcp_port else {
        return Ok(json!({ "error": "listener has no TCP port" }));
    };
    let ip: IpAddr = match host {
        Some(h) => h.parse().map_err(|_| std::io::Error::other("bad --tcp-host"))?,
        None => peer
            .direct
            .iter()
            .map(|a| a.ip())
            .find(|ip| ip.is_ipv4())
            .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST)),
    };
    let target = SocketAddr::new(ip, port);

    let t = Instant::now();
    let sock = TcpStream::connect(target).await?;
    sock.set_nodelay(true)?;
    let (mut r, mut w) = sock.into_split();
    w.write_all(b"E\x01").await?;
    let mut one = [0u8; 1];
    r.read_exact(&mut one).await?;
    let first_byte = t.elapsed();
    let rtts = pings(&mut r, &mut w, pings_n).await?;
    w.shutdown().await.ok();

    let sock = TcpStream::connect(target).await?;
    sock.set_nodelay(true)?;
    let (mut r, mut w) = sock.into_split();
    let tput = throughput(&mut r, &mut w, bytes).await?;

    Ok(json!({
        "target": target.to_string(),
        "setup_first_byte_ms": ms(first_byte),
        "rtt": rtt_summary(rtts),
        "throughput": tput,
    }))
}
