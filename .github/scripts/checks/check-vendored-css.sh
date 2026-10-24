#!/usr/bin/env bash
# The vendored Code-OSS CSS equals its pristine copy plus Ember's recorded patch, and the
# pristine copy is at the commit the OSE build pins (INTENT D23, issue #143). Offline.
set -euo pipefail
root="$(cd "$(dirname "$0")/../../.." && pwd)"
bash "$root/ember/ose/vendor-workbench-css.sh" --check
