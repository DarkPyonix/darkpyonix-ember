//! Per-source rate and concurrency guard in front of the embedded `letify_link` relay (FR-N9, #920).
//!
//! The server's own standing requests connect directly to the backend loopback relay address,
//! outside the public budget. External peers connect through [`RelayGuard`], which enforces:
//! - a per-source IP concurrent pre-auth / active cap (`max_per_ip`),
//! - a per-source IP sliding-window connection rate limit (`max_rate_per_ip` per `rate_window`),
//! - a global public cap (`max_total`) so a bounded set of flooding sources cannot starve
//!   the relay or block allowed peers from completing a handshake.

use std::collections::{HashMap, VecDeque};
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

/// Limits enforced by [`RelayGuard`] on public incoming relay connections.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RelayGuardConfig {
    /// Maximum simultaneous connections from a single source IP.
    pub max_per_ip: usize,
    /// Maximum new connections accepted from a single source IP within `rate_window`.
    pub max_rate_per_ip: usize,
    /// Sliding window for `max_rate_per_ip`.
    pub rate_window: Duration,
    /// Maximum simultaneous public connections across all sources.
    pub max_total: usize,
}

impl Default for RelayGuardConfig {
    fn default() -> Self {
        Self {
            max_per_ip: 4,
            max_rate_per_ip: 8,
            rate_window: Duration::from_secs(4),
            max_total: 64,
        }
    }
}

#[derive(Default)]
struct GuardState {
    total: usize,
    per_ip_active: HashMap<IpAddr, usize>,
    per_ip_recent: HashMap<IpAddr, VecDeque<Instant>>,
}

struct GuardPermit {
    ip: IpAddr,
    state: Arc<Mutex<GuardState>>,
}

impl Drop for GuardPermit {
    fn drop(&mut self) {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        s.total = s.total.saturating_sub(1);
        if let Some(count) = s.per_ip_active.get_mut(&self.ip) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                s.per_ip_active.remove(&self.ip);
            }
        }
    }
}

/// Checks and records admission for one incoming connection from `ip` at `now`.
fn try_admit(
    state: &Arc<Mutex<GuardState>>,
    cfg: RelayGuardConfig,
    ip: IpAddr,
    now: Instant,
) -> Option<GuardPermit> {
    let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
    if s.total >= cfg.max_total {
        return None;
    }
    let active = s.per_ip_active.get(&ip).copied().unwrap_or(0);
    if active >= cfg.max_per_ip {
        return None;
    }
    let recent = s.per_ip_recent.entry(ip).or_default();
    while let Some(&front) = recent.front() {
        if now.saturating_duration_since(front) >= cfg.rate_window {
            recent.pop_front();
        } else {
            break;
        }
    }
    if recent.len() >= cfg.max_rate_per_ip {
        return None;
    }
    recent.push_back(now);
    // Prune stale empty entries periodically when map grows.
    if s.per_ip_recent.len() > 256 {
        s.per_ip_recent.retain(|_, q| {
            q.front()
                .map(|&t| now.saturating_duration_since(t) < cfg.rate_window)
                .unwrap_or(false)
        });
    }
    s.total += 1;
    *s.per_ip_active.entry(ip).or_insert(0) += 1;
    Some(GuardPermit {
        ip,
        state: Arc::clone(state),
    })
}

/// Spawns a guarded TCP front-end listening on `public_addr` that forwards admitted
/// connections to `backend_addr`.
pub(crate) async fn bind_relay_guard(
    public_addr: SocketAddr,
    backend_addr: SocketAddr,
    cfg: RelayGuardConfig,
) -> io::Result<(SocketAddr, JoinHandle<()>)> {
    let listener = TcpListener::bind(public_addr).await?;
    let bound = listener.local_addr()?;
    let state = Arc::new(Mutex::new(GuardState::default()));
    let task = tokio::spawn(async move {
        loop {
            let (mut incoming, remote) = match listener.accept().await {
                Ok(pair) => pair,
                Err(e) => {
                    tracing::debug!(error = %e, "relay guard accept failed");
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    continue;
                }
            };
            let Some(permit) = try_admit(&state, cfg, remote.ip(), Instant::now()) else {
                tracing::debug!(%remote, "relay guard rejected connection over per-source or total budget");
                drop(incoming);
                continue;
            };
            tokio::spawn(async move {
                let _permit = permit;
                match TcpStream::connect(backend_addr).await {
                    Ok(mut backend) => {
                        let _ = incoming.set_nodelay(true);
                        let _ = backend.set_nodelay(true);
                        let _ = tokio::io::copy_bidirectional(&mut incoming, &mut backend).await;
                    }
                    Err(e) => {
                        tracing::debug!(%remote, error = %e, "relay guard backend connect failed");
                    }
                }
            });
        }
    });
    Ok((bound, task))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn fr_n9_guard_enforces_per_source_rate_and_concurrency_without_blocking_other_sources() {
        let state = Arc::new(Mutex::new(GuardState::default()));
        let cfg = RelayGuardConfig {
            max_per_ip: 2,
            max_rate_per_ip: 4,
            rate_window: Duration::from_secs(4),
            max_total: 10,
        };
        let attacker = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1));
        let allowed = IpAddr::V4(Ipv4Addr::new(198, 51, 100, 7));
        let t0 = Instant::now();

        // Attacker hits concurrent cap (2) and is rejected on 3rd.
        let p1 = try_admit(&state, cfg, attacker, t0).expect("1st");
        let p2 = try_admit(&state, cfg, attacker, t0).expect("2nd");
        assert!(try_admit(&state, cfg, attacker, t0).is_none(), "3rd concurrent from same IP refused");

        // Another source is still admitted freely.
        let ok = try_admit(&state, cfg, allowed, t0);
        assert!(ok.is_some(), "allowed source is not blocked by attacker");
        drop(ok);

        // Even after attacker drops connections, rapid churn hits the sliding-window rate cap (4).
        drop(p1);
        drop(p2);
        let p3 = try_admit(&state, cfg, attacker, t0 + Duration::from_millis(100)).expect("3rd in window");
        let p4 = try_admit(&state, cfg, attacker, t0 + Duration::from_millis(200)).expect("4th in window");
        drop(p3);
        drop(p4);
        assert!(
            try_admit(&state, cfg, attacker, t0 + Duration::from_millis(300)).is_none(),
            "5th connection within window refused by rate limit"
        );

        // Allowed source still succeeds while attacker is rate-limited.
        assert!(try_admit(&state, cfg, allowed, t0 + Duration::from_millis(300)).is_some());

        // After window expires, attacker can connect again.
        assert!(try_admit(&state, cfg, attacker, t0 + Duration::from_secs(5)).is_some());
    }
}
