//! Serving the node API over the peer-to-peer transport (SPEC `FR-N1`, `FR-N3`, `FR-N5`).
//!
//! The same [`crate::api::router`] that serves TCP is served on transport service
//! [`NODE_SERVICE`] through [`HttpListener`]: one transport stream = one HTTP/1.1 connection,
//! so WebSockets (exec, events, exec-server, terminal attach) work unchanged.
//!
//! **Device authentication (`FR-N3`).** Only peers on the allow-list may connect; others are
//! closed at accept, before any request is read. The list is the ember server(s) this node
//! serves, from `EMBER_NODE_ALLOWED_PEERS` and `<state dir>/allowed-peers`. Re-reading the file
//! (SIGHUP to the daemon) drops connections of peers no longer listed: that is revocation on
//! the node side. The bearer token is still required on top.
//!
//! Configuration:
//! - `EMBER_NODE_TRANSPORT`: `1`/`on` serves the transport in addition to TCP, `only` instead
//!   of TCP (then no `local.json` is written and `ember-term` cannot find the daemon); unset/`0`
//!   is TCP only.
//! - `<state dir>/transport.key`: the node's persistent identity (created on first start, 0600).
//! - `EMBER_RELAY_URL`: relay servers (see `ember_transport::RELAY_URL_ENV`); overrides the hub's.
//! - `<state dir>/hub.json`: the node's darkpyonix.dev registration (`ember-node hub register`,
//!   [`crate::hub`]). When present, the node publishes its address to the hub's directory and
//!   uses the hub's relay, so the server finds it by peer id alone (`FR-N2`).

use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};

use ember_transport::http::HttpListener;
use ember_transport::{PeerGate, PeerId, SecretKey, Transport};

use crate::api::{self, Node};
pub use crate::client::NODE_SERVICE;

pub const ENABLE_ENV: &str = "EMBER_NODE_TRANSPORT";
pub const ALLOWED_PEERS_ENV: &str = "EMBER_NODE_ALLOWED_PEERS";
/// One peer id (64 hex chars) per line; blank lines and `#` comments are ignored.
pub const ALLOWED_PEERS_FILE: &str = "allowed-peers";
pub const KEY_FILE: &str = "transport.key";

/// Which listeners the daemon runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportMode {
    /// TCP only (default).
    Off,
    /// TCP and the transport.
    Also,
    /// The transport only.
    Only,
}

impl TransportMode {
    pub fn parse(value: &str) -> anyhow::Result<Self> {
        Ok(match value.trim().to_ascii_lowercase().as_str() {
            "" | "0" | "off" | "false" | "no" => Self::Off,
            "1" | "on" | "true" | "yes" => Self::Also,
            "only" => Self::Only,
            other => anyhow::bail!("{ENABLE_ENV}={other:?}: expected 0, 1 or only"),
        })
    }

    pub fn from_env() -> anyhow::Result<Self> {
        Self::parse(&std::env::var(ENABLE_ENV).unwrap_or_default())
    }

    pub fn tcp(self) -> bool {
        self != Self::Only
    }

    pub fn transport(self) -> bool {
        self != Self::Off
    }
}

/// Parses peer ids separated by commas, whitespace or newlines; `#` starts a comment.
pub fn parse_peer_list(text: &str) -> anyhow::Result<Vec<PeerId>> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or_default();
        for item in line.split(|c: char| c == ',' || c.is_whitespace()).filter(|s| !s.is_empty()) {
            let peer: PeerId = item.parse().map_err(|e| anyhow::anyhow!("allowed peer {item:?}: {e}"))?;
            if !out.contains(&peer) {
                out.push(peer);
            }
        }
    }
    Ok(out)
}

/// `<state dir>/allowed-peers`.
pub fn allowed_peers_file(state_dir: &Path) -> PathBuf {
    state_dir.join(ALLOWED_PEERS_FILE)
}

/// The allow-list: `EMBER_NODE_ALLOWED_PEERS` plus the state dir's `allowed-peers` file (a
/// missing file is an empty list).
pub fn allowed_peers(state_dir: Option<&Path>) -> anyhow::Result<Vec<PeerId>> {
    let mut peers = parse_peer_list(&std::env::var(ALLOWED_PEERS_ENV).unwrap_or_default())?;
    if let Some(dir) = state_dir {
        match std::fs::read_to_string(allowed_peers_file(dir)) {
            Ok(text) => {
                for p in parse_peer_list(&text)? {
                    if !peers.contains(&p) {
                        peers.push(p);
                    }
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(peers)
}

/// Binds the real transport with the node's persistent key (`<state dir>/transport.key`): with
/// the hub's relay and directory when the node is registered ([`crate::hub::discovered_transport_config`]),
/// otherwise with the relay from `EMBER_RELAY_URL`.
pub async fn bind(state_dir: &Path) -> anyhow::Result<Transport> {
    let key = SecretKey::load_or_generate(state_dir.join(KEY_FILE))?;
    Ok(Transport::bind(crate::hub::discovered_transport_config(state_dir, key).await?).await?)
}

/// Starts listening on [`NODE_SERVICE`] now and returns the future that serves the node API to
/// peers `gate` admits (until dropped, or forever once the transport closes).
pub fn serve(
    transport: &Transport,
    node: Node,
    gate: PeerGate,
) -> io::Result<impl Future<Output = io::Result<()>> + Send + 'static> {
    let listener = transport.listen(NODE_SERVICE).map_err(io::Error::other)?;
    Ok(api::serve(HttpListener::with_gate(listener, gate), node))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_and_peer_lists_parse() {
        assert_eq!(TransportMode::parse("").unwrap(), TransportMode::Off);
        assert_eq!(TransportMode::parse("1").unwrap(), TransportMode::Also);
        assert_eq!(TransportMode::parse("only").unwrap(), TransportMode::Only);
        assert!(TransportMode::parse("maybe").is_err());

        let a = SecretKey::generate().peer_id();
        let b = SecretKey::generate().peer_id();
        let text = format!("# servers\n{a}  # home\n\n{b},{a}\n");
        assert_eq!(parse_peer_list(&text).unwrap(), vec![a, b]);
        assert!(parse_peer_list("not-a-key").is_err());

        let dir = tempfile::tempdir().unwrap();
        assert!(allowed_peers(Some(dir.path())).unwrap().is_empty() || std::env::var(ALLOWED_PEERS_ENV).is_ok());
        std::fs::write(allowed_peers_file(dir.path()), format!("{b}\n")).unwrap();
        assert!(allowed_peers(Some(dir.path())).unwrap().contains(&b));
    }
}
