#!/usr/bin/env python3
"""Compare vpm's readings of the real repository with apk-tools' own.

1. index: `apk adbdump --format json Packages.adb` vs `vpm dump-index --json`
   (every field of every package), and `apk query --available --fields all`
   (apk's runtime view, a different code path) vs vpm.
2. packages: `apk adbdump --format json X.apk` vs `vpm dump --json X.apk`
   for every package (info, paths, files, acl, hashes, targets, scripts).
3. contents: `apk extract` into a temp dir vs `vpm manifest` (type, mode,
   owner, size, mtime, SHA-256 of the content, link targets) for a
   selection of packages.
"""
import json, os, re, stat, subprocess, sys, hashlib, shutil, pwd, grp
from concurrent.futures import ThreadPoolExecutor

APK = "/root/valence/hosttools/bin/apk"
VPM = "/root/vpm-target/release/vpm"
REPO = "/root/valence/repo/x86_64"
KEYS = "/root/valence/keys/trusted"
TEST = "/root/vpm-test"
A = ["--arch", "x86_64", "--keys-dir", KEYS, "--repositories-file", "/dev/null",
     "--repository", REPO + "/Packages.adb", "--no-cache"]
HEXN = re.compile(r"^\((\d+) bytes\)$")

def run(cmd):
    r = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    if r.returncode != 0:
        raise RuntimeError("%s: rc=%d %s" % (" ".join(cmd), r.returncode, r.stderr.decode("utf-8", "replace")))
    return r.stdout

def jload(b):
    return json.loads(b.decode("utf-8", "surrogateescape"), strict=False)

def norm(v):
    """Drop absent-equivalent values (0, "", [], {}) recursively."""
    if isinstance(v, dict):
        out = {}
        for k, x in v.items():
            x = norm(x)
            if x in (0, "", [], {}, None):
                continue
            out[k] = x
        return out
    if isinstance(v, list):
        return [norm(x) for x in v]
    return v

def same(a, b, path, diffs):
    """a = apk, b = vpm."""
    if isinstance(a, str) and isinstance(b, str):
        m = HEXN.match(a)
        if m and a != b:  # apk prints long hex blobs as "(N bytes)"
            if len(b) // 2 != int(m.group(1)):
                diffs.append("%s: apk %r vs vpm %d hex bytes" % (path, a, len(b) // 2))
            return
    if type(a) != type(b):
        diffs.append("%s: type %s vs %s (%r / %r)" % (path, type(a).__name__, type(b).__name__, a, b))
        return
    if isinstance(a, dict):
        for k in sorted(set(a) | set(b)):
            if k not in a:
                diffs.append("%s.%s: only vpm has it: %r" % (path, k, b[k]))
            elif k not in b:
                diffs.append("%s.%s: only apk has it: %r" % (path, k, a[k]))
            else:
                same(a[k], b[k], path + "." + k, diffs)
    elif isinstance(a, list):
        if len(a) != len(b):
            diffs.append("%s: %d items vs %d" % (path, len(a), len(b)))
        for i, (x, y) in enumerate(zip(a, b)):
            same(x, y, "%s[%d]" % (path, i), diffs)
    elif a != b:
        diffs.append("%s: %r vs %r" % (path, a, b))

def report(title, diffs, limit=20):
    print("  %s: %s" % (title, "IDENTICAL" if not diffs else "%d DIFFERENCES" % len(diffs)))
    for d in diffs[:limit]:
        print("    " + d)

def index_checks():
    print("== 1. index")
    apk = norm(jload(run([APK] + A + ["adbdump", "--format", "json", REPO + "/Packages.adb"])))
    vpm = norm(jload(run([VPM, "--keys-dir", KEYS, "dump-index", "--json", REPO + "/Packages.adb"])))
    pk_a, pk_v = apk.get("packages", []), vpm.get("packages", [])
    print("  adbdump: %d packages; vpm: %d packages; description %r / %r" %
          (len(pk_a), len(pk_v), apk.get("description"), vpm.get("description")))
    diffs = []
    same(apk, vpm, "index", diffs)
    nfields = sum(len(p) for p in pk_a)
    ndeps = sum(len(p.get(k, [])) for p in pk_a for k in ("depends", "provides", "install-if", "replaces", "recommends"))
    print("  compared %d packages, %d top-level fields, %d dependency entries" % (len(pk_a), nfields, ndeps))
    report("adbdump vs vpm dump-index", diffs)

    # apk's runtime view (apk_pkgtmpl_from_adb + apk_blob_push_dep), needs a db root
    rt = TEST + "/apkroot"
    if rt in open("/proc/mounts").read():
        raise SystemExit("test root is mounted")
    shutil.rmtree(rt, ignore_errors=True)
    os.makedirs(rt)
    run([APK] + A + ["--root", rt, "--initdb", "add"])
    names = sorted({p["name"] for p in pk_v})
    q = jload(run([APK] + A + ["--root", rt, "query", "--available", "--fields", "all", "--format", "json"] + names))
    print("  apk query --available: %d records for %d names" % (len(q), len(names)))
    byq = {(r["name"], r["version"]): r for r in q}
    diffs = []
    keys = ["name", "version", "description", "arch", "license", "origin", "maintainer", "url",
            "build-time", "installed-size", "file-size", "provider-priority", "depends", "provides",
            "install-if", "recommends", "layer", "tags"]
    checked = 0
    for p in pk_v:
        r = byq.get((p["name"], p["version"]))
        if r is None:
            diffs.append("%s-%s: not in apk query output" % (p["name"], p["version"]))
            continue
        r = norm(r)
        for k in keys:
            same(r.get(k, None) if k in r else None, p.get(k, None) if k in p else None,
                 "%s-%s.%s" % (p["name"], p["version"], k), diffs)
        if "commit" in r or "repo-commit" in p:
            same(r.get("commit"), p.get("repo-commit"), "%s.commit" % p["name"], diffs)
        exp = REPO + "/%s-%s.apk" % (p["name"], p["version"])
        if r.get("download-url") != exp:
            diffs.append("%s: download-url %r" % (p["name"], r.get("download-url")))
        checked += 1
    print("  compared %d packages field by field with apk query" % checked)
    report("apk query vs vpm", diffs)
    shutil.rmtree(rt, ignore_errors=True)
    return [(p["name"], p["version"]) for p in pk_v]

def pkg_compare(nv):
    f = "%s/%s-%s.apk" % (REPO, nv[0], nv[1])
    try:
        a = norm(jload(run([APK] + A + ["adbdump", "--format", "json", f])))
        v = norm(jload(run([VPM, "--keys-dir", KEYS, "dump", "--json", f])))
    except Exception as e:
        return nv, ["error: %s" % e], {}
    d = []
    same(a, v, "%s-%s" % nv, d)
    st = {"dirs": len(a.get("paths", [])),
          "files": sum(len(p.get("files", [])) for p in a.get("paths", [])),
          "scripts": len(a.get("scripts", {})),
          "symlinks": sum(1 for p in a.get("paths", []) for x in p.get("files", []) if x.get("target", "").startswith("00a0")),
          "hardlinks": sum(1 for p in a.get("paths", []) for x in p.get("files", []) if x.get("target", "").startswith("0080"))}
    return nv, d, st

def packages_checks(nvs):
    print("== 2. packages (adbdump --format json vs vpm dump --json)")
    diffs, tot = [], {}
    with ThreadPoolExecutor(12) as ex:
        for nv, d, st in ex.map(pkg_compare, nvs):
            diffs += d
            for k, x in st.items():
                tot[k] = tot.get(k, 0) + x
    print("  compared %d packages: %s" % (len(nvs), ", ".join("%d %s" % (x, k) for k, x in sorted(tot.items()))))
    report("adbdump vs vpm dump", diffs)

def walk_manifest(root):
    """Manifest of an extracted tree, in vpm manifest's format."""
    out = {}
    for dp, dns, fns in os.walk(root):
        for n in dns + fns:
            p = os.path.join(dp, n)
            rel = os.path.relpath(p, root)
            st = os.lstat(p)
            user = pwd.getpwuid(st.st_uid).pw_name
            group = grp.getgrgid(st.st_gid).gr_name
            m = st.st_mode
            if stat.S_ISDIR(m):
                out[rel] = ("d", "%04o" % (m & 0o7777), user, group, "0", "0", "")
            elif stat.S_ISLNK(m):
                out[rel] = ("l", "%04o" % (m & 0o7777), user, group, "0", "0", os.readlink(p))
            elif stat.S_ISREG(m):
                h = hashlib.sha256(open(p, "rb").read()).hexdigest()
                out[rel] = ("f", "%04o" % (m & 0o7777), user, group, str(st.st_size), str(int(st.st_mtime)), h)
            else:
                out[rel] = ("?", "%04o" % (m & 0o7777), user, group, "0", "0", "")
    return out

def extract_compare(nv):
    f = "%s/%s-%s.apk" % (REPO, nv[0], nv[1])
    dest = "%s/ex/%s-%s" % (TEST, nv[0], nv[1])
    if dest in open("/proc/mounts").read():
        raise SystemExit("mounted")
    shutil.rmtree(dest, ignore_errors=True)
    os.makedirs(dest)
    diffs, warn = [], ""
    try:
        r = subprocess.run([APK] + A + ["extract", "--destination", dest, f], stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        warn = r.stdout.decode("utf-8", "replace")
        if r.returncode != 0:
            diffs.append("apk extract rc=%d: %s" % (r.returncode, warn.strip()))
        got = walk_manifest(dest)
        mine = {}
        for line in run([VPM, "--keys-dir", KEYS, "manifest", f]).decode("utf-8", "surrogateescape").splitlines():
            t, mode, u, g, size, mtime, path, extra = line.split("\t")
            if t == "h":
                t = "f"  # a hard link extracts as a regular file with the target's content
            mine[path] = (t, mode, u, g, size, mtime if t == "f" else "0", extra)
        for p in sorted(set(got) | set(mine)):
            if p not in got:
                diffs.append("%s: in vpm manifest, not extracted by apk" % p)
            elif p not in mine:
                diffs.append("%s: extracted by apk, not in vpm manifest" % p)
            elif got[p] != mine[p]:
                diffs.append("%s: apk %s vs vpm %s" % (p, got[p], mine[p]))
        n = len(mine)
    finally:
        shutil.rmtree(dest, ignore_errors=True)
    warns = [l for l in warn.splitlines() if "WARNING" in l or "ERROR" in l]
    return nv, diffs, n, warns

def extract_checks(nvs, stats_of):
    print("== 3. contents (apk extract vs vpm manifest)")
    names = [n for n, _ in nvs]
    want = set()
    for pre in ["busybox", "glibc", "glibc-dev", "glibc-locales", "valence-base", "bash", "coreutils",
                "util-linux", "openssl", "python3", "perl", "gcc", "mandoc", "apk-tools", "acl", "acl-dev",
                "acl-doc", "attr", "zlib", "zlib-dev", "ncurses", "ncurses-terminfo-base", "e2fsprogs",
                "kbd", "linux-pam", "shadow", "tzdata", "ca-certificates"]:
        if pre in names:
            want.add(pre)
    want |= {n for n in names if n.startswith("linux-firmware")}
    devs = [n for n in names if n.endswith("-dev")]
    docs = [n for n in names if n.endswith("-doc")]
    want |= set(devs[::max(1, len(devs) // 8)][:8]) | set(docs[::max(1, len(docs) // 8)][:8])
    # packages with hard links and with scripts
    want |= set(stats_of.get("hardlinks", [])[:8])
    want |= set(stats_of.get("scripts", [])[:8])
    sel = [nv for nv in nvs if nv[0] in want]
    tot_files, all_diffs, all_warn = 0, [], []
    with ThreadPoolExecutor(6) as ex:
        for nv, d, n, w in ex.map(extract_compare, sel):
            tot_files += n
            all_diffs += ["%s-%s: %s" % (nv[0], nv[1], x) for x in d]
            all_warn += ["%s-%s: %s" % (nv[0], nv[1], x) for x in w]
    print("  compared %d packages, %d entries: %s" % (len(sel), tot_files, " ".join(n for n, _ in sel)))
    report("apk extract vs vpm manifest", all_diffs, 40)
    if all_warn:
        print("  apk extract warnings: %d" % len(all_warn))
        for w in all_warn[:10]:
            print("    " + w)

def kinds(nvs):
    """Which packages have hard links / scripts (from vpm dump --json)."""
    hl, sc = [], []
    def one(nv):
        f = "%s/%s-%s.apk" % (REPO, nv[0], nv[1])
        v = jload(run([VPM, "--keys-dir", KEYS, "dump", "--json", f]))
        h = any(x.get("target", "").startswith("0080") for p in v.get("paths", []) for x in p.get("files", []))
        return nv[0], h, bool(v.get("scripts"))
    with ThreadPoolExecutor(12) as ex:
        for n, h, s in ex.map(one, nvs):
            if h: hl.append(n)
            if s: sc.append(n)
    return {"hardlinks": hl, "scripts": sc}

if __name__ == "__main__":
    nvs = index_checks()
    packages_checks(nvs)
    extract_checks(nvs, kinds(nvs))
