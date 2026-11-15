#!/usr/bin/env bash
# Tests .github/scripts/release/check-tag-version.sh (SPEC NFR-R1, #880): the release job stops
# when the tag's version differs from the client's Cargo.toml.
set -euo pipefail

script="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)/.github/scripts/release/check-tag-version.sh"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

fail() { echo "FAIL: $*" >&2; exit 1; }
ok() { echo "ok    $*"; }

cat > "$work/Cargo.toml" <<'TOML'
[package]
name = "ember-app"
version = "0.2.0"
edition = "2024"

[dependencies]
serde = { version = "1", features = ["derive"] }
velopack = "1.2.161"
TOML

bash "$script" v0.2.0 "$work/Cargo.toml" >/dev/null || fail "matching tag refused"
ok "a matching tag passes"

if bash "$script" v0.2.1 "$work/Cargo.toml" 2>/dev/null; then fail "different version accepted"; fi
ok "a different version fails"

if bash "$script" 0.2.0-beta "$work/Cargo.toml" 2>/dev/null; then fail "malformed tag accepted"; fi
ok "a tag that is not vMAJOR.MINOR.PATCH fails"

if bash "$script" v0.2.0 "$work/missing.toml" 2>/dev/null; then fail "missing manifest accepted"; fi
ok "a missing manifest fails"

printf '[dependencies]\nversion = "0.2.0"\n' > "$work/NoPkg.toml"
if bash "$script" v0.2.0 "$work/NoPkg.toml" 2>/dev/null; then fail "version outside [package] accepted"; fi
ok "a version outside [package] does not count"

# The real manifest has a [package] version the script can read.
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
v="$(awk '/^\[/{p=($0=="[package]");next} p&&/^version/{sub(/^[^"]*"/,"");sub(/".*$/,"");print;exit}' "$root/ember/app/Cargo.toml")"
bash "$script" "v$v" "$root/ember/app/Cargo.toml" >/dev/null || fail "ember/app/Cargo.toml version $v not read"
ok "ember/app/Cargo.toml is read ($v)"
