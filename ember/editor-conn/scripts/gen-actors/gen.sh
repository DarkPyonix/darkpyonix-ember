#!/usr/bin/env bash
# Regenerate ember/editor-conn/src/actors/generated.rs from extHost.protocol.ts at PINNED_COMMIT.
#
# Usage: ember/editor-conn/scripts/gen-actors/gen.sh [--check] [extHost.protocol.ts]
#
# Without a file it downloads src/vs/workbench/api/common/extHost.protocol.ts at the commit in
# PINNED_COMMIT (ember/editor-conn/src/lib.rs) into .scratch/gen-actors/. With --check it writes
# nothing and fails when the checked-in generated.rs differs from the generator's output, which is
# what CI runs (job editor-conn-pin). Needs node and npm; the TypeScript parser version is pinned
# in package-lock.json.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
conn="$(cd "$here/../.." && pwd)"
root="$(git -C "$conn" rev-parse --show-toplevel)"
out="$conn/src/actors/generated.rs"

check=0
if [ "${1:-}" = "--check" ]; then check=1; shift; fi

commit="$(sed -n 's/^pub const PINNED_COMMIT: &str = "\([0-9a-f]\{40\}\)";$/\1/p' "$conn/src/lib.rs")"
[ -n "$commit" ] || { echo "PINNED_COMMIT not found in $conn/src/lib.rs" >&2; exit 1; }

proto="${1:-}"
if [ -z "$proto" ]; then
  mkdir -p "$root/.scratch/gen-actors"
  proto="$root/.scratch/gen-actors/extHost.protocol.$commit.ts"
  if [ ! -s "$proto" ]; then
    curl -sSfL -o "$proto.part" \
      "https://raw.githubusercontent.com/microsoft/vscode/$commit/src/vs/workbench/api/common/extHost.protocol.ts"
    mv "$proto.part" "$proto"
  fi
fi

if [ ! -d "$here/node_modules/typescript" ]; then
  (cd "$here" && npm ci --no-audit --no-fund --silent)
fi

if [ "$check" = 1 ]; then
  tmp="$(mktemp "$root/.scratch/gen-actors/generated.XXXXXX")"
  trap 'rm -f "$tmp"' EXIT
  node "$here/gen.mjs" "$proto" "$commit" > "$tmp"
  if ! diff -u "$out" "$tmp"; then
    echo "src/actors/generated.rs differs from extHost.protocol.ts at $commit; run $0 and commit the result" >&2
    exit 1
  fi
  echo "src/actors/generated.rs matches extHost.protocol.ts at $commit"
else
  node "$here/gen.mjs" "$proto" "$commit" > "$out.part"
  mv "$out.part" "$out"
  echo "wrote $out from $commit"
fi
