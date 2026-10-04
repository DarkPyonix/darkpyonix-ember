#!/usr/bin/env bash
# SPEC NFR-L2 (E1): the launcher and conversation UI never allocate a webview.
#
# 1. Sources: no webview API is named in launcher-process code (crates/app/ and crates/client/).
# 2. Manifests: no webview crate is declared there.
# 3. Dependency graph: if cargo is available, no webview crate is reachable from ember-app on
#    any target (`cargo tree --target all`; resolves the graph without compiling).
#
# crates/app/tests/no_webview.rs runs the same checks under `cargo test`.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

dirs=()
for d in crates/app crates/client; do
  [ -d "$d" ] && dirs+=("$d")
done
if [ "${#dirs[@]}" -eq 0 ]; then
  echo "no launcher crates on this ref; skipping"
  exit 0
fi

fail=0

# 1. webview APIs in sources (tests excluded: they name what they forbid).
api_re='WKWebView|WebView2|ICoreWebView2|android[./]webkit[./]WebView|webkit2gtk|WebKitGTK|wry::|tauri::|dioxus_desktop'
hits="$(
  find "${dirs[@]}" \( -path '*/target' -o -path '*/tests' \) -prune -o -type f -name '*.rs' -print0 \
  | xargs -0 grep -n -E "$api_re" /dev/null 2>/dev/null || true
)"
if [ -n "$hits" ]; then
  echo "NFR-L2 violation: webview API referenced from launcher-process code:" >&2
  echo "$hits" >&2
  fail=1
fi

# 2 and 3. webview crates.
crate_re='^(wry|tauri|tauri-.*|webview2|webview2-.*|webkit2gtk|webkit2gtk-.*|javascriptcore-rs|objc2-web-kit|web-view|webview-sys|dioxus-desktop|dioxus-mobile|cef)$'
for d in "${dirs[@]}"; do
  declared="$(sed -n '/^\[.*dependencies\]/,/^\[/p' "$d/Cargo.toml" | sed -n 's/^\([A-Za-z0-9_-]*\)[[:space:]]*=.*/\1/p' | grep -E "$crate_re" || true)"
  if [ -n "$declared" ]; then
    echo "NFR-L2 violation: $d/Cargo.toml declares webview crates: $declared" >&2
    fail=1
  fi
done

if [ -f crates/app/Cargo.toml ] && command -v cargo >/dev/null 2>&1; then
  tree="$(cargo tree --manifest-path crates/app/Cargo.toml --target all --edges normal,build --prefix none --format '{p}' 2>&1)" || {
    echo "cargo tree failed:" >&2
    echo "$tree" >&2
    exit 1
  }
  found="$(echo "$tree" | awk '{print $1}' | sort -u | grep -E "$crate_re" || true)"
  if [ -n "$found" ]; then
    echo "NFR-L2 violation: webview crates reachable from ember-app:" >&2
    echo "$found" >&2
    fail=1
  fi
else
  echo "cargo not available: dependency graph not checked (sources and manifests were)"
fi

if [ "$fail" -ne 0 ]; then
  exit 1
fi
echo "no-webview OK: no webview crate or API in the launcher (NFR-L2)"
