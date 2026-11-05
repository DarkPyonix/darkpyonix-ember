#!/usr/bin/env bash
# Tests .github/scripts/bench/summarize-matrix.py (NFR-N1) on fixtures. No cargo, no network.
# Needs jq and python3 (CI runners have both; locally `uv run python` works the same).
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
script="$here/../summarize-matrix.py"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
py=(python3 -I)

fail() { echo "FAIL: $*" >&2; echo "$out" >&2; exit 1; }
ok() { echo "ok    $*"; }
has() { grep -Eq -- "$1" <<<"$out" || fail "expected /$1/"; }
hasnt() { ! grep -Eq -- "$1" <<<"$out" || fail "unexpected /$1/"; }

r="$work/results"
# home-office: 20 direct runs, the last two slow, so the p95 (19th value) is the slow one.
mkdir -p "$r/home-office"
for i in $(seq -w 1 20); do
    if [[ $i == 19 || $i == 20 ]]; then ms=5000.0; else ms=400.0; fi
    jq --argjson ms "$ms" '.setup_first_byte_ms = $ms' "$here/fixtures/direct.json" > "$r/home-office/$i.json"
done
echo 0 > "$r/home-office/failures.txt"
# udp-blocked: 18 relayed runs, 2 failed (exit file and an empty JSON).
mkdir -p "$r/udp-blocked"
for i in $(seq -w 1 18); do cp "$here/fixtures/relayed.json" "$r/udp-blocked/$i.json"; done
: > "$r/udp-blocked/19.json"; echo 1 > "$r/udp-blocked/19.exit"
: > "$r/udp-blocked/20.json"
echo 2 > "$r/udp-blocked/failures.txt"
# home-lte: only 5 runs. symmetric-nat: no directory.
mkdir -p "$r/home-lte"
for i in $(seq -w 1 5); do cp "$here/fixtures/direct.json" "$r/home-lte/$i.json"; done

out="$("${py[@]}" "$script" "$r")"
has '^home-office +normal +20 +0 +100\.0 +0\.0 +400\.0 ms +5000\.0 ms +400\.0 ms +2\.0 ms +3\.0 ms +90%'
ok "home-office: 100% direct, first byte p50 400 and p95 5000 ms, RTT delta 2 and 3 ms, throughput 90% of TCP"
has '^udp-blocked +udp-blocked +20 +2 +0\.0 +90\.0 +2000\.0 ms +2000\.0 ms'
ok "udp-blocked: 2 failures stay in the denominator, relayed share 90%"
has 'udp-blocked: TCP connect 100%: 90\.0% FAIL'
has 'udp-blocked: .*first byte p95 <= 3000 ms: 2000\.0 ms PASS'
ok "udp-blocked lines judged"
has 'home-office: first byte p95 <= 1500 ms: 5000\.0 ms FAIL'
has 'home-office: direct RTT over raw TCP p50 <= 5 ms: 2\.0 ms PASS; p95 <= 15 ms: 3\.0 ms PASS'
has 'home-office: throughput >= 80% of raw TCP: 90% PASS'
ok "home-office lines judged"
has 'Direct success excluding symmetric NAT and UDP-blocked: 100\.0% PASS'
has 'udp-blocked: relay -> direct upgrade p95 <= 10000 ms: - MISSING \(18 relayed-start run'
has 'home-office: relay -> direct upgrade p95 <= 10000 ms: - MISSING \(0 relayed-start'
ok "upgrade line is MISSING without relayed-start data"
has '^symmetric-nat +- +0 +- +MISSING'
has 'symmetric-nat: pair directory not found'
has 'home-lte: only 5 of 20 runs'
has 'udp-blocked: 2 failed run\(s\)'
ok "missing pair, under-20 runs and failures are flagged"
has 'Network change .*: MISSING'
has 'Mobile: .*: MISSING'
ok "manual lines are MISSING"
hasnt 'home-office: [0-9]+ direct run\(s\) without'
ok "baseline present, no baseline flag"

# A relayed run with an error baseline never counts as a direct run lacking a baseline.
hasnt 'udp-blocked: [0-9]+ direct run'

# --strict exits 1 when flagged, 0 when the default run is clean of flags.
if "${py[@]}" "$script" "$r" --strict >/dev/null; then fail "--strict should exit 1 with flags"; fi
ok "--strict exits 1 with flags"
clean="$work/clean"; mkdir -p "$clean/home-office"
for i in $(seq -w 1 20); do cp "$here/fixtures/direct.json" "$clean/home-office/$i.json"; done
"${py[@]}" "$script" "$clean" --expect home-office --strict >/dev/null || fail "clean run should exit 0"
ok "--strict exits 0 without flags"

# A direct run whose tcp_baseline is null is flagged.
nb="$work/nb"; mkdir -p "$nb/home-office"
for i in $(seq -w 1 20); do jq '.tcp_baseline = null' "$here/fixtures/direct.json" > "$nb/home-office/$i.json"; done
out="$("${py[@]}" "$script" "$nb" --expect home-office)"
has 'home-office: 20 direct run\(s\) without a usable tcp_baseline'
ok "null tcp_baseline is flagged"

out="$("${py[@]}" "$script" "$work/none" 2>&1 || true)"
has 'not a directory'
ok "bad results directory is an error"
echo "all summarize-matrix tests passed"
