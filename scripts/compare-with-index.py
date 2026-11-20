#!/usr/bin/env python3
"""compare-with-index.py: do the packages in repo/<arch> link what the same packages in another index linked?

A clean build that lacks a library fails, or its configure script quietly leaves a feature out. The second kind
shows only in the result: the package no longer depends on that library. This compares each built package's so:
dependencies with the ones the package of the same name has in a reference index (Melon's published
Packages.adb, where the desktop was last known to be whole), and lists packages the reference has and the
recipe no longer makes.

usage: scripts/compare-with-index.py INDEX [RECIPE...]     (default: every recipe that has been built)

Differences that come from the C library are left out: musl's libc.so against glibc's libc.so.6, libm.so.6 and
ld-linux, and the libraries that stood in on musl for what glibc has (libintl, libfts, libobstack, libargp).
"""
import glob, os, re, subprocess, sys
HERE = os.path.dirname(os.path.realpath(__file__)); M = os.path.dirname(HERE)
APK = f'{M}/hosttools/bin/apk'
LIBC = re.compile(r'^so:(libc\.so.*|libc\.musl.*|libm\.so\.6|ld-linux.*|libdl\.so\.2|libpthread\.so\.0|librt\.so\.1|libresolv\.so\.2|'
                  r'libutil\.so\.1|libintl\.so\.\d+|libfts\.so\.\d+|libobstack\.so\.\d+|libargp\.so\.\d+|libgcc_s\.so\.1|libcrypt\.so\.\d+|libucontext.*|libexecinfo.*)$')

def read_index(path):
    out = subprocess.run([APK, 'adbdump', path], capture_output=True, text=True).stdout
    pkgs, cur, key = {}, None, None
    for line in out.splitlines():
        m = re.match(r'  - name: (.*)', line)
        if m: cur = pkgs[m.group(1)] = {'origin': m.group(1), 'depends': [], 'provides': []}; key = None; continue
        if cur is None: continue
        m = re.match(r'    origin: (.*)', line)
        if m: cur['origin'] = m.group(1); continue
        m = re.match(r'    (depends|provides|install-if|replaces):', line)
        if m: key = m.group(1); continue
        m = re.match(r'      - (.*)', line)
        if m:
            if key in ('depends', 'provides'): cur[key].append(m.group(1))
            continue
        key = None
    return pkgs

def so(deps): return {re.split(r'[<>=~]', d)[0] for d in deps if d.startswith('so:') and not LIBC.match(re.split(r'[<>=~]', d)[0])}

def main():
    args = sys.argv[1:]
    if not args: sys.exit(__doc__)
    ref = read_index(args.pop(0))
    ours = read_index(f'{M}/repo/x86_64/Packages.adb')
    name = lambda n: n.replace('melon', 'valence')
    ref = {name(p): {**d, 'origin': name(d['origin'])} for p, d in ref.items()}
    built = sorted(os.path.basename(p) for p in glob.glob(f'{M}/repo/x86_64/.state/*'))
    quiet = 0
    for r in (args or built):
        theirs = {p for p, d in ref.items() if d['origin'] == r}
        mine = {p for p, d in ours.items() if d['origin'] == r}
        if not theirs:
            if args: print(f'{r}: not in the reference index')
            continue
        lines = []
        gone = sorted(p for p in theirs - mine if not p.endswith(('-doc', '-lang')))
        if gone: lines.append(f'  packages the reference has and this build does not: {" ".join(gone)}')
        for p in sorted(theirs & mine):
            a, b = so(ref[p]['depends']), so(ours[p]['depends'])
            if a - b: lines.append(f'  {p} no longer links: {" ".join(sorted(x[3:] for x in a - b))}')
            if b - a: lines.append(f'  {p} links in addition: {" ".join(sorted(x[3:] for x in b - a))}')
            pa, pb = so(ref[p]['provides']), so(ours[p]['provides'])
            if pa - pb: lines.append(f'  {p} no longer provides: {" ".join(sorted(x[3:] for x in pa - pb))}')
        if lines: print(f'{r}:'); print('\n'.join(lines))
        else: quiet += 1
    print(f'({quiet} recipes link what the reference linked)')

if __name__ == '__main__':
    main()
