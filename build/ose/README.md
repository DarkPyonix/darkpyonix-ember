# OSE: DarkPyonix's own build of Code-OSS

OSE is the default VS Code runtime of Ember's IDE window (`docs/INTENT.md` D10, `docs/SPEC.md`
FR-W4). It is the **VS Code web server ("REH web")** compiled by DarkPyonix from the MIT-licensed
[microsoft/vscode](https://github.com/microsoft/vscode) source at a pinned tag, with a
`product.json` that renames it and points its extension gallery at
[Open VSX](https://open-vsx.org). The other runtime, VSC, is the user's own installation of
Microsoft's VS Code (`code serve-web`, Microsoft Marketplace).

OSE is wrapped, never patched (E4): the only change to the upstream tree is `product.json`.
Behaviour changes for the IDE window come from `web/proxy/`.

| File | What it is |
| ---- | ---------- |
| `VERSION` | The Code-OSS release tag that is built (currently `1.139.1`, commit `04c0d99f4fb0`). |
| `product.overrides.json` | Deep-merged into upstream `product.json`: DarkPyonix names, Open VSX gallery, telemetry off. |
| `build.sh` | Fetch tag → apply overrides → `npm ci` → gulp `core-ci` + Copilot extension → gulp `vscode-reh-web-<target>-min-ci` → `tar.gz` + `.sha256`. CI only; see [Build notes](#build-notes). |
| `check-product.sh` | Fails if a `product.json` is not on Open VSX, has telemetry on, or names a Microsoft marketplace/update/telemetry endpoint. |
| `smoke.sh` | Starts a packaged build and checks: workbench served, Open VSX search, extension install. |

Builds run in GitHub Actions (`.github/workflows/ose.yml`), never on a developer machine:
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

Each build produces `dpx-ose-<tag>-<target>.tar.gz` and its `.sha256`. The archive is
self-contained: it bundles the Node runtime pinned in upstream `remote/.npmrc`, so the host does
not need Node. Pushing a tag `ose-v<tag>` (for example `ose-v1.139.1`, or `ose-v1.139.1-dpx.2` for a
rebuild) runs the matrix and publishes a GitHub release with all archives and `SHA256SUMS`.

To move to a new Code-OSS release: change `VERSION`, open a PR (the `product` job checks the new
upstream `product.json` against the overrides in seconds), then run the workflow manually or tag.

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

## Running it with `web/proxy/dpx`

```sh
tar -xzf dpx-ose-1.139.1-linux-arm64.tar.gz -C ~/.local/share
export DPX_OSE_SERVER=~/.local/share/dpx-ose-1.139.1-linux-arm64/bin/dpx-ose-server
# optional; this is the launcher's default (web/proxy/dpx/vscode/runtime.py DEFAULT_OSE_ARGS) plus telemetry off
export DPX_OSE_ARGS='--host {host} --port {port} --without-connection-token --accept-server-license-terms --server-data-dir {data_dir} --telemetry-level off'
cd web/proxy && python -m dpx.serve --root ~/work
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
  a CSP change in `web/proxy/`; tracked as an open item.
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
