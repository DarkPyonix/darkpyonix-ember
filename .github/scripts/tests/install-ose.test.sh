#!/usr/bin/env bash
# Tests ember/server/scripts/install-ose.sh (SPEC FR-W4 acceptance step 6, #83) offline: the
# release is a local folder reached through a file:// OSE_BASE_URL, and the archive holds a
# placeholder launcher that is never run.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
script="$root/ember/server/scripts/install-ose.sh"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

fail() { echo "FAIL: $*" >&2; exit 1; }
ok() { echo "ok    $*"; }
sum() { if command -v sha256sum >/dev/null; then sha256sum "$@"; else shasum -a 256 "$@"; fi; }

# ---- a fake release ose-v1.0.0 for linux-x64 -----------------------------------------------------
name="dpx-ose-1.0.0-linux-x64"
mkdir -p "$work/src/$name/bin" "$work/rel/ose-v1.0.0" "$work/rel/ose-v1.0.0-dpx.2"
printf '#!/bin/sh\necho placeholder\n' > "$work/src/$name/bin/dpx-ose-server"
chmod +x "$work/src/$name/bin/dpx-ose-server"
tar -C "$work/src" -czf "$work/rel/ose-v1.0.0/$name.tar.gz" "$name"
(cd "$work/rel/ose-v1.0.0" && sum "$name.tar.gz" > SHA256SUMS)
cp "$work/rel/ose-v1.0.0/"* "$work/rel/ose-v1.0.0-dpx.2/"

run() { # run <install dir> [VAR=value...]: the script with the fake release; stdout only
  local dir="$1"; shift
  env OSE_BASE_URL="file://$work/rel" OSE_TARGET=linux-x64 OSE_INSTALL_DIR="$dir" "$@" "$script" 2>"$work/err"
}

# 1. OSE_VERSION beside the script names the release (the layout of a release archive).
pkg="$work/pkg"
mkdir -p "$pkg"
cp "$script" "$pkg/install-ose.sh"
echo "ose-v1.0.0" > "$pkg/OSE_VERSION"
out="$(env OSE_BASE_URL="file://$work/rel" OSE_TARGET=linux-x64 OSE_INSTALL_DIR="$work/i1" "$pkg/install-ose.sh" 2>"$work/err")" \
  || fail "install from OSE_VERSION failed: $(cat "$work/err")"
[ "$out" = "$work/i1/$name/bin/dpx-ose-server" ] || fail "printed $out"
[ -x "$out" ] || fail "launcher not executable"
ok "OSE_VERSION names the release; launcher unpacked where ember-server looks"

# 2. A second run reuses the install without downloading.
out2="$(env OSE_BASE_URL="file:///nonexistent" OSE_TARGET=linux-x64 OSE_INSTALL_DIR="$work/i1" "$pkg/install-ose.sh" 2>"$work/err")" \
  || fail "reuse failed: $(cat "$work/err")"
[ "$out2" = "$out" ] || fail "reuse printed $out2"
ok "an installed release is reused"

# 3. In a checkout the release comes from ember/ose/VERSION.
want="ose-v$(tr -d '[:space:]' < "$root/ember/ose/VERSION")"
if env OSE_BASE_URL="file://$work/rel" OSE_TARGET=linux-x64 OSE_INSTALL_DIR="$work/i3" "$script" >/dev/null 2>"$work/err"; then
  fail "a release that is not in the fake folder installed"
fi
grep -q "release $want has no SHA256SUMS" "$work/err" || fail "checkout default is not $want: $(cat "$work/err")"
ok "checkout default is $want, and a missing release names it"

# 4. A rebuild tag finds the archive named after the Code-OSS tag.
out="$(run "$work/i4" OSE_RELEASE=ose-v1.0.0-dpx.2)" || fail "rebuild tag failed: $(cat "$work/err")"
[ "$out" = "$work/i4/$name/bin/dpx-ose-server" ] || fail "rebuild printed $out"
ok "rebuild tag ose-v1.0.0-dpx.2 installs $name"

# 5. A tampered archive is refused and nothing is unpacked.
cp -R "$work/rel/ose-v1.0.0" "$work/rel/ose-v1.0.1"
mv "$work/rel/ose-v1.0.1/$name.tar.gz" "$work/rel/ose-v1.0.1/dpx-ose-1.0.1-linux-x64.tar.gz"
sed -i.bak "s/$name/dpx-ose-1.0.1-linux-x64/" "$work/rel/ose-v1.0.1/SHA256SUMS"
echo tampered >> "$work/rel/ose-v1.0.1/dpx-ose-1.0.1-linux-x64.tar.gz"
if run "$work/i5" OSE_RELEASE=ose-v1.0.1 >/dev/null; then fail "a tampered archive installed"; fi
grep -q "does not match SHA256SUMS" "$work/err" || fail "tamper message: $(cat "$work/err")"
[ ! -e "$work/i5/dpx-ose-1.0.1-linux-x64" ] || fail "tampered archive was unpacked"
ok "a tampered archive is refused"

# 6. An archive SHA256SUMS does not list is refused.
cp -R "$work/rel/ose-v1.0.0" "$work/rel/ose-v1.0.2"
cp "$work/rel/ose-v1.0.2/$name.tar.gz" "$work/rel/ose-v1.0.2/dpx-ose-1.0.2-linux-x64.tar.gz"
if run "$work/i6" OSE_RELEASE=ose-v1.0.2 >/dev/null; then fail "an unlisted archive installed"; fi
grep -q "does not list dpx-ose-1.0.2-linux-x64.tar.gz" "$work/err" || fail "unlisted message: $(cat "$work/err")"
ok "an unlisted archive is refused"

# 7. A tag that is not an OSE tag is refused before any download.
if run "$work/i7" OSE_RELEASE=v0.1.1 >/dev/null; then fail "v0.1.1 accepted"; fi
grep -q "not an OSE release tag" "$work/err" || fail "tag message: $(cat "$work/err")"
ok "a non-OSE tag is refused"

echo "install-ose.test.sh: PASS"
