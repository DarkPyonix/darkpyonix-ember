#!/usr/bin/env bash
# Extension regression test for a packaged OSE build (SPEC NFR-W1, #84).
#
#   ember/ose/regress.sh <unpacked build dir>
#   ember/ose/regress.sh --self-test        offline, no build needed
#
# 1. installs every extension in regression/extensions.json from Open VSX with the server CLI
# 2. starts the server and runs OSE_REGRESS_CLIENT (a command that opens the workbench at the URL
#    in $OSE_REGRESS_URL for a few seconds; the extension host only starts when a client
#    connects). Without it, activation is "skipped" and only installs are checked.
# 3. parses the extension host logs and compares with regression/expected.json
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
reg="$here/regression"
if [ "${1:-}" = "--self-test" ]; then python3 -I "$reg/regress.py" --self-test; exit; fi
dir="${1:?usage: $0 <build dir> | --self-test}"
bin="$dir/bin/$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["serverApplicationName"])' "$dir/product.json")"
[ -x "$bin" ] || { echo "FAIL: no server launcher at $bin" >&2; exit 1; }
tmp="$(mktemp -d)"; pid=""
cleanup() { [ -n "$pid" ] && kill "$pid" 2>/dev/null || true; wait 2>/dev/null || true; rm -rf "$tmp"; }
trap cleanup EXIT
for scope in Machine User; do
  mkdir -p "$tmp/data/data/$scope"
  printf '{ "extensions.verifySignature": false }\n' > "$tmp/data/data/$scope/settings.json"
done

echo "== 1. install"
printf '{}' > "$tmp/results.json"
installed=""
while read -r ext; do
  if "$bin" --install-extension "$ext" --extensions-dir "$tmp/ext" --server-data-dir "$tmp/data" \
       --accept-server-license-terms > "$tmp/install.log" 2>&1; then
    installed="$installed $ext"; echo "  installed $ext"
  else
    echo "  install FAILED $ext"; tail -3 "$tmp/install.log"
  fi
done < <(python3 -I "$reg/regress.py" ids "$reg/extensions.json")

echo "== 2. activate"
port="$(python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1])')"
"$bin" --host 127.0.0.1 --port "$port" --without-connection-token --accept-server-license-terms \
  --server-data-dir "$tmp/data" --extensions-dir "$tmp/ext" --telemetry-level off > "$tmp/server.log" 2>&1 &
pid=$!
for _ in $(seq 1 "${OSE_REGRESS_TIMEOUT:-120}"); do
  curl -fsS "http://127.0.0.1:$port/" -o /dev/null 2>/dev/null && break; sleep 1
done
if [ -n "${OSE_REGRESS_CLIENT:-}" ]; then
  OSE_REGRESS_URL="http://127.0.0.1:$port/" bash -c "$OSE_REGRESS_CLIENT"
  sleep 5
else
  echo "  OSE_REGRESS_CLIENT not set: activation skipped"
fi
kill "$pid"; wait "$pid" 2>/dev/null || true; pid=""

echo "== 3. compare"
logs=$(find "$tmp/data" -name '*.log' -path '*exthost*' 2>/dev/null || true)
# shellcheck disable=SC2086
python3 -I "$reg/regress.py" parse-log $logs > "$tmp/results.json" 2>/dev/null || echo '{}' > "$tmp/results.json"
if [ -z "${OSE_REGRESS_CLIENT:-}" ]; then
  missing=0
  for e in $(python3 -I "$reg/regress.py" ids "$reg/extensions.json"); do
    case " $installed " in *" $e "*) ;; *) echo "FAIL: $e not installed"; missing=1 ;; esac
  done
  [ "$missing" = 0 ] && echo "regress (install only): PASS" ; exit "$missing"
fi
python3 -I "$reg/regress.py" compare "$reg/expected.json" "$tmp/results.json"
echo "regress: PASS"
