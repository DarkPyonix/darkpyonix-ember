#!/usr/bin/env bash
# SPEC FR-N5: no iroh or letify-link type appears outside the transport crate.
# Fails if "iroh" or "letify" (letify-link, letify_link) is mentioned in any Cargo.toml or .rs file
# outside ember/transport/. The backend feature `letify` that the server and node crates pass
# through to ember-transport is the one allowed mention: a Cargo feature name, not a type.
# Lock files are not checked: a crate depending on ember-transport legitimately pulls iroh in
# transitively.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
cd "$root"

hits="$(
  find . \
    \( -path ./ember/transport -o -path ./.git -o -path '*/target' -o -path ./.claude -o -path '*/node_modules' \) -prune -o \
    -type f \( -name '*.rs' -o -name 'Cargo.toml' \) -print0 \
  | xargs -0 grep -n -i -w 'iroh' /dev/null 2>/dev/null || true
)"
# Also catch iroh-* crate names and iroh_* paths (word-boundary misses "iroh_relay").
hits2="$(
  find . \
    \( -path ./ember/transport -o -path ./.git -o -path '*/target' -o -path ./.claude -o -path '*/node_modules' \) -prune -o \
    -type f \( -name '*.rs' -o -name 'Cargo.toml' \) -print0 \
  | xargs -0 grep -n -i -E 'iroh[-_]' /dev/null 2>/dev/null || true
)"
# letify-link: the crate name or its Rust path. The pass-through feature (`letify = [...]`) does
# not match: it names the Cargo feature, not the library.
hits3="$(
  find . \
    \( -path ./ember/transport -o -path ./.git -o -path '*/target' -o -path ./.claude -o -path '*/node_modules' \) -prune -o \
    -type f \( -name '*.rs' -o -name 'Cargo.toml' \) -print0 \
  | xargs -0 grep -n -i -E 'letify[-_]link' /dev/null 2>/dev/null || true
)"
all="$(printf '%s\n%s\n%s\n' "$hits" "$hits2" "$hits3" | sed '/^$/d' | sort -u)"

if [ -n "$all" ]; then
  echo "FR-N5 violation: 'iroh' or 'letify-link' referenced outside ember/transport/ (use ember-transport's API):" >&2
  echo "$all" >&2
  exit 1
fi
echo "transport isolation OK: no iroh or letify-link references outside ember/transport/"
