#!/usr/bin/env bash
# Publishes develop to the `release` branch, append-only, and opens or updates the release → main PR.
#
# main is protected and moves only through that PR, which the user merges. release never needs a
# force-push (user direction, 2026-10-04):
#   1. check out release, created from main the first time;
#   2. `git merge -s ours origin/main`, so main's history is an ancestor of release and the PR
#      never conflicts;
#   3. replace the tree with develop's tree and commit it;
#   4. drop the internal planning documents, which exist only on develop;
#   5. push without force;
#   6. create the PR, or retitle the existing one (its content follows the branch).
#
# Usage: .github/scripts/release/sync-release.sh [--dry-run]
# --dry-run builds the commit locally and prints what would be pushed, without pushing.
set -euo pipefail

dry_run=0
[[ "${1:-}" == "--dry-run" ]] && dry_run=1

# Internal documents: planning and agent instructions, not part of the public tree.
internal=(PROJECT.md AGENTS.md CLAUDE.md docs/INTENT.md docs/SPEC.md)

git fetch --quiet origin main develop
if git ls-remote --exit-code --heads origin release >/dev/null; then
    git fetch --quiet origin release
    git checkout --quiet -B release origin/release
else
    git checkout --quiet -B release origin/main
fi

if ! git merge-base --is-ancestor origin/main HEAD; then
    git merge --quiet -s ours --no-edit -m "Release: Record main as merged" origin/main
fi

develop_sha="$(git rev-parse --short origin/develop)"
# Take develop's tree exactly: files deleted on develop disappear here too.
git read-tree -u --reset origin/develop
for path in "${internal[@]}"; do
    git rm --quiet --cached --ignore-unmatch -- "$path"
    rm -f -- "$path"
done

if git diff --cached --quiet HEAD; then
    echo "release already matches develop ${develop_sha}"
else
    git commit --quiet -m "Release: Publish develop ${develop_sha}"
    echo "release: new commit $(git rev-parse --short HEAD) from develop ${develop_sha}"
fi

if (( dry_run )); then
    echo "dry run: not pushing; release tree:"
    git ls-tree --name-only HEAD
    exit 0
fi

git push --quiet origin release

# The same title in every repository (compose-rust, dioxus-compose).
title="Publish develop to main"
pr="$(gh pr list --base main --head release --state open --json number --jq '.[].number')"
if [[ -z "$pr" ]]; then
    gh pr create --base main --head release \
        --title "$title" \
        --body "Automated by .github/workflows/release-sync.yml. release follows develop without the internal planning documents (${internal[*]}). The user merges this PR."
else
    gh pr edit "$pr" --title "$title"
    echo "the release → main PR #$pr is open and now includes develop ${develop_sha}"
fi
