#!/bin/sh
# One-time privileged setup of project mount points for ember server
# (server/src/computers/mount.rs, docs/design/COMPUTERS.md § Project mount).
#
# A node's project directory is mounted on the server at the same absolute path. The mount itself
# needs no root (macOS mount_nfs / Linux fusermount3 on a directory the server user owns), but
# creating that directory may: e.g. /Users/<other user>/... or /home/<user>/... on macOS, or
# /Users/<user>/... on a Raspberry Pi. Run this once per path, as the user ember server runs as:
#
#   scripts/ember-mount-setup.sh /home/pi/proj [/Users/me/other ...]
#
# It prints every privileged command before running it with sudo. Use --dry-run to only print.
set -eu

DRY=0
if [ "${1:-}" = "--dry-run" ]; then
    DRY=1
    shift
fi
[ $# -gt 0 ] || { echo "usage: $0 [--dry-run] <absolute path>..." >&2; exit 2; }

ME=$(id -un)
OS=$(uname -s)

run() {
    echo "+ sudo $*"
    [ "$DRY" = 1 ] || sudo "$@"
}

for P in "$@"; do
    case "$P" in
        /*) ;;
        *) echo "$P: not an absolute path" >&2; exit 2 ;;
    esac
    if [ -d "$P" ] && [ -O "$P" ]; then
        echo "$P: exists and is owned by $ME; nothing to do"
        continue
    fi
    TOP=$(echo "$P" | cut -d/ -f2)
    if [ "$OS" = Darwin ]; then
        if [ "$TOP" = home ] && grep -Eq '^[[:space:]]*/home[[:space:]]' /etc/auto_master; then
            echo "$P: /home is an automount point (auto_home in /etc/auto_master)."
            echo "  Disabling it lets real directories live under /home (macOS user homes stay in /Users)."
            run sed -i.ember-bak -E 's|^([[:space:]]*/home[[:space:]])|#\1|' /etc/auto_master
            run automount -vc
        elif [ ! -e "/$TOP" ]; then
            echo "$P: /$TOP does not exist and the macOS system volume is read-only."
            echo "  /$TOP becomes a symlink to /System/Volumes/Data/$TOP via /etc/synthetic.conf;"
            echo "  reboot afterwards, then run this script again."
            run mkdir -p "/System/Volumes/Data/$TOP"
            echo "+ printf '$TOP\\tSystem/Volumes/Data/$TOP\\n' | sudo tee -a /etc/synthetic.conf"
            [ "$DRY" = 1 ] || printf '%s\tSystem/Volumes/Data/%s\n' "$TOP" "$TOP" | sudo tee -a /etc/synthetic.conf >/dev/null
            continue
        fi
    fi
    run mkdir -p "$P"
    run chown "$ME" "$P"
    echo "$P: ready (owned by $ME)"
done
