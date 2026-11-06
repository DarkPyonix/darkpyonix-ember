//! The receive mode (SPEC `FR-F9`).
//!
//! [`decide`] only says what the mode asks for. The caller runs the registered-peer, size and
//! name checks first (see [`crate::session`]), so auto-accept never skips them.

use std::collections::HashSet;

use ember_transport::PeerId;

/// How a device treats an incoming offer. Default: [`ReceiveMode::Ask`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ReceiveMode {
    /// Ask the user for every offer.
    #[default]
    Ask,
    /// Accept without asking from these devices, ask for the others.
    AutoAcceptFrom(HashSet<PeerId>),
    /// Refuse every offer.
    Off,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Prompt,
    Accept,
    Refuse,
}

pub fn decide(peer: PeerId, mode: &ReceiveMode) -> Decision {
    match mode {
        ReceiveMode::Ask => Decision::Prompt,
        ReceiveMode::AutoAcceptFrom(set) if set.contains(&peer) => Decision::Accept,
        ReceiveMode::AutoAcceptFrom(_) => Decision::Prompt,
        ReceiveMode::Off => Decision::Refuse,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ember_transport::SecretKey;

    fn peer() -> PeerId {
        SecretKey::generate().peer_id()
    }

    #[test]
    fn fr_f9_ask_prompts() {
        assert_eq!(decide(peer(), &ReceiveMode::Ask), Decision::Prompt);
        assert_eq!(ReceiveMode::default(), ReceiveMode::Ask);
    }

    #[test]
    fn fr_f9_off_refuses_everyone() {
        assert_eq!(decide(peer(), &ReceiveMode::Off), Decision::Refuse);
    }

    #[test]
    fn fr_f9_auto_accept_only_from_chosen_devices() {
        let (a, b) = (peer(), peer());
        let mode = ReceiveMode::AutoAcceptFrom([a].into_iter().collect());
        assert_eq!(decide(a, &mode), Decision::Accept);
        assert_eq!(decide(b, &mode), Decision::Prompt);
    }
}
