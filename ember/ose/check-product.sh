#!/usr/bin/env bash
# Verify an OSE product.json (SPEC FR-W4): Open VSX gallery, telemetry off, no Microsoft
# marketplace / telemetry / update endpoints.
#
#   ember/ose/check-product.sh <path/to/product.json>
#
# Accepts either the patched source product.json or the one inside a packaged build
# (<build>/product.json). Pure python3 stdlib, so it runs on CI runners and on a Pi.
# Exit 0 = pass, 1 = violation, 2 = usage error.
set -euo pipefail

if [ $# -ne 1 ] || [ ! -f "$1" ]; then
  echo "usage: $0 <product.json>" >&2
  exit 2
fi

python3 - "$1" <<'PY'
import json, re, sys

path = sys.argv[1]
with open(path, encoding="utf-8") as f:
    p = json.load(f)

errors, notes = [], []

# 1. The gallery must be Open VSX.
g = p.get("extensionsGallery") or {}
if g.get("serviceUrl") != "https://open-vsx.org/vscode/gallery":
    errors.append(f"extensionsGallery.serviceUrl is {g.get('serviceUrl')!r}, expected Open VSX")
if g.get("itemUrl") != "https://open-vsx.org/vscode/item":
    errors.append(f"extensionsGallery.itemUrl is {g.get('itemUrl')!r}, expected Open VSX")
for k in ("extensionUrlTemplate", "resourceUrlTemplate", "publisherUrl"):
    v = g.get(k)
    if v and not v.startswith("https://open-vsx.org/"):
        errors.append(f"extensionsGallery.{k} is {v!r}, expected an open-vsx.org URL")

# 2. Telemetry off: no 1DS key, enableTelemetry not true.
if p.get("enableTelemetry") is True:
    errors.append("enableTelemetry is true")
if "aiConfig" in p:
    errors.append("aiConfig (telemetry key) is present")

# 3. No Microsoft marketplace / CDN-for-extensions / update / voice endpoints anywhere.
FORBIDDEN = [
    r"marketplace\.visualstudio\.com",
    r"gallery\.vsassets\.io",
    r"gallerycdn\.vsassets\.io",
    r"vscode-unpkg\.net",
    r"vscode-sync\.trafficmanager\.net",   # settings sync
    r"update\.code\.visualstudio\.com",
    r"az764295\.vo\.msecnd\.net",           # legacy update/download CDN
    r"mai\.microsoft\.com",                 # voice service
    r"default\.exp-tas\.com",               # experiments (TAS)
]
for key in ("updateUrl", "tasConfig", "settingsSyncStore", "voiceWsUrl", "msftInternalDomains"):
    if key in p:
        errors.append(f"{key} is present")

def walk(node, where):
    if isinstance(node, dict):
        for k, v in node.items():
            yield from walk(v, f"{where}.{k}" if where else k)
    elif isinstance(node, list):
        for i, v in enumerate(node):
            yield from walk(v, f"{where}[{i}]")
    elif isinstance(node, str):
        yield where, node

for where, s in walk(p, ""):
    for pat in FORBIDDEN:
        if re.search(pat, s):
            errors.append(f"{where} references a forbidden endpoint: {s}")
    # Informational: other Microsoft hosts (doc links, the webview host CDN) are allowed but listed.
    if re.search(r"https?://[^/\"]*(microsoft\.com|aka\.ms|vscode-cdn\.net|visualstudio\.com)", s) \
            and not any(re.search(pat, s) for pat in FORBIDDEN):
        notes.append(f"{where}: {s}")

print(f"checked {path}")
for n in notes:
    print(f"  note (allowed Microsoft host): {n}")
if errors:
    for e in errors:
        print(f"  FAIL: {e}", file=sys.stderr)
    sys.exit(1)
print("  OK: Open VSX gallery, telemetry off, no Microsoft marketplace/telemetry/update endpoints")
PY
