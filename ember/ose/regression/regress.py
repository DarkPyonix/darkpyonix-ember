#!/usr/bin/env python3
"""Extension regression helpers for OSE (SPEC NFR-W1, issue #84).

  regress.py parse-log <exthost.log>...      print {id: activated|failed} as JSON
  regress.py compare <expected.json> <results.json>   exit 1 on a regression
  regress.py ids <extensions.json>           print one extension id per line
  regress.py --self-test                     offline check of the parsing and comparison

Status values: activated, failed, not-activated, install-failed, skipped.
The log line formats are VS Code's extension host messages; they are PROVISIONAL until a real
OSE run confirms them (README, "Extension regression").
"""
import json, os, re, sys

ACTIVATE = re.compile(r"_doActivateExtension\s+([\w.-]+)")
FAILED = re.compile(r"Activating extension '?([\w.-]+)'? failed", re.I)


def parse_log(text):
    out = {}
    for line in text.splitlines():
        m = FAILED.search(line)
        if m:
            out[m.group(1).lower()] = "failed"
            continue
        m = ACTIVATE.search(line)
        if m:
            out.setdefault(m.group(1).lower(), "activated")
    return out


def compare(expected, results):
    """Return (failures, warnings). expected: {id: {"expect": "pass"|"known-fail"}}."""
    fails, warns = [], []
    for ext, spec in sorted(expected.items()):
        got = results.get(ext.lower(), "not-activated")
        want = spec.get("expect", "pass")
        if want == "pass" and got != "activated":
            fails.append(f"{ext}: expected activation, got {got}")
        elif want == "known-fail" and got == "activated":
            warns.append(f"{ext}: known-fail now activates; update expected.json")
    for ext in sorted(set(results) - {e.lower() for e in expected}):
        warns.append(f"{ext}: not in expected.json")
    return fails, warns


def self_test():
    here = os.path.dirname(os.path.abspath(__file__))
    with open(os.path.join(here, "fixture-exthost.log")) as f:
        got = parse_log(f.read())
    assert got == {"redhat.vscode-yaml": "activated", "esbenp.prettier-vscode": "activated",
                   "golang.go": "failed", "vscodevim.vim": "failed"}, got
    exp = {"redhat.vscode-yaml": {"expect": "pass"}, "golang.go": {"expect": "pass"},
           "ms-python.python": {"expect": "pass"}, "vscodevim.vim": {"expect": "known-fail"},
           "esbenp.prettier-vscode": {"expect": "known-fail"}}
    fails, warns = compare(exp, got)
    assert fails == ["golang.go: expected activation, got failed",
                     "ms-python.python: expected activation, got not-activated"], fails
    assert warns == ["esbenp.prettier-vscode: known-fail now activates; update expected.json"], warns
    for name in ("extensions.json", "expected.json"):
        with open(os.path.join(here, name)) as f:
            json.load(f)
    with open(os.path.join(here, "extensions.json")) as f:
        ids = set(json.load(f)["extensions"])
    with open(os.path.join(here, "expected.json")) as f:
        assert ids == set(json.load(f)["results"]), "extensions.json and expected.json differ"
    print("regress.py self-test: PASS")


def main(a):
    if a[:1] == ["--self-test"]:
        return self_test()
    if a[:1] == ["parse-log"]:
        res = {}
        for p in a[1:]:
            with open(p, errors="replace") as f:
                for k, v in parse_log(f.read()).items():
                    if res.get(k) != "failed":
                        res[k] = v
        print(json.dumps(res, indent=1, sort_keys=True))
    elif a[:1] == ["ids"]:
        print("\n".join(json.load(open(a[1]))["extensions"]))
    elif a[:1] == ["compare"]:
        exp = json.load(open(a[1]))["results"]
        fails, warns = compare(exp, json.load(open(a[2])))
        for w in warns: print("warn:", w)
        for x in fails: print("FAIL:", x)
        sys.exit(1 if fails else 0)
    else:
        sys.exit(__doc__)


main(sys.argv[1:])
