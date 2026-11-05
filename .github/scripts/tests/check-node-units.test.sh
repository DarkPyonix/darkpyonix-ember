#!/usr/bin/env bash
# Tests .github/scripts/checks/check-node-units.sh (SPEC FR-P6): it must fail when a unit file or
# a required key is missing, and pass on the real files. Shell only.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
script="$root/.github/scripts/checks/check-node-units.sh"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

fail() { echo "FAIL: $*" >&2; exit 1; }
ok() { echo "ok    $*"; }

fresh() { rm -rf "$work/r"; mkdir -p "$work/r"; cp -R "$root/ember" "$work/r/ember" 2>/dev/null || true; }
run() { (cd "$work/r" && bash "$script" >/dev/null 2>&1); }

# 1. the real tree passes
(cd "$root" && bash "$script" >/dev/null 2>&1) || fail "real tree should pass"
ok "real tree passes"

# 2. missing files fail
rm -rf "$work/r"; mkdir -p "$work/r/ember/node"
run && fail "missing service/ should fail"
ok "missing files fail"

# 3. each key removed in turn fails
for pair in "ember-node.service:KillMode=process" "ember.node.plist:AbandonProcessGroup"; do
    f="${pair%%:*}"; key="${pair##*:}"
    mkdir -p "$work/r/ember/node/service"
    cp "$root/ember/node/service/ember-node.service" "$root/ember/node/service/ember.node.plist" "$work/r/ember/node/service/"
    grep -v "$key" "$root/ember/node/service/$f" > "$work/r/ember/node/service/$f"
    run && fail "$f without $key should fail"
    ok "$f without $key fails"
done
