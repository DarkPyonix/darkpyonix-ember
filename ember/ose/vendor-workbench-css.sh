#!/usr/bin/env bash
# Vendor the Code-OSS workbench CSS Ember needs (INTENT D23, issue #143).
#
#   ember/ose/vendor-workbench-css.sh                 fetch at the pinned commit, write pristine/
#                                                     copies, apply workbench-has.patch to make the
#                                                     files Ember loads
#   ember/ose/vendor-workbench-css.sh --check         offline: pristine/ + patch == vendored files
#   ember/ose/vendor-workbench-css.sh --verify-pristine   network: pristine/ == upstream at the commit
#
# The commit is PINNED_COMMIT in ember/editor-conn/src/lib.rs, the same pin the OSE build uses
# (release tag in ember/ose/VERSION). pristine/ holds upstream plus a header, so CI needs no
# network. workbench-has.patch is Ember's only edit: it turns :has() rules into state classes
# (issue #142), because the renderer's stylo does not parse :has().
# Needs: bash, curl, patch, diff. Downloads go to .scratch/vendor-workbench-css/.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"
out="$root/ember/app/assets/code-oss"
commit="$(sed -n 's/^pub const PINNED_COMMIT: &str = "\([0-9a-f]\{40\}\)";.*/\1/p' "$root/ember/editor-conn/src/lib.rs")"
[ -n "$commit" ] || { echo "cannot read PINNED_COMMIT" >&2; exit 2; }

# vendored name | path in microsoft/vscode
FILES="
views.css src/vs/workbench/browser/parts/views/media/views.css
titlebarpart.css src/vs/workbench/browser/parts/titlebar/media/titlebarpart.css
actionWidget.css src/vs/platform/actionWidget/browser/actionWidget.css
"

header() { # name srcpath
  printf '/* Vendored from microsoft/vscode (Code-OSS), MIT License. Copyright (c) 2015 - present Microsoft Corporation.\n'
  printf ' * Source: %s\n * Commit: %s\n' "$2" "$commit"
  printf ' * License: LICENSE-Code-OSS.txt in this folder. Do not edit by hand; see ember/ose/vendor-workbench-css.sh. */\n'
}

fetch_pristine() { # destdir
  local dl="$root/.scratch/vendor-workbench-css" name src
  mkdir -p "$dl" "$1"
  echo "$FILES" | while read -r name src; do
    [ -n "$name" ] || continue
    curl -sfL "https://raw.githubusercontent.com/microsoft/vscode/$commit/$src" -o "$dl/$name"
    { header "$name" "$src"; cat "$dl/$name"; } > "$1/$name"
  done
  curl -sfL "https://raw.githubusercontent.com/microsoft/vscode/$commit/LICENSE.txt" -o "$dl/LICENSE.txt"
  cp "$dl/LICENSE.txt" "$out/LICENSE-Code-OSS.txt"
}

apply_patch() { # srcdir destdir
  mkdir -p "$2"
  local name src
  echo "$FILES" | while read -r name src; do
    if [ -n "$name" ]; then cp "$1/$name" "$2/$name"; fi
  done
  (cd "$2" && patch -p1 -s < "$out/workbench-has.patch")
}

case "${1:-}" in
  "")
    fetch_pristine "$out/pristine"
    apply_patch "$out/pristine" "$out"
    echo "vendored at $commit"
    ;;
  --check)
    mkdir -p "$root/.scratch"; tmp="$(mktemp -d "$root/.scratch/vendor.XXXXXX")"; trap 'rm -rf "$tmp"' EXIT
    apply_patch "$out/pristine" "$tmp"
    rc=0
    echo "$FILES" | while read -r name src; do
      [ -n "$name" ] || continue
      grep -q "Commit: $commit" "$out/pristine/$name" || { echo "$name: pristine header is not at $commit" >&2; exit 1; }
    done || rc=1
    for f in views.css titlebarpart.css actionWidget.css; do
      diff -q "$tmp/$f" "$out/$f" >/dev/null || { echo "$f differs from pristine + patch" >&2; rc=1; }
    done
    [ "$rc" = 0 ] && echo "vendored CSS matches pristine + patch at $commit"
    exit "$rc"
    ;;
  --verify-pristine)
    mkdir -p "$root/.scratch"; tmp="$(mktemp -d "$root/.scratch/vendor.XXXXXX")"; trap 'rm -rf "$tmp"' EXIT
    out_save="$out"; fetch_pristine "$tmp" 
    diff -r "$tmp" "$out_save/pristine" && echo "pristine matches upstream at $commit"
    ;;
  *) echo "usage: $0 [--check|--verify-pristine]" >&2; exit 2 ;;
esac
