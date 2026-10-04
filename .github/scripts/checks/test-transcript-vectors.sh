#!/usr/bin/env bash
# Runs the shared transcript vectors (ember/vectors/transcripts/) against both parsers:
# the Python adapters in ember/proxy/dpx/agents and the Rust importers in ember/server/src/history.
set -euo pipefail
root="$(cd "$(dirname "$0")/../../.." && pwd)"

echo "== ember/proxy (python3 -m unittest)"
(cd "$root/ember/proxy" && python3 -m unittest tests.test_transcript_vectors -v)

echo "== ember/server (cargo test history)"
(cd "$root/ember/server" && cargo test --lib history)
