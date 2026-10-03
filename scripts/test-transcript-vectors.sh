#!/usr/bin/env bash
# Runs the shared transcript vectors (tests/vectors/transcripts/) against both parsers:
# the Python adapters in web/proxy/dpx/agents and the Rust importers in crates/server/src/history.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"

echo "== web/proxy (python3 -m unittest)"
(cd "$root/web/proxy" && python3 -m unittest tests.test_transcript_vectors -v)

echo "== crates/server (cargo test history)"
(cd "$root/crates/server" && cargo test --lib history)
