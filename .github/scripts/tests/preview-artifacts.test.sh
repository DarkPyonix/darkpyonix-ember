#!/usr/bin/env bash
# Exercise the macOS preview pipeline with stub capture tools and no app build.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
scratch="${RUNNER_TEMP:-$root/.scratch}"
mkdir -p "$scratch"
work="$(mktemp -d "$scratch/preview-artifacts.XXXXXX")"
trap 'rm -r "$work"' EXIT
mkdir "$work/bin"

cat > "$work/bin/stub" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
case "${0##*/}" in
    ember-app)
        echo 'first frame drawn'
        exec sleep 20
        ;;
    osascript) echo 1 ;;
    screencapture|sips) echo 'stub image' > "${!#}" ;;
    uv)
        # The pipeline supplies output paths in the render and join TSV files.
        mode="$7"
        [[ "$mode" == render || "$mode" == join ]]
        while [[ "$1" != --jobs ]]; do shift; done
        while IFS=$'\t' read -r first second third rest; do
            if [[ "$mode" == render ]]; then
                echo 'stub mockup' > "$first"
            else
                echo 'stub pair' > "$third"
            fi
        done < "$2"
        if [[ "$mode" == join && "${PREVIEW_TEST_JOIN_FAIL:-0}" == 1 ]]; then
            echo 'stub join failure' >&2
            exit 1
        fi
        ;;
    *) exit 2 ;;
esac
EOF
chmod +x "$work/bin/stub"
for tool in ember-app osascript screencapture sips uv; do
    ln -s stub "$work/bin/$tool"
done

run_preview() {
    PATH="$work/bin:$PATH" MOCKUP_DIR="$root/docs/design/mockup" EMBER_APP_BIN="$work/bin/ember-app" \
        OUT_DIR="$1" PREVIEW_TEST_JOIN_FAIL="$2" SETTLE_SECS=0 WINDOW_TIMEOUT=2 \
        bash "$root/.github/scripts/preview/side-by-side.sh" --only files
}

assert_matrix() {
    local output="$1" size theme name
    for size in 1440x900 1024x800 768x900 390x844; do
        for theme in light dark; do
            name="files-$size-$theme-pair.png"
            [[ -s "$output/pairs/$name" ]]
            grep -Fq "[$name](pairs/$name)" "$output/index.md"
        done
    done
    [[ "$(find "$output/pairs" -name '*.png' | wc -l | tr -d ' ')" == 8 ]]
}

run_preview "$work/complete" 0 > "$work/complete.log" 2>&1 || { cat "$work/complete.log" >&2; exit 1; }
assert_matrix "$work/complete"
echo 'ok: all four sizes in both themes are linked from the artifact index'

status=0
run_preview "$work/failed" 1 > "$work/failed.log" 2>&1 || status=$?
assert_matrix "$work/failed"
grep -Fq 'stub join failure' "$work/failed/logs/join.log"
if [[ "$status" != 1 ]]; then
    echo "expected a failed join to exit 1, got $status" >&2
    exit 1
fi
echo 'ok: a failed join still writes the index and logs and exits 1'
