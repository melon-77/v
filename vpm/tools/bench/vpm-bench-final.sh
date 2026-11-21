B=/root/vpm-test/bench
du -sh $B/* 2>/dev/null | sort -h | tail -12
ls $B
echo "mounts: $(grep -c vpm-test /proc/mounts)"
cd /mnt/c/Users/a/AppData/Local/Temp/claude/39afd7d0aa98685a/9f48dd28-e365-4eae-aba4-ec797468f31d/scratchpad/vpm && git -c safe.directory='*' status --short && git -c safe.directory='*' log --oneline | head -3
grep -c . $B/results.tsv
python3 -I - $B/exp.tsv <<'EOF'
import csv, statistics, sys
rows = [r for r in csv.DictReader(open(sys.argv[1]), delimiter="\t") if r["run"] != "0"]
g = {}
for r in rows:
    g.setdefault((r["set"], r["variant"]), []).append(r)
for (s, v), rs in g.items():
    def med(k):
        x = [float(r[k]) for r in rs if r[k]]
        return statistics.median(x) if x else float("nan")
    w = [float(r["wall"]) for r in rs]
    print("| %s | %s | %d | %.3f s (%.3f-%.3f) | %.0f | %.0f | %.0f |" % (s, v, len(rs), statistics.median(w), min(w), max(w), med("unpack_ms"), med("syncfs1_ms"), med("place_ms")))
EOF
