#!/usr/bin/env bash
# SPEC NFR-R1: the release workflow follows the CI naming rule (AGENTS.md "CI names") and keeps
# the properties a release needs: tag trigger, locked builds, checksums, no em-dash.
set -euo pipefail
f=.github/workflows/publish-release.yml
fail=0
need() { grep -Eq -e "$1" "$f" || { echo "error: $f: $2" >&2; fail=1; }; }
[ -f "$f" ] || { echo "error: $f missing" >&2; exit 1; }
need '^name: Publish to [A-Za-z]' 'workflow name must be "Publish to <target>"'
need 'tags: \["v\*"\]' 'must trigger on v* tags'
need 'cargo build --release --locked' 'builds must use --locked'
need 'macos-arm64' 'macOS arm64 target missing'
need 'linux-x86_64' 'Linux x86_64 target missing'
need 'SHA256SUMS' 'SHA256SUMS missing'
need '--verify-tag' 'release must verify the tag'
need 'codesign --force --deep -s -' 'ad hoc signing step missing'
need 'ember/app' 'client job must build ember/app'
need 'ember-app' 'client binary ember-app missing'
[ -f ember/app/Dioxus.toml ] || { echo 'error: ember/app/Dioxus.toml missing' >&2; fail=1; }
need '^  client:' 'client app job missing'
need 'EMBER_CLIENT_RELEASE' 'client job must be gated by EMBER_CLIENT_RELEASE'
for fmt in 'app dmg' 'zip msix' 'AppImage'; do need "formats: \"$fmt\"" "client format list '$fmt' missing"; done
need 'scripts/packager-macos/package-macos' 'macOS packager path missing'
need 'scripts/packager-windows/Cargo\.toml' 'Windows packager path missing'
need 'scripts/packager-linux/appimage\.sh' 'Linux packager path missing'
need 'dist/\*\.dmg' 'release must upload .dmg'
need 'dist/\*\.AppImage' 'release must upload .AppImage'
need 'dist/\*\.msix' 'release must upload .msix'
# Velopack packaging and the feed (NFR-R1, NFR-R2, #880).
need 'release/check-tag-version\.sh' 'tag version check missing'
need 'release/pack-velopack\.sh' 'Velopack packaging step missing'
need 'VPK_VERSION: "[0-9]+\.[0-9]+\.[0-9]+"' 'vpk version must be pinned'
for ch in osx-arm64 win-x64 linux-x64 linux-arm64; do need "channel: $ch" "Velopack channel $ch missing"; done
need 'dist/\*-Setup\.exe' 'release must upload Setup.exe'
need 'dist/\*\.nupkg' 'release must upload the .nupkg files'
need 'dist/releases\.\*\.json' 'release must upload releases.<channel>.json'
for s in .github/scripts/release/check-tag-version.sh .github/scripts/release/pack-velopack.sh; do
    [ -f "$s" ] || { echo "error: $s missing" >&2; fail=1; }
done
grep -q 'scripts/package-velopack\.py' .github/scripts/release/pack-velopack.sh 2>/dev/null \
    || { echo 'error: pack-velopack.sh must call compose-rust scripts/package-velopack.py' >&2; fail=1; }
if grep -q "pull_request\|branches:" "$f"; then echo "error: $f must be tag-only" >&2; fail=1; fi
exit "$fail"
