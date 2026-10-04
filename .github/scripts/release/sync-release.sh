#!/usr/bin/env bash
# Publishes develop to the `release` branch, append-only, and opens or retitles the release to main
# PR. main is protected and moves only through that PR, which the user merges.
#
# Each release commit records these parents, in order:
#   1. the previous release commit (absent the first time);
#   2. main, when it is not already an ancestor, the way `git merge -s ours main` records it, so
#      the PR never conflicts after the user merged the last one;
#   3. develop, so main's history carries develop's commits with their original authors (user
#      decision, 2026-10-04).
# Its tree is develop's tree without the internal planning documents.
#
# The commit is built with plumbing (a temporary index, write-tree, commit-tree): nothing is
# checked out, so the working tree and this script's own file never change while it runs. It
# pushes without force and creates no commit when release already has develop's tree, main and
# develop in its history.
#
# Environment: SYNC_RELEASE_NO_PR=1 skips the GitHub PR step (used by scripts/tests).
set -euo pipefail

# Internal documents: planning and agent instructions, not part of the public tree.
internal=(PROJECT.md AGENTS.md CLAUDE.md docs/INTENT.md docs/SPEC.md)
title="Publish develop to main"   # the same title in compose-rust and dioxus-compose

git fetch --quiet origin main develop
prev=""
if git ls-remote --exit-code --heads origin release >/dev/null; then
    git fetch --quiet origin release
    prev="$(git rev-parse origin/release)"
fi
main="$(git rev-parse origin/main)"
develop="$(git rev-parse origin/develop)"

index="$(mktemp)"
trap 'rm -f "$index"' EXIT
rm -f "$index"
GIT_INDEX_FILE="$index" git read-tree "$develop"
GIT_INDEX_FILE="$index" git rm --quiet --cached --ignore-unmatch -- "${internal[@]}"
tree="$(GIT_INDEX_FILE="$index" git write-tree)"

need_main=1
need_develop=1
if [[ -n "$prev" ]]; then
    git merge-base --is-ancestor "$main" "$prev" && need_main=0
    git merge-base --is-ancestor "$develop" "$prev" && need_develop=0
fi

if [[ -n "$prev" && "$tree" == "$(git rev-parse "$prev^{tree}")" ]] && (( !need_main && !need_develop )); then
    echo "release already has develop $(git rev-parse --short "$develop")"
else
    parents=()
    [[ -n "$prev" ]] && parents+=(-p "$prev")
    (( need_main )) && parents+=(-p "$main")
    parents+=(-p "$develop")
    commit="$(git commit-tree "$tree" "${parents[@]}" -m "Release: Publish develop $(git rev-parse --short "$develop")")"
    git push --quiet origin "$commit:refs/heads/release"
    echo "release: new commit $(git rev-parse --short "$commit") from develop $(git rev-parse --short "$develop")"
fi

[[ "${SYNC_RELEASE_NO_PR:-}" == 1 ]] && exit 0

pr="$(gh pr list --base main --head release --state open --json number --jq '.[].number')"
if [[ -z "$pr" ]]; then
    gh pr create --base main --head release --title "$title" \
        --body "Automated by .github/workflows/release-sync.yml. release follows develop, keeps develop's commits in its history, and leaves out the internal planning documents (${internal[*]}). The user merges this PR."
else
    gh pr edit "$pr" --title "$title"
    echo "the release to main PR #$pr is open and now includes develop $(git rev-parse --short "$develop")"
fi
