#!/usr/bin/env bash
# Runs the FR-T7 team runner against real agents (#82, docs/design/POST-1-0-REAL-TEAMS.md 4.2).
#
#   EMBER_E2E_TEAM_LEAD_ACCOUNT=<id> EMBER_E2E_TEAM_MATE_ACCOUNT=<id> ember/server/scripts/team-run.sh
#
# The two accounts must already be logged in (Ember does not log in for you, FR-U1); a missing
# one ends the run as skipped. EMBER_E2E_TEAM_COMPUTER=<computer id> makes it a two-computer run
# (T2), else it is T1 on this machine. EMBER_E2E_TEAM_DIR is a project path that exists on both
# computers; EMBER_E2E_TEAM_OUT is where evidence.json goes.
#
# Status: the runner skeleton (V1) judges a scripted team in CI
# (fr_t7_team_runner_judges_a_scripted_team); the real-agent run is not built yet (V2), so this
# prints what it would run and reports skipped.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export EMBER_E2E_TEAM=1
exec cargo test --locked --manifest-path "$here/../Cargo.toml" --test live_team -- --ignored --nocapture fr_t7_live_team
