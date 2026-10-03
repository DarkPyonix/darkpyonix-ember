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

- **Coding, research and document sub-agents never run builds.** No `cargo build`, `test`,
  `check`, `clippy` or `run`, and no Chrome or `code serve-web` smoke runs. They write code and
  tests, research, and write documents, and their report says what was not compiled. (User rule,
  2026-10-03: seven sub-agents building at once drove the machine to load 189.)
- **Builds and tests go to one temporary builder sub-agent**, one build at a time with
  `CARGO_BUILD_JOBS=2`, each worktree in its own `target/`. The session does not build itself;
  without a builder, push and let CI build. Never share a `CARGO_TARGET_DIR` between worktrees.
  (User, 2026-10-03: "빌드 작업 니가 직접 하지 말고 서브 에이전트 하나 임시로 만들어서 개한테
  시켜야지", "니가 작업 붙잡고 있으면 다른 일들도 진행이 안되잖아".)
- Use sub-agents generously for parallel work, each in its own worktree under
  `.claude/worktrees/<name>/`.
- `crates/client/` depends on `crates/server/`, and `crates/server/` on `crates/node/`, by path,
  and CI runs `cargo test --locked` per crate. When a dependency changes in `crates/node/` or
  `crates/server/`, rebuild the dependants so their `Cargo.lock` files update, and commit them in
  the same PR.

## Where files go

Everything an agent makes stays inside this repository: worktrees in `.claude/worktrees/<name>/`,
throwaway work, probes and downloads in `.scratch/<name>/` (both ignored). Not `/tmp`, not a
directory beside this checkout, not the home directory. Large files in a worktree are linked, not
copied. If a task seems to need a path outside the repository, ask first.

**The repository root is fixed.** No new folder or file at the root without the user's approval:
propose what to add and why, and wait. Approved root entries (2026-10-03, regrouped by #60):
`.github/`, `.gitignore`, `.vscode/`, `AGENTS.md`, `CLAUDE.md`, `LICENSE`, `PROJECT.md`,
`README.md`, `build/`, `crates/`, `docs/`, `extensions/`, `scripts/`, `tests/`, `web/`, plus the
ignored `.claude/` and `.scratch/`. Rust crates live in `crates/<name>/`, the VS Code Web wrapper
in `web/proxy/`, the OSE build pipeline in `build/ose/`, and shared test vectors in
`tests/vectors/`.

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
- Remove merged branches regularly, without archive tags (user, 2026-10-03: "머지된거 전부
  정리하고, 아카이브는 왜 남겨?"). The one exception is `archive/pre-restructure`, which marks
  the tree before the root regrouping (#60).
- Doc-only changes may be committed on `develop` and pushed directly.
- Subject format `<Type>: <imperative summary>` with `Feat`, `Fix`, `Refactor`, `Docs`, `Test`,
  `Chore`. Reference SPEC IDs when relevant.
- **No `Co-Authored-By` trailer and no "Generated with" line** in commits or PR bodies.
- Do not commit `.DS_Store`, `.scratch/`, build outputs or local databases.

## Writing

The house style is thisisthepy/pythonx-compose `docs/style/writing.md` (develop) [user,
2026-10-03: "문서 말투는 pythonx-compose 쪽 기준으로"]. In short: present facts in the present
tense, and anything not built yet with its status and issue number; short declarative sentences;
a reason beside every rule; English and Korean carry the same content without translating word
for word; user documents in 합니다체, internal documents in 한다체.

- **No em-dash (U+2014)** in documents or code (comments, doc strings, strings), because the user
  ruled it out (2026-10-03). Use a comma, a colon or parentheses, or split the sentence. An en-dash
  in a numeric range is fine. `scripts/check-no-em-dash.sh` enforces it in CI; recorded data
  (`.json`, `.jsonl`) is exempt.
- **Install and run examples use uv, ppp (pypackpack) and tcl (toolchain-lite) only**, never
  `pip install`, because those are the toolchains the user supports (2026-10-03). Rust examples use
  cargo; Node examples stay as they are.

## Who to ask

Questions about project information or Ember's direction go to the darkpyonix leader session, not
the user. The user is asked only for what needs their own approval (force-push, permissions,
publishing) or what the leader cannot answer.

## Verification

A milestone is complete only when its SPEC acceptance criteria pass. Report failures with the
actual output; never mark an item done on assumption. Checks that need real hardware or a person
(a second computer, a real network, the screen) are reported as not yet verified.
