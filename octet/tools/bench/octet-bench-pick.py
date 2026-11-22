#!/usr/bin/env python3
"""Pick the big benchmark set: valence-base plus the N largest packages
(by installed size in the index) that install together without conflict,
with both apk and octet (--no-scripts).

usage: oct-bench-pick.py INDEX_JSON WORKDIR N > names.txt
Candidates are tried largest first; each is kept when
`apk --simulate add valence-base <kept...> <cand>` succeeds. The set is
then installed for real with apk and with octet; a package named in a file
conflict is dropped and the next candidate takes its place.
"""
import json, os, re, subprocess, sys

APK = "/root/valence/hosttools/bin/apk"
OCT = os.environ.get("OCT", "/root/octet-target/release/oct")
REPO = "/root/valence/repo/x86_64/Packages.adb"
KEYS = "/root/valence/keys/trusted"


def log(*a):
    print(*a, file=sys.stderr, flush=True)


def safe_rm(d):
    assert d.startswith("/root/octet-test/") and len(d) > len("/root/octet-test/")
    with open("/proc/mounts") as f:
        if any(" " + d in l for l in f):
            sys.exit("mounts under %s, not removing" % d)
    if os.path.lexists(d):
        subprocess.run(["rm", "-rf", "--one-file-system", d], check=True)


def apk(root, names, simulate):
    # --simulate needs an existing database: the sim root has valence-base
    cmd = [APK, "--root", root] + ([] if simulate else ["--initdb"]) + ["--arch", "x86_64", "--keys-dir", KEYS,
           "--repositories-file", "/dev/null", "--repository", REPO, "--no-cache",
           "--no-scripts"] + (["--simulate"] if simulate else []) + ["add", "valence-base"] + names
    return subprocess.run(cmd, capture_output=True, text=True)


def octet(root, names):
    cmd = [OCT, "--root", root, "--initdb", "--repository", REPO, "--keys-dir", KEYS,
           "--plain", "--no-scripts", "add", "valence-base"] + names
    return subprocess.run(cmd, capture_output=True, text=True)


def main():
    index, work, n = sys.argv[1], sys.argv[2], int(sys.argv[3])
    pk = json.load(open(index))["packages"]
    pk.sort(key=lambda p: -p.get("installed-size", 0))
    cands = [p["name"] for p in pk if p["name"] != "valence-base"]
    size = {p["name"]: (p.get("installed-size", 0), p.get("file-size", 0)) for p in pk}
    os.makedirs(work, exist_ok=True)
    sim = os.path.join(work, "sim")
    kept, rejected = [], {}
    ci = 0
    safe_rm(sim)
    r = apk(sim, [], False)
    if r.returncode != 0:
        sys.exit("apk: cannot make the simulation root:\n" + r.stdout + r.stderr)
    while True:
        while len(kept) < n and ci < len(cands):
            c = cands[ci]; ci += 1
            if c in rejected:
                continue
            r = apk(sim, kept + [c], True)
            if r.returncode == 0:
                kept.append(c)
                log("keep %-40s %6.1f MB installed" % (c, size[c][0] / 1e6))
            else:
                why = (r.stderr.strip().splitlines() or r.stdout.strip().splitlines() or ["?"])
                rejected[c] = "resolve: " + " | ".join(why[:3])
                log("skip %-40s %s" % (c, rejected[c][:150]))
                if len(rejected) > 60:
                    sys.exit("too many rejections; something is wrong")
        # real installs: file conflicts
        bad = None
        for tool in ("apk", "octet"):
            root = os.path.join(work, "try-" + tool)
            safe_rm(root)
            r = apk(root, kept, False) if tool == "apk" else octet(root, kept)
            out = r.stdout + r.stderr
            safe_rm(root)
            errs = "\n".join(l for l in out.splitlines()
                             if re.match(r"\s*(ERROR|oct: error|oct: warning)", l) or "both have" in l
                             or "conflict" in l or "overwrite" in l)
            if r.returncode != 0 or errs:
                log("%s failed (rc %d):\n%s" % (tool, r.returncode, errs[:2000]))
                # the conflicting package latest in the kept order is dropped
                named = [k for k in kept if re.search(r"(^|[\s/(])%s-[0-9]" % re.escape(k), errs)]
                bad = named[-1] if named else kept[-1]
                break
        if bad is None:
            break
        rejected[bad] = "file conflict"
        kept.remove(bad)
        log("drop %s (file conflict)" % bad)
    safe_rm(sim)
    tot = sum(size[k][0] for k in kept)
    log("%d picked, %.1f MB installed size (requested packages only)" % (len(kept), tot / 1e6))
    for k in kept:
        print(k)
    with open(os.path.join(work, "rejected.txt"), "w") as f:
        for k, v in rejected.items():
            f.write("%s\t%s\n" % (k, v))


main()
