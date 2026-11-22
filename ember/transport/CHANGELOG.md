# Changelog: ember-transport

## Unreleased

### Behaviour changes

- **UDP hole punching is on by default on the letify backend** (EM-11, #10, SPEC `FR-N1`,
  `FR-N2`). An endpoint with a rendezvous punches a direct UDP path to the peer it dials and
  answers the punch offers of the devices it admits; the relay carries the session only while no
  direct path is live. A dial to the main server itself is never punched.
- **The address observer of a punch defaults to the main server's relay host on UDP 7443**,
  pinned to the relay's peer id. `EMBER_LETIFY_OBSERVER` still overrides it.
- **`EMBER_LETIFY_UDP_PUNCH=off` turns punching off**: the endpoint then neither offers nor
  answers a punch, and its sessions use TCP and the relay as before.
- letify-link is pinned to the crates.io release `=3.0.5` instead of a git revision.
