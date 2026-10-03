# EDITOR-CONNECTION.md — How Ember's editor core talks to a Code-OSS server

> Status: design + first code (`editor-conn/`, package `ember-editor-conn`), **not compiled yet**.
> Milestone: M8 (`PROJECT.md`), SPEC §E (`FR-E1`–`FR-E5`). Written 2026-10-03.

## 1. Why this exists

"Open IDE → Ember" is our own editor core. dioxus-compose rebuilds the Code-OSS workbench DOM in
Rust and draws it with Code-OSS's own CSS; Monaco is replaced by a native code-editor widget. There
is no webview and no JS engine in that window. Two rules from `INTENT.md` frame the problem:

- **E4 / D2 / D11:** the official Node **extension host is never reimplemented**. Extensions keep
  running, unmodified, in the extension host that the Code-OSS server spawns.
- **E6:** where Ember and VS Code disagree, VS Code is right. Ember copies VS Code's behaviour on
  the wire, not just its intent.

So everything the workbench's JavaScript did to talk to the server has to be done in Rust: open the
two persistent connections, speak the IPC channel protocol on one and the extension-host RPC
protocol on the other, keep the extension host's copy of every open document in step, and answer
the extension host's calls well enough that extensions run. `IMPLEMENTATION.md` §3 picks the first
features: category 2, i.e. diagnostics, CodeLens, hover and then inline completions. Its §6 and
`PROJECT.md` Q2 flag the open question this document answers: **how stable is the renderer ↔
extension-host protocol?**

The default runtime is OSE (Code-OSS built by DarkPyonix, Open VSX; `INTENT.md` D10). Because we
build OSE ourselves, we always know the exact server commit. That fact carries much of §5.

## 2. Pinned source

All citations are to `microsoft/vscode` at tag **`1.139.1`**, commit
**`04c0d99f4fb0d8afe6ce4f0c58e31e183ac3e4b1`**: the release OSE is built from (`ose/VERSION`).
Paths are relative to `src/vs/`. Line numbers are at that commit.

The pin has one source of truth per side, and they are checked against each other:

- `ose/VERSION` names the tag OSE is built from (`ose/build.sh`).
- `ember_editor_conn::PINNED_VERSION` / `PINNED_COMMIT` (`editor-conn/src/lib.rs`) name the tag and
  commit the crate's tables and citations come from. The unit test `pin_matches_ose_version` fails
  if `PINNED_VERSION` differs from `ose/VERSION`; the CI job `editor-conn-pin`
  (`.github/workflows/checks.yml`) clones the `ose/VERSION` tag, re-runs `gen_rpc_ids.sh`, and
  fails if `rpc_ids.rs`, `PINNED_VERSION` or `PINNED_COMMIT` differ (§5).
- At connect time, `handshake::verify_server` compares the server's `GET /version` with
  `PINNED_COMMIT` (§3.4).

The crate was first written against `0036dcb6` (1.141.0) and re-aligned to 1.139.1 on 2026-10-03.
Between the two, the proxy table is identical (168 ids, same order) and no method or DTO of the
minimal subset (§4) changed; `extHost.protocol.ts` differs in five hunks only (authentication
options, progress DTOs, SCM, chat notebook edits, `$checkMcpServerAllowed`). Only line numbers
moved.

| Short name | Path |
| ---------- | ---- |
| `ipc.net.ts` | `base/parts/ipc/common/ipc.net.ts` |
| `ipc.ts` | `base/parts/ipc/common/ipc.ts` |
| `node/ipc.net.ts` | `base/parts/ipc/node/ipc.net.ts` |
| `remoteAgentConnection.ts` | `platform/remote/common/remoteAgentConnection.ts` |
| `managedSocket.ts` | `platform/remote/common/managedSocket.ts` |
| `agentServer.ts` | `server/node/remoteExtensionHostAgentServer.ts` |
| `serverServices.ts` | `server/node/serverServices.ts` |
| `rpcProtocol.ts` | `workbench/services/extensions/common/rpcProtocol.ts` |
| `proxyIdentifier.ts` | `workbench/services/extensions/common/proxyIdentifier.ts` |
| `extHost.protocol.ts` | `workbench/api/common/extHost.protocol.ts` |
| `extensionHostProtocol.ts` | `workbench/services/extensions/common/extensionHostProtocol.ts` |
| `remoteExtensionHost.ts` | `workbench/services/extensions/common/remoteExtensionHost.ts` |
| `extensionHostProcess.ts` | `workbench/api/node/extensionHostProcess.ts` |

## 3. Protocol facts

### 3.1 Topology

A workbench window opens **two persistent connections** to the server, both over the same HTTP
port. Each one is an HTTP upgrade followed by a `PersistentProtocol` stream:

1. **Management** (`ConnectionType.Management = 1`). It carries the IPC channel protocol (§3.6)
   for every server-side service: filesystem, environment, extension scanning, terminals, and so on.
2. **Extension host** (`ConnectionType.ExtensionHost = 2`). The server spawns a Node extension
   host and hands it the socket. From then on the socket carries the RPC protocol (§3.9),
   end-to-end between the renderer and the extension host.

`ConnectionType.Tunnel = 3` exists for port forwarding and is out of scope here.
(`remoteAgentConnection.ts` L26-30.)

Plain HTTP serves the rest. `GET /version` returns the server commit (`agentServer.ts` L145-148).
`/vscode-remote-resource?path=…` serves files such as extension icons (L162-186).

### 3.2 Upgrade and transport

- The client puts `reconnectionToken=<uuid>&reconnection=<bool>` in the query
  (`remoteAgentConnection.ts` L235). The server takes the token and flags from the query
  (`agentServer.ts` `handleUpgrade` L201-237). The request path is ignored.
- **`skipWebSocketFrames=true`** makes the server answer `101` and then use the raw TCP socket,
  with no WebSocket framing and no permessage-deflate (`node/ipc.net.ts` `upgradeToISocket`
  L22-86). Code-OSS uses this mode itself for managed sockets: `managedSocket.ts`
  `makeRawSocketHeaders` L11-26 sends `GET ws://localhost${path}?${query}&skipWebSocketFrames=true`.
  Ember uses this mode too, so the code runs over any byte stream: TCP, TLS, or later an
  `ember-transport` (iroh) stream. The browser path (`skipWebSocketFrames=false`) instead uses real
  WebSocket binary frames with optional deflate (`node/ipc.net.ts` L292ff.). Ember never needs it.
- The upgrade itself does **not** check the connection token. The token is checked in the `auth`
  handshake message (§3.4). The HTTP routes check it as `?tkn=` or the `vscode-tkn` cookie
  (`agentServer.ts` L157; `base/common/network.ts` L187-188).

### 3.3 PersistentProtocol framing (`ipc.net.ts`)

Every message has a 13-byte header: `type u8 | id u32be | ack u32be | len u32be`, followed by
`len` bytes of data. The header is written at L474-478 and read at L363-370, and
`ProtocolConstants.HeaderLength = 13` is at L290. The doc comment above `Protocol` says 9 bytes;
it is stale.

| type | name | meaning |
| ---- | ---- | ------- |
| 1 | Regular | Payload. Numbered 1, 2, 3… per direction, acknowledged, and replayed after a reconnect. |
| 2 | Control | Handshake JSON. Not counted and not acknowledged. |
| 3 | Ack | Header only. `ack` = the highest regular id received. |
| 5 | Disconnect | Graceful, permanent close. |
| 6 | ReplayRequest | The receiver saw a gap, so the sender resends its unacked queue. Rate-limited to once per 10 s (L1019). |
| 7 / 8 | Pause / Resume | Gate the peer's writer. The server sends Pause just before handing an ext-host socket over. The extension host sends Resume once it has adopted the socket (`extensionHostProcess.ts` L240/244/281). |
| 9 | KeepAlive | Sent every 5 s and also carries `ack`. |

The state machine is `PersistentProtocol` (L816-1232). `send` is at L1077, `_receiveMessage` at
L995, and `endAcceptReconnection` at L974. On a reconnect, `endAcceptReconnection` sends one
`Ack` and then the whole unacked queue.

Timers, from `ProtocolConstants` (L289-313):

- Ack within 2 s of receiving a message.
- KeepAlive every 5 s.
- The socket is declared dead after 20 s with no incoming data. Keep-alive-based detection was
  added upstream in April 2026.
- Reconnection grace is 3 h. The server shortens it to 5 min once any other client connects.

### 3.4 Handshake (`remoteAgentConnection.ts` L228-310; server `agentServer.ts` L274-397)

The handshake is three Control messages, all JSON:

1. Client → `{"type":"auth","auth":<connectionToken | "00000000000000000000">,"data":<challenge>}`.
   If the server has a mandatory token and it does not match, the server rejects
   (`agentServer.ts` L308-310).
2. Server → `{"type":"sign","data":<challenge2>,"signedData":<sig>}`.
3. Client → `{"type":"connectionType","commit"?:…,"signedData":…,"desiredConnectionType":1|2,"args"?:…}`.
   For an extension host, `args` is `IRemoteExtensionHostStartParams`, e.g. `{ "language": "en" }`
   (L348-354).
4. Server → the first reply:
   - Management: `{"type":"ok"}` (L429/437).
   - Extension host: `Pause`, then `{}` or `{"debugPort":n}` (L476-477, L486-487).
   - On refusal: `{"type":"error","reason":…}`.

**Signing.** `vsda` is a closed Microsoft module, and Code-OSS does not ship it. Without it:

- the client's `sign` is the identity function and `validate` always succeeds
  (`platform/sign/common/abstractSignService.ts`);
- the server accepts any `signedData` (`agentServer.ts` L362-364).

A Microsoft build that does have `vsda` also accepts `signedData == connectionToken` ("web client",
L363). Ember therefore sends the connection token when it has one, and echoes `challenge2`
otherwise. That works against both OSE and VSC (`INTENT.md` D10). Ember cannot verify the server's
signature. Server authenticity comes from the transport (TLS, or the iroh peer identity).

**Commit check.** If both sides send a commit and they differ, the server refuses with
"Client refused: version mismatch" (L351-357). The extension host runs its own check on
`initData.commit` and exits with code 55 on a mismatch (`extensionHostProcess.ts` L340-347).

Ember gates before that (§5 mitigation 3):

1. `handshake::verify_server(stream, &opts)` sends `GET /version` (HTTP/1.0 on a fresh stream;
   the route needs no connection token, `agentServer.ts` L145-148) and compares the body with
   `PINNED_COMMIT`. Any other commit, including the empty body of a dev server, returns
   `Error::UnsupportedServerVersion { server: Some(commit) }`.
2. The caller puts the verified commit in `ConnectOptions::commit`, so the server repeats the check.
   `handshake::connect` refuses a `ConnectOptions::commit` other than `PINNED_COMMIT` before any I/O,
   and maps the server's "version mismatch" refusal to `UnsupportedServerVersion { server: None }`.
3. The same commit goes into `InitDataParams::commit` for the extension host.

`UnsupportedServerVersion` is permanent (`handshake::is_permanent`): the editor core falls back to
"Open in VS Code" instead of retrying.

### 3.5 Reconnection

The reconnection token identifies a logical connection. To reconnect, open a new socket with
`reconnection=true`, run the same handshake, and the server swaps the socket under the existing
protocol (`agentServer.ts` L415-431, L462-479).

Back-off is 0, 5, 5, 10, 10, 10, 10, 10, 30 s, then 30 s repeating
(`remoteAgentConnection.ts` L649), within the grace time.

A server `error` reply is permanent: "Unknown reconnection token (never seen | seen before)",
"Duplicate reconnection token", an auth mismatch, or a version mismatch. Network errors and
timeouts are retried (L698-728).

A lost management connection is fatal to the window upstream. A lost extension-host connection
is not (`reconnectionFailureIsFatal`, L754-787).

### 3.6 IPC channel protocol (`ipc.ts`) — management connection

- **Values** (`serialize`/`deserialize`, L268-327) are a one-byte tag followed by the value:
  `0` undefined, `1` string, `2` Buffer, `3` VSBuffer, `4` array, `5` JSON object, `6` int32.
  Lengths and ints are VQL: 7 bits per byte, least-significant group first (L172-209). A negative
  int is treated as a u32 and takes 5 bytes.
- **Requests** (L40-45, L714-750):
  - `[100, id, channel, command]` followed by `arg` (Promise);
  - `[101, id]` (cancel);
  - `[102, id, channel, event]` followed by `arg` (listen);
  - `[103, id]` (dispose).
- **Responses** (L66-72):
  - `[200]` (Initialize);
  - `[201, id]` followed by data (success);
  - `[202, id]` followed by `{message, name, stack}` (error);
  - `[203, id]` followed by any value (error object);
  - `[204, id]` followed by data (event).
- **Start of a connection** (`IPCClient` constructor, L1015-1031): the client first sends its
  context alone, which for management is `{remoteAuthority, clientId}`
  (`remoteAgentConnection.ts` L760-763). Both sides run a `ChannelServer`, and each sends `[200]`.
  A `ChannelClient` sends nothing until it has seen the peer's `[200]`. The protocol is symmetric:
  Ember hosts no channels and answers any server-initiated request with an error.
- **URIs** sent to the server are `vscode-remote://<authority>/<path>`. The server's URI
  transformer maps them to `file:` on the way in and back on the way out
  (`base/common/uriTransformer.ts` L18-47). A URI serializes as
  `{"$mid":1,"scheme","authority","path","query","fragment"}`, with `MarshalledId.Uri = 1`.

Channels Ember uses (registered in `serverServices.ts` L383-406):

| Channel | Commands used | Source |
| ------- | ------------- | ------ |
| `remoteFilesystem` | `stat [uri]`, `readFile [uri, opts?]` → VSBuffer, `writeFile [uri, VSBuffer, {create,overwrite,unlock,atomic}]`, `readdir [uri]` → `[[name, FileType]]`, `mkdir`, `delete`, `rename`, `watch [sessionId, req, uri, {recursive, excludes, includes?}]`, `unwatch [sessionId, req]`; event `fileChange [sessionId]` → `IFileChange[]` or an error string | `platform/files/common/diskFileSystemProviderClient.ts` L80-259; server `platform/files/node/diskFileSystemProviderServer.ts` L40-65 |
| `remoteextensionsenvironment` | `getEnvironmentData {remoteAuthority, profile?}` → `IRemoteAgentEnvironmentDTO` (pid, appRoot, storage homes, log paths, OS, `reconnectionGraceTime`) | `workbench/services/remote/common/remoteAgentEnvironmentChannel.ts` L18-80 |
| `remoteExtensionsScanner` | `scanExtensions [language, profileLocation?, workspaceExtLocations, devLocations?, languagePack?]` → `IExtensionDescription[]`; `whenExtensionsReady` | `workbench/services/remote/common/remoteExtensionsScanner.ts` L36-61 |

Filesystem errors arrive as `202` with `name = "<code> (FileSystemError)"`, for example
`EntryNotFound` (`platform/files/common/files.ts` L813-858).

### 3.7 Extension-host bootstrap

1. Ember opens the ExtensionHost connection (§3.4). The server spawns the extension host and
   passes it the socket together with any bytes it has already read
   (`agentServer.ts` L486-503; `extensionHostProcess.ts` L207-246).
2. The extension host sends a 1-byte regular message, `2` (`Ready`).
3. Ember sends `IExtensionHostInitData` as one regular JSON message
   (`extensionHostProtocol.ts` L28-63; built by `remoteExtensionHost.ts` L206-262).
4. The extension host replies `1` (`Initialized`). Before that, it checks the commit (§3.4) and
   watches `parentPid`. `3` means `Terminate`
   (`extensionHostProtocol.ts` L123-143; `remoteExtensionHost.ts` L146-180).

The init data (L28-63) contains:

- `version`, `commit`;
- `parentPid` (the server's pid, from `getEnvironmentData`);
- `environment` (appRoot, storage homes, appLanguage, …);
- `workspace` (`{id, name, configuration}`, or `null`);
- `extensions` = `{versionId, allExtensions, myExtensions, activationEvents}`. `allExtensions` is
  the `scanExtensions` output passed through. `myExtensions` holds `ExtensionIdentifier`s as
  `{value, _lower}`. `activationEvents` maps each extension id to its activation events, explicit
  **and implicit**;
- `logLevel`, `logsLocation`, `autoStart: true`;
- `remote: {isRemote: true, authority, connectionData}`;
- `uiKind`, `telemetryInfo`.

**Two calls the extension host waits for.** Extensions do not activate until the renderer has
called both of these:

- `ExtHostWorkspace.$initializeWorkspace(workspaceData | null, trusted)`. The ext host waits on it
  at `extHostExtensionService.ts` L218.
- `ExtHostConfiguration.$initializeConfiguration(IConfigurationInitData)`. A barrier
  (`extHostConfiguration.ts` L107-131) is awaited in `api/node/extHostExtensionService.ts` L179.

Upstream, those calls come from the `MainThreadWorkspace` and `MainThreadConfiguration`
constructors (`api/browser/mainThreadWorkspace.ts` L66, `mainThreadConfiguration.ts` L29).

The defaults in `IConfigurationInitData` must be built by the renderer from:

- every extension's `contributes.configuration`;
- the core settings the workbench registers.

The extension host has no other source for default values.

**Activation the extension host does on its own:** `*`, `workspaceContains:`, and
`onStartupFinished` (`extHostExtensionService.ts` L671-687). `workspaceContains:` calls back into
`MainThreadWorkspace.$checkExists`.

**Activation the renderer must send:** `$activateByEvent("onLanguage:<id>")` when a document
opens (`services/language/common/languageService.ts` L289), `onCommand:<id>` before running a
contributed command, `onView:<id>`, and so on.

The implicit activation events in `activationEvents` are computed in the renderer by about 20
contribution-point generators (`platform/extensionManagement/common/implicitActivationEvents.ts`
L38-75; the generators live in `workbench/**`). Since VS Code 1.74 an extension may leave out
`onCommand:` events for its own contributed commands, so these must be reproduced.

### 3.8 Documents in the extension host

The extension host mirrors each open document. A document must first be announced:

- `ExtHostDocumentsAndEditors.$acceptDocumentsAndEditorsDelta({addedDocuments: [IModelAddedData]})`;
- `IModelAddedData` = `{uri, versionId, lines[], EOL, languageId, isDirty, encoding}`
  (`extHost.protocol.ts` L2390-2398, L2453-2463).

Changes then go through
`ExtHostDocuments.$acceptModelChanged(uri, ISerializedModelContentChangedEvent, isDirty)`
(L2404, in `ExtHostDocumentsShape` L2399-2405). The event is
`{changes: [{range, rangeOffset, rangeLength, text}], eol, versionId, isUndoing, isRedoing, isFlush, isEolChange, detailedReason?}`
(`editor/common/textModelEvents.ts` L87-127; `editor/common/model/mirrorTextModel.ts` L12-29).

The changes of one operation are ordered **end-to-start**, and the extension host applies them in
sequence (`mirrorTextModel.ts` `onEvents` L91-105). So every range and offset is in pre-edit
coordinates. Coordinates are 1-based lines and 1-based **UTF-16** columns; offsets count UTF-16
code units.

`rangeOffset`, `rangeLength` and `text` reach extensions verbatim as
`TextDocumentContentChangeEvent` (`api/common/extHostDocuments.ts` L180-224). The same handler
throws `unknown document` for any document that was never announced.

### 3.9 RPC protocol (`rpcProtocol.ts`)

Every message starts with `type u8` and `req u32be`. The body depends on the type:

| type | name | body |
| ---- | ---- | ---- |
| 1 | RequestJSONArgs | `rpcId u8`, `method` (shortString: u8 length + utf8), `args` (longString: u32 length + utf8, a JSON array) |
| 2 | RequestJSONArgsWithCancellation | same as 1 |
| 3 | RequestMixedArgs | `rpcId u8`, `method`, `u8 count`, then per arg: a type byte and its payload |
| 4 | RequestMixedArgsWithCancellation | same as 3 |
| 5 | Acknowledged | — |
| 6 | Cancel | — |
| 7 | ReplyOKEmpty (`undefined`) | — |
| 8 | ReplyOKVSBuffer | `u32 len` + bytes |
| 9 | ReplyOKJSON | longString |
| 10 | ReplyOKJSONWithBuffers | `u32 count`, longString JSON with `{"$$ref$$": i}` placeholders, then `count` buffers |
| 11 | ReplyErrError | longString: JSON of `{$isError, name, message, stack}` |
| 12 | ReplyErrEmpty | — |

Mixed-mode argument types: `1` = JSON string, `2` = VSBuffer, `3` = object with buffers,
`4` = `undefined`.

Sources: `MessageType` L940-953, `ArgType` L955-960, `MessageIO` L714-938, mixed arrays
L617-700.

Behaviour:

- Mixed mode is used only when an argument is a VSBuffer, an object with buffers, or `undefined`
  (L716-729).
- A trailing `CancellationToken` is never sent. It selects the *WithCancellation* message type,
  and the receiver appends a fresh token (L364-367).
- Every request is acknowledged immediately on receipt (L378). A peer that has not acked within
  3 s is marked unresponsive (L119).
- `rpcId` is one byte: `ProxyIdentifier.nid`, assigned by a global counter in declaration order
  of the `createProxyIdentifier` calls (`proxyIdentifier.ts` L42). At the pinned commit those
  calls are only in `extHost.protocol.ts`:
  - `MainContext` (L4046-4134) gets ids 1–87;
  - `ExtHostContext` (L4136-4218) gets ids 88–168.
  - Examples: `MainThreadDiagnostics` = 16, `MainThreadLanguageFeatures` = 26,
    `ExtHostDocuments` = 95, `ExtHostLanguageFeatures` = 104.
- The remote extension host runs a URI transformer over every RPC argument and reply
  (`extensionHostProcess.ts` L459-468), so Ember uses `vscode-remote://` URIs here as well.

## 4. Minimal subset

Read "→EH" as Ember calling the extension host, and "EH→" as the extension host calling Ember.

**Bootstrap (required before anything works)**

| Direction | Call | Notes |
| --------- | ---- | ----- |
| mgmt | `getEnvironmentData`, `scanExtensions` | Input to the init data |
| →EH | init data (raw JSON after `Ready`) | §3.7 |
| →EH | `ExtHostConfiguration.$initializeConfiguration` | Defaults from extension manifests + Ember core defaults |
| →EH | `ExtHostWorkspace.$initializeWorkspace` | Workspace folders as `vscode-remote` URIs |
| →EH | `ExtHostExtensionService.$activateByEvent` | `onLanguage:`, `onCommand:`, … |
| EH→ | `MainThreadStorage.$initializeExtensionStorage` → `undefined` | Awaited in every activation; must answer |
| EH→ | everything else | Reply per `exthost::default_reply` (mostly `undefined`) so the extension host never stalls |

**Documents (FR-E1 bridge)**

| Direction | Call |
| --------- | ---- |
| →EH | `ExtHostDocumentsAndEditors.$acceptDocumentsAndEditorsDelta` (documents; editors and active editor for `window.activeTextEditor`) |
| →EH | `ExtHostDocuments.$acceptModelChanged` / `$acceptModelSaved` / `$acceptDirtyStateChanged` / `$acceptModelLanguageChanged` |

**FR-E2 diagnostics**

| Direction | Call |
| --------- | ---- |
| EH→ | `MainThreadDiagnostics.$changeMany(owner, [uri, IMarkerData[] \| undefined][])` and `$clear(owner)` (`extHost.protocol.ts` L254-257) |

**FR-E3 CodeLens and hover**

| Direction | Call |
| --------- | ---- |
| EH→ | `MainThreadLanguageFeatures.$registerCodeLensSupport(handle, selector, eventHandle?)`, `$emitCodeLensEvent`, `$registerHoverProvider(handle, selector)`, `$unregister(handle)` (L529-537) |
| →EH | `$provideCodeLenses` / `$resolveCodeLens` / `$releaseCodeLenses`, and `$provideHover` / `$releaseHover` (L2956-2964) |

**FR-E4 inline completions**

| Direction | Call |
| --------- | ---- |
| EH→ | `$registerInlineCompletionsSupport` (17 positional args, L558-576), `$emitInlineCompletionsChange` (L577) |
| →EH | `$provideInlineCompletions(handle, uri, position, InlineCompletionContext)`, `$handleInlineCompletionDidShow`, `$handleInlineCompletionEndOfLifetime`, `$freeInlineCompletionsList` (L2995-3000) |

**Commands**

| Direction | Call |
| --------- | ---- |
| EH→ | `MainThreadCommands.$registerCommand` / `$unregisterCommand` / `$executeCommand` (L135-139) |
| →EH | `ExtHostCommands.$executeContributedCommand` (L2373), for CodeLens and completion commands |

**Files**

| Direction | Call |
| --------- | ---- |
| mgmt | `remoteFilesystem` stat / read / write / readdir / watch (§3.6) |

**Deferred, and why**

| Item | Why it waits |
| ---- | ------------ |
| `ExtHostEditors.$acceptEditorPropertiesChanged` (selections, visible ranges) | Many extensions read `activeTextEditor.selection`. Needed soon after FR-E4. |
| `$acceptConfigurationChanged` | Needed once settings can change at runtime. |
| `MainThreadWorkspace.$checkExists` | `workspaceContains:` activation. Answers `false` for now. |
| Implicit activation events beyond commands, languages and views | Covered by the generator work in §6 step 5. |
| Webviews (FR-E5) | Separate design. |

## 5. Version-stability risks

Measured by diffing release tags against `0036dcb6` (1.141.0) on 2026-10-03. The pin then moved
back to 1.139.1 (§2); 1.139.1 → 1.141.0 changed neither the proxy table nor the subset.

| Surface | Evidence | Risk |
| ------- | -------- | ---- |
| `PersistentProtocol` framing and handshake (`ipc.net.ts`, `remoteAgentConnection.ts`) | Wire format unchanged since at least 2023. Since then the file only gained behavioural changes: graceful disconnect (2024-07), keep-alive timeout (2026-04). | **Low** |
| IPC serialization (`ipc.ts`) | Format unchanged for years | **Low** |
| RPC message format (`rpcProtocol.ts`) | Only 6 commits since 2023. The 1.100.0 → 1.141 diff is limited to cancellation bookkeeping, with no wire change. | **Low** |
| **Proxy numbering** (`rpcId`) | 1.100.0 had 142 ids, 1.120.0 had 162, 1.130.0–1.141.0 have 168. `ExtHostDocuments` moved 81 → 92 → 95. One insertion in `MainContext` shifts every later id. | **High.** A mismatch silently delivers calls to the wrong actor. |
| Method signatures of the subset | `$provideHover`, `$provideCodeLenses`, `$provideInlineCompletions`, `$changeMany`, `$acceptDocumentsAndEditorsDelta` and both `$initialize*` calls are identical in 1.100, 1.120 and 1.141. `$acceptModelChanged` changed its event type: `IModelChangedEvent` became `ISerializedModelContentChangedEvent` (adds `detailedReason`) between 1.100 and 1.120. | **Medium** |
| Signatures at the AI edge | `$registerInlineCompletionsSupport` grew from 8 positional args (1.100) to 17 (1.120+). `extHost.protocol.ts` had 236 commits in the last 12 months, mostly chat and AI surfaces. | **High for FR-E4.** Copilot-class APIs move fastest. |
| Init data, implicit activation events, configuration defaults | Renderer-side logic, not a protocol. It drifts as features are added. | **Medium** |
| Proposed APIs (`enabledApiProposals`) | The extension host enforces them by product.json. They are not a wire issue. | Low here |

**Mitigations (the plan depends on these):**

1. **Pin per OSE build.** We build OSE ourselves (`INTENT.md` D10), so each Ember release names
   one Code-OSS commit. `ember-editor-conn` carries the tag and commit its tables came from
   (`PINNED_VERSION`, `PINNED_COMMIT`), tied to `ose/VERSION` by a unit test and by CI (§2).
2. **Generate, don't hand-write.** `editor-conn/scripts/gen_rpc_ids.sh` regenerates the proxy
   table from `extHost.protocol.ts`. It fails if `createProxyIdentifier` appears in another file,
   because then module load order would decide the numbering. The next step is to generate the
   subset's DTO types from the TypeScript too. That needs a small `ts-morph` script at OSE build
   time; it runs at our build, not on the client.
3. **Gate at connect time.** Read `GET /version`, and refuse the native editor core, falling back
   to "Open in VS Code", unless the commit is one Ember has tables for. Implemented as
   `handshake::verify_server` → `Error::UnsupportedServerVersion` (§3.4); today the only such
   commit is `PINNED_COMMIT`.
   - For OSE the gate always passes, because Ember ships with its OSE build.
   - For VSC (the user's Microsoft build) the commit is arbitrary. A table can be generated for each
     public release tag: the tags are public, and the table is only the identifier order. Unknown
     commits fall back.
4. **CI.** Done for the pinned tag: the `editor-conn-pin` job in `.github/workflows/checks.yml`
   shallow-clones `microsoft/vscode` at the `ose/VERSION` tag, runs `gen_rpc_ids.sh`, and fails
   if `editor-conn/src/rpc_ids.rs` or `PINNED_VERSION` / `PINNED_COMMIT` differ. So bumping
   `ose/VERSION` without regenerating the table fails the PR. Still to do: a canary that fetches
   each *new* upstream release tag, regenerates the table, diffs ids and subset signatures, and
   opens an issue on any change. That turns `PROJECT.md` Q2 into a tracked number, not a surprise.
5. **Fail loud.** Any reply to an unknown method, a `ReplyErr` "Unknown actor", or a decode
   failure in the subset is logged with the pinned commit. Ember never guesses.

**Bottom line for Q2:** the transport layers are stable. The RPC numbering and the AI-facing
signatures are not, but they are mechanically derivable from source we already build. Pinning the
runtime turns "protocol stability" from a research risk into a release-engineering step.

## 6. Plan

1. **Done in this change (not compiled):**
   - `editor-conn/` crate: framing, the `PersistentProtocol` state machine and driver, upgrade and
     handshake, the reconnect loop, the IPC client, the `remoteFilesystem` client, the management
     calls, the RPC codec and peer, the pinned proxy table and its generator script, init data,
     the typed subset with default replies, and the document bridge.
   - Unit tests against hand-built frames, plus a fake-server handshake over `tokio::io::duplex`.
2. **Compile, test, and run against a live OSE server.** The harness is written (not compiled or
   run yet): `editor-conn/tests/live_ose.rs`, `#[ignore]` and gated on `EMBER_OSE_SERVER` (the
   `bin/dpx-ose-server` of an unpacked OSE build; `editor-conn/scripts/fetch-ose-artifact.sh`
   downloads the newest `ose` workflow artifact for the current platform and prints that path).
   It starts the server on a free port with `--without-connection-token` and a temp
   `--server-data-dir` (with `extensions.verifySignature: false`, as `ose/smoke.sh`), then over
   plain TCP: `verify_server`; management handshake, IPC, `getEnvironmentData`, the
   `remoteFilesystem` commands and a watch event; `scanExtensions` (the built-in
   `vscode.json-language-features` must be listed); the extension-host bootstrap, both
   `$initialize*` calls, `DocumentBridge::open_in_editor` on a JSON file, an edit that breaks it,
   and `MainThreadDiagnostics.$changeMany` from the JSON language server; then a clean shutdown.
   Each step prints its time. Run:
   `cargo test --manifest-path editor-conn/Cargo.toml --test live_ose -- --ignored --nocapture`
   (`EMBER_OSE_VERBOSE=1` lists every extension-host call). Still to do: capture real frames
   into `testdata/editor-conn/` and replay them in unit tests.
3. **Session object.** Add an `EditorSession` that owns both connections, the reconnect policy,
   the `DocumentBridge`, the provider registries (handle → selector), and request routing:
   `provide_hover(uri, pos)` picks the providers whose selectors match and fans out.
   Selector matching needs `languages.score` semantics (language, scheme, glob pattern).
   **Written (not compiled), with step 4's three bridges:** `editor/` (package `ember-editor`),
   designed in `EDITOR-SESSION.md`. Reconnect is still to do.
4. **FR-E2 → FR-E3 → FR-E4** in that order, as `IMPLEMENTATION.md` §3 sets out. Each is a
   registry plus a request/response pair plus widget rendering.
5. **Activation and configuration fidelity.** Port the implicit-activation generators and core
   configuration defaults, generated from the workbench's `registerConfiguration` calls at OSE
   build time.
6. **Transport.** Swap TCP for `ember-transport` (iroh) streams. Nothing above `AsyncRead +
   AsyncWrite` changes.

### 6.1 Crate API (summary)

| Module | Surface |
| ------ | ------- |
| crate root | `PINNED_VERSION`, `PINNED_COMMIT`, `check_server_commit`; `Error::UnsupportedServerVersion` |
| `handshake` | `verify_server(stream, &ConnectOptions)` → server commit or `UnsupportedServerVersion`; `server_commit(stream, …)`; `connect(stream, &ConnectOptions, ConnectionType, args)` → `(Connection, first_reply)`; `reconnect(&handle, stream, …)`; `reconnect_loop(&handle, dial, …, grace)`; `ExtensionHostStartParams` |
| `connection` | `Connection { handle, events }`; `ConnectionHandle::{send, send_control, recv_control, close, replace_transport, finish_reconnect}`; `ConnEvent::{Message, Disconnected, Lost}` |
| `ipc` | `IpcClient::start(conn, ctx)` → `(client, lifecycle_rx)`; `call(channel, cmd, IpcValue)`; `listen` / `unlisten`; `IpcValue`; `serialize` / `deserialize` |
| `remote_fs` | `RemoteFs::{stat, read_file, write_file, readdir, mkdir, delete, rename, subscribe_changes, watch, unwatch}` |
| `management` | `get_environment_data`, `scan_extensions`, `RemoteAgentConnectionContext` |
| `exthost` | `InitDataParams::to_json`; `initialize(conn, &init)` → `(RpcPeer, RpcEvent rx)`; `bootstrap_calls`; `configuration_init_data`; typed `Call` builders for the subset; `MainThreadCall::parse`; `default_reply` |
| `rpc` | `RpcMessage::{encode, decode}`; `RpcPeer::{start_call, call, fire, cancel, respond, connection}`; `Arg`; `Reply` |
| `rpc_ids` | `id_of(name)`, `name_of(id)`, `PROXY_IDS` |
| `document` | `DocumentMirror::{apply, replace_all, set_eol, position_from_byte, offset_at}`; `DocumentBridge::{open, open_in_editor, change, close, saved, set_language}` (`open_in_editor` also adds one visible, active editor) |

### 6.2 What the dioxus-compose code-editor widget must provide

The bridge is only correct if the widget reports and accepts exactly these things.

**Change events, one per atomic operation**

- A list of `(range, text)` in **pre-operation** coordinates. Edits must not overlap. Multi-cursor
  typing is one operation.
- The reason: normal, undo or redo.
- A monotonically increasing version.
- IME composition must not emit intermediate states as separate versions unless the text model
  really changed. Monaco does emit during composition, so match whatever Monaco does at the
  pinned commit (E6).

**Coordinates**

- 1-based lines.
- 1-based UTF-16 columns, or byte offsets that the bridge converts
  (`DocumentMirror::position_from_byte`).
- The widget's text model must use the same line splitting as Monaco (`\r\n`, `\r`, `\n`) and
  one EOL per document. Inserted text is normalized to the document's EOL.

**Editor state, for `ITextEditorAddData` and later `$acceptEditorPropertiesChanged`**

- Selections (anchor and active) and visible ranges.
- Tab size, insert spaces, and which editor is active.

**Overlays**

- *Squiggles.* Draw ranges with severity (1/2/4/8) and tags (unnecessary / deprecated), clamped
  to the current text. The markers carry `modelVersionId`; drop stale ones.
- *CodeLens.* A zone *above* a line: whole-line height insertion that shifts layout, with
  clickable command titles, `$(icon)` theme-icon syntax, and lazy resolve when scrolled into view.
- *Hover.* A popup at a position. Its content is Markdown (`IMarkdownString`) with code blocks,
  `$(icon)`, command links (`command:` URIs, only when `isTrusted`), and a range to highlight.
  The widget needs a Markdown renderer that matches the workbench's CSS.
- *Inline completion (ghost text).* Render `insertText` from the cursor in a dimmed style. It may
  be multi-line and may replace a `range` that starts before the cursor. The widget must support:
  - Tab to accept;
  - accepting a word or a line (partial accept);
  - Esc to reject;
  - typing through a suggestion while it still matches;
  - hooks for `didShow`, end of lifetime (accepted / rejected / ignored), and free.

**Triggers**

- Inline completion on typing (Automatic) and on explicit command (Explicit), with a debounce.
  The provider supplies `debounceDelayMs` at registration.
- Hover on mouse rest and on the keyboard command.
- Diagnostics and CodeLens refresh when the extension signals it (`$emitCodeLensEvent`) or on
  document change.

**Cancellation**

- Any in-flight provider request must be cancellable when the cursor or text moves on. The bridge
  maps that to RPC `Cancel`.

## 7. Open questions

- **E6 vs. IME.** Exactly which change events Monaco emits during CJK composition at the pinned
  commit. This decides how the widget batches composition. Needs a capture from a real workbench.
- **Selector scoring.** Port `languages.score` (`editor/common/languageSelector.ts`) as-is, or
  generate it.
- **Configuration defaults for core settings** (`editor.*`, `files.*`). Generate them from the
  workbench at OSE build time, or hand-maintain a list.
- **VSC runtime.** Is a per-release proxy table for Microsoft builds acceptable to ship, given
  that it is derived from the MIT source of the same tag? Or is the native editor core OSE-only?
