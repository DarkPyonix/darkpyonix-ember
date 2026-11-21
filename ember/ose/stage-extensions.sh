#!/usr/bin/env bash
# Stage the DarkPyonix editor extensions as built-in extensions of a Code-OSS checkout (SPEC FR-W6).
#
#   ember/ose/stage-extensions.sh <code-oss checkout>
#
# Upstream's gulp build packages every folder in <checkout>/extensions/ that has a package.json,
# so copying the runtime files there makes the extension built in (installed by default, present on
# first launch). No upstream source is modified. The notebook extension is built first (esbuild);
# OSE_EXT_SKIP_BUILD=1 reuses an existing dist/ (used by check-extensions.sh).
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
src="${1:?usage: $0 <code-oss checkout>}"
[ -d "$src/extensions" ] || { echo "error: $src/extensions not found" >&2; exit 1; }

nb="$repo/extensions/vscode-darkpyonix"
if [ "${OSE_EXT_SKIP_BUILD:-}" != 1 ]; then
  (cd "$nb" && npm ci && npm run build)
fi
[ -f "$nb/dist/extension.js" ] || { echo "error: $nb/dist/extension.js missing (build the extension)" >&2; exit 1; }

rm -rf "$src/extensions/darkpyonix-notebook" "$src/extensions/darkpyonix-theme"
mkdir -p "$src/extensions/darkpyonix-notebook" "$src/extensions/darkpyonix-theme"
cp "$nb/package.json" "$nb/LICENSE" "$src/extensions/darkpyonix-notebook/"
cp -R "$nb/dist" "$src/extensions/darkpyonix-notebook/dist"
th="$repo/extensions/vscode-darkpyonix-theme"
cp "$th/LICENSE" "$src/extensions/darkpyonix-theme/"
# In OSE the DarkPyonix theme is the default colour theme (SPEC FR-W6, #15): the staged copy of
# the manifest contributes configurationDefaults (a window-scoped setting, which Code-OSS accepts
# from an extension). The published theme in extensions/ stays a plain theme. The values match the
# proxy's THEME_CONFIGURATION_DEFAULTS (ember/proxy/dpx/vscode/html_rewrite.py).
command -v jq >/dev/null || { echo "error: jq is needed to stage the theme defaults" >&2; exit 1; }
jq '.contributes.configurationDefaults = {
      "workbench.colorTheme": "DarkPyonix Dark",
      "workbench.preferredDarkColorTheme": "DarkPyonix Dark",
      "workbench.preferredLightColorTheme": "DarkPyonix Light"
    }' "$th/package.json" > "$src/extensions/darkpyonix-theme/package.json"
cp -R "$th/themes" "$src/extensions/darkpyonix-theme/themes"
echo "staged darkpyonix-notebook and darkpyonix-theme into $src/extensions"
