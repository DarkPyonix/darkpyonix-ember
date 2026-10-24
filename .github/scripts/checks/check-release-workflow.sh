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
need '^  client:' 'client app job missing'
need 'EMBER_CLIENT_RELEASE' 'client job must be gated by EMBER_CLIENT_RELEASE'
for fmt in 'app dmg' 'zip msix' 'AppImage'; do need "formats: \"$fmt\"" "client format list '$fmt' missing"; done
need 'dist/\*\.dmg' 'release must upload .dmg'
need 'dist/\*\.AppImage' 'release must upload .AppImage'
need 'dist/\*\.msix' 'release must upload .msix'
if grep -q "pull_request\|branches:" "$f"; then echo "error: $f must be tag-only" >&2; fail=1; fi
exit "$fail"
