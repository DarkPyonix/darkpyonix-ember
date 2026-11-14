#!/usr/bin/env bash
# Packs the client app with Velopack for one OS and architecture (SPEC NFR-R1, NFR-R2, #880),
# through compose-rust's scripts/package-velopack.py, and copies what the release needs into
# OUT_DIR: Setup.exe (Windows), the .dmg and portable .zip (macOS), the .AppImage (Linux), every
# .nupkg of the channel and releases.<channel>.json.
#
# compose-rust's script is still being revised; its call is kept here, in one place, with the
# inputs at the top. Run from the repository root by the `client` job of publish-release.yml.
set -euo pipefail

# ---- inputs (environment) -------------------------------------------------------------------
COMPOSE_RUST_DIR="${COMPOSE_RUST_DIR:-compose-rust}"          # the compose-rust checkout
PROJECT_DIR="${PROJECT_DIR:-ember/app}"                       # Cargo.toml and Dioxus.toml (app id dev.darkpyonix.ember)
EXECUTABLE="${EXECUTABLE:?EXECUTABLE: the built ember-app binary}"
PACK_VERSION="${PACK_VERSION:?PACK_VERSION: the tag without the v}"
PACK_TITLE="${PACK_TITLE:-Ember}"
# One channel per OS and architecture: Velopack's defaults (osx, win, linux) collide when two
# architectures of one OS share a release. The installed app checks the channel it was packed with.
CHANNEL="${CHANNEL:?CHANNEL: osx-arm64, win-x64, linux-x64 or linux-arm64}"
ICON="${ICON:-}"                                              # optional; vpk uses its default icon without one
OUT_DIR="${OUT_DIR:-dist}"                                    # what the release uploads
WORK_DIR="${WORK_DIR:-velopack}"                              # previous release and vpk output
REPO="${REPO:-${GITHUB_REPOSITORY:-DarkPyonix/darkpyonix-ember}}"
VPK="${VPK:-vpk}"
# ---------------------------------------------------------------------------------------------

py="$(command -v python3 || command -v python || true)"
[ -n "$py" ] || { echo "error: no python3 or python on PATH" >&2; exit 1; }
script="$COMPOSE_RUST_DIR/scripts/package-velopack.py"
[ -f "$script" ] || { echo "error: $script not found (compose-rust ref without Velopack packaging?)" >&2; exit 1; }

prev="$WORK_DIR/previous"
out="$WORK_DIR/out"
mkdir -p "$prev" "$out" "$OUT_DIR"

# The newest published release's feed and packages of this channel, for the delta. The release of
# the tag being built does not exist yet (the publish job creates it). No release, or none with this
# channel, means a first release: no delta.
if gh release download --repo "$REPO" --dir "$prev" --clobber \
        --pattern "releases.$CHANNEL.json" --pattern "*-$CHANNEL-*.nupkg" 2>"$WORK_DIR/download.log"; then
    echo "previous release assets for $CHANNEL:"
    ls -l "$prev"
else
    echo "no previous release assets for $CHANNEL ($(tr '\n' ' ' < "$WORK_DIR/download.log")); packing without a delta"
fi

args=(
    --project "$PROJECT_DIR"
    --executable "$EXECUTABLE"
    --pack-version "$PACK_VERSION"
    --pack-title "$PACK_TITLE"
    --channel "$CHANNEL"
    --vpk "$VPK"
    --output-dir "$out"
)
[ -n "$ICON" ] && args+=(--icon "$ICON")
# package-velopack.py expects a delta whenever --delta-dir is given, so only with a previous feed.
[ -f "$prev/releases.$CHANNEL.json" ] && args+=(--delta-dir "$prev")

echo "+ $py $script ${args[*]}"
"$py" "$script" "${args[@]}"

# Everything the feed names stays reachable under releases/latest/download: the packages copied
# from the previous release go up again with the new ones.
shopt -s nullglob
copied=0
for f in "$out"/*.nupkg "$out/releases.$CHANNEL.json" "$out"/*-Setup.exe "$out"/*-Portable.zip \
         "$out"/*.AppImage "$out"/*.dmg; do
    cp "$f" "$OUT_DIR/"
    copied=$((copied + 1))
done
[ -f "$OUT_DIR/releases.$CHANNEL.json" ] || { echo "error: vpk wrote no releases.$CHANNEL.json" >&2; exit 1; }
echo "copied $copied Velopack files to $OUT_DIR"
