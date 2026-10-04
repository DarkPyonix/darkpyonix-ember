#!/usr/bin/env bash
# Smoke-test a packaged OSE build (SPEC FR-W4 acceptance, automated part).
#
#   build/ose/smoke.sh <unpacked build dir>     e.g. build/ose/.work/dpx-ose-1.139.1-linux-x64
#
#   1. product.json passes build/ose/check-product.sh (Open VSX, telemetry off, no MS marketplace)
#   2. the server starts with the flags web/proxy/dpx launches it with and serves the workbench
#   3. an Open VSX search through the gallery URL in product.json returns results
#   4. the server CLI installs an extension from Open VSX (OSE_SMOKE_EXTENSION)
#
# Needs only curl and python3 besides the build itself. Starts nothing that outlives it.
set -euo pipefail

dir="${1:?usage: $0 <build dir>}"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ext="${OSE_SMOKE_EXTENSION:-redhat.vscode-yaml}"
timeout_s="${OSE_SMOKE_TIMEOUT:-120}"

bin="$dir/bin/$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["serverApplicationName"])' "$dir/product.json")"
[ -x "$bin" ] || { echo "FAIL: no server launcher at $bin" >&2; exit 1; }

tmp="$(mktemp -d)"
pid=""
cleanup() { [ -n "$pid" ] && kill "$pid" 2>/dev/null || true; wait 2>/dev/null || true; rm -rf "$tmp"; }
trap cleanup EXIT

echo "== 1. product.json"
"$here/check-product.sh" "$dir/product.json"

# Code-OSS has no Marketplace signature verifier (@vscode/vsce-sign is Microsoft-only), so
# installs fail with "Signature verification was not executed" unless verification is off.
# web/proxy/dpx/vscode/runtime.py writes the same machine setting for the OSE runtime.
# The running server reads Machine settings; the server CLI (--install-extension) reads the
# default profile's User settings (remoteExtensionHostAgentCli.ts), so both get it.
for scope in Machine User; do
  mkdir -p "$tmp/data/data/$scope"
  printf '{ "extensions.verifySignature": false }\n' > "$tmp/data/data/$scope/settings.json"
done

echo "== 2. server starts and serves the workbench"
port="$(python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1])')"
# Same flags as DEFAULT_OSE_ARGS in web/proxy/dpx/vscode/runtime.py, plus telemetry off.
"$bin" --host 127.0.0.1 --port "$port" --without-connection-token --accept-server-license-terms \
  --server-data-dir "$tmp/data" --telemetry-level off > "$tmp/server.log" 2>&1 &
pid=$!
ok=0
for _ in $(seq 1 "$timeout_s"); do
  if curl -fsS "http://127.0.0.1:$port/" -o "$tmp/index.html" 2>/dev/null; then ok=1; break; fi
  kill -0 "$pid" 2>/dev/null || break
  sleep 1
done
if [ "$ok" != 1 ] || ! grep -q "workbench" "$tmp/index.html"; then
  echo "FAIL: server did not serve the workbench on port $port" >&2
  cat "$tmp/server.log" >&2
  exit 1
fi
echo "  OK: workbench served on 127.0.0.1:$port"
if grep -E -q "marketplace\.visualstudio\.com|vscode-unpkg\.net|gallery\.vsassets\.io" "$tmp/index.html"; then
  echo "FAIL: served page references the Microsoft Marketplace" >&2; exit 1
fi
kill "$pid"; wait "$pid" 2>/dev/null || true; pid=""

echo "== 3. Open VSX search through the configured gallery"
service="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["extensionsGallery"]["serviceUrl"])' "$dir/product.json")"
curl -fsS -X POST "$service/extensionquery" \
  -H 'Content-Type: application/json' -H 'Accept: application/json;api-version=3.0-preview.1' \
  -d '{"filters":[{"criteria":[{"filterType":10,"value":"yaml"}],"pageNumber":1,"pageSize":5,"sortBy":0,"sortOrder":0}],"flags":914}' \
  -o "$tmp/query.json"
n="$(python3 -c 'import json,sys;print(len(json.load(open(sys.argv[1]))["results"][0]["extensions"]))' "$tmp/query.json")"
[ "$n" -gt 0 ] || { echo "FAIL: Open VSX search returned no extensions" >&2; exit 1; }
echo "  OK: $n results from $service"

echo "== 4. install $ext from Open VSX via the server CLI"
"$bin" --install-extension "$ext" --extensions-dir "$tmp/ext" --server-data-dir "$tmp/data" \
  --accept-server-license-terms
ls "$tmp/ext" | grep -i -q "^${ext}-" || { echo "FAIL: $ext not installed into $tmp/ext" >&2; ls -la "$tmp/ext" >&2 || true; exit 1; }
echo "  OK: $(ls "$tmp/ext" | grep -i "^${ext}-")"

echo "smoke: PASS"
