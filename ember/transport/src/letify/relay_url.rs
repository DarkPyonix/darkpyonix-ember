//! The relay as an Ember URL: `letify://<relay peer id hex>@host:port`.
//!
//! letify-link authenticates a relay by its node id (SPEC letify-link, Relay item 3), so the
//! address alone is not enough. On Ember the relay is the main server, whose peer id every device
//! already knows.

use letify_link::NodeId;

use crate::PeerId;

/// A relay a node or client registered.
#[derive(Clone, Debug)]
pub(super) struct RelayTarget {
    /// `host:port` of the relay's TCP listener.
    pub(super) addr: String,
    /// The relay's node id.
    pub(super) node: NodeId,
    /// The full URL, as written.
    pub(super) url: String,
}

/// Parses `letify://<64 hex>@host:port`.
pub(super) fn parse_relay_url(url: &str) -> Result<RelayTarget, String> {
    let url = url.trim();
    let rest = url.strip_prefix("letify://").ok_or_else(|| format!("{url:?} is not a letify:// relay URL"))?;
    let (id, addr) = rest
        .split_once('@')
        .ok_or_else(|| format!("{url:?}: expected letify://<relay peer id>@host:port"))?;
    let addr = addr.trim_end_matches('/');
    if addr.is_empty() || !addr.contains(':') {
        return Err(format!("{url:?}: the relay address must be host:port"));
    }
    let peer: PeerId = id.parse().map_err(|e| format!("{url:?}: relay peer id: {e}"))?;
    let node = NodeId::try_from_bytes(*peer.as_bytes()).map_err(|e| format!("{url:?}: relay peer id: {e}"))?;
    Ok(RelayTarget { addr: addr.to_string(), node, url: url.to_string() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SecretKey;

    #[test]
    fn a_relay_url_names_the_relay_peer_and_its_address() {
        let peer = SecretKey::generate().peer_id();
        let t = parse_relay_url(&format!("letify://{peer}@relay.example.org:7843/")).unwrap();
        assert_eq!(t.addr, "relay.example.org:7843");
        assert_eq!(t.node.as_bytes(), peer.as_bytes());
        assert!(parse_relay_url("https://relay.example.org").is_err());
        assert!(parse_relay_url("letify://nothex@host:1").is_err());
        assert!(parse_relay_url(&format!("letify://{peer}@host")).is_err());
    }
}
