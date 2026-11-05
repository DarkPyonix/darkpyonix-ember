#!/usr/bin/env bash
# NFR-N1: run one network pair from the dialing machine. Repeat for each pair.
# Start the listener on the other machine and save its JSON line to an address file.
# Usage: run-matrix.sh PAIR ADDRESS_FILE OUTPUT_DIR [dial options...]
# Set BENCH_BIN to an already-built ember-transport-bench executable.
set -uo pipefail

if (( $# < 3 )); then
    echo "usage: $0 PAIR ADDRESS_FILE OUTPUT_DIR [dial options...]" >&2
    exit 2
fi
pair=$1
address_file=$2
output_dir=$3
shift 3
if [[ ! $pair =~ ^[a-zA-Z0-9_-]+$ ]]; then
    echo "PAIR must contain only letters, digits, underscores, or hyphens" >&2
    exit 2
fi
bench_bin=${BENCH_BIN:-ember-transport-bench}
if [[ ! -r $address_file ]]; then
    echo "cannot read listener address: $address_file" >&2
    exit 2
fi
address=$(cat "$address_file")
if [[ -z $address ]]; then
    echo "listener address is empty" >&2
    exit 2
fi
# Refuse to overwrite previous results, including failed attempts.
results="$output_dir/$pair"
if ! mkdir -p "$output_dir" || ! mkdir "$results"; then
    echo "cannot create new results directory: $results" >&2
    exit 2
fi
printf '%s\n' "$address" > "$results/listener.json"
failures=0
for run in $(seq -w 1 20); do
    if "$bench_bin" dial "$address" "$@" > "$results/$run.json" 2> "$results/$run.stderr"; then
        printf 'run %s: success\n' "$run"
    else
        status=$?
        printf 'run %s: failed (exit %s)\n' "$run" "$status" >&2
        printf '%s\n' "$status" > "$results/$run.exit"
        failures=$((failures + 1))
    fi
done
printf '%s\n' "$failures" > "$results/failures.txt"
(( failures == 0 ))
