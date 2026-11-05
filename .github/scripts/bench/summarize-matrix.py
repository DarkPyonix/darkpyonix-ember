#!/usr/bin/env python3
"""NFR-N1: summarize the matrix results written by run-matrix.sh.

Usage: uv run python -I .github/scripts/bench/summarize-matrix.py RESULTS_DIR
           [--expect PAIR ...] [--symmetric PAIR ...] [--udp-blocked PAIR ...] [--strict]

RESULTS_DIR holds one directory per network pair, each with 01.json .. 20.json, optional
NN.exit and failures.txt. A run counts as failed when its .exit file exists, its JSON is
missing, empty or unreadable. Failures stay in every denominator (NFR-N1).

Percentile: sorted samples, index round((n - 1) * p), the same rule as ember-transport-bench,
so the p95 of 20 runs is the 19th value. Standard library only.

Flags (each makes `--strict` exit 1): an expected pair has no directory, a pair has fewer than
20 runs, a run failed. Lines with no automatic data (network change, mobile, direct to relay
fallback) always print MISSING: the bench does not measure them.
"""
import argparse
import json
import sys
from pathlib import Path

RUNS = 20
# Pairs expected when --expect is not given (NFR-N1 matrix). Names are the PAIR argument of
# run-matrix.sh, so a different naming needs --expect.
DEFAULT_EXPECT = ["home-office", "home-lte", "symmetric-nat", "udp-blocked"]
MANUAL_LINES = [
    "Network change (Wi-Fi <-> LTE): streams survive, stall p95 <= 3 s",
    "Direct -> relay fallback: no stream reset",
    "Mobile: background battery <= 2%/h (Android, iOS), foreground reconnect p95 <= 1 s",
]


def pct(values, p):
    s = sorted(values)
    return s[round((len(s) - 1) * p)] if s else None


def num(x):
    return x if isinstance(x, (int, float)) and not isinstance(x, bool) else None


def load_pair(d):
    """Return (runs, failed): runs are parsed reports, failed counts every other attempt."""
    runs, failed, seen = [], 0, set()
    for p in d.iterdir():
        stem = p.name.split(".")[0]
        if p.suffix in (".json", ".exit") and stem.isdigit():
            seen.add(stem)
    for stem in sorted(seen):
        if (d / f"{stem}.exit").exists():
            failed += 1
            continue
        try:
            runs.append(json.loads((d / f"{stem}.json").read_text()))
        except (OSError, ValueError):
            failed += 1
    return runs, failed


def is_direct(r):
    path = r.get("initial_path")
    return (isinstance(path, str) and path.startswith("direct")) or num(r.get("time_to_direct_ms")) is not None


def is_relayed(r):
    path = r.get("initial_path")
    return isinstance(path, str) and path.startswith("relayed")


def baseline(r):
    """The tcp_baseline dict when it holds data, else None (null or {"error": ...})."""
    b = r.get("tcp_baseline")
    if not isinstance(b, dict) or "error" in b:
        return None
    return b


def fmt(v, unit=""):
    return "-" if v is None else (f"{v:.1f}{unit}" if isinstance(v, float) else f"{v}{unit}")


def verdict(value, ok, missing_note="MISSING"):
    if value is None:
        return missing_note
    return "PASS" if ok(value) else "FAIL"


def summarize(name, d, kind):
    runs, failed = load_pair(d)
    total = len(runs) + failed
    s = {"name": name, "kind": kind, "total": total, "failed": failed, "flags": [], "rows": []}
    if total < RUNS:
        s["flags"].append(f"only {total} of {RUNS} runs")
    if failed:
        s["flags"].append(f"{failed} failed run(s), counted in the denominator")
    denom = max(total, 1)
    direct = [r for r in runs if is_direct(r)]
    relayed = [r for r in runs if is_relayed(r) and not is_direct(r)]
    s["direct_pct"] = 100.0 * len(direct) / denom
    s["relayed_pct"] = 100.0 * len(relayed) / denom
    fb = [v for r in runs if (v := num(r.get("setup_first_byte_ms"))) is not None]
    s["fb_p50"], s["fb_p95"] = pct(fb, 0.5), pct(fb, 0.95)
    ttd = [v for r in direct if (v := num(r.get("time_to_direct_ms"))) is not None]
    s["direct_p95"] = pct(ttd, 0.95)
    up = [v for r in relayed if (v := num(r.get("time_to_direct_ms"))) is not None]
    s["upgrade_p95"] = pct(up, 0.95) if up else None
    s["upgrade_runs"] = len(relayed)

    d50, d95, ratios, no_base = [], [], [], 0
    for r in direct:
        b = baseline(r)
        rtt, brtt = r.get("rtt"), (b or {}).get("rtt")
        tp, btp = r.get("throughput"), (b or {}).get("throughput")
        if not b or not isinstance(rtt, dict) or not isinstance(brtt, dict):
            no_base += 1
            continue
        for key, out in (("p50_ms", d50), ("p95_ms", d95)):
            a, c = num(rtt.get(key)), num(brtt.get(key))
            if a is not None and c is not None:
                out.append(a - c)
        a = num((tp or {}).get("mbit_per_s"))
        c = num((btp or {}).get("mbit_per_s"))
        if a is not None and c:
            ratios.append(a / c)
    s["no_base"] = no_base
    s["rtt_d50"], s["rtt_d95"] = pct(d50, 0.5), pct(d95, 0.95)
    s["tput_ratio"] = pct(ratios, 0.5)
    if no_base:
        s["flags"].append(f"{no_base} direct run(s) without a usable tcp_baseline")
    return s


def render(summaries, missing_pairs):
    out = []
    out.append("NFR-N1 matrix summary (failures stay in the denominator; p95 = nearest rank of 20)")
    out.append("")
    head = ["pair", "kind", "runs", "fail", "direct%", "relayed%", "1stbyte p50", "1stbyte p95",
            "direct p95", "RTT d p50", "RTT d p95", "tput/TCP"]
    rows = [head]
    for s in summaries:
        rows.append([
            s["name"], s["kind"], str(s["total"]), str(s["failed"]),
            fmt(s["direct_pct"]), fmt(s["relayed_pct"]),
            fmt(s["fb_p50"], " ms"), fmt(s["fb_p95"], " ms"), fmt(s["direct_p95"], " ms"),
            fmt(s["rtt_d50"], " ms"), fmt(s["rtt_d95"], " ms"),
            "-" if s["tput_ratio"] is None else f"{100 * s['tput_ratio']:.0f}%",
        ])
    for name in missing_pairs:
        rows.append([name, "-", "0", "-"] + ["MISSING"] * 8)
    widths = [max(len(r[i]) for r in rows) for i in range(len(head))]
    for r in rows:
        out.append("  ".join(c.ljust(w) for c, w in zip(r, widths)).rstrip())
    out.append("")
    out.append("Lines (SPEC NFR-N1):")
    plain = [s for s in summaries if s["kind"] == "normal"]
    if plain:
        tot = sum(s["total"] for s in plain)
        hit = sum(s["direct_pct"] * max(s["total"], 1) / 100 for s in plain)
        rate = 100.0 * hit / max(tot, 1)
        out.append(f"  Direct success excluding symmetric NAT and UDP-blocked: {rate:.1f}% "
                   f"{verdict(rate, lambda v: v >= 85)} (>= 85%)")
    else:
        out.append("  Direct success excluding symmetric NAT and UDP-blocked: MISSING (no normal pair)")
    for s in summaries:
        n, k = s["name"], s["kind"]
        if k == "symmetric":
            conn = 100.0 * (s["total"] - s["failed"]) / max(s["total"], 1)
            out.append(f"  {n}: relay connect 100%: {conn:.1f}% {verdict(conn, lambda v: v >= 100)}"
                       f" (relayed share {fmt(s['relayed_pct'])}%, direct {fmt(s['direct_pct'])}%)")
        if k == "udp-blocked":
            conn = 100.0 * (s["total"] - s["failed"]) / max(s["total"], 1)
            out.append(f"  {n}: TCP connect 100%: {conn:.1f}% {verdict(conn, lambda v: v >= 100)}; "
                       f"first byte p95 <= 3000 ms: {fmt(s['fb_p95'], ' ms')} "
                       f"{verdict(s['fb_p95'], lambda v: v <= 3000)}")
        out.append(f"  {n}: first byte p95 <= 1500 ms: {fmt(s['fb_p95'], ' ms')} "
                   f"{verdict(s['fb_p95'], lambda v: v <= 1500)}")
        out.append(f"  {n}: direct established p95 <= 5000 ms: {fmt(s['direct_p95'], ' ms')} "
                   f"{verdict(s['direct_p95'], lambda v: v <= 5000)}")
        out.append(f"  {n}: direct RTT over raw TCP p50 <= 5 ms: {fmt(s['rtt_d50'], ' ms')} "
                   f"{verdict(s['rtt_d50'], lambda v: v <= 5)}; p95 <= 15 ms: {fmt(s['rtt_d95'], ' ms')} "
                   f"{verdict(s['rtt_d95'], lambda v: v <= 15)}")
        tr = s["tput_ratio"]
        out.append(f"  {n}: throughput >= 80% of raw TCP: "
                   f"{'-' if tr is None else f'{100 * tr:.0f}%'} {verdict(tr, lambda v: v >= 0.8)}")
        out.append(f"  {n}: relay -> direct upgrade p95 <= 10000 ms: {fmt(s['upgrade_p95'], ' ms')} "
                   f"{verdict(s['upgrade_p95'], lambda v: v <= 10000)} ({s['upgrade_runs']} relayed-start run(s))")
    for line in MANUAL_LINES:
        out.append(f"  {line}: MISSING (not measured by the bench)")
    flags = [f"{s['name']}: {f}" for s in summaries for f in s["flags"]]
    flags += [f"{n}: pair directory not found" for n in missing_pairs]
    out.append("")
    if flags:
        out.append("Flags:")
        out += [f"  {f}" for f in flags]
    else:
        out.append("Flags: none")
    out.append("")
    out.append("Not a pass decision: any MISSING line means the matrix is not complete; the owner "
               "records the verdict in #10.")
    return "\n".join(out), flags


def main(argv):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("results", type=Path)
    ap.add_argument("--expect", nargs="+", default=None, help="pair names that must exist")
    ap.add_argument("--symmetric", nargs="*", default=None, help="pairs with a symmetric NAT side")
    ap.add_argument("--udp-blocked", nargs="*", default=None, help="pairs that block UDP")
    ap.add_argument("--strict", action="store_true", help="exit 1 when anything is flagged")
    a = ap.parse_args(argv)
    if not a.results.is_dir():
        print(f"error: not a directory: {a.results}", file=sys.stderr)
        return 2
    dirs = sorted(p for p in a.results.iterdir() if p.is_dir())
    expect = a.expect if a.expect is not None else DEFAULT_EXPECT
    sym = a.symmetric if a.symmetric is not None else [n for n in expect if "sym" in n]
    udp = a.udp_blocked if a.udp_blocked is not None else [n for n in expect if "udp" in n]
    sym += [p.name for p in dirs if "sym" in p.name and a.symmetric is None]
    udp += [p.name for p in dirs if "udp" in p.name and a.udp_blocked is None]
    summaries = []
    for p in dirs:
        kind = "udp-blocked" if p.name in udp else "symmetric" if p.name in sym else "normal"
        summaries.append(summarize(p.name, p, kind))
    present = {p.name for p in dirs}
    missing = [n for n in expect if n not in present]
    text, flags = render(summaries, missing)
    print(text)
    return 1 if (a.strict and flags) else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
