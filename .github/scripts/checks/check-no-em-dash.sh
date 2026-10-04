#!/usr/bin/env bash
# Fails if an em-dash (U+2014) appears in a human-written file.
#
# AGENTS.md forbids em-dashes in docs, comments, doc strings and strings (user direction,
# 2026-10-03). Branches written before the rule keep bringing them back through merges, so the
# rule is checked rather than remembered. Recorded data is excluded: .json/.jsonl (the transcript
# vectors use the character on purpose to test Unicode handling) and test fixtures, which hold
# output captured from real CLIs and must stay as the tool printed it.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../../.."

# An en-dash (U+2013) in a numeric range is allowed.
dash=$'—'
hits="$(git grep -n --fixed-strings -- "$dash" -- \
    '*.md' '*.rs' '*.py' '*.js' '*.ts' '*.html' '*.css' '*.sh' '*.yml' '*.yaml' '*.toml' '*.txt' \
    ':!.github/scripts/checks/check-no-em-dash.sh' ':!**/fixtures/**' || true)"

if [[ -n "$hits" ]]; then
    echo "error: em-dashes found (AGENTS.md forbids them; use a comma, a colon or parentheses, or split the sentence)" >&2
    echo "$hits" >&2
    exit 1
fi
echo "ok: no em-dashes"
