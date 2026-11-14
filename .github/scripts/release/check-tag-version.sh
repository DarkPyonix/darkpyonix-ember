#!/usr/bin/env bash
# SPEC NFR-R1 (#880): the release tag's version must equal the client's crate version.
# The app shows CARGO_PKG_VERSION in 설정 while its Velopack package carries the tag's version,
# and the update check compares the two, so a mismatch would offer the same release forever.
#
# Usage: check-tag-version.sh <tag> [<Cargo.toml>]   (tag like v0.1.0; default ember/app/Cargo.toml)
set -euo pipefail

tag="${1:?usage: check-tag-version.sh <tag> [<Cargo.toml>]}"
manifest="${2:-ember/app/Cargo.toml}"

[ -f "$manifest" ] || { echo "error: $manifest not found" >&2; exit 1; }

want="${tag#v}"
if ! printf '%s\n' "$want" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$'; then
    echo "error: tag $tag is not vMAJOR.MINOR.PATCH" >&2
    exit 1
fi

# The first `version = "..."` inside [package].
have="$(awk '
    /^\[/ { in_pkg = ($0 == "[package]") ; next }
    in_pkg && /^[[:space:]]*version[[:space:]]*=/ {
        sub(/^[^"]*"/, ""); sub(/".*$/, ""); print; exit
    }
' "$manifest")"

if [ -z "$have" ]; then
    echo "error: no [package] version in $manifest" >&2
    exit 1
fi
if [ "$have" != "$want" ]; then
    echo "error: tag $tag is version $want but $manifest says $have; bump the crate or tag $have" >&2
    exit 1
fi
echo "ok: tag $tag matches $manifest version $have"
