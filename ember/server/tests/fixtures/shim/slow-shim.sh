#!/bin/sh
# Stands in for ember-shim in fr_s8_a_slow_shim_start_is_awaited (#924): it is called as
# `slow-shim.sh --dir <dir> --id <id>` and starts the real shim, whose path is in
# <dir>/real-shim.path, only after 6 s (longer than the old fixed 5 s wait for the socket).
sleep 6
exec "$(cat "$2/real-shim.path")" "$@"
