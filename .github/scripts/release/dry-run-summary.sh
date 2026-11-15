#!/usr/bin/env bash
# SPEC NFR-R1 (#880): the summary of a release dry run (manual dispatch of publish-release.yml).
# Lists every file the run packed, with size and sha256, and fails when a target or channel lacks
# a file the tag release would upload, so the first real tag does not find it out.
#
# Usage: dry-run-summary.sh <dir> <tag> <client: true|false>
#   <dir> holds one folder per workflow artifact, as actions/download-artifact writes them:
#   dpx-ember-<target>/ (server and node tarballs) and dpx-ember-app-<target>/ (client).
set -euo pipefail

dir="${1:?usage: dry-run-summary.sh <dir> <tag> <client>}"
tag="${2:?usage: dry-run-summary.sh <dir> <tag> <client>}"
client="${3:-true}"

sum() { if command -v sha256sum >/dev/null; then sha256sum "$1"; else shasum -a 256 "$1"; fi | cut -d' ' -f1; }
size() { wc -c < "$1" | tr -d ' '; }

[ -d "$dir" ] || { echo "error: $dir not found" >&2; exit 1; }
echo "files in $dir:"
find "$dir" -type f | LC_ALL=C sort | while read -r f; do
    printf '%12s  %s  %s\n' "$(size "$f")" "$(sum "$f")" "${f#"$dir"/}"
done

missing=0
miss() { echo "error: missing $1" >&2; missing=$((missing + 1)); }
# has <folder> <glob> <what>: at least one file in the folder matches the glob.
has() {
    local d="$dir/$1" f
    for f in "$d"/$2; do [ -f "$f" ] && return 0; done
    miss "$3 ($1/$2)"
}

for target in macos-arm64 linux-x86_64 linux-aarch64; do
    for crate in server node; do
        name="ember-$crate-$tag-$target.tar.gz"
        has "dpx-ember-$target" "$name" "$crate tarball for $target"
        has "dpx-ember-$target" "$name.sha256" "$crate checksum for $target"
    done
done

if [ "$client" = "true" ]; then
    # target channel installer-glob, then the non-Velopack outputs of the same job.
    while read -r target channel installer extra; do
        d="dpx-ember-app-$target"
        has "$d" "releases.$channel.json" "Velopack feed releases.$channel.json for channel $channel"
        has "$d" "*-full.nupkg" "full .nupkg for channel $channel"
        has "$d" "$installer" "installer for channel $channel"
        # Split the globs without expanding them against the current directory.
        set -f; globs=($extra); set +f
        for g in ${globs[@]+"${globs[@]}"}; do has "$d" "$g" "client output for $target"; done
    done <<LIST
macos-arm64 osx-arm64 *.dmg ember-app-$tag-macos-arm64.dmg ember-app-$tag-macos-arm64.app.zip
windows-x86_64 win-x64 *-Setup.exe ember-app-$tag-windows-x86_64.zip
linux-x86_64 linux-x64 *.AppImage
linux-aarch64 linux-arm64 *.AppImage
LIST
fi

if [ "$missing" -gt 0 ]; then
    echo "error: $missing expected file(s) missing; a tag release would be incomplete" >&2
    exit 1
fi
echo "ok: every expected file is present for $tag (client: $client)"
