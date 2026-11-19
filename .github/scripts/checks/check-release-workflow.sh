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
# Dry run (#880, leader 2026-11-15): a manual dispatch packs without publishing; only a v* tag push
# reaches the publish job, and the summary checks every expected file.
need 'workflow_dispatch:' 'dry run (workflow_dispatch) missing'
need "if: \\$\\{\\{ github\\.event_name == 'push' && startsWith\\(github\\.ref, 'refs/tags/v'\\)" 'publish job must run only on a v* tag push'
need 'name: Dry run summary' 'dry run summary job missing'
need 'release/dry-run-summary\.sh' 'dry run summary script missing'
# Android APK (NFR-R1, FR-L36, #880, leader 2026-11-15): its own job under the client gate, built with
# the repository's script, uploaded under the client artifact prefix and attached to the release.
need '^  android:' 'Android client job missing'
need 'name: Client app \(android-arm64\)' 'Android job must be named "Client app (android-arm64)"'
need 'scripts/build-android\.sh --release --cargo-args=--locked' 'Android job must build with build-android.sh --release and --locked'
need 'DX_VERSION: "[0-9]+\.[0-9]+\.[0-9]+"' 'dioxus-cli version must be pinned'
need 'targets: aarch64-linux-android' 'Android job needs the aarch64-linux-android target'
need 'lib/arm64-v8a/libmain\.so' 'Android job must check the APK holds lib/arm64-v8a/libmain.so'
need 'apksigner" verify --print-certs' 'Android job must report how the APK is signed'
need 'ember-app-\$TAG-android-arm64\.apk' 'APK must be named ember-app-<tag>-android-arm64.apk'
need 'name: dpx-ember-app-android-arm64' 'Android artifact dpx-ember-app-android-arm64 missing'
need 'needs: \[version, build, client, android\]' 'publish and dry run must need the Android job'
need "needs\.android\.result == 'success' \|\| needs\.android\.result == 'skipped'" 'publish must accept a skipped Android job and refuse a failed one'
need 'dist/\*\.apk' 'release must upload the .apk'
need 'debug-signed' 'release notes must say when the APK is debug-signed'
[ "$(grep -c 'needs: \[version, build, client, android\]' "$f")" = 2 ] \
    || { echo "error: $f: both the publish and the dry run summary jobs must need android" >&2; fail=1; }
android_if="$(sed -n '/^  android:/,/^  [a-z-]*:$/p' "$f" | grep -m1 '^    if:' || true)"
client_if="$(sed -n '/^  client:/,/^  [a-z-]*:$/p' "$f" | grep -m1 '^    if:' || true)"
[ -n "$android_if" ] && [ "$android_if" = "$client_if" ] \
    || { echo "error: $f: the Android job must run under the client job's condition" >&2; fail=1; }
grep -q 'proguard-keep\.pro' ember/app/Dioxus.toml 2>/dev/null \
    || { echo 'error: ember/app/Dioxus.toml must name android/proguard-keep.pro (R8 in a signed build)' >&2; fail=1; }
if git ls-files 2>/dev/null | grep -Eiq '\.(jks|keystore)$'; then
    echo 'error: a keystore is committed; it belongs in the EMBER_ANDROID_* secrets' >&2; fail=1
fi
[ "$(grep -c 'contents: write' "$f")" = 1 ] || { echo "error: $f: only the publish job may have contents: write" >&2; fail=1; }
for s in .github/scripts/release/check-tag-version.sh .github/scripts/release/pack-velopack.sh \
         .github/scripts/release/dry-run-summary.sh ember/app/scripts/build-android.sh; do
    [ -f "$s" ] || { echo "error: $s missing" >&2; fail=1; }
done
grep -q 'scripts/package-velopack\.py' .github/scripts/release/pack-velopack.sh 2>/dev/null \
    || { echo 'error: pack-velopack.sh must call compose-rust scripts/package-velopack.py' >&2; fail=1; }
if grep -q "pull_request\|branches:" "$f"; then echo "error: $f must be tag-only" >&2; fail=1; fi
exit "$fail"
