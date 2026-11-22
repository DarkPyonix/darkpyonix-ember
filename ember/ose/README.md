# OSE: DarkPyonix's own build of Code-OSS

OSE is the default VS Code runtime of Ember's IDE window (`docs/INTENT.md` D10, `docs/SPEC.md`
FR-W4). It is the **VS Code web server ("REH web")** compiled by DarkPyonix from the MIT-licensed
[microsoft/vscode](https://github.com/microsoft/vscode) source at a pinned tag, with a
`product.json` that renames it and points its extension gallery at
[Open VSX](https://open-vsx.org). The other runtime, VSC, is the user's own installation of
Microsoft's VS Code (`code serve-web`, Microsoft Marketplace).

OSE is wrapped, never patched (E4): the only change to the upstream tree is `product.json`.
Behaviour changes for the IDE window come from `ember/proxy/`.

| File | What it is |
| ---- | ---------- |
| `VERSION` | The Code-OSS release tag that is built (currently `1.139.1`, commit `04c0d99f4fb0`). |
| `product.overrides.json` | Deep-merged into upstream `product.json`: DarkPyonix names, Open VSX gallery, telemetry off. |
| `build.sh` | Fetch tag → apply overrides → `npm ci` → gulp `core-ci` + Copilot extension → gulp `vscode-reh-web-<target>-min-ci` → `tar.gz` + `.sha256`. CI only; see [Build notes](#build-notes). |
| `check-product.sh` | Fails if a `product.json` is not on Open VSX, has telemetry on, or names a Microsoft marketplace/update/telemetry endpoint. |
| `regress.sh` | Extension regression (NFR-W1, #84): installs `regression/extensions.json` from Open VSX at the pinned versions, parses extension host logs, compares with `regression/expected.json`. Activation needs a client (`OSE_REGRESS_CLIENT`); `--self-test` runs offline. See [Extension regression](#extension-regression). |
| `smoke.sh` | Starts a packaged build and checks: workbench served, Open VSX search, extension install. |

Builds run in GitHub Actions (`.github/workflows/test-ose.yml`), never on a developer machine:
`build.sh` refuses to run unless `CI=true` (or `OSE_ALLOW_LOCAL_BUILD=1`). `build.sh
--product-only` is the exception: it only downloads `product.json`, applies the overrides and
checks the result, so it is safe anywhere.

## Targets and releases

| Target | Runner | Notes |
| ------ | ------ | ----- |
| `linux-x64` | `ubuntu-22.04` | glibc 2.35 build, runs on glibc ≥ 2.35 hosts |
| `linux-arm64` | `ubuntu-22.04-arm` | Raspberry Pi 4/5 on a **64-bit** OS (Raspberry Pi OS Bookworm, glibc 2.36) |
| `darwin-arm64` | `macos-15` | Apple silicon |
| `darwin-x64` | `macos-15-intel` | Intel Mac |
| `windows-x64` | `windows-2022` | Windows 10 and later, x64; launcher `bin/dpx-ose-server.cmd` (#83; built and smoke-tested in CI run 43692860054) |

Each build produces `dpx-ose-<tag>-<target>.tar.gz` and its `.sha256`. The archive is
self-contained: it bundles the Node runtime pinned in upstream `remote/.npmrc`, so the host does
not need Node. Pushing a tag `ose-v<tag>` (for example `ose-v1.139.1`, or `ose-v1.139.1-dpx.2` for a
rebuild) runs the matrix in `test-ose.yml`. When that run succeeds, `publish-ose.yml` downloads
that run's archives, checks them against their checksums and publishes a GitHub release with all
archives and `SHA256SUMS`, so the release holds exactly the files the build produced. A release
does not expire, unlike the 14-day workflow artifacts.

An Ember install gets its OSE from that release (FR-W4 acceptance step 6, #83). The `ember-server`
and `ember-node` archives of an Ember release carry `install-ose.sh` and an `OSE_VERSION` file with
the matching tag (`ose-v<the tag in VERSION>`); running `./install-ose.sh` beside them downloads
the archive for the machine with curl, checks it against the release's `SHA256SUMS`, unpacks it into
`~/.local/share` and prints the launcher path. The OSE manager finds it there without
`EMBER_OSE_SERVER`. When no OSE is found, the IDE target's reason and the extension routes say so and
name this step. In a checkout, `ember/server/scripts/install-ose.sh` reads `ember/ose/VERSION`.

Old "OSE build" runs with no jobs (2026-11-06 to 11-14, all `pull_request` on `release`): the
release-sync pull requests (#964 and earlier) were opened by `github-actions[bot]`, and the
workflow then still had a `pull_request` trigger. GitHub held those runs for approval
(`action_required`) and later recorded them as failed with no jobs. No build failed. The trigger is
gone since development CI became dispatch-only (`ci.yml` calls this workflow), so they do not recur.

After the build, the **Live checks** job runs once per target on the same runner type
(`docs/SPEC.md`, FR-W4 acceptance step 4). It downloads the archive the run built, verifies the
checksum, unpacks it, and runs the Ember editor session's live tests against it
(`ember/editor-conn/tests/live_ose.rs`, `ember/editor/tests/live_session.rs`,
`ember/editor/tests/fr_l31_14_live_extensions.rs`), then the extension regression. A failing live
test fails the run, so `publish-ose.yml` publishes nothing from it. This job is written and has
not run in CI yet.

### Windows

`windows-x64` is built natively on a `windows-2022` runner (`docs/INTENT.md` D49, provisional,
#83). The same `build.sh` runs under Git Bash and calls Code-OSS's
`vscode-reh-web-win32-x64-min-ci` task, the task upstream's own Windows pipeline uses
([product-build-win32-compile.yml](https://github.com/microsoft/vscode/blob/1.139.1/build/azure-pipelines/win32/steps/product-build-win32-compile.yml)).
Native modules need the Visual Studio 2022 C++ build tools and Python, which the runner image has
([VS Code: How to Contribute](https://github.com/microsoft/vscode/wiki/How-to-Contribute)). It is
not cross-compiled, because native modules are built for the build host.

The archive has the same shape as the others, `dpx-ose-<tag>-windows-x64.tar.gz` and `.sha256`,
so the fetch script, the jobs and the release handle one format; Windows 10 and later read it
with their own `tar.exe` (`tar -xzf dpx-ose-<tag>-windows-x64.tar.gz`).

The launcher is `bin/dpx-ose-server.cmd`, which runs `node.exe out\server-main.js` from the
build root ([code-server.cmd](https://github.com/microsoft/vscode/blob/1.139.1/resources/server/bin/code-server.cmd)).
`EMBER_OSE_SERVER` names the `.cmd`, but `ember-node` and `smoke.sh` start those two directly,
so stopping the child stops the server and no `cmd.exe` re-quotes the arguments. Without
`EMBER_OSE_SERVER`, `ember-node` looks for `%LOCALAPPDATA%\dpx-ose-*\bin\dpx-ose-server.cmd`.

Known limits on Windows (not built):

- The live tests (`live_ose.rs`, `live_session.rs`, `fr_l31_14_live_extensions.rs`) are
  `#![cfg(unix)]`: they stop the server with process groups and `pkill`. The **Smoke
  (windows-x64)** job runs `smoke.sh` on the downloaded archive instead (start, `/version`, Open
  VSX search and install).
- Stopping an OSE kills `node.exe` only; the extension host and other children it started are
  not killed with it (no job object).
- The token files keep the default ACL: any process of the same user and an administrator can
  read them.

To move to a new Code-OSS release: change `VERSION`, open a PR (the `product` job checks the new
upstream `product.json` against the overrides in seconds), then run the workflow manually or tag.

## Extension regression

`regress.sh` answers one question for a list of extensions: does each one install from Open VSX
into this OSE build and activate in its extension host without an error (NFR-W1, #84).

- `regression/extensions.json` is the list. Each entry is `{id, version, open}`. `version` is the
  exact Open VSX version that is installed, so two runs test the same extension code. `null`
  installs the newest version the OSE accepts; the run prints the installed folders, and the
  version is pinned from that. `open` is a file of `regression/workspace/` whose language
  triggers the extension, or `null` for an extension that activates at startup.
- `regression/expected.json` records the expected result of each: `pass`, or `known-fail` with a
  `reason`. A `pass` that does not activate fails the run. A `known-fail` that activates prints a
  warning, so the record is corrected.
- `regression/workspace/` is a small project (YAML, JavaScript, Python, Go, Rust, TOML, Java). It
  is copied for each run and never built.

The extension host starts only when a client connects, so the run needs one. The client is
Ember's own editor session, `ember/editor/tests/nfr_w1_extension_regression.rs`: it connects, opens
every `open` file and stays connected for `OSE_REGRESS_SETTLE` seconds (default 30).

```sh
export EMBER_OSE_SERVER="$(ember/editor-conn/scripts/fetch-ose-artifact.sh)"
export OSE_REGRESS_CLIENT='cargo test --locked --manifest-path ember/editor/Cargo.toml --test nfr_w1_extension_regression -- --ignored --nocapture'
OSE_REGRESS_OUT=.scratch/regress ember/ose/regress.sh "$(dirname "$(dirname "$EMBER_OSE_SERVER")")"
```

`OSE_REGRESS_OUT` keeps `results.json` (the status of every listed extension), `installed.txt` and
the extension host logs. In CI the Live checks job runs this per target and uploads the folder as
`ose-regress-<target>`.

Status today: the list is the five extensions the leader approved on 2026-11-15: two language
servers (`redhat.vscode-yaml`, `dbaeumer.vscode-eslint`), the most used language extension
(`ms-python.python`), a debugger (`ms-python.debugpy`) and a webview panel (`mhutchie.git-graph`).
The live checks ran once on all four targets (run 43670763653) with the earlier list; this list
has not run yet, so every `expected.json` entry is an unverified `pass` and the step does not
fail the job. `ms-python.debugpy` and `mhutchie.git-graph` have no pinned version: their first
resolving run prints the version to pin. The check is "installed and activated without a failure
line". Results are compared with our own previous OSE release, not with another vendor's build.

### Deep checks (#929)

A second run, `OSE_REGRESS_MODE=deep`, makes the deeper check of every entry with a `deep` field,
through the same client command (`nfr_w1_deep_checks` runs, the activation client skips itself).
The workspace copy is made a git repository with one commit first, so Git Graph shows its full
page. Each check writes `pass`, `fail` or `not-run` with a reason to `OSE_REGRESS_DEEP/<id>.json`;
`regress.py compare-deep` compares them with `deep.expect` in `expected.json` and writes
`deep.json`. Only a `fail` where `pass` is expected fails the run; `not-run` prints a warning.

```sh
OSE_REGRESS_MODE=deep OSE_REGRESS_OUT=.scratch/regress-deep ember/ose/regress.sh "$(dirname "$(dirname "$EMBER_OSE_SERVER")")"
```

- **Webview** (`mhutchie.git-graph`, written, not yet run): the client runs `git-graph.view`
  (retried while the extension registers it), waits for `$createWebviewPanel` and `$setHtml`,
  reports the page shown (`$onDidChangeWebviewPanelViewStates`) and ready, sends
  `{"command":"loadRepos","check":false}` with `$onMessage` and waits for a `$postMessage` whose
  `command` is `loadRepos` (Git Graph's `respondLoadRepos`, `src/gitGraphView.ts`).
- **Debug** (`ms-python.debugpy`, not built): reports `not-run`, or `not-run: no python3` when the
  machine has no `python3` (the check needs one for debugpy's adapter and the program). The
  editor client answers every `MainThreadDebugService` call with a default reply and never calls
  `ExtHostDebugService` (#445). What one session of `sample.py` with a breakpoint on line 2 needs,
  as the workbench does it at 1.139.1:
  1. Record what the extension host registers: `$registerDebugTypes(types)`,
     `$registerDebugConfigurationProvider(type, triggerKind, hasProvide, hasResolve, hasResolve2, handle)`,
     `$registerDebugAdapterDescriptorFactory(type, handle)` (and their `$unregister*`).
  2. Resolve the configuration `{type: "debugpy", request: "launch", program, console: "internalConsole", stopOnEntry: false}`
     through the provider's handle: `$resolveDebugConfiguration(handle, folder, config, token)`,
     `$substituteVariables(folder, config)`, then
     `$resolveDebugConfigurationWithSubstitutedVariables(handle, folder, config, token)`.
  3. Start the adapter: `$startDASession(adapterHandle, IDebugSessionFullDto {id, type, name, parent, folderUri, configuration})`
     (the extension host asks the descriptor factory itself), then `$acceptDebugSessionStarted(session)`.
  4. Speak DAP through `$sendDAMessage(adapterHandle, message)` and receive `$acceptDAMessage(adapterHandle, message)`
     (and `$acceptDAError`, `$acceptDAExit`): `initialize`, `launch`, wait for the `initialized`
     event, `setBreakpoints` (source `sample.py`, line 2), `configurationDone`, wait for the
     `stopped` event, `threads`, `stackTrace` (the top frame is `answer` at line 2), `disconnect`,
     then `$stopDASession(adapterHandle)` and `$acceptDebugSessionTerminated(session)`.
     A `runInTerminal` reverse request would need `$runInTerminal`; `internalConsole` avoids it.
     Unverified: whether debugpy's resolver finds an interpreter through `ms-python.python` with
     no workbench UI, or needs `"python": "<path of python3>"` in the configuration.

  Shapes: [`extHost.protocol.ts`](https://github.com/microsoft/vscode/blob/1.139.1/src/vs/workbench/api/common/extHost.protocol.ts)
  (`MainThreadDebugServiceShape` L2271, `ExtHostDebugServiceShape` L3320, the breakpoint and
  session DTOs L3241-3318), [`extHostDebugService.ts`](https://github.com/microsoft/vscode/blob/1.139.1/src/vs/workbench/api/common/extHostDebugService.ts)
  (`$startDASession` L597, `$sendDAMessage` L697) and
  [`mainThreadDebugService.ts`](https://github.com/microsoft/vscode/blob/1.139.1/src/vs/workbench/api/browser/mainThreadDebugService.ts)
  (the resolve calls L197-295, the adapter L508-517). Actor ids: `MainThreadDebugService` 14,
  `ExtHostDebugService` 92 (`ember/editor-conn/src/rpc_ids.rs`).

What the run does not show: behaviour after activation (a language server answering, a formatter
formatting), an activation that starts and never finishes, and a comparison with the same
extensions under Microsoft's build.

## Release check on a Raspberry Pi and a Mac

`docs/SPEC.md` FR-W4 acceptance step 5 asks for one manual check per release on a Raspberry Pi
(64-bit OS) and a Mac, because CI runners are not those machines and nobody looks at the window
there. Run it on the machine itself, in a checkout of this repository, with `gh` signed in.

```sh
tag=ose-v1.139.1    # the release to check

# 1. Download the published archive for this machine, check it against the release's SHA256SUMS,
#    unpack it into .scratch/ose. Prints the launcher path.
server="$(OSE_RELEASE=$tag ember/editor-conn/scripts/fetch-ose-artifact.sh)"

# 2. The automated part: product.json, start, Open VSX search, install.
ember/ose/smoke.sh "$(dirname "$(dirname "$server")")"

# 3. The part that needs a person. Open the printed URL in a browser.
export DPX_OSE_SERVER="$server"
cd ember/proxy && python -m dpx.serve --root ~/work
```

In the IDE window, check three things:

1. The window opens and shows the folder.
2. The Extensions view finds an extension by name (Open VSX) and installs it.
3. Help → About shows "DarkPyonix OSE" and the Code-OSS version of the tag.

Then add a row below, in the pull request or commit that follows the release.

| Release | Machine and OS | Date | Steps 1 to 3 of the script | Window checks 1 to 3 | By |
| ------- | -------------- | ---- | -------------------------- | -------------------- | -- |
| (none yet) | | | | | |

No release has been checked this way yet.

## Build notes

`build.sh` follows upstream's own pipeline (`build/azure-pipelines/{linux,darwin}/steps/product-build-*-compile.yml`
at the pinned tag) wherever it can. Where it differs, it is because that step depends on
Microsoft-internal resources. The upstream tree stays unpatched in every case.

- **Linux native modules: run `build/npm/preinstall.ts` before `npm ci`.** The root `.npmrc`
  builds native modules (native-keymap, @vscode/spdlog, kerberos, ...) against Electron's headers.
  Their V8 headers use a deprecation-attribute syntax that GCC < 13 cannot parse
  (`v8config.h: error: expected identifier before '__attribute__'`,
  [GCC bug 69585](https://gcc.gnu.org/bugzilla/show_bug.cgi?id=69585)). Upstream ships fixed
  copies in `build/npm/gyp/custom-headers`, and `preinstall.ts` lays them over the node-gyp header
  cache. npm runs the root `preinstall` only after the dependencies have been built, so upstream
  calls the script explicitly first, and `build.sh` does the same on Linux. Upstream builds with a
  Chromium clang and a glibc 2.28 sysroot (`build/azure-pipelines/linux/setup-env.sh`). OSE uses
  the runner's GCC 11 instead, which is why the archive needs glibc ≥ 2.35 (see Targets).
- **macOS:** `GYP_DEFINES=kerberos_use_rtld=false`, as upstream sets it.
- **Copilot.** At 1.139 the MIT Copilot Chat extension lives in-tree (`extensions/copilot`). Every
  REH packaging task ends with `prepareBuiltInCopilotRipgrepShim` (`build/lib/copilot.ts`),
  which needs `extensions/copilot/node_modules/@github/copilot/sdk` in the output. Upstream puts it
  there with a Copilot VSIX built by a separate job. That job needs the private
  `microsoft/vscode-capi` repository and is downloaded by `build/azure-pipelines/common/downloadCopilotVsix.ts`
  into `.build/extensions/copilot`. Upstream's local fallback task, `compile-copilot-extension-build`,
  builds the extension from source, but `build/.moduleignore` strips `@github/copilot/**` from it,
  so the one-shot `vscode-reh-web-<target>-min` task always fails at the shim outside Microsoft.
  `build.sh` therefore runs upstream's CI sequence: `gulp core-ci`, then
  `compile-copilot-extension-build` in place of the VSIX download, then copies
  `@github/copilot/{package.json,sdk/}` (what `extensions/copilot/script/postinstall.ts`
  materialized, and what the VSIX's `.vscodeignore` keeps) into `.build/extensions/copilot`, then
  `gulp vscode-reh-web-<target>-min-ci`. The shim then prunes the SDK to the target and fetches the
  version-matched `@github/copilot-<platform>` native package, which it checks against the
  integrity in `extensions/copilot/package-lock.json`.
  Copilot cannot be left out through `product.json`: the shim step runs unconditionally, and
  removing it means patching `build/gulpfile.reh.ts`, which is what VSCodium does
  (`patches/53-ext-copilot-remove-it.patch`). The extension is inert until a user signs in to
  GitHub Copilot (see "Microsoft hosts that remain").
- `core-ci` also builds the desktop bundle and type-checks `src/` with tsgo, as upstream's
  pipeline does. That adds a few minutes, but it is the only named gulp task that produces
  `out-vscode-reh-web-min` without also running the Copilot packaging step.
- `GITHUB_TOKEN` (the workflow's read-only token) is passed to the build to avoid GitHub API rate
  limits when built-in extensions and packages are fetched.

## Running it with `ember/proxy/dpx`

```sh
tar -xzf dpx-ose-1.139.1-linux-arm64.tar.gz -C ~/.local/share
export DPX_OSE_SERVER=~/.local/share/dpx-ose-1.139.1-linux-arm64/bin/dpx-ose-server
# optional; this is the launcher's default (ember/proxy/dpx/vscode/runtime.py DEFAULT_OSE_ARGS) plus telemetry off
export DPX_OSE_ARGS='--host {host} --port {port} --without-connection-token --accept-server-license-terms --server-data-dir {data_dir} --telemetry-level off'
cd ember/proxy && python -m dpx.serve --root ~/work
```

`dpx.serve` starts the OSE server on a free loopback port and fronts it with the proxy; the server
is never exposed directly, and the proxy's login replaces the connection token (hence
`--without-connection-token`). Default extensions (FR-W6) are installed with
`dpx-ose-server --install-extension <id> --extensions-dir <data_dir>/extensions`, from Open VSX.

Running the server alone, for a quick look:

```sh
"$DPX_OSE_SERVER" --host 127.0.0.1 --port 8080 --without-connection-token \
  --accept-server-license-terms --server-data-dir /tmp/ose-data --telemetry-level off
```

Server flags used here (from upstream `src/vs/server/node/serverEnvironmentService.ts`): `--host`,
`--port` (`0` = random), `--connection-token` / `--connection-token-file` /
`--without-connection-token`, `--accept-server-license-terms`, `--server-data-dir`,
`--server-base-path`, `--socket-path`, `--telemetry-level off|crash|error|all`,
`--default-folder`, `--install-extension`, `--extensions-dir`.

## Known limits when `ember-server` or `ember-node` runs it

The OSE manager (`ember/node/src/ose.rs`, SPEC FR-W4a) starts one `dpx-ose-server` per project
folder. Two limits are known and not fixed (#896):

- **OSE children are killed only on a graceful stop.** The manager kills them when it is dropped.
  After a crash or a SIGKILL of the node or the server they keep running, because the units use
  `KillMode=process` (systemd) and `AbandonProcessGroup` (launchd) so that terminal keepers
  survive the daemon. This was already true of the server's own OSE. The next start does not
  reuse them: it removes the stale token files and starts a new OSE per folder, so a leftover
  process has to be stopped by hand.
- **Windows token files get no ACL hardening.** The 0600 mode and the 0700 directories of the
  connection token file are Unix only. On Windows the file has the permissions of its directory.

## What `product.overrides.json` changes, and why

- **Names.** `nameShort`/`nameLong` "DarkPyonix OSE", `applicationName` `dpx-ose`,
  `dataFolderName` `.dpx-ose`, `serverApplicationName` `dpx-ose-server` (the launcher is
  `bin/dpx-ose-server`), `serverDataFolderName` `.dpx-ose-server`, plus the matching
  `tunnelApplicationName`, `urlProtocol`, bundle/desktop ids. Nothing is named "Visual Studio Code"
  and no Microsoft logo or icon is added (Code-OSS ships none).
- **Open VSX.** `extensionsGallery`:
  `serviceUrl` `https://open-vsx.org/vscode/gallery` (the workbench derives `/extensionquery` and
  `/vscode/{publisher}/{name}/latest` from it), `itemUrl` `https://open-vsx.org/vscode/item`,
  `publisherUrl` `https://open-vsx.org/namespace`, `extensionUrlTemplate`
  `https://open-vsx.org/vscode/gallery/{publisher}/{name}/latest` (the latest-version fallback),
  `resourceUrlTemplate` `https://open-vsx.org/vscode/unpkg/{publisher}/{name}/{version}/{path}`
  (web extensions' files, used by the browser extension host), and `controlUrl` (Open VSX's
  malicious/deprecated list, as VSCodium uses). Field names are from upstream
  `src/vs/base/common/product.ts` and
  `src/vs/platform/extensionManagement/common/extensionGalleryManifestService.ts`.
  Because the gallery is set before building, upstream's build fetches the built-in
  `ms-vscode.js-debug*` extensions from Open VSX too (`build/lib/builtInExtensions.ts`).
- **Telemetry off.** `enableTelemetry: false`. Upstream's `supportsTelemetry()` returns false for a
  built product without `enableTelemetry`, and the 1DS appender is only created with
  `aiConfig.ariaKey`, which Code-OSS does not have and `check-product.sh` forbids.
- **Removed:** `voiceWsUrl` (Microsoft's hosted voice service).
- `quality: stable`, as VSCodium and code-server set it.

### Microsoft hosts that remain (allowed, listed by `check-product.sh`)

- `webviewContentExternalBaseUrlTemplate` → `https://{{uuid}}.vscode-cdn.net/...`: the web
  workbench loads each webview's host page (`.../webview/browser/pre/index.html`, static files of
  the open-source tree) from a per-webview subdomain for origin isolation, and the server's CSP
  only allows `'self'` and `*.vscode-cdn.net` as frame sources. This is not the marketplace and
  carries no telemetry, but it does mean webviews (Markdown preview, notebooks, extension webviews)
  need internet access to Microsoft's CDN. Self-hosting it needs a separate origin per webview plus
  a CSP change in `ember/proxy/`; tracked as an open item.
- `defaultChatAgent.*` links (aka.ms docs, `api.github.com/copilot_internal`): the MIT-licensed
  built-in Copilot Chat extension (`extensions/copilot`) and its setup UI. It contacts GitHub only
  when a user signs in to Copilot.

## Licence

- The source is [microsoft/vscode](https://github.com/microsoft/vscode), MIT licence
  (`LICENSE.txt` in the archive). OSE redistributes it under that licence. Built-in extensions are
  MIT as well (they are in the same repository, or from MIT repositories such as
  `microsoft/vscode-js-debug` via Open VSX).
- OSE is **not** Visual Studio Code. Microsoft's product licence, branding, Marketplace and
  telemetry apply to Microsoft's builds only; none of them are in OSE. The Microsoft Marketplace's
  terms limit its use to Microsoft's products, which is why OSE uses Open VSX.
- Extensions installed from Open VSX carry their own licences.
- VSCodium (MIT, [VSCodium/vscodium](https://github.com/VSCodium/vscodium)) was used as a reference
  for which `product.json` fields matter and for its build order; no VSCodium code, patches or
  branding are included.
