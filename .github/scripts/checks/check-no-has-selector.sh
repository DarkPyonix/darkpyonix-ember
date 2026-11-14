#!/usr/bin/env bash
# No `:has(` in Ember-owned CSS (issue #142): the renderer's stylo does not parse :has(), so a
# rule using it is silently dropped. Use a state class set by the chrome rsx instead. The
# pristine/ upstream copies are exempt; they are not loaded.
set -euo pipefail
root="$(cd "$(dirname "$0")/../../.." && pwd)"
cd "$root"
hits="$(git ls-files '*.css' | grep -v '/pristine/' | grep -v '^docs/' | grep -v '^ember/proxy/' \
  | xargs -r grep -n ':has(' || true)"
if [ -n "$hits" ]; then
  echo "':has(' found in Ember-owned CSS; use a state class instead:" >&2
  echo "$hits" >&2
  exit 1
fi
echo "no :has( in Ember-owned CSS"
