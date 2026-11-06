//! The door-agnostic configuration parsing. Moved without change from `guest_door.rs`; the tests
//! in `ember/server/tests/fr_t14_guest_door_characterization.rs` pin it.

use crate::api::origin::host_of;
use crate::door::DoorError;
use crate::guest_door::PUBLIC_URL_VAR;

pub(crate) fn parse_public_url(u: &str) -> Result<String, DoorError> {
    let bad = || DoorError(format!("{PUBLIC_URL_VAR}={u:?} must be http(s)://host[:port] (scheme and authority only)"));
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
