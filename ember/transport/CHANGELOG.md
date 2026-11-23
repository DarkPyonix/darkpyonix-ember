# Changelog: ember-transport

## Unreleased

### Behaviour changes

- **Hole punching is on by default on the letify backend** (EM-11, #10, SPEC `FR-N1`, `FR-N2`).
  letify-link's public punch driver runs each punch: TCP simultaneous open first, UDP beside
  it. An endpoint with a rendezvous punches a direct path to the peer it dials and answers the
  punch offers of the devices it admits; the relay carries the session only while no direct path
  is live, and a failed punch leaves the session on the relay. A dial to the main server itself
  is never punched.
- **The address observer of a punch defaults to the main server's relay host on UDP 7443**,
  pinned to the relay's peer id. `EMBER_LETIFY_OBSERVER` still overrides it.
- **letify's signalling Worker carries the punch exchange by default** (owner, 2026-11-23,
  ember#10). The dialing side punches through `letify.darkpyonix.dev` and wakes the peer with an
  18 byte message through the hub's pkarr mailbox; the peer polls the Worker only after a
  wake-up, for one punch. Concurrent Worker punches (the main server answering several devices,
  and dials) run at once on one windowed `Demux` (letify-link 3.0.8), not one at a time. A
  Worker failure falls back to the punch over Ember's own rendezvous,
  then to the relay. `EMBER_LETIFY_WORKER_RENDEZVOUS=0` turns it off.
- **`EMBER_LETIFY_UDP_PUNCH=off` turns punching off**: the endpoint then neither offers nor
  answers a punch, and its sessions use TCP and the relay as before.
- letify-link is pinned to the crates.io release `=3.0.8` (its public punch driver, with the
  `signal` feature the signalling Worker client, and `rendezvous::Demux`) instead of a git
  revision.
