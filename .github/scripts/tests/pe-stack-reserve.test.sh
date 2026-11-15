#!/usr/bin/env bash
# Tests .github/scripts/checks/pe-stack-reserve.py against hand-built PE headers (#880).
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
script="$here/../checks/pe-stack-reserve.py"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
py=python3; command -v python3 >/dev/null || py=python

# make <file> <magic: 20b|10b> <reserve bytes>
make() {
  "$py" - "$1" "$2" "$3" <<'PY'
import struct, sys
path, magic, reserve = sys.argv[1], int(sys.argv[2], 16), int(sys.argv[3])
pe = 0x80
data = bytearray(1024)
data[0:2] = b"MZ"
struct.pack_into("<I", data, 0x3C, pe)
data[pe:pe + 4] = b"PE\0\0"
opt = pe + 24
struct.pack_into("<H", data, opt, magic)
struct.pack_into("<Q" if magic == 0x20B else "<I", data, opt + 72, reserve)
open(path, "wb").write(data)
PY
}

fail=0
ok() { echo "ok    $1"; }
bad() { echo "FAIL  $1" >&2; fail=1; }

make "$tmp/big.exe" 20b 16777216
out="$("$py" "$script" "$tmp/big.exe" --min-bytes 16777216)" && grep -q "16777216 bytes (16.0 MiB)" <<<"$out" \
  && ok "a 16 MiB PE32+ reserve passes and is printed" || bad "a 16 MiB PE32+ reserve passes and is printed"

make "$tmp/small.exe" 20b 1048576
if "$py" "$script" "$tmp/small.exe" --min-bytes 16777216 >/dev/null 2>"$tmp/err"; then
  bad "the 1 MiB default is refused"
else
  grep -q "below the required" "$tmp/err" && ok "the 1 MiB default is refused" || bad "the 1 MiB default is refused"
fi

make "$tmp/pe32.exe" 10b 8388608
"$py" "$script" "$tmp/pe32.exe" --min-bytes 8388608 >/dev/null && ok "a PE32 header is read" || bad "a PE32 header is read"

printf 'not an executable' > "$tmp/text.exe"
if "$py" "$script" "$tmp/text.exe" >/dev/null 2>&1; then bad "a file that is not a PE is an error"; else ok "a file that is not a PE is an error"; fi

exit "$fail"
