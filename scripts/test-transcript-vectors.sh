#!/usr/bin/env bash
# Runs the shared transcript vectors (testdata/transcripts/) against both parsers:
# the Python adapters in proxy/dpx/agents and the Rust importers in server/src/history.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"

echo "== proxy (python3 -m unittest)"
(cd "$root/proxy" && python3 -m unittest tests.test_transcript_vectors -v)

echo "== server (cargo test history)"
(cd "$root/server" && cargo test --lib history)
