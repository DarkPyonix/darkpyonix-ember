#!/usr/bin/env bash
# Tests .github/scripts/release/sync-release.sh against a throwaway origin.
#
# main must carry develop's commits with their original authors (user decision, 2026-10-04), so
# the release commit records develop as a parent. Checked here: every develop commit is an
# ancestor of release, main is an ancestor of release, the internal documents are absent, a
# second run is a no-op, and a main-side merge commit does not break the next sync.
set -euo pipefail

script="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)/.github/scripts/release/sync-release.sh"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

fail() { echo "FAIL: $*" >&2; exit 1; }
ok() { echo "ok    $*"; }

export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1
export SYNC_RELEASE_NO_PR=1   # no GitHub in this test

git init -q --bare -b main "$work/origin.git"
git clone -q "$work/origin.git" "$work/seed" 2>/dev/null
cd "$work/seed"
git config user.name seed && git config user.email seed@example.com
commit_as() { # name file
    echo "$2 by $1" > "$2"
    git add "$2"
    GIT_AUTHOR_NAME="$1" GIT_AUTHOR_EMAIL="$1@example.com" git commit -q -m "Add $2"
}
commit_as alice README.md
git push -q origin main
git checkout -q -b develop
mkdir -p docs/guide ember/x
for f in PROJECT.md AGENTS.md CLAUDE.md docs/INTENT.md docs/SPEC.md docs/guide/index.html ember/x/lib.rs; do
    commit_as bob "$f"
done
commit_as carol docs/ARCHITECTURE.md
git push -q origin develop

run_sync() {
    rm -rf "$work/ci"
    git clone -q "$work/origin.git" "$work/ci" 2>/dev/null
    (cd "$work/ci" && git config user.name bot && git config user.email bot@example.com && bash "$script")
}
check_release() {
    git -C "$work/seed" fetch -q origin
    local rel=origin/release
    for c in $(git -C "$work/seed" rev-list origin/develop); do
        git -C "$work/seed" merge-base --is-ancestor "$c" "$rel" || fail "develop commit $c is not in release"
    done
    ok "every develop commit is an ancestor of release"
    git -C "$work/seed" merge-base --is-ancestor origin/main "$rel" || fail "main is not an ancestor of release"
    ok "main is an ancestor of release"
    for f in PROJECT.md AGENTS.md CLAUDE.md docs/INTENT.md docs/SPEC.md; do
        ! git -C "$work/seed" cat-file -e "$rel:$f" 2>/dev/null || fail "$f is published"
    done
    for f in docs/guide/index.html docs/ARCHITECTURE.md ember/x/lib.rs README.md; do
        git -C "$work/seed" cat-file -e "$rel:$f" 2>/dev/null || fail "$f is missing"
    done
    ok "internal documents absent, the rest present"
    authors="$(git -C "$work/seed" log --format=%an "$rel")"; grep -qx carol <<<"$authors" || fail "carol's commit is not in release's history"
    ok "develop authors are in release's history"
}

run_sync
check_release
first="$(git -C "$work/seed" rev-parse origin/release)"

run_sync
git -C "$work/seed" fetch -q origin
[[ "$(git -C "$work/seed" rev-parse origin/release)" == "$first" ]] || fail "second run made a commit"
ok "second run is a no-op"

# The user merges release into main with a merge commit; develop moves on.
cd "$work/seed"
git checkout -q main && git merge -q --no-ff --no-edit origin/release && git push -q origin main
git checkout -q develop && commit_as dave ember/x/more.rs && git push -q origin develop
run_sync
check_release
git -C "$work/seed" merge-base --is-ancestor "$first" origin/release || fail "release was rewritten"
ok "release is append-only after a main merge"
echo "all sync-release tests passed"
