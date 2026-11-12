#!/bin/sh
set -eu
root=$(git rev-parse --show-toplevel)
dir="$root/ember/app/scripts/linux-gui"
docker build -t ember-linux-gui "$dir"
docker volume create ember-linux-gui-target >/dev/null
docker run -d --name ember-linux-gui -v "$root:/work:ro" -v ember-linux-gui-target:/target ember-linux-gui sh -c 'cargo build --release --locked || exit; Xvfb :99 -screen 0 1440x900x24 -ac & xvfb_pid=$!; sleep 2; openbox & /target/release/ember-app; kill "$xvfb_pid"'
