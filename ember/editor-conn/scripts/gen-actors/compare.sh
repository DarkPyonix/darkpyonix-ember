#!/usr/bin/env bash
# May a Code-OSS commit join the supported set? (SPEC FR-E10, INTENT D50)
#
# Usage: ember/editor-conn/scripts/gen-actors/compare.sh <commit>
#
# Downloads extHost.protocol.ts at <commit> into .scratch/gen-actors/, generates its actor table
# with gen.mjs, and runs compare.mjs against the pinned src/actors/generated.rs, src/rpc_ids.rs and
# coverage.toml: the proxy ids must be the same and every `bridged` method must keep its signature.
# Exit 0 admits the commit: add it to SUPPORTED_BUILDS in src/lib.rs with the report in the PR.
# Needs node and npm (the TypeScript parser version is pinned in package-lock.json).
set -euo pipefail

commit="${1:?a full microsoft/vscode commit sha}"
[[ "$commit" =~ ^[0-9a-f]{40}$ ]] || { echo "not a full commit sha: $commit" >&2; exit 2; }

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
conn="$(cd "$here/../.." && pwd)"
root="$(git -C "$conn" rev-parse --show-toplevel)"
work="$root/.scratch/gen-actors"
mkdir -p "$work"

proto="$work/extHost.protocol.$commit.ts"
if [ ! -s "$proto" ]; then
  curl -sSfL -o "$proto.part" \
    "https://raw.githubusercontent.com/microsoft/vscode/$commit/src/vs/workbench/api/common/extHost.protocol.ts"
  mv "$proto.part" "$proto"
fi

if [ ! -d "$here/node_modules/typescript" ]; then
  (cd "$here" && npm ci --no-audit --no-fund --silent)
fi

candidate="$work/generated.$commit.rs"
node "$here/gen.mjs" "$proto" "$commit" > "$candidate"
node "$here/compare.mjs" "$conn/src/actors/generated.rs" "$candidate" "$proto" "$conn/src/rpc_ids.rs" "$conn/coverage.toml"
