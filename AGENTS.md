# AGENTS.md

Working agreements for every agent (Claude Code, Codex, Antigravity, sub-agents) in this
repository. `CLAUDE.md` only imports this file.

## Project

`darkpyonix-ember` (repo `DarkPyonix/darkpyonix-ember`) is Ember: a multi-provider, LLM-based
development environment and remote IDE. `PROJECT.md` has scope and milestones; `docs/INTENT.md`
decisions; `docs/SPEC.md` requirements; `docs/ARCHITECTURE.md` processes; `docs/IMPLEMENTATION.md`
VS Code questions. Kernel, manager and hub APIs belong to `darkpyonix-core`; Ember links to them.

## Spec and tests first

1. An ID exists in `docs/SPEC.md` before the behaviour lands. A decision that changes scope goes
   into `docs/INTENT.md` first, with what it rejected.
2. Every decision in INTENT and SPEC is tagged **[user]** or **[provisional]**. A tag changes in its
   own commit when the user confirms or overturns it.
3. Failing test first, then code. A requirement with no test is not done.
4. A wrapped agent is integrated against its actual current behaviour (CLI flags, protocol,
   session files), verified by an integration test, never by assumption.
5. The client stays on `dioxus-compose`, with no webview in the launcher or conversation UI
   (`scripts/check-no-webview.sh`). [user, 2026-10-03]

## Sub-agents and builds

- **Sub-agents never run builds.** No `cargo build`, `test`, `check`, `clippy` or `run`, and no
  Chrome or `code serve-web` smoke runs. They write code and tests, research, and write documents,
  and their report says what was not compiled. (User rule, 2026-10-03: seven sub-agents building at
  once drove the machine to load 189.)
- The session builds and tests, one build at a time with `CARGO_BUILD_JOBS=2`, each worktree in its
  own `target/`, or pushes and lets CI build. Never share a `CARGO_TARGET_DIR` between worktrees.
- Use sub-agents generously for parallel work, each in its own worktree under
  `.claude/worktrees/<name>/`.
- `client/` depends on `server/`, and `server/` on `node/`, by path, and CI runs
  `cargo test --locked` per crate. When a dependency changes in `node/` or `server/`, rebuild the
  dependants so their `Cargo.lock` files update, and commit them in the same PR.

## Where files go

Everything an agent makes stays inside this repository: worktrees in `.claude/worktrees/<name>/`,
throwaway work, probes and downloads in `.scratch/<name>/` (both ignored). Not `/tmp`, not a
directory beside this checkout, not the home directory. Large files in a worktree are linked, not
copied. If a task seems to need a path outside the repository, ask first.

**The repository root is fixed.** No new folder or file at the root without the user's approval:
propose what to add and why, and wait. Approved root entries (2026-10-03): `.github/`,
`.gitignore`, `.vscode/`, `AGENTS.md`, `CLAUDE.md`, `LICENSE`, `PROJECT.md`, `README.md`, `app/`,
`bridge/`, `client/`, `docs/`, `editor/`, `editor-conn/`, `extensions/`, `hub/`, `node/`, `ose/`,
`proxy/`, `scripts/`, `server/`, `testdata/`, `transport/`, plus the ignored `.claude/` and
`.scratch/`. A regrouping of these is proposed in #60 and waits for approval.

## Git

- Branches: `develop` (integration, where work lands) and `main` (protected, default). Only
  `main`, `develop` and `release` live on the remote permanently.
- Work branches are named `feat/<topic>` (existing `feature/*` branches keep their names until
  merged).
- **Push right after every commit.** Never push to `main` directly. Force-push only with the
  user's confirmation.
- New features: search issues first
  (`gh issue list -R DarkPyonix/darkpyonix-ember --state all --search "<keyword>"`). If none
  fits, write one with completion criteria. Commit on a feature branch off `develop`, push, and
  open a PR into `develop` with `Closes #<N>`. Merge only through that PR. `develop` is not the
  default branch, so close the issue by hand after merging:
  `gh issue close <N> --comment "Landed via #<PR>"`.
- **Merge gate:** wait for every check to finish and merge only if none failed. Merge with
  `gh pr merge --delete-branch`, then remove the local branch and its worktree. Delete a branch
  only after `gh pr view --json state` says `MERGED`.
- Remove merged branches regularly. A branch whose history must be kept gets an
  `archive/<name>` tag first, then is deleted.
- Doc-only changes may be committed on `develop` and pushed directly.
- Subject format `<Type>: <imperative summary>` with `Feat`, `Fix`, `Refactor`, `Docs`, `Test`,
  `Chore`. Reference SPEC IDs when relevant.
- **No `Co-Authored-By` trailer and no "Generated with" line** in commits or PR bodies.
- Do not commit `.DS_Store`, `.scratch/`, build outputs or local databases.

## Who to ask

Questions about project information or Ember's direction go to the darkpyonix leader session, not
the user. The user is asked only for what needs their own approval (force-push, permissions,
publishing) or what the leader cannot answer.

## Verification

A milestone is complete only when its SPEC acceptance criteria pass. Report failures with the
actual output; never mark an item done on assumption. Checks that need real hardware or a person
(a second computer, a real network, the screen) are reported as not yet verified.
