#!/bin/sh
set -eu
[ "$#" -gt 0 ] || { echo 'Usage: key.sh <keys>' >&2; exit 1; }
docker exec ember-linux-gui xdotool key "$@"
