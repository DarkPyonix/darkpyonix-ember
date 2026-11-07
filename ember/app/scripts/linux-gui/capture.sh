#!/bin/sh
set -eu
case ${1-} in ''|*[!a-zA-Z0-9_-]*) echo 'Usage: capture.sh <name> (letters, digits, underscore, hyphen)' >&2; exit 1;; esac
root=$(git rev-parse --show-toplevel)
mkdir -p "$root/.scratch/linux-gui"
docker exec ember-linux-gui import -window root png:- > "$root/.scratch/linux-gui/$1.png"
