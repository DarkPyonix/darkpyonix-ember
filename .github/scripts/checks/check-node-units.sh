#!/usr/bin/env bash
# SPEC FR-P6: the service units for ember-node must not kill the PTY keepers and shells with the
# daemon. systemd needs KillMode=process; launchd needs AbandonProcessGroup true. Run from the
# repository root. Shell only.
set -euo pipefail
fail=0
need() { # file pattern message
    [ -f "$1" ] || { echo "error: $1 missing" >&2; fail=1; return; }
    grep -Eq -e "$2" "$1" || { echo "error: $1: $3" >&2; fail=1; }
}
svc=ember/node/dist/ember-node.service
plist=ember/node/dist/ember.node.plist
need "$svc" '^KillMode=process[[:space:]]*$' 'KillMode=process missing (control-group kills the keepers)'
need "$svc" '^ExecStart=.*ember-node' 'ExecStart for ember-node missing'
need "$svc" '^EnvironmentFile=' 'EnvironmentFile (EMBER_NODE_TOKEN) missing'
need "$plist" '<key>AbandonProcessGroup</key>' 'AbandonProcessGroup key missing'
if [ -f "$plist" ]; then
    grep -Eq -A1 '<key>AbandonProcessGroup</key>' "$plist" && \
    grep -A1 '<key>AbandonProcessGroup</key>' "$plist" | grep -q '<true/>' || { echo "error: $plist: AbandonProcessGroup must be <true/>" >&2; fail=1; }
fi
need "$plist" '<key>Label</key>' 'Label missing'
need "$plist" '<key>ProgramArguments</key>' 'ProgramArguments missing'
exit "$fail"
