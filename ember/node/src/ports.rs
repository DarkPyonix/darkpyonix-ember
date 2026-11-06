//! Listening TCP ports of the processes that belong to this node's sessions (SPEC FR-P7).
//!
//! Only processes that descend from a terminal or job the node itself started are listed, so
//! system services, other users' processes, the node and ember-server never appear. Only the
//! executable name is reported, never a command line, environment, cwd or user name, because
//! those can hold secrets. Everything is best effort: an unreadable `/proc` entry or a failed
//! `lsof` shortens the list, it never fails the request.
//!
//! The parsers (`/proc/net/tcp`, `lsof -F`), the scope classification and the process tree walk
//! are pure and unit-tested. Linux matches socket inodes from `/proc/<pid>/fd` against the
//! LISTEN rows of `/proc/net/tcp{,6}`; macOS runs `lsof` limited to the owned pids; other
//! platforms answer an empty list.

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::process::Stdio;
use std::time::Duration;

use crate::proto::{ListeningPort, PortOwner, PortScope};

/// A terminal or job the node tracks: its root pid and who it is.
#[derive(Debug, Clone)]
pub struct Root {
    pub pid: u32,
    pub owner: PortOwner,
}

/// A process the node owns (a root or a descendant of one).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Owned {
    pub owner: PortOwner,
    /// Executable name only.
    pub name: String,
}

/// One LISTEN row of `/proc/net/tcp` or `/proc/net/tcp6`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcListener {
    pub ip: IpAddr,
    pub port: u16,
    pub inode: u64,
}

/// One listening socket of `lsof -F pcn`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LsofListener {
    pub pid: u32,
    pub command: String,
    /// `None` for the wildcard `*`.
    pub ip: Option<IpAddr>,
    pub port: u16,
}

const LSOF_TIMEOUT: Duration = Duration::from_secs(5);

// ---------------------------------------------------------------------------------------------
// Pure parts
// ---------------------------------------------------------------------------------------------

/// Whether this platform can list ports (Linux reads `/proc`, macOS runs `lsof`).
pub fn ports_supported() -> bool {
    cfg!(any(target_os = "linux", target_os = "macos"))
}

fn parse_v4(hex: &str) -> Option<Ipv4Addr> {
    if hex.len() != 8 {
        return None;
    }
    let word = u32::from_str_radix(hex, 16).ok()?;
    // The kernel prints the address as a host-order u32, so little-endian bytes are the octets.
    Some(Ipv4Addr::from(word.to_le_bytes()))
}

fn parse_v6(hex: &str) -> Option<Ipv6Addr> {
    if hex.len() != 32 || !hex.is_ascii() {
        return None;
    }
    let mut bytes = [0u8; 16];
    for i in 0..4 {
        let word = u32::from_str_radix(&hex[i * 8..i * 8 + 8], 16).ok()?;
        bytes[i * 4..i * 4 + 4].copy_from_slice(&word.to_le_bytes());
    }
    Some(Ipv6Addr::from(bytes))
}

/// The LISTEN (`st == 0A`) rows of `/proc/net/tcp` (`v6 == false`) or `/proc/net/tcp6`.
/// The header and malformed lines are skipped.
pub fn parse_proc_net_tcp(text: &str, v6: bool) -> Vec<ProcListener> {
    let mut out = Vec::new();
    for line in text.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        // sl local rem st tx:rx tr:when retrnsmt uid timeout inode ...
        if f.len() < 10 || f[3] != "0A" {
            continue;
        }
        let Some((addr, port)) = f[1].split_once(':') else { continue };
        let Ok(port) = u16::from_str_radix(port, 16) else { continue };
        let ip = if v6 { parse_v6(addr).map(IpAddr::V6) } else { parse_v4(addr).map(IpAddr::V4) };
        let (Some(ip), Ok(inode)) = (ip, f[9].parse::<u64>()) else { continue };
        if inode == 0 {
            continue;
        }
        out.push(ProcListener { ip, port, inode });
    }
    out
}

/// 127.0.0.0/8 and ::1 are `Loopback`, the unspecified address is `Any`, everything else `Lan`.
/// An IPv4-mapped IPv6 address is classified as its IPv4 address.
pub fn classify(ip: IpAddr) -> PortScope {
    let ip = match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(IpAddr::V6(v6)),
        v4 => v4,
    };
    if ip.is_unspecified() {
        PortScope::Any
    } else if ip.is_loopback() {
        PortScope::Loopback
    } else {
        PortScope::Lan
    }
}

/// Keep only the listeners whose inode is held by an allowed pid (`inode_pid`: inode to pid).
pub fn filter_by_inodes<'a>(
    listeners: &'a [ProcListener],
    inode_pid: &'a HashMap<u64, u32>,
) -> impl Iterator<Item = (&'a ProcListener, u32)> + 'a {
    listeners.iter().filter_map(move |l| inode_pid.get(&l.inode).map(|p| (l, *p)))
}

/// Keep only the lsof listeners whose pid is in `allowed`.
pub fn filter_by_pids(listeners: Vec<LsofListener>, allowed: &HashSet<u32>) -> Vec<LsofListener> {
    listeners.into_iter().filter(|l| allowed.contains(&l.pid)).collect()
}

/// Parse `lsof -nP -iTCP -sTCP:LISTEN -F pcn`: `p<pid>`, `c<command>`, then one `n<addr>:<port>`
/// per socket (`127.0.0.1:3000`, `*:8080`, `[::1]:3000`).
pub fn parse_lsof(text: &str) -> Vec<LsofListener> {
    let mut out = Vec::new();
    let mut pid: Option<u32> = None;
    let mut command = String::new();
    for line in text.lines() {
        let Some(tag) = line.chars().next() else { continue };
        let rest = &line[tag.len_utf8()..];
        match tag {
            'p' => {
                pid = rest.trim().parse().ok();
                command.clear();
            }
            'c' => command = rest.to_string(),
            'n' => {
                let Some(pid) = pid else { continue };
                // "127.0.0.1:3000" or "*:8080"; an established-style "a->b" is not a listener.
                let name = rest.split_whitespace().next().unwrap_or("");
                if name.contains("->") {
                    continue;
                }
                let Some((host, port)) = name.rsplit_once(':') else { continue };
                let Ok(port) = port.parse::<u16>() else { continue };
                let host = host.trim_start_matches('[').trim_end_matches(']');
                let ip = if host == "*" {
                    None
                } else {
                    match host.parse::<IpAddr>() {
                        Ok(ip) => Some(ip),
                        Err(_) => continue,
                    }
                };
                out.push(LsofListener { pid, command: command.clone(), ip, port });
            }
            _ => {}
        }
    }
    out
}

/// Every pid at or below a root, from `(pid, parent)` edges. A pid under two roots belongs to
/// the first root in `roots` order (roots are walked breadth first, one after another). `names`
/// supplies the executable name per pid.
pub fn owned_pids(
    edges: &[(u32, u32)],
    names: &HashMap<u32, String>,
    roots: &[Root],
) -> HashMap<u32, Owned> {
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for &(pid, parent) in edges {
        if pid != parent {
            children.entry(parent).or_default().push(pid);
        }
    }
    let mut owned: HashMap<u32, Owned> = HashMap::new();
    for root in roots {
        let mut queue = VecDeque::from([root.pid]);
        while let Some(pid) = queue.pop_front() {
            if owned.contains_key(&pid) {
                continue;
            }
            owned.insert(
                pid,
                Owned { owner: root.owner.clone(), name: names.get(&pid).cloned().unwrap_or_default() },
            );
            if let Some(kids) = children.get(&pid) {
                queue.extend(kids.iter().copied());
            }
        }
    }
    owned
}

fn dedupe_sorted(mut v: Vec<ListeningPort>) -> Vec<ListeningPort> {
    v.sort_by(|a, b| (a.port, a.scope as u8, &a.owner.id).cmp(&(b.port, b.scope as u8, &b.owner.id)));
    v.dedup_by(|a, b| a.port == b.port && a.scope == b.scope && a.owner == b.owner);
    v
}

fn from_proc(
    listeners: &[ProcListener],
    inode_pid: &HashMap<u64, u32>,
    owned: &HashMap<u32, Owned>,
) -> Vec<ListeningPort> {
    let v = filter_by_inodes(listeners, inode_pid)
        .filter_map(|(l, pid)| {
            let o = owned.get(&pid)?;
            Some(ListeningPort {
                port: l.port,
                scope: classify(l.ip),
                process: o.name.clone(),
                owner: o.owner.clone(),
            })
        })
        .collect();
    dedupe_sorted(v)
}

fn from_lsof(listeners: Vec<LsofListener>, owned: &HashMap<u32, Owned>) -> Vec<ListeningPort> {
    let allowed: HashSet<u32> = owned.keys().copied().collect();
    let v = filter_by_pids(listeners, &allowed)
        .into_iter()
        .filter_map(|l| {
            let o = owned.get(&l.pid)?;
            Some(ListeningPort {
                port: l.port,
                scope: l.ip.map(classify).unwrap_or(PortScope::Any),
                process: if o.name.is_empty() { l.command.clone() } else { o.name.clone() },
                owner: o.owner.clone(),
            })
        })
        .collect();
    dedupe_sorted(v)
}

// ---------------------------------------------------------------------------------------------
// Glue
// ---------------------------------------------------------------------------------------------

/// The listening ports of every process at or below `roots`. Best effort, never fails.
pub async fn listening(roots: Vec<Root>) -> Vec<ListeningPort> {
    if roots.is_empty() {
        return Vec::new();
    }
    // Every call walks the process list (and runs lsof on macOS): repeated polls inside a second
    // for the same roots reuse the last answer.
    let key: Vec<(u32, String)> = roots.iter().map(|r| (r.pid, format!("{:?}{}", r.owner.kind, r.owner.id))).collect();
    if let Some((at, k, ports)) = PORTS_CACHE.lock().ok().and_then(|g| g.clone()) {
        if k == key && at.elapsed() < PORTS_CACHE_TTL {
            return ports;
        }
    }
    let ports = listening_uncached(roots).await;
    if let Ok(mut g) = PORTS_CACHE.lock() {
        *g = Some((std::time::Instant::now(), key, ports.clone()));
    }
    ports
}

/// How long the last answer is reused.
const PORTS_CACHE_TTL: Duration = Duration::from_secs(1);

type PortsCache = Option<(std::time::Instant, Vec<(u32, String)>, Vec<ListeningPort>)>;
static PORTS_CACHE: std::sync::Mutex<PortsCache> = std::sync::Mutex::new(None);

async fn listening_uncached(roots: Vec<Root>) -> Vec<ListeningPort> {
    let owned = tokio::task::spawn_blocking(move || snapshot_owned(&roots)).await.unwrap_or_default();
    if owned.is_empty() {
        return Vec::new();
    }
    if cfg!(target_os = "linux") {
        tokio::task::spawn_blocking(move || linux_ports(&owned)).await.unwrap_or_default()
    } else if cfg!(target_os = "macos") {
        let mut pids: Vec<u32> = owned.keys().copied().collect();
        pids.sort_unstable();
        match run_lsof(&pids).await {
            Some(text) => from_lsof(parse_lsof(&text), &owned),
            None => Vec::new(),
        }
    } else {
        Vec::new()
    }
}

/// Walk the live process list for the descendants of `roots`. The node's own pid is never owned.
fn snapshot_owned(roots: &[Root]) -> HashMap<u32, Owned> {
    use sysinfo::{ProcessesToUpdate, System};
    let mut sys = System::new();
    sys.refresh_processes(ProcessesToUpdate::All, true);
    let mut edges = Vec::new();
    let mut names = HashMap::new();
    for (pid, p) in sys.processes() {
        let pid = pid.as_u32();
        if let Some(parent) = p.parent() {
            edges.push((pid, parent.as_u32()));
        }
        names.insert(pid, p.name().to_string_lossy().into_owned());
    }
    let mut owned = owned_pids(&edges, &names, roots);
    owned.remove(&std::process::id());
    owned
}

fn linux_ports(owned: &HashMap<u32, Owned>) -> Vec<ListeningPort> {
    let mut listeners = Vec::new();
    for (file, v6) in [("/proc/net/tcp", false), ("/proc/net/tcp6", true)] {
        if let Ok(text) = std::fs::read_to_string(file) {
            listeners.extend(parse_proc_net_tcp(&text, v6));
        }
    }
    if listeners.is_empty() {
        return Vec::new();
    }
    let wanted: HashSet<u64> = listeners.iter().map(|l| l.inode).collect();
    let mut inode_pid: HashMap<u64, u32> = HashMap::new();
    for &pid in owned.keys() {
        let Ok(dir) = std::fs::read_dir(format!("/proc/{pid}/fd")) else { continue };
        for entry in dir.flatten() {
            let Ok(target) = std::fs::read_link(entry.path()) else { continue };
            let t = target.to_string_lossy();
            if let Some(inode) = t.strip_prefix("socket:[").and_then(|r| r.strip_suffix(']')) {
                if let Ok(inode) = inode.parse::<u64>() {
                    if wanted.contains(&inode) {
                        inode_pid.entry(inode).or_insert(pid);
                    }
                }
            }
        }
    }
    from_proc(&listeners, &inode_pid, owned)
}

/// Where lsof lives on macOS. A fixed path, never a PATH lookup, so a planted `lsof` cannot run.
const LSOF_PATHS: [&str; 2] = ["/usr/sbin/lsof", "/usr/bin/lsof"];

/// The most lsof output that is read (the pid list already bounds it).
const LSOF_MAX_BYTES: u64 = 1 << 20;

/// `lsof` limited to `pids`; `None` when it is missing, fails to start or times out.
pub async fn run_lsof(pids: &[u32]) -> Option<String> {
    use tokio::io::AsyncReadExt;
    let bin = LSOF_PATHS.iter().find(|p| std::path::Path::new(p).exists())?;
    let list = pids.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
    let mut cmd = tokio::process::Command::new(bin);
    cmd.args(["-nP", "-iTCP", "-sTCP:LISTEN", "-F", "pcn", "-a", "-p", &list])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let mut child = cmd.spawn().ok()?;
    let stdout = child.stdout.take()?;
    // lsof exits 1 when nothing matches; its stdout is still the answer.
    let read = async {
        let mut buf = Vec::new();
        stdout.take(LSOF_MAX_BYTES).read_to_end(&mut buf).await.ok()?;
        let _ = child.kill().await;
        Some(buf)
    };
    let buf = tokio::time::timeout(LSOF_TIMEOUT, read).await.ok()??;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::PortOwnerKind;

    const TCP: &str = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 0100007F:0BB8 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 12345 1 0000000000000000 100 0 0 10 0
   1: 00000000:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 23456 1 0000000000000000 100 0 0 10 0
   2: 0100007F:0BB8 0100007F:D2A2 01 00000000:00000000 00:00000000 00000000  1000        0 34567 1 0000000000000000 20 4 30 10 -1
   3: 0A00A8C0:0050 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 45678 1 0000000000000000 100 0 0 10 0
";
    const TCP6: &str = "  sl  local_address                         remote_address                        st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 00000000000000000000000001000000:0BB9 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 56789 1 0000000000000000 100 0 0 10 0
   1: 00000000000000000000000000000000:1F91 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 67890 1 0000000000000000 100 0 0 10 0
   2: 0000000000000000FFFF00000100007F:0BBA 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 78901 1 0000000000000000 100 0 0 10 0
";

    #[test]
    fn fr_p7_proc_net_tcp_keeps_only_listen_rows() {
        let v = parse_proc_net_tcp(TCP, false);
        assert_eq!(v.len(), 3);
        assert_eq!(v[0], ProcListener { ip: "127.0.0.1".parse().unwrap(), port: 3000, inode: 12345 });
        assert_eq!(v[1], ProcListener { ip: "0.0.0.0".parse().unwrap(), port: 8080, inode: 23456 });
        assert_eq!(v[2], ProcListener { ip: "192.168.0.10".parse().unwrap(), port: 80, inode: 45678 });
    }

    #[test]
    fn fr_p7_proc_net_tcp6_decodes_words_and_mapped_addresses() {
        let v = parse_proc_net_tcp(TCP6, true);
        assert_eq!(v.len(), 3);
        assert_eq!(v[0].ip, "::1".parse::<IpAddr>().unwrap());
        assert_eq!(v[0].port, 3001);
        assert_eq!(v[1].ip, "::".parse::<IpAddr>().unwrap());
        assert_eq!(v[2].ip, "::ffff:127.0.0.1".parse::<IpAddr>().unwrap());
    }

    #[test]
    fn fr_p7_proc_net_tcp_ignores_garbage() {
        assert!(parse_proc_net_tcp("", false).is_empty());
        assert!(parse_proc_net_tcp("not a table\n0: zz:zz 0 0A 0 0 0 0 0 5\n", false).is_empty());
    }

    #[test]
    fn fr_p7_scope_classification() {
        let s = |a: &str| classify(a.parse().unwrap());
        assert_eq!(s("127.0.0.1"), PortScope::Loopback);
        assert_eq!(s("127.1.2.3"), PortScope::Loopback);
        assert_eq!(s("::1"), PortScope::Loopback);
        assert_eq!(s("::ffff:127.0.0.1"), PortScope::Loopback);
        assert_eq!(s("0.0.0.0"), PortScope::Any);
        assert_eq!(s("::"), PortScope::Any);
        assert_eq!(s("192.168.0.10"), PortScope::Lan);
        assert_eq!(s("10.0.0.2"), PortScope::Lan);
        assert_eq!(s("203.0.113.5"), PortScope::Lan);
        assert_eq!(s("fe80::1"), PortScope::Lan);
    }

    #[test]
    fn fr_p7_inode_filter_drops_unowned_sockets() {
        let l = parse_proc_net_tcp(TCP, false);
        let map = HashMap::from([(12345u64, 77u32)]);
        let got: Vec<_> = filter_by_inodes(&l, &map).map(|(l, p)| (l.port, p)).collect();
        assert_eq!(got, vec![(3000, 77)]);
    }

    const LSOF: &str = "p501\ncnode\nn127.0.0.1:3000\nn*:8080\np502\ncPython\nn[::1]:4000\nn[::]:4001\np503\ncsshd\nn10.0.0.2:22\n";

    #[test]
    fn fr_p7_lsof_output_parses() {
        let v = parse_lsof(LSOF);
        assert_eq!(v.len(), 5);
        assert_eq!(v[0], LsofListener { pid: 501, command: "node".into(), ip: Some("127.0.0.1".parse().unwrap()), port: 3000 });
        assert_eq!(v[1].ip, None);
        assert_eq!(v[1].port, 8080);
        assert_eq!(v[2], LsofListener { pid: 502, command: "Python".into(), ip: Some("::1".parse().unwrap()), port: 4000 });
        assert_eq!(v[3].ip, Some("::".parse().unwrap()));
        assert_eq!(v[4].pid, 503);
        assert!(parse_lsof("").is_empty());
        assert!(parse_lsof("n127.0.0.1:1\n").is_empty(), "a name before any pid is dropped");
    }

    #[test]
    fn fr_p7_lsof_filter_keeps_allowed_pids_only() {
        let allowed = HashSet::from([501u32, 502]);
        let v = filter_by_pids(parse_lsof(LSOF), &allowed);
        assert!(v.iter().all(|l| l.pid != 503));
        assert_eq!(v.len(), 4);
    }

    fn root(pid: u32, kind: PortOwnerKind, id: &str) -> Root {
        Root { pid, owner: PortOwner { kind, id: id.into() } }
    }

    #[test]
    fn fr_p7_process_tree_walk_owns_descendants_only() {
        // 1 -> 10 (term t1) -> 11 -> 12 ; 1 -> 20 (job j1) -> 21 ; 1 -> 30 (unowned) -> 31
        let edges = [(10, 1), (11, 10), (12, 11), (20, 1), (21, 20), (30, 1), (31, 30)];
        let names: HashMap<u32, String> = [(10, "bash"), (12, "python3"), (21, "node")]
            .into_iter()
            .map(|(p, n)| (p, n.to_string()))
            .collect();
        let roots = [root(10, PortOwnerKind::Term, "t1"), root(20, PortOwnerKind::Job, "j1")];
        let owned = owned_pids(&edges, &names, &roots);
        let mut pids: Vec<u32> = owned.keys().copied().collect();
        pids.sort();
        assert_eq!(pids, vec![10, 11, 12, 20, 21]);
        assert_eq!(owned[&12].owner.id, "t1");
        assert_eq!(owned[&12].name, "python3");
        assert_eq!(owned[&21].owner.kind, PortOwnerKind::Job);
        assert!(!owned.contains_key(&1) && !owned.contains_key(&31));
    }

    #[test]
    fn fr_p7_process_tree_walk_survives_cycles() {
        let edges = [(2, 1), (1, 2)];
        let owned = owned_pids(&edges, &HashMap::new(), &[root(1, PortOwnerKind::Term, "t")]);
        assert_eq!(owned.len(), 2);
    }

    #[test]
    fn fr_p7_unsupported_platforms_say_so() {
        assert_eq!(ports_supported(), cfg!(any(target_os = "linux", target_os = "macos")));
    }

    #[test]
    fn fr_p7_proc_rows_join_owned_pids_into_ports() {
        let l = parse_proc_net_tcp(TCP, false);
        let owned = HashMap::from([(
            77u32,
            Owned { owner: PortOwner { kind: PortOwnerKind::Job, id: "j".into() }, name: "node".into() },
        )]);
        let inode_pid = HashMap::from([(12345u64, 77u32), (23456u64, 99u32)]);
        let ports = from_proc(&l, &inode_pid, &owned);
        assert_eq!(ports.len(), 1, "pid 99 is not owned");
        assert_eq!(ports[0].port, 3000);
        assert_eq!(ports[0].scope, PortScope::Loopback);
        assert_eq!(ports[0].process, "node");
    }
}
