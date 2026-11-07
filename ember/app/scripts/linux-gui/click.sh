#!/bin/sh
set -eu
[ "$#" -eq 2 ] || { echo 'Usage: click.sh x y' >&2; exit 1; }
docker exec ember-linux-gui xdotool mousemove "$1" "$2" click 1
