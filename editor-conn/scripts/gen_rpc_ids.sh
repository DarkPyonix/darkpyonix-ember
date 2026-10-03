#!/usr/bin/env bash
# Regenerate the PROXY_IDS table body in editor-conn/src/rpc_ids.rs for a Code-OSS checkout.
#
# Usage: editor-conn/scripts/gen_rpc_ids.sh <path-to-vscode-checkout>
#
# The checkout must be the tag in ose/VERSION (the Code-OSS release OSE is built from).
#
# Prints the array body (one `"Name", // nid` line per identifier) to stdout. Paste it between
# `pub const PROXY_IDS: &[&str] = &[` and `];`, update PINNED_VERSION (= ose/VERSION) and
# PINNED_COMMIT (= the commit printed on stderr) in src/lib.rs, and re-run the tests. CI
# (.github/workflows/checks.yml, job editor-conn-pin) runs this at the ose/VERSION tag and fails if
# src/rpc_ids.rs or the two constants differ.
#
# The rule being reproduced: ProxyIdentifier.nid = ++ProxyIdentifier.count in module evaluation
# order (src/vs/workbench/services/extensions/common/proxyIdentifier.ts). The script fails if any
# file other than extHost.protocol.ts calls createProxyIdentifier, because then the evaluation
# order across modules would matter and a line-order scan would be wrong.
set -euo pipefail

src="${1:?path to a microsoft/vscode checkout}"
proto="$src/src/vs/workbench/api/common/extHost.protocol.ts"
[ -f "$proto" ] || { echo "not found: $proto" >&2; exit 1; }

others=$(grep -rl "createProxyIdentifier" "$src/src/vs" --include='*.ts' \
  | grep -v '/test/' \
  | grep -v 'extHost.protocol.ts$' \
  | grep -v 'proxyIdentifier.ts$' || true)
if [ -n "$others" ]; then
  echo "createProxyIdentifier is also called from:" >&2
  echo "$others" >&2
  echo "the numbering depends on module load order; update this script" >&2
  exit 2
fi

grep "createProxyIdentifier<" "$proto" \
  | sed -E 's/^[[:space:]]*([A-Za-z0-9_]+):.*$/\1/' \
  | awk '{ printf "    \"%s\", // %d\n", $1, NR }'

echo "commit: $(git -C "$src" rev-parse HEAD 2>/dev/null || echo unknown)" >&2
