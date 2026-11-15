#!/usr/bin/env bash
# Extension regression test for a packaged OSE build (SPEC NFR-W1, #84).
#
#   ember/ose/regress.sh <unpacked build dir>
#   ember/ose/regress.sh --self-test        offline, no build needed
#
# 1. installs every extension in regression/extensions.json from Open VSX with the server CLI, at
#    its pinned version (id@version) when it has one
# 2. starts the server on a copy of regression/workspace/ and runs OSE_REGRESS_CLIENT: the
#    extension host only starts when a client connects. The client is Ember's editor session:
#
#      OSE_REGRESS_CLIENT='cargo test --locked --manifest-path ember/editor/Cargo.toml --test nfr_w1_extension_regression -- --ignored --nocapture'
#
#    It gets OSE_REGRESS_ADDR (127.0.0.1:<port>), OSE_REGRESS_WS (the workspace copy) and
#    OSE_REGRESS_URL (http://127.0.0.1:<port>/, for a browser client). Without a client,
#    activation is skipped and only the installs are checked.
# 3. parses the extension host logs and compares with regression/expected.json
#
# OSE_REGRESS_OUT=<dir> keeps what the run found there: results.json (the final status of every
# listed extension), installed.txt (the installed folders, so an unpinned version can be pinned)
# and the extension host logs.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
reg="$here/regression"
if [ "${1:-}" = "--self-test" ]; then python3 -I "$reg/regress.py" --self-test; exit; fi
dir="${1:?usage: $0 <build dir> | --self-test}"
bin="$dir/bin/$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["serverApplicationName"])' "$dir/product.json")"
[ -x "$bin" ] || { echo "FAIL: no server launcher at $bin" >&2; exit 1; }
tmp="$(mktemp -d)"; pid=""
out="${OSE_REGRESS_OUT:-}"
keep() { # copy what the run found to OSE_REGRESS_OUT, if set; never fails the run
  [ -n "$out" ] || return 0
  mkdir -p "$out/logs" || return 0
  for f in results.json installed.txt install-failed.txt server.log; do
    if [ -f "$tmp/$f" ]; then cp "$tmp/$f" "$out/" || true; fi
  done
  find "$tmp/data" -name '*.log' -path '*exthost*' -exec cp {} "$out/logs/" \; 2>/dev/null || true
}
cleanup() { [ -n "$pid" ] && kill "$pid" 2>/dev/null || true; wait 2>/dev/null || true; keep; rm -rf "$tmp"; }
trap cleanup EXIT
for scope in Machine User; do
  mkdir -p "$tmp/data/data/$scope"
  printf '{ "extensions.verifySignature": false }\n' > "$tmp/data/data/$scope/settings.json"
done
cp -R "$reg/workspace" "$tmp/ws"

echo "== 1. install"
: > "$tmp/install-failed.txt"
while read -r ext spec; do
  if "$bin" --install-extension "$spec" --extensions-dir "$tmp/ext" --server-data-dir "$tmp/data" \
       --accept-server-license-terms < /dev/null > "$tmp/install.log" 2>&1; then
    echo "  installed $spec"
  else
    echo "  install FAILED $spec"; tail -3 "$tmp/install.log"
    echo "$ext" >> "$tmp/install-failed.txt"
  fi
done < <(python3 -I "$reg/regress.py" specs "$reg/extensions.json")
ls "$tmp/ext" 2>/dev/null | grep -v '^\.' | grep -v '^extensions\.json$' > "$tmp/installed.txt" || true
echo "  installed folders:"; sed 's/^/    /' "$tmp/installed.txt"

if [ -z "${OSE_REGRESS_CLIENT:-}" ]; then
  echo "== 2. activate: OSE_REGRESS_CLIENT not set, skipped"
  if [ -s "$tmp/install-failed.txt" ]; then
    sed 's/^/FAIL: not installed: /' "$tmp/install-failed.txt"; exit 1
  fi
  echo "regress (install only): PASS"; exit 0
fi

echo "== 2. activate"
port="$(python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1])')"
"$bin" --host 127.0.0.1 --port "$port" --without-connection-token --accept-server-license-terms \
  --server-data-dir "$tmp/data" --extensions-dir "$tmp/ext" --telemetry-level off > "$tmp/server.log" 2>&1 &
pid=$!
up=0
for _ in $(seq 1 "${OSE_REGRESS_TIMEOUT:-120}"); do
  if curl -fsS "http://127.0.0.1:$port/" -o /dev/null 2>/dev/null; then up=1; break; fi
  kill -0 "$pid" 2>/dev/null || break
  sleep 1
done
if [ "$up" != 1 ]; then echo "FAIL: the server did not start on port $port" >&2; cat "$tmp/server.log" >&2; exit 1; fi
client_status=0
OSE_REGRESS_URL="http://127.0.0.1:$port/" OSE_REGRESS_ADDR="127.0.0.1:$port" OSE_REGRESS_WS="$tmp/ws" \
  bash -c "$OSE_REGRESS_CLIENT" || client_status=$?
sleep 5
kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true; pid=""

echo "== 3. compare"
logs=$(find "$tmp/data" -name '*.log' -path '*exthost*' 2>/dev/null || true)
# shellcheck disable=SC2086
python3 -I "$reg/regress.py" parse-log $logs > "$tmp/parsed.json" 2>/dev/null || echo '{}' > "$tmp/parsed.json"
status=0
python3 -I "$reg/regress.py" compare "$reg/expected.json" "$tmp/parsed.json" "$tmp/install-failed.txt" "$tmp/results.json" || status=$?
echo "  results:"; sed 's/^/    /' "$tmp/results.json"
if [ "$client_status" != 0 ]; then echo "FAIL: the activation client exited with $client_status" >&2; exit 1; fi
[ "$status" = 0 ] || exit "$status"
echo "regress: PASS"
