//! The door-agnostic configuration parsing. Moved without change from `guest_door.rs`; the tests
//! in `ember/server/tests/fr_t14_guest_door_characterization.rs` pin it.

use std::collections::HashSet;
use std::net::{IpAddr, SocketAddr};

use crate::api::origin::host_of;
use crate::door::limits::normalize_ip;
use crate::door::{err, DoorError};

pub(crate) fn parse_public_url(var: &str, u: &str) -> Result<String, DoorError> {
    let bad = || DoorError(format!("{var}={u:?} must be http(s)://host[:port] (scheme and authority only)"));
    let u = u.strip_suffix('/').unwrap_or(u);
    let (scheme, rest) = u.split_once("://").ok_or_else(bad)?;
    if scheme != "http" && scheme != "https" {
        return Err(bad());
    }
    if rest.is_empty() || rest.contains(['/', '?', '#', '@', ' ', '\\']) {
        return Err(bad());
    }
    if host_of(rest).is_empty() {
        return Err(bad());
    }
    Ok(format!("{scheme}://{rest}"))
}

/// Whether two listen addresses clash: equal, or the same port with an unspecified address
/// (0.0.0.0, ::) on either side, which takes that port on every interface.
pub(crate) fn listen_clash(a: SocketAddr, b: SocketAddr) -> bool {
    let same_port = a.port() == b.port();
    a == b || (same_port && (a.ip().is_unspecified() || b.ip().is_unspecified()))
}

/// The names a door reads and the noun its messages use (`"guest door"` for the guest door).
pub(crate) struct Spec<'a> {
    pub listen_var: &'a str,
    pub allow_remote_var: &'a str,
    pub trusted_var: &'a str,
    pub hosts_var: &'a str,
    pub public_url_var: &'a str,
    pub noun: &'a str,
}

/// The parsed pieces of a door's environment.
pub(crate) struct DoorEnv {
    pub listen: SocketAddr,
    pub public_url: Option<String>,
    pub hosts: HashSet<String>,
    pub trusted: Vec<IpAddr>,
}

impl DoorEnv {
    /// `others` are the listen addresses (with the variable name shown in the error) that the door
    /// must not clash with. The checks run in this order: listen syntax, clash with each of
    /// `others`, remote opt-in, trusted proxies, public URL.
    pub(crate) fn parse(
        get: impl Fn(&str) -> Option<String>,
        others: &[(&str, SocketAddr)],
        spec: &Spec<'_>,
    ) -> Result<Option<Self>, DoorError> {
        let listen_var = spec.listen_var;
        let raw = get(listen_var).unwrap_or_default();
        let raw = raw.trim();
        if raw.is_empty() {
            return Ok(None);
        }
        let listen: SocketAddr = match raw.parse() {
            Ok(a) => a,
            Err(_) => return err(format!("{listen_var}={raw:?} is not an ip:port address")),
        };
        for (name, other) in others {
            if listen_clash(listen, *other) {
                return err(format!("{listen_var} must differ from {name} ({other})"));
            }
        }
        // A non-loopback bind needs the owner's explicit word; main.rs logs a warning when it happens.
        if !listen.ip().is_loopback() && get(spec.allow_remote_var).map(|v| v.trim() == "1") != Some(true) {
            let (allow_remote_var, noun) = (spec.allow_remote_var, spec.noun);
            return err(format!(
                "{listen_var}={listen} is not a loopback address: set {allow_remote_var}=1 to expose the \
                 {noun} to a network (it speaks plain HTTP: put TLS in front of it)"
            ));
        }
        let mut trusted: Vec<IpAddr> = Vec::new();
        for e in get(spec.trusted_var).unwrap_or_default().split(',') {
            let e = e.trim();
            if e.is_empty() {
                continue;
            }
            match e.parse::<IpAddr>() {
                Ok(ip) => trusted.push(normalize_ip(ip)),
                Err(_) => return err(format!("{}: {e:?} is not an IP address", spec.trusted_var)),
            }
        }
        let public_url = match get(spec.public_url_var).map(|v| v.trim().to_string()).filter(|v| !v.is_empty()) {
            Some(u) => Some(parse_public_url(spec.public_url_var, &u)?),
            None => None,
        };
        let mut hosts: HashSet<String> = HashSet::new();
        if !listen.ip().is_unspecified() {
            hosts.insert(listen.ip().to_string().to_ascii_lowercase());
        }
        if listen.ip().is_loopback() {
            hosts.insert("localhost".into());
        }
        for h in get(spec.hosts_var).unwrap_or_default().split(',') {
            let h = host_of(h.trim());
            if !h.is_empty() {
                hosts.insert(h);
            }
        }
        if let Some(u) = &public_url {
            let authority = u.split_once("://").map(|x| x.1).unwrap_or("");
            hosts.insert(host_of(authority));
        }
        Ok(Some(Self { listen, public_url, hosts, trusted }))
    }
}
