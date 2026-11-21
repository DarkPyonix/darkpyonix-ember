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
# The staged theme makes itself the default (FR-W6, #15), with labels the theme really has and the
# same values as the proxy's defaults.
theme_pkg="$tmp/src/extensions/darkpyonix-theme/package.json"
for key in workbench.colorTheme workbench.preferredDarkColorTheme workbench.preferredLightColorTheme; do
  value="$(jq -r --arg k "$key" '.contributes.configurationDefaults[$k] // empty' "$theme_pkg")"
  [ -n "$value" ] || fail "staged theme sets no $key default"
  jq -e --arg v "$value" '[.contributes.themes[].label] | index($v) != null' "$theme_pkg" >/dev/null \
    || fail "$key default \"$value\" is not a theme label"
  grep -q "\"$key\": \"$value\"" "$repo/ember/proxy/dpx/vscode/html_rewrite.py" \
    || fail "$key default \"$value\" differs from the proxy's THEME_CONFIGURATION_DEFAULTS"
done
[ "$(jq -r '.contributes.configurationDefaults["workbench.colorTheme"]' "$theme_pkg")" = "DarkPyonix Dark" ] \
  || fail "OSE's default colour theme is not DarkPyonix Dark"
jq -e '.contributes.configurationDefaults == null' "$repo/extensions/vscode-darkpyonix-theme/package.json" >/dev/null \
  || fail "the published theme in extensions/ must not force itself as the default"
echo "check-extensions: PASS"
