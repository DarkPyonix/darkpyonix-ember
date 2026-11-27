#!/usr/bin/env bash
# Captures every client screen next to its mockup render and writes one joined image per
# screen, size and theme (FR-L37, #222). macOS only for now: the capture uses screencapture,
# CoreGraphics window ids and sips. On any other OS it stops with an error.
#
# usage: [MOCKUP_DIR=<mockup folder>] .github/scripts/preview/side-by-side.sh [options]
#   --only <screen>[,<screen>]   only these rows of screens.tsv (may be repeated)
#   --size <WxH|W>[,...]         only these sizes (default: 1440x900 1024x800 768x900 390x844)
#   --theme <light|dark>[,...]   only these themes (default: light dark)
#   --list                       print the screen names and exit
#
# Environment:
#   MOCKUP_DIR     the mockup folder (home.html, vcs.html, ...): docs/design/mockup in this
#                  repository, as the CI job "Screen previews" passes it (#222). Without it the
#                  run writes the app captures only (no mockup renders, no pairs).
#   EMBER_APP_BIN  the client binary (default: ember/app/target/debug/ember-app). A debug build,
#                  because the EMBER_CR_* review hooks exist in debug builds only.
#   OUT_DIR        output folder (default: .scratch/preview).
#   CHROME_BIN     Chrome or Chromium for the mockup (default: Playwright's cached Chromium,
#                  then Google Chrome).
#   SETTLE_SECS    wait after the first frame before the capture (default 1.8).
#   WINDOW_TIMEOUT seconds to wait for the app's first frame and window (default 20).
#   PAIR_SCALE     pixels per point in the pair image and the mockup render (default 2).
#   KEEP_DATA=1    keep the run's EMBER_DATA_DIR folders (default: removed at the end).
#
# Output: $OUT_DIR/pairs/<screen>-<size>-<theme>-pair.png (app left, mockup right),
# $OUT_DIR/app/ (captures converted to sRGB), $OUT_DIR/raw/ (captures as taken),
# $OUT_DIR/mockup/, $OUT_DIR/logs/ and $OUT_DIR/index.md.
#
# Rules it keeps: the window is found by the window id of the app's own pid, never by
# front-most; no key or click is sent to any application; only the processes this script
# started are stopped, by pid.
#
# Exit status: 0 every pair written; 1 a pair is missing (mockup render or join failed);
# 2 wrong usage or a missing tool; 3 an app window was not found or not captured.
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../../.." && pwd)"
TABLE="$HERE/screens.tsv"
HELPER="$HERE/side_by_side.py"
WINID="$HERE/window-id.js"
SRGB="/System/Library/ColorSync/Profiles/sRGB Profile.icc"

ALL_SIZES="1440x900 1024x800 768x900 390x844"
ALL_THEMES="light dark"

die() { echo "error: $*" >&2; exit 2; }
usage() { sed -n '2,35p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; }

only="" sizes="" themes="" list=0
while [[ $# -gt 0 ]]; do
    case "$1" in
        --only) [[ $# -ge 2 ]] || die "--only needs a screen name"; only="$only ${2//,/ }"; shift 2 ;;
        --size) [[ $# -ge 2 ]] || die "--size needs a size"; sizes="$sizes ${2//,/ }"; shift 2 ;;
        --theme) [[ $# -ge 2 ]] || die "--theme needs light or dark"; themes="$themes ${2//,/ }"; shift 2 ;;
        --list) list=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) die "unknown argument: $1 (see --help)" ;;
    esac
done

[[ -f "$TABLE" ]] || die "screen table not found: $TABLE"

# The table's screen names, in order.
names=""
while IFS=$'\t' read -r n _rest; do
    case "$n" in ''|'#'*) continue ;; esac
    names="$names $n"
done < "$TABLE"

if [[ $list -eq 1 ]]; then
    for n in $names; do echo "$n"; done
    exit 0
fi

[[ "$(uname -s)" == "Darwin" ]] || die "side-by-side.sh runs on macOS only for now (this is $(uname -s))"

for n in $only; do
    case " $names " in *" $n "*) ;; *) die "--only $n: no such screen (see --list)" ;; esac
done

# --size takes WxH or the width alone.
picked_sizes=""
for s in $sizes; do
    hit=""
    for a in $ALL_SIZES; do
        if [[ "$s" == "$a" || "$s" == "${a%x*}" ]]; then hit="$a"; fi
    done
    [[ -n "$hit" ]] || die "--size $s: not one of $ALL_SIZES"
    picked_sizes="$picked_sizes $hit"
done
[[ -n "$picked_sizes" ]] || picked_sizes="$ALL_SIZES"

picked_themes=""
for t in $themes; do
    case "$t" in light|dark) picked_themes="$picked_themes $t" ;; *) die "--theme $t: light or dark" ;; esac
done
[[ -n "$picked_themes" ]] || picked_themes="$ALL_THEMES"

# Without MOCKUP_DIR only the app is captured (#222): nothing is rendered or joined, so uv
# (the Python helpers) is not needed either.
APP_ONLY=0
if [[ -z "${MOCKUP_DIR:-}" ]]; then
    APP_ONLY=1
    echo "mockup: MOCKUP_DIR is not set, so this run writes app captures only (no pairs)"
else
    [[ -d "$MOCKUP_DIR" ]] || die "MOCKUP_DIR is not a folder: $MOCKUP_DIR"
    MOCKUP_DIR="$(cd "$MOCKUP_DIR" && pwd)"
fi
APP_BIN="${EMBER_APP_BIN:-$ROOT/ember/app/target/debug/ember-app}"
[[ -x "$APP_BIN" ]] || die "app binary not found or not executable: $APP_BIN (set EMBER_APP_BIN)"
tools="osascript screencapture sips"
[[ $APP_ONLY -eq 1 ]] || tools="$tools uv"
for tool in $tools; do
    command -v "$tool" >/dev/null 2>&1 || die "$tool is not on PATH"
done
[[ -f "$SRGB" ]] || die "sRGB profile not found: $SRGB"

OUT_DIR="${OUT_DIR:-$ROOT/.scratch/preview}"
mkdir -p "$OUT_DIR" || die "cannot create OUT_DIR: $OUT_DIR"
OUT_DIR="$(cd "$OUT_DIR" && pwd)"
SETTLE_SECS="${SETTLE_SECS:-1.8}"
WINDOW_TIMEOUT="${WINDOW_TIMEOUT:-20}"
PAIR_SCALE="${PAIR_SCALE:-2}"

RUN_ID="run-$(date +%Y%m%d-%H%M%S)-$$"
DATA_ROOT="$OUT_DIR/data/$RUN_ID"   # fresh per run, one folder per capture below it
mkdir -p "$OUT_DIR/pairs" "$OUT_DIR/app" "$OUT_DIR/raw" "$OUT_DIR/mockup" "$OUT_DIR/logs" "$DATA_ROOT"
MOCK_JOBS="$DATA_ROOT/mock-jobs.tsv"
JOIN_JOBS="$DATA_ROOT/join-jobs.tsv"
ROWS="$DATA_ROOT/rows.tsv"
: > "$MOCK_JOBS"; : > "$JOIN_JOBS"; : > "$ROWS"

APP_PID=""
HELPER_PID=""

# Stops one process this script started, by pid, and waits for it.
stop_pid() {
    local pid="$1" i
    [[ -n "$pid" ]] || return 0
    kill "$pid" 2>/dev/null || true
    for i in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15; do
        kill -0 "$pid" 2>/dev/null || break
        sleep 0.2
    done
    if kill -0 "$pid" 2>/dev/null; then kill -9 "$pid" 2>/dev/null || true; fi
    wait "$pid" 2>/dev/null || true
}

cleanup() {
    stop_pid "$APP_PID"; APP_PID=""
    stop_pid "$HELPER_PID"; HELPER_PID=""
    if [[ "${KEEP_DATA:-0}" != "1" && -n "${RUN_ID:-}" && -d "$OUT_DIR/data/$RUN_ID" ]]; then
        if [[ -f "$ROWS" ]]; then cp "$ROWS" "$OUT_DIR/logs/rows-$RUN_ID.tsv" 2>/dev/null || true; fi
        rm -rf "$OUT_DIR/data/$RUN_ID"
        rmdir "$OUT_DIR/data" 2>/dev/null || true
    fi
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# Runs the Python helper through uv in the background, so the trap can stop it by pid.
run_helper() {
    local with="$1" log="$2"; shift 2
    uv run --no-project --with "$with" python3 "$HELPER" "$@" > "$log" 2>&1 < /dev/null &
    HELPER_PID=$!
    local rc=0
    wait "$HELPER_PID" || rc=$?
    HELPER_PID=""
    return "$rc"
}

wanted() { [[ -z "$only" ]] && return 0; case " $only " in *" $1 "*) return 0 ;; esac; return 1; }

# Review hooks left in the caller's environment must not leak into the captures, and the app
# must not reach a real server.
for v in $(env | sed -n 's/^\(EMBER_CR_[A-Z_]*\)=.*/\1/p'); do unset "$v"; done
unset EMBER_SERVER_URL

# 1. Mockup renders: one browser for all of them.
mock_rc=0
while IFS=$'\t' read -r name route envs murl mstore mact <&3; do
    case "$name" in ''|'#'*) continue ;; esac
    wanted "$name" || continue
    [[ "$murl" == "-" || $APP_ONLY -eq 1 ]] && continue
    for size in $picked_sizes; do
        for theme in $picked_themes; do
            printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$OUT_DIR/mockup/$name-$size-$theme.png" \
                "${size%x*}" "${size#*x}" "$theme" "$murl" "$mstore" "$mact" >> "$MOCK_JOBS"
            rm -f "$OUT_DIR/mockup/$name-$size-$theme.png"
        done
    done
done 3< "$TABLE"

if [[ -s "$MOCK_JOBS" ]]; then
    echo "mockup: rendering $(wc -l < "$MOCK_JOBS" | tr -d ' ') pages from $MOCKUP_DIR"
    run_helper playwright "$OUT_DIR/logs/mockup-render.log" render \
        --mockup-dir "$MOCKUP_DIR" --jobs "$MOCK_JOBS" --scale "$PAIR_SCALE" || mock_rc=$?
    if [[ $mock_rc -ne 0 ]]; then
        echo "mockup: render reported failures (status $mock_rc), see $OUT_DIR/logs/mockup-render.log" >&2
        grep -E '^(FAIL|error)' "$OUT_DIR/logs/mockup-render.log" >&2 || tail -n 5 "$OUT_DIR/logs/mockup-render.log" >&2
    fi
fi

# 2. App captures: one launch per screen, size and theme, each with its own data folder.
no_window=0
capture() {
    local name="$1" size="$2" theme="$3" route="$4" envs="$5"
    local base="$name-$size-$theme"
    local log="$OUT_DIR/logs/$base.log" data="$DATA_ROOT/$base"
    local raw="$OUT_DIR/raw/$base.png" app="$OUT_DIR/app/$base.png"
    local wid="" i tries
    rm -f "$raw" "$app" "$OUT_DIR/pairs/$base-pair.png"
    mkdir -p "$data"
    [[ "$envs" == "-" ]] && envs=""
    # $envs is split on purpose: it holds NAME=value words from the table.
    # shellcheck disable=SC2086
    env EMBER_DEMO_DATA=1 EMBER_DATA_DIR="$data" EMBER_CR_SHELL="$theme" EMBER_CR_SIZE="$size" \
        EMBER_CR_ROUTE="$route" $envs "$APP_BIN" > "$log" 2>&1 < /dev/null &
    APP_PID=$!
    tries=$(( WINDOW_TIMEOUT * 5 ))
    STATUS="no-window"
    for (( i = 0; i < tries; i++ )); do
        if ! kill -0 "$APP_PID" 2>/dev/null; then STATUS="app-exited"; break; fi
        # compose-rust logs this line when the first frame is on screen.
        if grep -q "first frame drawn" "$log" 2>/dev/null; then STATUS="drawn"; break; fi
        sleep 0.2
    done
    if [[ "$STATUS" != "app-exited" ]]; then
        sleep "$SETTLE_SECS"
        wid="$(osascript -l JavaScript "$WINID" "$APP_PID" 2>/dev/null || true)"
        if [[ -z "$wid" ]]; then
            STATUS="no-window"
        elif ! screencapture -x -o -l"$wid" "$raw" 2>> "$log" || [[ ! -s "$raw" ]]; then
            STATUS="capture-failed"
        # Captures carry the display profile: compare colours only on the sRGB copy.
        elif ! sips -m "$SRGB" "$raw" --out "$app" >> "$log" 2>&1 || [[ ! -s "$app" ]]; then
            rm -f "$app"; STATUS="srgb-failed"
        else
            STATUS="ok"
        fi
    fi
    stop_pid "$APP_PID"; APP_PID=""
    [[ "${KEEP_DATA:-0}" == "1" ]] || rm -rf "$data"
}

while IFS=$'\t' read -r name route envs murl mstore mact <&3; do
    case "$name" in ''|'#'*) continue ;; esac
    wanted "$name" || continue
    for size in $picked_sizes; do
        for theme in $picked_themes; do
            base="$name-$size-$theme"
            capture "$name" "$size" "$theme" "$route" "$envs"
            if [[ "$STATUS" != "ok" ]]; then
                no_window=$(( no_window + 1 ))
                echo "FAIL $base app: $STATUS (log: $OUT_DIR/logs/$base.log)" >&2
            else
                echo "OK   $base app"
            fi
            mock="-"
            if [[ "$murl" != "-" && $APP_ONLY -eq 1 ]]; then
                mock="no-mockup-dir"
            elif [[ "$murl" != "-" ]]; then
                mock="missing"
                if [[ -s "$OUT_DIR/mockup/$base.png" ]]; then
                    mock="ok"
                    if [[ "$STATUS" == "ok" ]]; then
                        printf '%s\t%s\t%s\t%s\n' "$OUT_DIR/app/$base.png" "$OUT_DIR/mockup/$base.png" \
                            "$OUT_DIR/pairs/$base-pair.png" "${size%x*}" >> "$JOIN_JOBS"
                    fi
                fi
            fi
            printf '%s\t%s\t%s\t%s\t%s\n' "$name" "$size" "$theme" "$STATUS" "$mock" >> "$ROWS"
        done
    done
done 3< "$TABLE"

# 3. Join: app on the left, mockup on the right.
join_rc=0
if [[ -s "$JOIN_JOBS" ]]; then
    run_helper pillow "$OUT_DIR/logs/join.log" join --jobs "$JOIN_JOBS" --scale "$PAIR_SCALE" \
        || join_rc=$?
    if [[ $join_rc -ne 0 ]]; then
        echo "join: reported failures (status $join_rc), see $OUT_DIR/logs/join.log" >&2
    fi
fi

# 4. index.md
pairs=0 missing=0 app_only=0
INDEX="$OUT_DIR/index.md"
{
    echo "# Preview pairs"
    echo
    echo "Written by \`.github/scripts/preview/side-by-side.sh\` (FR-L37) on $(date '+%Y-%m-%d %H:%M:%S')."
    if [[ $APP_ONLY -eq 1 ]]; then
        echo "App captures only (\`$APP_BIN\`), converted to sRGB: MOCKUP_DIR was not set, so there are no pairs."
    else
        echo "Left: the app (\`$APP_BIN\`), converted to sRGB. Right: the mockup (\`$MOCKUP_DIR\`)."
    fi
    echo "Pair images hold $PAIR_SCALE px per point. Logs are in \`logs/\`."
    echo
    echo "| Screen | Size | Theme | Pair | App | Mockup |"
    echo "|---|---|---|---|---|---|"
    while IFS=$'\t' read -r name size theme app mock; do
        base="$name-$size-$theme"
        if [[ -s "$OUT_DIR/pairs/$base-pair.png" ]]; then
            cell="[$base-pair.png](pairs/$base-pair.png)"
        elif [[ "$mock" == "-" ]]; then
            cell="none (no mockup state in the table)"
        elif [[ "$mock" == "no-mockup-dir" ]]; then
            cell="none (MOCKUP_DIR not set)"
        elif [[ "$app" != "ok" ]]; then
            cell="MISSING (app: $app)"
        elif [[ "$mock" != "ok" ]]; then
            cell="MISSING (mockup not rendered)"
        else
            cell="MISSING (join failed)"
        fi
        app_cell="$app"; [[ "$app" == "ok" ]] && app_cell="[app](app/$base.png)"
        mock_cell="$mock"; [[ "$mock" == "ok" ]] && mock_cell="[mockup](mockup/$base.png)"
        echo "| $name | $size | $theme | $cell | $app_cell | $mock_cell |"
    done < "$ROWS"
} > "$INDEX"

while IFS=$'\t' read -r name size theme app mock; do
    if [[ -s "$OUT_DIR/pairs/$name-$size-$theme-pair.png" ]]; then
        pairs=$(( pairs + 1 ))
    elif [[ "$mock" == "-" || "$mock" == "no-mockup-dir" ]]; then
        app_only=$(( app_only + 1 ))
    else
        missing=$(( missing + 1 ))
    fi
done < "$ROWS"
{
    echo
    echo "Pairs: $pairs. Missing pairs: $missing. App only: $app_only. App captures that failed: $no_window."
} >> "$INDEX"

echo "pairs: $pairs, missing: $missing, app only: $app_only, app captures failed: $no_window"
echo "index: $INDEX"
if [[ $no_window -gt 0 ]]; then exit 3; fi
if [[ $missing -gt 0 || $mock_rc -ne 0 || $join_rc -ne 0 ]]; then exit 1; fi
exit 0
