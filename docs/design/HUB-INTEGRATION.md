# Hub integration — Ember on darkpyonix.dev (FR-N2)

Status: **implemented, not compiled yet** (written without running cargo). Contract:
`darkpyonix-core` `docs/api/hub.openapi.yaml` v0.3.0 (branch `feat/m4-hub-workers`), Worker
source `hub/worker/src/{devices,directory,pkarr}.ts`.

## What it does

| Need (SPEC) | How |
| ----------- | --- |
| A computer joins by signing in; no address or port entered (FR-N2) | Device link: `POST /v1/device-links` → user code + verification URL → person approves (browser, or Ember as the account's main server) → device polls `POST /v1/device-links/{id}/token` with an Ed25519 signature over `darkpyonix-hub/v2/link\n<link_id>\n<challenge>` → device token. |
| Address directory | The transport publishes its signed record to `PUT /pkarr/{z32 key}` and resolves peers with `GET /pkarr/{key}?token=<device token>`. |
| Relay | The hub's relay host (`https://relay.<hub host>`, iroh relay protocol at `/relay`). |
| Add a computer without pasting a `PeerAddr` | `GET /v1/devices` → pick → `POST /api/v1/hub/devices/{endpoint_id}/computer {token}`; the computer row stores the bare peer id, resolved through the directory. |
| FR-N3 allow-list from the account | Opt-in sync: `hub` rows of the server's `devices` table = account devices (minus the server). |
| Revocation on the hub | `GET /v1/me` → `401` with a token that worked = removed. Recorded, surfaced, token no longer used. |

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
- **FR-N5 holds:** no iroh type leaves `transport/`; the hub crate, server and node only see
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

## Pieces

| Where | What |
| ----- | ---- |
| `transport/src/config.rs` | `HubDirectory`; `TransportConfig::hub`. |
| `transport/src/iroh_backend.rs` | `enable_hub` (iroh `PkarrPublisher` + token-switchable `HubResolver`), `set_directory_token`, `set_relays`. |
| `transport/src/key.rs` | `SecretKey::sign`, `PeerId::verify` (the link proof of possession). |
| `hub/` (`ember-hub`, new) | `HubConfig` (`EMBER_HUB_URL`, default `https://darkpyonix.dev`; relay derived as `relay.<host>`, override `EMBER_HUB_RELAY_URL`; `EMBER_RELAY_URL` wins), `HubClient`, `DeviceLink`, `Registration` / `RegistrationFile` (0600), `check_registration` / `watch_registration`, `z32`, `fake::FakeHub`. |
| `server/src/hub/` | `ServerHub`: link as `main_server`, token sealed with `secret.key` (AAD `ember/hub-device-token/v1:<endpoint>`) in `hub_registration` (migration 8), revocation watch (60 s), device list, add computer, approve codes, devices sync. `api::router` (status, devices, add computer: also over the transport) and `api::admin_router` (link, check, forget, remove, codes, sync: TCP only). |
| `server/src/devices/` | `source` column (`local` / `hub`), `Devices::sync_from_hub`. |
| `node/src/hub.rs` | `ember-node hub register|status|forget`, `<state dir>/hub.json`, transport config from the registration, watch (revocation → `revoked_at`, token cleared), `EMBER_NODE_HUB_ALLOW_SERVERS=1` admits the account's main servers. SIGHUP picks up a new registration. |

### Server API

Shared (TCP + transport): `GET /api/v1/hub`, `GET /api/v1/hub/devices`,
`POST /api/v1/hub/devices/{endpoint_id}/computer {name?, token}`.
Local only: `POST|DELETE /api/v1/hub/link`, `POST /api/v1/hub/check`,
`DELETE /api/v1/hub/registration`, `DELETE /api/v1/hub/devices/{id}`,
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

- `hub/tests/link_flow.rs` — approve in browser, claim once, re-link refused; the main server
  approves, a computer cannot; deny, expiry, wrong-key proof, malformed request.
- `hub/tests/revocation.rs` — watch sees `Revoked` after removal, stops; unreachable ≠ revoked.
- `hub/tests/directory.rs` — two real (iroh) endpoints on localhost publish to the fake hub's
  `/pkarr` and dial by peer id alone; an unregistered endpoint is not stored and a tokenless one
  resolves nothing until `set_directory_token`.
- `server/tests/hub.rs` — link via API (sealed token), add a computer from the device list and
  reach it over the (fake) transport, approve a node's code, devices sync + revocation through
  it, revocation detected by `check`, by any hub call, and the watcher; 503 when off.
- `node/tests/hub.rs` — `register` writes 0600 `hub.json` and configures the transport; removal
  marks `revoked_at` and drops hub-admitted servers.
- `scripts/check-transport-isolation.sh` passes (run).

## Spec gaps (for the darkpyonix leader)

Things Ember needs that `hub.openapi.yaml` v0.3.0 does not provide:

1. **Relay (and directory) discovery.** No endpoint names the relay host. Ember derives
   `https://relay.<hub host>`, which breaks for self-hosted or test hubs on other layouts. Ask:
   an unauthenticated `GET /v1/config` (or `/.well-known/darkpyonix-hub`) returning
   `{relay_urls, pkarr_url, api_version}`.
2. **Machine-readable revocation.** A removed device's token gets the same `401` as a malformed or
   unknown one, so "removed" is inferred. Ask: an error `code` in the `{"error"}` body
   (e.g. `device_removed`, with `removed_at`), or `410 Gone` for removed devices' tokens.
3. **A client role.** Roles are `main_server | computer`. Ember's native clients (phones,
   laptops) also need to be devices of the account for the FR-N3 allow-list, and must not show
   up in "add a computer". Ask: role `client` (no account rights).
4. **Approving `main_server` links.** A main server's token can approve any code, including one
   asking for `main_server` (account rights). Ask: links with role `main_server` approvable only
   by a browser session.
5. **Self-removal.** A computer cannot leave the account (`DELETE /v1/devices/{id}` needs account
   rights). Ask: allow a device to delete itself with its own token (`DELETE /v1/me` or own id).
6. **Rejoining after removal.** Removed keys are never reused, so a server removed by mistake must
   take a new identity, and every node's allow-list entry for it changes. Ask: an owner-only
   `POST /v1/devices/{id}/restore` (session), or re-linking a removed key with session approval.
7. **Change notification for the device list.** Ember polls `GET /v1/devices` (60 s) to sync the
   allow-list, so a removal on the hub reaches the server's gate within a poll, not "within one
   heartbeat" (FR-N3). Ask: `ETag`/`If-None-Match` on `/v1/devices`, plus a long-poll or SSE
   `GET /v1/events` (device added/removed) — or relay-side disconnect is enough only for relayed
   paths, not direct ones.
8. **Per-device application metadata.** Nothing says a `computer` runs ember node (vs another
   DarkPyonix machine) or which version/services it offers. Ask: an optional `app` object at link
   time (`{"name":"ember-node","version":…,"services":["ember-node/1"]}`), returned in `Device`.
9. **Token in the query for `GET /pkarr`.** Needed because iroh's resolver sends no headers;
   tokens in URLs can end up in logs. Ask: the Worker never logs query strings for `/pkarr`, and
   optionally a separate resolve-only token (scope: `GET /pkarr` of the account) so the full
   device token is not put in URLs.
10. **Device rename.** No `PATCH /v1/devices/{id} {name}`; Ember shows hub names as computer names.
11. **Link resume.** `GET /v1/device-links/{link_id}` (status without the signature) would let a
    restarted device show "pending/denied/expired" without polling the claim endpoint.

## Open (Ember-side)

- The native client crate has no hub calls yet (`client/src/api.rs` has no computers calls
  either); the routes are ready for it.
- Approving codes from a phone over the transport is local-only for now (it grants account
  membership).
- Lock files: `server`, `node`, `transport` `Cargo.lock` must be refreshed for the new
  dependencies before CI's `cargo test --locked` passes.
