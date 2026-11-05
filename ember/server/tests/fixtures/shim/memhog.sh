#!/bin/sh
# Holds about 6 MB in a shell variable, then idles: exceeds a 1 MiB memory limit (FR-S8).
x=$(head -c 6000000 /dev/zero | tr '\0' a)
echo ready
sleep 30
