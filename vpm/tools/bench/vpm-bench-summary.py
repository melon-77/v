#!/usr/bin/env python3
"""Summarises vpm-bench.sh results: vpm-bench-summary.py RESULTS.TSV SIZES.TSV

Per configuration (set, cache, scripts) and tool: median and spread
(min-max) of the wall time of the command, of the command plus a syncfs
of the target filesystem afterwards (time until the data is on disk), of
the CPU time (user+sys); vpm's own phase times; MB per second of the
installed tree.
"""
import csv, statistics, sys
from collections import defaultdict

res = list(csv.DictReader(open(sys.argv[1]), delimiter="\t"))
sizes = {}
try:
    for r in csv.DictReader(open(sys.argv[2]), delimiter="\t"):
        sizes[(r["set"], r["tool"])] = r
except FileNotFoundError:
    pass

groups = defaultdict(list)
for r in res:
    groups[(r["set"], r["cache"], r["scripts"], r["tool"])].append(r)


def f(x):
    try:
        return float(x)
    except (TypeError, ValueError):
        return None


def stat(rows, key):
    v = [f(r[key]) for r in rows if f(r[key]) is not None]
    if not v:
        return None
    return statistics.median(v), min(v), max(v)


def fmt(s, unit="s", d=3):
    if s is None:
        return "-"
    m, lo, hi = s
    return "%.*f%s (%.*f-%.*f)" % (d, m, unit, d, lo, d, hi)


order = {"apk": 0, "vpm": 1, "xbps": 2}
keys = sorted(groups, key=lambda k: (k[0], k[1] != "cold", k[2], order.get(k[3], 9)))
print("| set | cache | scripts | tool | runs | failed | wall: median (min-max) | wall + syncfs | CPU user+sys | max RSS | vpm resolve / fetch+unpack / switch | installed | MB/s (wall) |")
print("|---|---|---|---|---|---|---|---|---|---|---|---|---|")
for k in keys:
    rows = groups[k]
    for r in rows:
        r["cpu"] = str((f(r["user"]) or 0) + (f(r["sys"]) or 0))
    w = stat(rows, "wall")
    ws = stat(rows, "wall_sync")
    cpu = stat(rows, "cpu")
    rss = stat(rows, "maxrss_kb")
    phases = "-"
    if k[3] == "vpm":
        p = [stat(rows, x) for x in ("resolve", "unpack", "switch")]
        if all(p):
            phases = " / ".join("%.3f" % x[0] for x in p)
    sz = sizes.get((k[0], k[3]))
    inst = "-"
    mbps = "-"
    if sz:
        mb = int(sz["bytes"]) / 1e6
        inst = "%.0f MB, %s files, %s pkgs" % (mb, sz["files"], sz["pkgs"])
        if w:
            mbps = "%.0f" % (mb / w[0])
    failed = sum(1 for r in rows if r["rc"] != "0")
    print("| %s | %s | %s | %s | %d | %d | %s | %s | %s | %s | %s | %s | %s |" % (
        k[0], k[1], "yes" if k[2] == "1" else "no", k[3], len(rows), failed, fmt(w), fmt(ws), fmt(cpu),
        "%.0f MB" % (rss[0] / 1024) if rss else "-", phases, inst, mbps))

print()
print("vpm / apk, ratio of medians (below 1: vpm faster):")
print()
print("| set | cache | scripts | wall | wall + syncfs | CPU |")
print("|---|---|---|---|---|---|")
seen = set()
for k in keys:
    c = k[:3]
    if c in seen or (c + ("apk",)) not in groups or (c + ("vpm",)) not in groups:
        continue
    seen.add(c)
    a, v = groups[c + ("apk",)], groups[c + ("vpm",)]
    out = []
    for key in ("wall", "wall_sync", "cpu"):
        sa, sv = stat(a, key), stat(v, key)
        out.append("%.2f" % (sv[0] / sa[0]) if sa and sv and sa[0] > 0 else "-")
    print("| %s | %s | %s | %s |" % (c[0], c[1], "yes" if c[2] == "1" else "no", " | ".join(out)))
