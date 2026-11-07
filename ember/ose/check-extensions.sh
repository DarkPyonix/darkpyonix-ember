#!/usr/bin/env bash
# Cheap check for SPEC FR-W6 on OSE: build.sh stages both extensions, and staging yields
# loadable built-in extension folders. No Code-OSS clone, no build.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
fail() { echo "FAIL: $*" >&2; exit 1; }

grep -q 'stage-extensions.sh' "$here/build.sh" || fail "build.sh does not call stage-extensions.sh"
awk '/stage-extensions.sh/{s=NR} /gulp core-ci/{g=NR} END{exit !(s && g && s<g)}' "$here/build.sh" \
  || fail "extensions must be staged before gulp core-ci"

tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/src/extensions" "$tmp/nb"
repo="$(cd "$here/../.." && pwd)"
created=0
if [ ! -f "$repo/extensions/vscode-darkpyonix/dist/extension.js" ]; then
  mkdir -p "$repo/extensions/vscode-darkpyonix/dist"
  echo "" > "$repo/extensions/vscode-darkpyonix/dist/extension.js"; created=1
fi
OSE_EXT_SKIP_BUILD=1 "$here/stage-extensions.sh" "$tmp/src" >/dev/null
[ "$created" = 1 ] && rm -rf "$repo/extensions/vscode-darkpyonix/dist"
for d in darkpyonix-notebook darkpyonix-theme; do
  [ -f "$tmp/src/extensions/$d/package.json" ] || fail "$d not staged"
done
[ -f "$tmp/src/extensions/darkpyonix-notebook/dist/extension.js" ] || fail "notebook dist missing"
ls "$tmp/src/extensions/darkpyonix-theme/themes"/*.json >/dev/null || fail "theme files missing"
echo "check-extensions: PASS"
