#!/usr/bin/env bash
# Tests .github/scripts/release/dry-run-summary.sh (SPEC NFR-R1, #880): a complete dry run passes
# and a missing file fails with a message naming it.
set -euo pipefail

script="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)/.github/scripts/release/dry-run-summary.sh"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

fail() { echo "FAIL: $*" >&2; exit 1; }
ok() { echo "ok    $*"; }
t=v0.2.0

make_tree() { # dir
    local d="$1" target crate
    for target in macos-arm64 linux-x86_64 linux-aarch64; do
        mkdir -p "$d/dpx-ember-$target"
        for crate in server node; do
            echo x > "$d/dpx-ember-$target/ember-$crate-$t-$target.tar.gz"
            echo x > "$d/dpx-ember-$target/ember-$crate-$t-$target.tar.gz.sha256"
        done
    done
    mkdir -p "$d"/dpx-ember-app-{macos-arm64,windows-x86_64,linux-x86_64,linux-aarch64}
    m="$d/dpx-ember-app-macos-arm64"
    touch "$m/releases.osx-arm64.json" "$m/Ember-0.2.0-osx-arm64-full.nupkg" "$m/Ember-osx-arm64.dmg" \
          "$m/ember-app-$t-macos-arm64.dmg" "$m/ember-app-$t-macos-arm64.app.zip"
    w="$d/dpx-ember-app-windows-x86_64"
    touch "$w/releases.win-x64.json" "$w/Ember-0.2.0-win-x64-full.nupkg" "$w/Ember-win-x64-Setup.exe" \
          "$w/ember-app-$t-windows-x86_64.zip" "$w/Ember.msix"
    for p in x86_64:linux-x64 aarch64:linux-arm64; do
        l="$d/dpx-ember-app-linux-${p%%:*}"
        touch "$l/releases.${p#*:}.json" "$l/Ember-0.2.0-${p#*:}-full.nupkg" "$l/Ember.AppImage"
    done
}

make_tree "$work/full"
bash "$script" "$work/full" "$t" true >/dev/null || fail "complete tree refused"
ok "a complete dry run passes"

make_tree "$work/nofeed"
rm "$work/nofeed/dpx-ember-app-linux-aarch64/releases.linux-arm64.json"
if bash "$script" "$work/nofeed" "$t" true >/dev/null 2>"$work/err"; then fail "missing feed accepted"; fi
grep -q 'releases.linux-arm64.json for channel linux-arm64' "$work/err" || fail "message does not name the feed and channel: $(cat "$work/err")"
ok "a missing feed fails, naming the file and channel"

make_tree "$work/nosetup"
rm "$work/nosetup/dpx-ember-app-windows-x86_64/Ember-win-x64-Setup.exe"
if bash "$script" "$work/nosetup" "$t" true >/dev/null 2>&1; then fail "missing Setup.exe accepted"; fi
ok "a missing installer fails"

make_tree "$work/notar"
rm "$work/notar/dpx-ember-linux-aarch64/ember-node-$t-linux-aarch64.tar.gz"
if bash "$script" "$work/notar" "$t" true >/dev/null 2>&1; then fail "missing tarball accepted"; fi
ok "a missing tarball fails"

mkdir -p "$work/server"
make_tree "$work/server"; rm -rf "$work/server"/dpx-ember-app-*
bash "$script" "$work/server" "$t" false >/dev/null || fail "server-only run refused without the client"
ok "without the client, only the tarballs are expected"
