#!/usr/bin/env bash
# Build DarkPyonix OSE: the Code-OSS (MIT) VS Code web server ("REH web"), renamed and pointed at
# Open VSX, packaged as a self-contained tar.gz (it bundles its own Node).
#
#   build/ose/build.sh [--target linux-x64|linux-arm64|darwin-arm64|darwin-x64] [--tag <code-oss tag>]
#                [--work <dir>] [--out <dir>] [--skip-install] [--smoke]
#   build/ose/build.sh --product-only      # cheap: fetch only product.json, apply overrides, check
#
# Meant for CI (.github/workflows/test-ose.yml). A full build takes tens of minutes and ~8 GB of RAM,
# so it refuses to run outside CI unless OSE_ALLOW_LOCAL_BUILD=1 is set.
#
# Steps (each mirrors a documented upstream step; see build/ose/README.md for sources):
#   1. fetch    shallow-clone microsoft/vscode at the tag in build/ose/VERSION
#   2. product  merge build/ose/product.overrides.json into product.json, drop Microsoft-only keys,
#               then build/ose/check-product.sh
#   3. install  (Linux) build/npm/preinstall.ts first, then npm ci (npm + package-lock.json, not yarn)
#   4. build    gulp core-ci -> compile-copilot-extension-build -> stage the Copilot SDK into
#               .build/extensions/copilot -> gulp vscode-reh-web-<platform>-<arch>-min-ci
#   5. package  dpx-ose-<tag>-<target>.tar.gz + .sha256, product check on the packaged output
#   6. smoke    (--smoke) build/ose/smoke.sh on the packaged server
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

TAG="$(tr -d '[:space:]' < "$here/VERSION")"
TARGET=""
WORK="${OSE_WORK_DIR:-$here/.work}"
OUT="${OSE_OUT_DIR:-$here/dist}"
SKIP_INSTALL=0
SMOKE=0
PRODUCT_ONLY=0
MAX_OLD_SPACE="${OSE_MAX_OLD_SPACE:-8192}"

while [ $# -gt 0 ]; do
  case "$1" in
    --target) TARGET="$2"; shift 2 ;;
    --tag) TAG="$2"; shift 2 ;;
    --work) WORK="$2"; shift 2 ;;
    --out) OUT="$2"; shift 2 ;;
    --skip-install) SKIP_INSTALL=1; shift ;;
    --smoke) SMOKE=1; shift ;;
    --product-only) PRODUCT_ONLY=1; shift ;;
    -h|--help) sed -n '2,21p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

log() { printf '\n==> %s\n' "$*"; }
die() { echo "error: $*" >&2; exit 1; }

# Merge the overrides into a product.json in place, then verify it. Drops keys that point at
# Microsoft-only services:
#   voiceWsUrl  - Microsoft's hosted voice endpoint (mai.microsoft.com)
apply_product() {
  local product="$1"
  jq -s '.[0] * .[1] | del(.voiceWsUrl)' "$product" "$here/product.overrides.json" > "$product.new"
  mv "$product.new" "$product"
  "$here/check-product.sh" "$product"
}

if [ "$PRODUCT_ONLY" = 1 ]; then
  # No clone, no install, no build: safe anywhere (used by CI on pull requests).
  command -v jq >/dev/null && command -v curl >/dev/null || die "jq and curl are required"
  mkdir -p "$WORK/product-check"
  curl -fsSL "https://raw.githubusercontent.com/microsoft/vscode/$TAG/product.json" \
    -o "$WORK/product-check/product.json"
  log "product.json for $TAG + build/ose/product.overrides.json"
  apply_product "$WORK/product-check/product.json"
  exit 0
fi

if [ "${CI:-}" != "true" ] && [ "${OSE_ALLOW_LOCAL_BUILD:-}" != "1" ]; then
  die "OSE builds run in GitHub Actions (.github/workflows/test-ose.yml). Set OSE_ALLOW_LOCAL_BUILD=1 to build here anyway."
fi

# --- target -------------------------------------------------------------------------------
host_target() {
  local os arch
  case "$(uname -s)" in Linux) os=linux ;; Darwin) os=darwin ;; *) die "unsupported OS $(uname -s)" ;; esac
  case "$(uname -m)" in x86_64|amd64) arch=x64 ;; arm64|aarch64) arch=arm64 ;; *) die "unsupported arch $(uname -m)" ;; esac
  echo "$os-$arch"
}
[ -n "$TARGET" ] || TARGET="$(host_target)"
case "$TARGET" in
  linux-x64|linux-arm64|darwin-arm64|darwin-x64) ;;
  *) die "unsupported target $TARGET (linux-x64, linux-arm64, darwin-arm64, darwin-x64)" ;;
esac
# Native node modules (node-pty, spdlog, ...) are compiled for the build machine, so the target
# must match the host. Cross builds need upstream's sysroot setup, which this script does not do.
[ "$TARGET" = "$(host_target)" ] || die "target $TARGET does not match this host ($(host_target)); build on a matching runner"

for tool in git jq node npm python3 tar; do
  command -v "$tool" >/dev/null || die "$tool is required"
done

SRC="$WORK/vscode"
NAME="dpx-ose-$TAG-$TARGET"
mkdir -p "$WORK" "$OUT"

# --- 1. fetch ------------------------------------------------------------------------------
log "fetch microsoft/vscode @ $TAG"
if [ -d "$SRC/.git" ] && [ "$(git -C "$SRC" describe --tags --exact-match 2>/dev/null || true)" = "$TAG" ]; then
  git -C "$SRC" checkout -- product.json
else
  # Keep download caches (.build/node, .build/builtInExtensions) a CI cache may have restored.
  rm -rf "$WORK/.build-cache"
  [ -d "$SRC/.build" ] && mv "$SRC/.build" "$WORK/.build-cache"
  rm -rf "$SRC"
  git clone --depth 1 --branch "$TAG" https://github.com/microsoft/vscode.git "$SRC"
  if [ -d "$WORK/.build-cache" ]; then mv "$WORK/.build-cache" "$SRC/.build"; fi
fi
COMMIT="$(git -C "$SRC" rev-parse HEAD)"
echo "commit $COMMIT"

want_node="$(tr -d '[:space:]v' < "$SRC/.nvmrc")"
have_node="$(node -p 'process.versions.node')"
# build/npm/preinstall.ts requires the same major and >= the .nvmrc version (and refuses yarn).
if [ "${want_node%%.*}" != "${have_node%%.*}" ]; then
  die "Node $have_node does not match the tag's .nvmrc ($want_node); upstream's preinstall check would fail"
fi

# --- 2. product.json -------------------------------------------------------------------------
log "apply build/ose/product.overrides.json"
apply_product "$SRC/product.json"

# --- 3. install ------------------------------------------------------------------------------
export ELECTRON_SKIP_BINARY_DOWNLOAD=1       # the web server does not need Electron
export PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1    # nor test browsers
export ONNXRUNTIME_NODE_INSTALL=skip         # upstream sets this too; no ONNX runtime download
export NODE_OPTIONS="--max-old-space-size=$MAX_OLD_SPACE"
# Kerberos on macOS: link directly instead of dlopen (upstream product-build-darwin-compile.yml).
[ "${TARGET%%-*}" = darwin ] && export GYP_DEFINES="${GYP_DEFINES:+$GYP_DEFINES }kerberos_use_rtld=false"
if [ "$SKIP_INSTALL" = 0 ]; then
  if [ "${TARGET%%-*}" = linux ]; then
    # npm (v7+) runs the root package's preinstall only after the dependencies' install scripts,
    # but build/npm/preinstall.ts is what downloads the Electron/Node headers and overlays
    # build/npm/gyp/custom-headers on them (v8config.h: deprecation-attribute syntax GCC < 13 can
    # parse, https://gcc.gnu.org/bugzilla/show_bug.cgi?id=69585). Without it, native modules such as
    # native-keymap fail to compile with Ubuntu 22.04's GCC 11. Upstream runs it explicitly before
    # npm ci for the same reason (build/azure-pipelines/linux/steps/product-build-linux-compile.yml).
    log "preinstall (v8 header overlay)"
    (cd "$SRC" && node build/npm/preinstall.ts)
  fi
  log "npm ci"
  for attempt in 1 2 3; do
    (cd "$SRC" && npm ci) && break
    [ "$attempt" = 3 ] && die "npm ci failed 3 times"
    echo "npm ci failed (attempt $attempt), retrying in 30s"; sleep 30
  done
fi

# --- 4. build ----------------------------------------------------------------------------------
# Upstream's own sequence (build/azure-pipelines/{linux,darwin}/steps/product-build-*-compile.yml):
#   gulp core-ci                         built-in extensions + media, esbuild bundles incl.
#                                        out-vscode-reh-web-min
#   (download the Copilot VSIX into .build/extensions/copilot)
#   gulp vscode-reh-web-<target>-min-ci  native extensions, Node runtime, package, Copilot shim
# The Copilot VSIX comes from a separate upstream job that needs the private microsoft/vscode-capi
# repo, so it cannot be reproduced here. Instead the extension is built from source with upstream's
# own non-CI task (compile-copilot-extension-build), and the @github/copilot SDK that the VSIX would
# carry is staged next to it (see stage_copilot_sdk). The one-shot vscode-reh-web-<target>-min task
# is not used: it runs the same compile task, but the copied extension lacks the SDK
# (build/.moduleignore strips @github/copilot/**), so its final Copilot shim step
# (prepareBuiltInCopilotRipgrepShim, build/lib/copilot.ts) always fails outside Microsoft's pipeline.

# Mirror the VSIX layout for node_modules/@github/copilot: its package.json (with the ./sdk export
# that extensions/copilot/script/postinstall.ts adds) and the sdk/ tree that postinstall
# materialized. The -ci task's shim then prunes it to the target platform and adds the
# version-matched native prebuilds and ripgrep, exactly as it does for the VSIX.
stage_copilot_sdk() {
  local from="$SRC/extensions/copilot/node_modules/@github/copilot"
  local to="$SRC/.build/extensions/copilot/node_modules/@github/copilot"
  [ -f "$SRC/.build/extensions/copilot/package.json" ] || die "compile-copilot-extension-build did not produce .build/extensions/copilot"
  [ -f "$from/sdk/index.js" ] || die "Copilot SDK missing at $from/sdk (extensions/copilot postinstall did not run?)"
  rm -rf "$to"
  mkdir -p "$to"
  cp "$from/package.json" "$to/"
  cp -R "$from/sdk" "$to/sdk"
  echo "staged $(du -sh "$to" | cut -f1) of @github/copilot into .build/extensions/copilot"
}

rm -rf "$SRC/.build/extensions"   # what upstream's clean-extensions-build does
log "gulp core-ci"
(cd "$SRC" && npm run gulp core-ci)
log "gulp compile-copilot-extension-build"
(cd "$SRC" && npm run gulp compile-copilot-extension-build)
log "stage Copilot SDK"
stage_copilot_sdk
log "gulp vscode-reh-web-$TARGET-min-ci"
(cd "$SRC" && npm run gulp "vscode-reh-web-$TARGET-min-ci")

BUILT="$WORK/vscode-reh-web-$TARGET"
[ -d "$BUILT" ] || die "expected build output at $BUILT"
SERVER_BIN="bin/$(jq -r .serverApplicationName "$SRC/product.json")"
[ -x "$BUILT/$SERVER_BIN" ] || die "server launcher $SERVER_BIN missing from the build"

# --- 5. package ----------------------------------------------------------------------------------
log "package $NAME"
"$here/check-product.sh" "$BUILT/product.json"
rm -rf "${WORK:?}/$NAME"
mv "$BUILT" "$WORK/$NAME"
jq -n --arg tag "$TAG" --arg commit "$COMMIT" --arg target "$TARGET" \
      --arg node "$have_node" --arg bin "$SERVER_BIN" \
      --arg built "$(date -u +%Y-%m-%dT%H:%M:%SZ)" --arg run "${GITHUB_SERVER_URL:-}/${GITHUB_REPOSITORY:-}/actions/runs/${GITHUB_RUN_ID:-}" \
      '{name:"DarkPyonix OSE", code_oss_tag:$tag, code_oss_commit:$commit, target:$target,
        build_node:$node, server_bin:$bin, built_at:$built, ci_run:$run}' \
  > "$WORK/$NAME/BUILDINFO.json"
cp "$here/README.md" "$WORK/$NAME/OSE-README.md"

tarball="$OUT/$NAME.tar.gz"
tar -C "$WORK" -czf "$tarball" "$NAME"
if command -v sha256sum >/dev/null; then
  (cd "$OUT" && sha256sum "$NAME.tar.gz" > "$NAME.tar.gz.sha256")
else
  (cd "$OUT" && shasum -a 256 "$NAME.tar.gz" > "$NAME.tar.gz.sha256")
fi
cat "$OUT/$NAME.tar.gz.sha256"

# --- 6. smoke ------------------------------------------------------------------------------------
if [ "$SMOKE" = 1 ]; then
  log "smoke test"
  "$here/smoke.sh" "$WORK/$NAME"
fi

log "done: $tarball"
if [ -n "${GITHUB_OUTPUT:-}" ]; then
  { echo "name=$NAME"; echo "tarball=$tarball"; echo "dir=$WORK/$NAME"; } >> "$GITHUB_OUTPUT"
fi
