# Hub integration: Ember on darkpyonix.dev (FR-N2)

Status: **implemented, not compiled yet** (written without running cargo). Contract:
`darkpyonix-core` `docs/api/hub.openapi.yaml` (info v0.3.0) and SPEC FR-H1, FR-H8–H11,
NFR-H2 as merged for review in darkpyonix-core PR #34 (branch `feat/m4-hub-ember-gaps`, base
`feat/m4-hub-workers`). Worker source `crates/hub/worker/src/`.

## What it does

| Need (SPEC) | How |
| ----------- | --- |
| A computer joins by signing in; no address or port entered (FR-N2) | Device link: `POST /v1/device-links` → user code + verification URL → person approves (browser, or Ember as the account's main server for `computer` / `client` links) → device polls `POST /v1/device-links/{id}/token` with an Ed25519 signature over `darkpyonix-hub/v2/link\n<link_id>\n<challenge>` → device token (`dpd_`, headers only) + resolve token (`dpr_`). A restarted device resumes with `GET /v1/device-links/{id}`. |
| Address directory | The transport publishes its signed record to `PUT /pkarr/{z32 key}` and resolves peers with `GET /pkarr/{key}?token=<resolve token>` (a device token there is refused, NFR-H2). |
| Relay | `GET /v1/config` `relay_urls`; fallback the hub's relay host (`https://relay.<hub host>`, iroh relay protocol at `/relay`). |
| Add a computer without pasting a `PeerAddr` | `GET /v1/devices` → pick → `POST /api/v1/hub/devices/{endpoint_id}/computer {token}`; the computer row stores the bare peer id, resolved through the directory. |
| FR-N3 allow-list from the account | Opt-in sync: `hub` rows of the server's `devices` table = account devices (minus the server). |
| Revocation on the hub | `401 {code: device_removed}` (or a code-less `401` from a hub older than the codes) = removed; seen by a held `GET /v1/devices?wait=25` (FR-H9) within the hub's ~2 s check when `api_version` ≥ 1, else by `GET /v1/me` every 60 s. Recorded, surfaced, token no longer used. `401 {code: invalid_credentials}` is not a revocation. |
| Leaving / rejoining | `forget` removes the device on the hub with its own token (`DELETE /v1/devices/{own id}`). A removed key rejoins after the owner re-admits it (`POST /v1/devices/{id}/readmit`, session, 15 min) and approves its new link in the browser (FR-H11). |
| What a device runs | `PATCH /v1/devices/{own id} {app: {kind, version, services}}` after registering and at start: `ember-server` / `ember-node` (FR-H10). |

## Design choice: iroh's pkarr publisher/resolver, behind the transport's own type

The hub's `/pkarr` **is** iroh's pkarr relay protocol (the spec says the stock `PkarrPublisher` and
`PkarrResolver` work against it). So the directory is not a custom `AddressDirectory`; it is
`ember_transport::HubDirectory { pkarr_url, token, publish_direct }` (plain data in
`TransportConfig::hub`), which the iroh backend turns into iroh's `PkarrPublisher` plus a
`PkarrResolver` whose URL carries the token.

Why not a `HubDirectory: AddressDirectory` outside the transport:

- **Signing.** `PUT /pkarr` needs a pkarr `SignedPacket` (BEP 44 signature over a DNS packet with
  `_iroh` TXT records). `AddressDirectory::publish(&PeerAddr)` has no key and no packet encoder;
  re-implementing iroh-dns's encoding outside the transport would duplicate iroh internals and
  break FR-N5 in spirit.
- **End-to-end verification.** iroh's resolver verifies the packet against the peer's key, so the
  hub cannot redirect a peer. A JSON resolver (`/v1/devices/{id}/addresses`) would trust the hub.
- **Republish, TTL, backoff** come with iroh's publisher.
- **FR-N5 holds:** no iroh type leaves `crates/transport/`; the hub crate, server and node only see
  `HubDirectory`, `Transport::enable_hub`, `Transport::set_directory_token`, `Transport::set_relays`.

The `AddressDirectory` slot stays (tests, `MemoryDirectory`). The in-memory backend ignores
`HubDirectory` (it resolves by id already).

Runtime switching: the server and the node can register while running.
`Transport::enable_hub` adds the publisher and resolver to the running endpoint (iroh's
`AddressLookupServices::add` replays the last address data to a new service), and
`Transport::set_relays` swaps the relay map (`Endpoint::insert_relay` / `remove_relay`).
`set_directory_token(None)` disables resolving after a revocation.

`publish_direct` is on: the hub shows a record only to devices of the same account, and direct
addresses let peers skip the relay. Turning it off keeps IP addresses away from the hub.

## Hub additions used (PR #34)

**Discovery (FR-H8).** `HubConfig::discover()` asks `GET /v1/config` (public, cacheable 5
minutes; 5 s timeout):
`{api_version: 1, hub_version, relay_urls, pkarr_url, link_url}`.

- `relay_urls` replace the derived `https://relay.<host>` (all go to the transport as
  `RelayConfig::Custom`, trailing `/` trimmed); `EMBER_HUB_RELAY_URL` still wins, and
  `EMBER_RELAY_URL` wins over both. `pkarr_url` replaces `<hub>/pkarr`. `link_url` is where Ember
  sends a person to approve codes it may not approve itself.
- `404` / `405` / `501`, an unreachable hub, or an undecodable answer: the derived values, as
  before.
- When it is asked: ember server at start only when registered (or `EMBER_HUB_URL` is set), and
  when a registration completes, so an unregistered server does not contact the hub; ember node
  when it binds the transport (registered or `EMBER_HUB_URL`), on SIGHUP after a new
  registration, and once per watch. `GET /api/v1/hub` shows `relay_urls`, `relay_source`
  (`derived` / `explicit` / `hub`), `pkarr_url`, `api_version` and `long_poll`.

**Device list: `ETag` and long-poll (FR-H9).** `HubClient::devices_since(etag, wait)` sends
`If-None-Match` and, with `wait`, `?wait=<1..25>` (request timeout `wait + 15 s`). The weak
`ETag` (`W/"v12"`) is the list's version: it changes on add, restore, remove, rename, app, or
on/offline, not on `last_seen` alone. `304` = unchanged; `200` = new list and `ETag`; the hub
checks about every 2 s, and a waiting device that is removed gets `401 device_removed`.
`DeviceWatcher` loops on it: the first call returns the list at once, later calls return only
when it changed.

- Long-poll is the default when `/v1/config` says `api_version` ≥ 1 (the version that added the
  endpoint; `api_version` changes only on breaking changes). Without `/v1/config` the watcher
  polls every 60 s (still conditional).
- A hub that answers `wait` at once three times in a row, or rejects it with `400`, drops the
  watcher to polling. Errors back off (1 s doubling, capped at the period and 30 s).
- A watcher on a device token also treats its own absence from the list as removal.
- Users: `ember_hub::watch_registration_with`, `ServerHub::spawn_watch` (revocation + devices
  allow-list sync, on change), ember node's `hub::spawn_watch` (revocation + admitting main
  servers).

**Removed device vs bad token.** Every `401` has a `code`. `device_removed` →
`HubError::DeviceRemoved` → revocation (recorded, token dropped). `invalid_credentials` →
`HubError::InvalidCredentials` → `RegistrationState::Rejected`: logged and shown
(`POST /api/v1/hub/check` → `rejected`, `ember-node hub status`), the token kept. A code-less
`401` (only from a hub older than the codes) is still read as removal. The hub does not use
`410`; Ember does not treat it specially.

**Resolve token (NFR-H2).** The pkarr resolver's URL carries the read-only `dpr_` token; a device
token in `?token=` is refused (`401 invalid_credentials`). The claim returns it; a registration
from before gets one with `POST /v1/me/resolve-token` (device token in the header; rotates, so
only when missing). ember node keeps it in `hub.json`; ember server seals it like the device token
(AAD `ember/hub-resolve-token/v1:<endpoint>`, store migration 11 adds `resolve_nonce`,
`resolve_ciphertext`).

**Approving codes (FR-H1).** The main server's token approves `computer` and `client` links; a
link asking for `main_server`, or a re-admitted key's link, needs a signed-in browser session
(the hub answers `403`, denying is allowed). `POST /api/v1/hub/link-codes/{code}` then answers
`403` pointing to `<link_url>?code=<code>`.

**Leaving and rejoining (FR-H1, FR-H11).** `DELETE /api/v1/hub/registration` and
`ember-node hub forget` remove the device on the hub with its own token, then forget it locally
(`?local=1` / `--local`: local only). A device the hub removed may link again with the same key;
until the owner re-admits it (`POST /v1/devices/{id}/readmit`, a session, valid 15 minutes) the
hub answers `409`, which Ember turns into those instructions. The re-admitted key's link is
approved in the browser and the same device row comes back with new tokens.

**Link resume (FR-H1).** ember server stores the pending `link_id` (`hub_pending_link`, store
migration 11) and ember node `<state dir>/hub-link.json`; after a restart
`DeviceLink::resume_by_id` reads `GET /v1/device-links/{id}` and keeps polling a pending or
approved link (a denied, expired or claimed one is reported and forgotten).

**App record (FR-H10).** `PATCH /v1/devices/{own id} {app}` after registering and at start:
`{kind: "ember-server", version: <crate version>, services: ["ember-server-v1"]}` and
`{kind: "ember-node", …, services: ["ember-node-v1"]}`. Service labels are the transport service
names in the hub's `^[a-z][a-z0-9-]{0,31}$` form (`/` → `-v`).

**Client role (FR-H1, provisional).** `Role::Client` exists in the hub crate (connect, list,
publish / resolve, relay; no account rights, names or shares) for the Ember client to register
with; nothing in Ember uses it yet.

## Pieces

| Where | What |
| ----- | ---- |
| `crates/transport/src/config.rs` | `HubDirectory`; `TransportConfig::hub`. |
| `crates/transport/src/iroh_backend.rs` | `enable_hub` (iroh `PkarrPublisher` + token-switchable `HubResolver`), `set_directory_token`, `set_relays`. |
| `crates/transport/src/key.rs` | `SecretKey::sign`, `PeerId::verify` (the link proof of possession). |
| `crates/hub/` (`ember-hub`, new) | `HubConfig` (`EMBER_HUB_URL`, default `https://darkpyonix.dev`; relay and directory from `/v1/config` via `discover`, else relay derived as `relay.<host>`; override `EMBER_HUB_RELAY_URL`; `EMBER_RELAY_URL` wins), `HubInfo`, `HubClient` (`config`, `devices_since`), `DeviceWatcher`, `DeviceLink`, `Registration` / `RegistrationFile` (0600), `check_registration` / `watch_registration` / `watch_registration_with`, `z32`, `fake::FakeHub` (the PR #34 contract: `/v1/config`, versioned weak `ETag` + `?wait=0..25`, `device_removed` / `invalid_credentials`, resolve tokens, `PATCH`, self-removal, readmit, link status, `client` role). |
| `crates/server/src/hub/` | `ServerHub`: link as `main_server` (resumed after a restart), device and resolve tokens sealed with `secret.key` in `hub_registration` (migrations 10 and 11), revocation watch (long-poll, else 60 s), app record, device list, add computer, approve codes, devices sync, leave. `api::router` (status, devices, add computer: also over the transport) and `api::admin_router` (link, check, forget, remove, codes, sync: TCP only). |
| `crates/server/src/devices/` | `source` column (`local` / `hub`), `Devices::sync_from_hub`. |
| `crates/node/src/hub.rs` | `ember-node hub register|status|forget [--local]`, `<state dir>/hub.json` (+ `hub-link.json` while waiting), app record, transport config from the registration, watch (revocation → `revoked_at`, token cleared), `EMBER_NODE_HUB_ALLOW_SERVERS=1` admits the account's main servers. SIGHUP picks up a new registration. |

### Server API

Shared (TCP + transport): `GET /api/v1/hub`, `GET /api/v1/hub/devices`,
`POST /api/v1/hub/devices/{endpoint_id}/computer {name?, token}`.
Local only: `POST|DELETE /api/v1/hub/link`, `POST /api/v1/hub/check`,
`DELETE /api/v1/hub/registration[?local=1]`, `DELETE /api/v1/hub/devices/{id}`,
`GET|POST /api/v1/hub/link-codes/{code}`, `POST|PUT /api/v1/hub/sync-devices`.
A revoked registration answers `410 Gone`; the hub being off (no `EMBER_TRANSPORT`, or
`EMBER_HUB_URL=off`) answers `503`.

### When the hub is used

- Server: only with `EMBER_TRANSPORT=1`. The transport is bound with the hub's relay and
  directory when registered, or when `EMBER_HUB_URL` is set explicitly; otherwise it starts as
  before (no address is sent to darkpyonix.dev by an unregistered server) and switches when
  registration completes.
- Node: same rule, from `hub.json`.
- The node's API bearer token is still required when adding a computer (FR-N3 keeps it as a
  second factor); the hub does not carry it.

## Tests (not run yet)

- `crates/hub/tests/link_flow.rs`: approve in browser, claim once (with a resolve token), re-link
  refused; the main server approves computer and client links, a computer cannot, a
  `main_server` link needs the browser; deny, expiry, wrong-key proof, malformed request; resume
  by link id after a restart (pending, claimed, denied, unknown); a removed key rejoins only after
  readmission approved in the browser, as the same device row.
- `crates/hub/tests/revocation.rs`: watch sees `Revoked` after removal, stops; unreachable ≠ revoked;
  a removed device gets `DeviceRemoved`, a bogus token `InvalidCredentials`.
- `crates/hub/tests/config_longpoll.rs`: `/v1/config` discovery (relays, pkarr, explicit relay kept);
  fallback on `404` and when unreachable; weak `ETag` → `304`, held `304` at `wait`, `200` on
  change (online bumps the version); `wait=26` is `400`; long-poll returns within ~0.2 s of a
  change; `watch_registration_with` sees a removal within 1.5 s with a one-hour period; polling
  without `/v1/config`, and fallback when a hub ignores `wait`; `device_removed` is revocation,
  `invalid_credentials` is not; a watcher expecting itself treats its absence as removal; a device
  token in `/pkarr?token=` is refused and a resolve token accepted, an old registration gets one
  (rotation revokes the previous); rename, app (own token only), client without account rights,
  self-removal.
- `crates/hub/tests/directory.rs`: two real (iroh) endpoints on localhost publish to the fake hub's
  `/pkarr` and dial by peer id alone (resolving with resolve tokens); an unregistered endpoint is
  not stored, a tokenless one or one with a device token resolves nothing, the resolve token set
  at runtime works.
- `crates/server/tests/hub.rs`: link via API (sealed token), add a computer from the device list and
  reach it over the (fake) transport, approve a node's code, devices sync + revocation through
  it, revocation detected by `check`, by any hub call, and the watcher; the long-poll watcher
  (60 s period) syncs a new device and stops on removal within 2 s; a rejected token is not a
  revocation; the resolve token is stored sealed and read back, the app is reported; approving a
  `main_server` code answers 403 with the browser link; leaving removes the server on the hub
  (`?local=1` does not); a link pending at restart is resumed; a removed server relinks after
  readmission; 503 when off.
- `crates/node/tests/hub.rs`: `register` writes 0600 `hub.json` and configures the transport (and the
  discovered relay, or the derived one when `/v1/config` is `404`), resolving with the resolve
  token, reporting its app; removal marks `revoked_at` and drops hub-admitted servers (through
  the long-poll); re-registering is refused until readmitted, then works; an old registration
  gets a resolve token on bind; `forget` leaves the account (`--local` does not); an interrupted
  `register` resumes the same link.
- `scripts/check-transport-isolation.sh` passes (run).

## Spec gaps (for the darkpyonix leader)

The eleven gaps Ember reported against v0.3.0 are answered by PR #34: 1 `/v1/config` (FR-H8),
2 `401` codes, 3 `client` role (provisional), 4 `main_server` links need a session, 5 self-removal,
6 readmit (FR-H11, provisional), 7 `ETag` + long-poll (FR-H9, provisional), 8 `app` (FR-H10,
provisional), 9 resolve tokens and no URL logging (NFR-H2), 10 `PATCH` rename, 11 link status.
Left open or worth confirming:

- The long-poll wakes on a ~2 s check, so revocation and list changes reach Ember within about
  2 s plus a round trip, not instantly.
- `api_version` stays `1` while FR-H9 is provisional: if the long-poll were withdrawn without a
  bump, Ember would still send `wait` (harmless: the hub would answer at once and Ember would
  fall back to polling after three immediate answers).
- App `services` labels: Ember uses `ember-server-v1` / `ember-node-v1` (transport service names
  do not fit the label pattern). The hub's example uses names like `kernel-manager`; say if there
  is a preferred vocabulary.
- A device whose claim succeeded but which lost its tokens (crash between claim and save) can
  only recover by remove + readmit; a claimed link's status does not say whether tokens were
  delivered.

## Open (Ember-side)

- The native client crate has no hub calls yet (`crates/client/src/api.rs` has no computers calls
  either); the routes are ready for it.
- Approving codes from a phone over the transport is local-only for now (it grants account
  membership).
- The Ember client does not register yet; `Role::Client` and the link flow are ready for it.
- Lock files: `server`, `node`, `transport` `Cargo.lock` must be refreshed for the new
  dependencies before CI's `cargo test --locked` passes.
