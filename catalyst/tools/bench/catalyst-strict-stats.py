"""Verdicts for the results of catalyst-strict.sh and catalyst-strict2.sh: catalyst vs apk per (system, job, cache), on wall time and on wall time + sync.

A verdict is only "faster" or "slower" when both hold:
  - a two-sided Mann-Whitney U test gives p < 0.01, and
  - the 95% bootstrap confidence interval of median(catalyst) / median(apk) lies entirely below 0.95
    ("faster") or entirely above 1.05 ("slower").
Otherwise: "no clear difference". usage: benchstats.py RESULTS.tsv"""
import csv, math, random, statistics as st, sys
from collections import defaultdict

rows = list(csv.DictReader(open(sys.argv[1]), delimiter='\t'))
data = defaultdict(list)
for r in rows:
    for metric in ('wall', 'wall_sync'):
        data[(r['set'], r['job'], r['cache'], metric, r['tool'])].append(float(r[metric]))

def mwu_p(a, b):
    """Two-sided Mann-Whitney U, normal approximation with tie correction."""
    allv = sorted([(v, 0) for v in a] + [(v, 1) for v in b])
    ranks, i = {}, 0
    rank_sum_a, n = 0.0, len(allv)
    ties = []
    while i < n:
        j = i
        while j + 1 < n and allv[j + 1][0] == allv[i][0]:
            j += 1
        r = (i + j) / 2 + 1
        ties.append(j - i + 1)
        for k in range(i, j + 1):
            if allv[k][1] == 0:
                rank_sum_a += r
        i = j + 1
    n1, n2 = len(a), len(b)
    u = rank_sum_a - n1 * (n1 + 1) / 2
    mu = n1 * n2 / 2
    tie = sum(t ** 3 - t for t in ties) / ((n1 + n2) * (n1 + n2 - 1))
    sigma = math.sqrt(n1 * n2 / 12 * ((n1 + n2 + 1) - tie))
    if sigma == 0:
        return 1.0
    z = (abs(u - mu) - 0.5) / sigma
    return math.erfc(z / math.sqrt(2))

def boot_ci(a, b, reps=4000, seed=1):
    rnd = random.Random(seed)
    rs = []
    for _ in range(reps):
        ra = [rnd.choice(a) for _ in a]
        rb = [rnd.choice(b) for _ in b]
        rs.append(st.median(ra) / st.median(rb))
    rs.sort()
    return rs[int(0.025 * reps)], rs[int(0.975 * reps)]

def fmt(v):
    return f'{v * 1000:.0f} ms' if v < 1 else f'{v:.2f} s'

print('| system | job | cache | apk | catalyst | catalyst / apk (95% CI) | p | verdict |')
print('|---|---|---|---|---|---|---|---|')
keys = sorted({k[:4] for k in data}, key=lambda k: (k[1], k[0], k[2], k[3]))
for s, job, cache, metric in keys:
    a = data[(s, job, cache, metric, 'cat')]
    b = data[(s, job, cache, metric, 'apk')]
    if not a or not b:
        continue
    lo, hi = boot_ci(a, b)
    p = mwu_p(a, b)
    ratio = st.median(a) / st.median(b)
    verdict = 'no clear difference'
    if p < 0.01 and hi < 0.95:
        verdict = '**faster**'
    elif p < 0.01 and lo > 1.05:
        verdict = 'slower'
    label = cache + (', + sync' if metric == 'wall_sync' else '')
    print(f'| {s} | {job} | {label} | {fmt(st.median(b))} | {fmt(st.median(a))} | {ratio:.2f} ({lo:.2f}-{hi:.2f}) | {p:.1g} | {verdict} |')
