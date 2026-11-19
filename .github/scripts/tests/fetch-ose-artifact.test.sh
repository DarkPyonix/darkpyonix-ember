#!/usr/bin/env bash
# Tests the release mode of ember/editor-conn/scripts/fetch-ose-artifact.sh (SPEC FR-W4 acceptance
# step 5, #83): OSE_RELEASE=<tag> downloads the published archive for the target, checks it
# against the release's SHA256SUMS and unpacks it. Offline: `gh` is a fake on PATH that copies
# files from a local directory, and the archive holds a placeholder launcher that is never run.
set -euo pipefail

script="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)/ember/editor-conn/scripts/fetch-ose-artifact.sh"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

fail() { echo "FAIL: $*" >&2; exit 1; }
ok() { echo "ok    $*"; }
sum() { if command -v sha256sum >/dev/null; then sha256sum "$@"; else shasum -a 256 "$@"; fi; }

# ---- a fake release -----------------------------------------------------------------------------
name="dpx-ose-1.0.0-linux-x64"
mkdir -p "$work/src/$name/bin" "$work/release" "$work/bin"
printf '#!/bin/sh\necho placeholder\n' > "$work/src/$name/bin/dpx-ose-server"
chmod +x "$work/src/$name/bin/dpx-ose-server"
printf '{"serverApplicationName":"dpx-ose-server","commit":"0123abcd"}\n' > "$work/src/$name/product.json"
tar -C "$work/src" -czf "$work/release/$name.tar.gz" "$name"
(cd "$work/release" && sum "$name.tar.gz" > SHA256SUMS)

# ---- a fake gh: only `release download <tag> -R <repo> -p <pattern>... -D <dir>` ----------------
cat > "$work/bin/gh" <<'GH'
#!/usr/bin/env bash
set -euo pipefail
echo "$*" >> "$FAKE_GH_LOG"
[ "${1:-}" = release ] && [ "${2:-}" = download ] || { echo "fake gh: unexpected call: $*" >&2; exit 2; }
dir=""
while [ $# -gt 0 ]; do
  case "$1" in -D) dir="$2"; shift 2 ;; *) shift ;; esac
done
[ -n "$dir" ] || { echo "fake gh: no -D" >&2; exit 2; }
mkdir -p "$dir"
cp "$FAKE_RELEASE_DIR"/* "$dir"/
GH
chmod +x "$work/bin/gh"

run() { # run <cache dir> <tag>; prints the script's stdout
  PATH="$work/bin:$PATH" FAKE_GH_LOG="$work/gh.log" FAKE_RELEASE_DIR="$work/release" \
    OSE_REPO=example/repo OSE_TARGET=linux-x64 OSE_CACHE_DIR="$1" OSE_RELEASE="$2" bash "$script"
}
calls() { if [ -f "$work/gh.log" ]; then wc -l < "$work/gh.log" | tr -d ' '; else echo 0; fi; }

# 1. A good release: stdout is the launcher path and nothing else.
out="$(run "$work/cache1" ose-v1.0.0 2>"$work/err1")" || { cat "$work/err1" >&2; fail "a good release was refused"; }
[ "$out" = "$work/cache1/ose-v1.0.0-linux-x64/$name/bin/dpx-ose-server" ] || fail "stdout is not the launcher path: $out"
[ -x "$out" ] || fail "the launcher is not executable"
grep -q -- "release download ose-v1.0.0 -R example/repo" "$work/gh.log" || fail "gh was not asked for the release: $(cat "$work/gh.log")"
ok "a good release is verified, unpacked and its launcher printed"

# 2. A second call reuses the unpacked copy.
before="$(calls)"
out2="$(run "$work/cache1" ose-v1.0.0 2>/dev/null)" || fail "the cached release was refused"
[ "$out2" = "$out" ] || fail "the cached call printed another path: $out2"
[ "$(calls)" = "$before" ] || fail "the cached call downloaded again"
ok "an unpacked release is not downloaded again"

# 3. An archive that differs from SHA256SUMS is refused and not unpacked.
cp -R "$work/release" "$work/release-good"
printf 'tampered' >> "$work/release/$name.tar.gz"
if run "$work/cache2" ose-v1.0.0 >/dev/null 2>"$work/err3"; then fail "a tampered archive was accepted"; fi
[ ! -e "$work/cache2/ose-v1.0.0-linux-x64/$name" ] || fail "a tampered archive was unpacked"
ok "an archive that does not match SHA256SUMS fails and is not unpacked"

# 4. SHA256SUMS that does not list the archive is refused.
cp "$work/release-good/$name.tar.gz" "$work/release/$name.tar.gz"
echo "0000000000000000000000000000000000000000000000000000000000000000  other.tar.gz" > "$work/release/SHA256SUMS"
if run "$work/cache3" ose-v1.0.0 >/dev/null 2>"$work/err4"; then fail "an unlisted archive was accepted"; fi
grep -q "SHA256SUMS" "$work/err4" || fail "the error does not name SHA256SUMS: $(cat "$work/err4")"
[ ! -e "$work/cache3/ose-v1.0.0-linux-x64/$name" ] || fail "an unlisted archive was unpacked"
ok "an archive SHA256SUMS does not list fails"

# 5. A release without SHA256SUMS is refused (the release check never skips the checksum).
rm "$work/release/SHA256SUMS"
if run "$work/cache4" ose-v1.0.0 >/dev/null 2>"$work/err5"; then fail "a release without SHA256SUMS was accepted"; fi
ok "a release without SHA256SUMS fails"

# 6. A tag that is not ose-v* is refused before anything is downloaded.
before="$(calls)"
if run "$work/cache5" v1.0.0 >/dev/null 2>"$work/err6"; then fail "a tag that is not ose-v* was accepted"; fi
[ "$(calls)" = "$before" ] || fail "gh was called for a malformed tag"
ok "a tag that is not ose-v* fails without a download"

# 7. The Windows target (#83): the archive holds bin/dpx-ose-server.cmd (no execute bit) and
#    that is the launcher printed.
wname="dpx-ose-1.0.0-windows-x64"
mkdir -p "$work/wsrc/$wname/bin" "$work/wrelease"
printf '@echo off\r\n' > "$work/wsrc/$wname/bin/dpx-ose-server.cmd"
chmod -x "$work/wsrc/$wname/bin/dpx-ose-server.cmd"
printf '{"serverApplicationName":"dpx-ose-server","commit":"0123abcd"}\n' > "$work/wsrc/$wname/product.json"
tar -C "$work/wsrc" -czf "$work/wrelease/$wname.tar.gz" "$wname"
(cd "$work/wrelease" && sum "$wname.tar.gz" > SHA256SUMS)
wout="$(PATH="$work/bin:$PATH" FAKE_GH_LOG="$work/gh.log" FAKE_RELEASE_DIR="$work/wrelease" \
  OSE_REPO=example/repo OSE_TARGET=windows-x64 OSE_CACHE_DIR="$work/cache6" OSE_RELEASE=ose-v1.0.0 \
  bash "$script" 2>"$work/err7")" || { cat "$work/err7" >&2; fail "a good Windows release was refused"; }
[ "$wout" = "$work/cache6/ose-v1.0.0-windows-x64/$wname/bin/dpx-ose-server.cmd" ] || fail "Windows launcher path: $wout"
ok "the windows-x64 release prints bin/dpx-ose-server.cmd"

echo "fetch-ose-artifact.test.sh: PASS"
