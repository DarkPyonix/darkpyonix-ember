#!/usr/bin/env python3
"""Extension regression helpers for OSE (SPEC NFR-W1, issue #84).

  regress.py parse-log <exthost.log>...      print {id: activated|failed} as JSON
  regress.py compare <expected.json> <results.json> [<install-failed.txt>] [<merged.json>]
                                             exit 1 on a regression; ids listed in
                                             install-failed.txt (one per line) count as
                                             install-failed; merged.json gets the final statuses
  regress.py ids <extensions.json>           print one extension id per line
  regress.py specs <extensions.json>         print one "id id@version" pair per line (the second
                                             word is what --install-extension takes; it is the
                                             bare id when no version is pinned)
  regress.py --self-test                     offline check of the parsing and comparison

extensions.json entries are {"id", "version", "open"} (SPEC NFR-W1); a bare "publisher.name"
string is still read as an unpinned entry with nothing to open.

Status values: activated, failed, not-activated, install-failed.
"activated" means the extension host logged the start of the activation and no failure for it;
an activation that starts and never finishes is not told apart (SPEC NFR-W1, "Not covered").
The two log lines are VS Code's extension host messages at the pinned tag
(`extHostExtensionService.ts` and `extHostExtensionActivator.ts`, read at 1.139.1); no real OSE
run has confirmed them yet.
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


def entries(doc):
    """The list of extensions.json as [{"id", "version", "open"}]."""
    out = []
    for e in doc["extensions"]:
        if isinstance(e, str):
            e = {"id": e}
        out.append({"id": e["id"], "version": e.get("version"), "open": e.get("open")})
    return out


def spec(entry):
    """What `--install-extension` takes: id@version when pinned, else the id."""
    return f"{entry['id']}@{entry['version']}" if entry.get("version") else entry["id"]


# Extensions the OSE ships: they activate in every run and are not part of the list.
BUILTIN_PREFIXES = ("vscode.", "ms-vscode.", "github.copilot")


def merge(expected, results, install_failed=()):
    """Final status per expected id: {id as expected.json spells it: status}."""
    failed_installs = {i.lower() for i in install_failed}
    return {ext: "install-failed" if ext.lower() in failed_installs else results.get(ext.lower(), "not-activated")
            for ext in expected}


def compare(expected, results, install_failed=()):
    """Return (failures, warnings). expected: {id: {"expect": "pass"|"known-fail"}}."""
    fails, warns = [], []
    final = merge(expected, results, install_failed)
    for ext, spec in sorted(expected.items()):
        got = final[ext]
        want = spec.get("expect", "pass")
        if want == "pass" and got != "activated":
            fails.append(f"{ext}: expected activation, got {got}")
        elif want == "known-fail" and got == "activated":
            warns.append(f"{ext}: known-fail now activates; update expected.json")
    for ext in sorted(set(results) - {e.lower() for e in expected}):
        if not ext.startswith(BUILTIN_PREFIXES):
            warns.append(f"{ext}: not in expected.json")
    return fails, warns


def self_test():
    here = os.path.dirname(os.path.abspath(__file__))
    with open(os.path.join(here, "fixture-exthost.txt")) as f:
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
    # A failed install is reported as such, whatever the log says; ids compare without case.
    fails, _ = compare({"Redhat.VSCode-YAML": {"expect": "pass"}, "golang.Go": {"expect": "known-fail"}},
                       got, ["redhat.vscode-yaml"])
    assert fails == ["Redhat.VSCode-YAML: expected activation, got install-failed"], fails
    assert merge({"golang.Go": {}, "a.b": {}}, got, ["A.B"]) == {"golang.Go": "failed", "a.b": "install-failed"}
    # Built-in extensions in the log are not reported as missing from expected.json.
    _, warns = compare({}, {"vscode.json-language-features": "activated", "x.y": "activated"})
    assert warns == ["x.y: not in expected.json"], warns
    # Entries: objects or bare ids; a pinned version becomes id@version.
    es = entries({"extensions": ["a.b", {"id": "c.d", "version": "1.2.3", "open": "f.txt"}, {"id": "e.f", "version": None}]})
    assert [spec(e) for e in es] == ["a.b", "c.d@1.2.3", "e.f"], es
    assert es[1]["open"] == "f.txt" and es[0]["open"] is None
    # The real data: same ids in both files, and every `open` file exists in workspace/.
    with open(os.path.join(here, "extensions.json")) as f:
        real = entries(json.load(f))
    with open(os.path.join(here, "expected.json")) as f:
        exp_ids = {k.lower() for k in json.load(f)["results"]}
    ids = [e["id"].lower() for e in real]
    assert len(ids) == len(set(ids)), "an extension is listed twice"
    assert set(ids) == exp_ids, "extensions.json and expected.json differ"
    for e in real:
        assert re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9-]*\.[A-Za-z0-9][A-Za-z0-9-]*", e["id"]), e["id"]
        assert e["version"] is None or re.fullmatch(r"\d+\.\d+\.\d+([-+][0-9A-Za-z.-]+)?", e["version"]), e
        if e["open"]:
            assert not os.path.isabs(e["open"]) and ".." not in e["open"].split("/"), e
            assert os.path.isfile(os.path.join(here, "workspace", e["open"])), f"{e['id']}: workspace/{e['open']} missing"
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
        print("\n".join(e["id"] for e in entries(json.load(open(a[1])))))
    elif a[:1] == ["specs"]:
        print("\n".join(f"{e['id']} {spec(e)}" for e in entries(json.load(open(a[1])))))
    elif a[:1] == ["compare"]:
        exp = json.load(open(a[1]))["results"]
        results = json.load(open(a[2]))
        install_failed = []
        if len(a) > 3 and os.path.isfile(a[3]):
            with open(a[3]) as f:
                install_failed = [line.strip() for line in f if line.strip()]
        if len(a) > 4:
            with open(a[4], "w") as f:
                json.dump(merge(exp, results, install_failed), f, indent=1, sort_keys=True)
                f.write("\n")
        fails, warns = compare(exp, results, install_failed)
        for w in warns: print("warn:", w)
        for x in fails: print("FAIL:", x)
        sys.exit(1 if fails else 0)
    else:
        sys.exit(__doc__)


main(sys.argv[1:])
