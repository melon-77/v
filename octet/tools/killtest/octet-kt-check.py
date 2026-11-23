"""Snapshots and checks of a octet root for the kill test (octet-killtest.sh).

usage:
  octet-kt-check.py snap  ROOT OUT.json
      the state of a root: every live entry (outside Programs, var/lib/octet,
      var/cache/octet) with type, mode, owner, size, mtime, SHA-256, inode and
      link target; who owns which path; the Programs folders; the world; the
      files of the lookup index
  octet-kt-check.py crash ROOT PRE.json REF.json
      the state right after a kill: every path is either what it was before
      the upgrade (PRE: same inode for files outside etc/, same content for
      etc/) or what it is after a clean upgrade (REF: same content, and for
      files outside etc/ the inode of the new Programs copy); a path present
      before and after is never missing; nothing else appeared; /bin/sh,
      /usr/lib/libc.so.6 and the dynamic loader exist and work under chroot
  octet-kt-check.py final ROOT REF.json
      the state after the completing run equals the clean upgrade REF:
      every entry, the owners, Programs (versions, Current, metadata), the
      world, the lookup index (canonical: rebuilt by a recovery, it equals the
      clean run's), the hard links; no journal, temporary or .partial left
  octet-kt-check.py crit ROOT              only the /bin/sh, libc, loader checks
  octet-kt-check.py phase ROOT             where an upgrade stopped
  octet-kt-check.py pickfile REF.json SEED VERSION
      a random file (outside etc/) of a random package at VERSION, not the
      last of its package

The last line is "RESULT ok" or "RESULT FAIL <n>".
"""
import hashlib, json, os, stat, subprocess, sys

SKIP = {'Programs', 'var/lib/octet', 'var/cache/octet'}
# appended to by the test packages' scripts: a re-run script adds a line
VOLATILE = {'var/log/octet-kt.log'}
TMP_PREFIX = '.octet.'  # rootfs::tmp_name
CHROOT_ENV = {'PATH': '/usr/sbin:/usr/bin:/sbin:/bin'}


def sha(p):
    h = hashlib.sha256()
    with open(p, 'rb') as f:
        while True:
            b = f.read(1 << 20)
            if not b:
                break
            h.update(b)
    return h.hexdigest()


def entry(full):
    st = os.lstat(full)
    m = st.st_mode
    if stat.S_ISLNK(m):
        return {'t': 'l', 'target': os.readlink(full), 'uid': st.st_uid, 'gid': st.st_gid}
    if stat.S_ISDIR(m):
        return {'t': 'd', 'mode': stat.S_IMODE(m), 'uid': st.st_uid, 'gid': st.st_gid}
    if stat.S_ISREG(m):
        return {'t': 'f', 'mode': stat.S_IMODE(m), 'uid': st.st_uid, 'gid': st.st_gid, 'size': st.st_size,
                'mtime': int(st.st_mtime), 'sha': sha(full), 'ino': st.st_ino, 'nlink': st.st_nlink}
    return {'t': 'o', 'fmt': stat.S_IFMT(m), 'mode': stat.S_IMODE(m), 'uid': st.st_uid, 'gid': st.st_gid,
            'rdev': st.st_rdev}


def same(a, b):
    """Same entry, inode and link count aside."""
    return all(a.get(k) == b.get(k) for k in (set(a) | set(b)) - {'ino', 'nlink'})


def show(e):
    if e is None:
        return 'absent'
    e = dict(e)
    if 'sha' in e:
        e['sha'] = e['sha'][:12]
    if 'mode' in e:
        e['mode'] = oct(e['mode'])
    return ' '.join('%s=%s' % kv for kv in sorted(e.items()))


def walk(root):
    out = {}
    for dp, dns, fns in os.walk(root):
        rel = os.path.relpath(dp, root)
        rel = '' if rel == '.' else rel
        keep = []
        for d in dns:
            p = rel + '/' + d if rel else d
            if p in SKIP:
                continue
            full = os.path.join(dp, d)
            if os.path.islink(full):
                fns.append(d)
            else:
                keep.append(d)
                out[p] = entry(full)
        dns[:] = keep
        for f in fns:
            p = rel + '/' + f if rel else f
            if p in SKIP:
                continue
            out[p] = entry(os.path.join(dp, f))
    return out


def canon(root, rel, follow_last=False):
    """The canonical root-relative path, symlinks resolved inside the root
    (as after chroot), like octet's rootfs::resolve."""
    stack = []
    pending = [c for c in rel.split('/') if c][::-1]
    links = 0
    missing = False
    while pending:
        c = pending.pop()
        if c == '.':
            continue
        if c == '..':
            if stack:
                stack.pop()
            continue
        if not missing:
            cand = '/'.join(stack + [c])
            try:
                st = os.lstat(os.path.join(root, cand))
                if stat.S_ISLNK(st.st_mode) and (pending or follow_last):
                    links += 1
                    if links > 40:
                        raise OSError('too many symlinks: ' + rel)
                    t = os.readlink(os.path.join(root, cand))
                    if t.startswith('/'):
                        stack = []
                    pending.extend([x for x in t.split('/') if x][::-1])
                    continue
            except (FileNotFoundError, NotADirectoryError):
                missing = True
        stack.append(c)
    return '/'.join(stack)


def unescape(s):
    if '\\' not in s:
        return s
    out, i = bytearray(), 0
    b = s.encode('utf-8', 'surrogateescape')
    while i < len(b):
        if b[i:i + 1] != b'\\':
            out += b[i:i + 1]
            i += 1
        elif b[i + 1:i + 2] == b'x':
            out.append(int(b[i + 2:i + 4], 16))
            i += 4
        else:
            out += {b'\\': b'\\', b't': b'\t', b'n': b'\n'}[b[i + 1:i + 2]]
            i += 2
    return out.decode('utf-8', 'surrogateescape')


def programs(root):
    """Programs/<name>: its entries, Current, and the metadata of Current."""
    progs = os.path.join(root, 'Programs')
    res = {}
    for name in sorted(os.listdir(progs)):
        d = os.path.join(progs, name)
        r = {'entries': sorted(os.listdir(d)) if os.path.isdir(d) else ['<not a directory>']}
        cur = os.path.join(d, 'Current')
        if os.path.islink(cur):
            ver = os.readlink(cur)
            r['current'] = ver
            md = os.path.join(d, ver, '.octet')
            r['info'] = sha(os.path.join(md, 'info'))
            r['files'] = sha(os.path.join(md, 'files'))
            sd = os.path.join(md, 'scripts')
            r['scripts'] = {s: sha(os.path.join(sd, s)) for s in sorted(os.listdir(sd))} if os.path.isdir(sd) else {}
        res[name] = r
    return res


def owned(root):
    """canonical path -> [name, version, path as recorded, kind] for every
    entry of every installed package."""
    res = {}
    progs = os.path.join(root, 'Programs')
    for name in sorted(os.listdir(progs)):
        cur = os.path.join(progs, name, 'Current')
        if not os.path.islink(cur):
            continue
        ver = os.readlink(cur)
        with open(os.path.join(progs, name, ver, '.octet', 'files'), encoding='utf-8', errors='surrogateescape') as f:
            for line in f:
                x = line.rstrip('\n').split('\t')
                path = unescape(x[9])
                res[canon(root, path)] = [name, ver, path, x[0]]
    return res


def read(p):
    try:
        with open(p, encoding='utf-8', errors='surrogateescape') as f:
            return f.read()
    except FileNotFoundError:
        return None


def index_files(root):
    """The lookup index (var/lib/octet/index): SHA-256 of every file by its
    path in the index; None when there is none."""
    d = os.path.join(root, 'var/lib/octet/index')
    if not os.path.isdir(d):
        return None
    out = {}
    for dp, dns, fns in os.walk(d):
        for f in fns:
            full = os.path.join(dp, f)
            out[os.path.relpath(full, d)] = sha(full)
    return out


def snap(root):
    return {'entries': walk(root), 'owned': owned(root), 'programs': programs(root),
            'world': read(os.path.join(root, 'var/lib/octet/world')), 'index': index_files(root)}


# ------------------------------------------------------------ critical paths

def elf_interp(path):
    with open(path, 'rb') as f:
        h = f.read(64)
        if h[:4] != b'\x7fELF' or h[4] != 2 or h[5] != 1:
            return None
        phoff = int.from_bytes(h[32:40], 'little')
        phentsize = int.from_bytes(h[54:56], 'little')
        phnum = int.from_bytes(h[56:58], 'little')
        for i in range(phnum):
            f.seek(phoff + i * phentsize)
            ph = f.read(phentsize)
            if int.from_bytes(ph[0:4], 'little') == 3:  # PT_INTERP
                off = int.from_bytes(ph[8:16], 'little')
                size = int.from_bytes(ph[32:40], 'little')
                f.seek(off)
                return f.read(size).rstrip(b'\0').decode()
    return None


def chroot_run(root, argv, expect):
    try:
        r = subprocess.run(['chroot', root] + argv, env=CHROOT_ENV, stdin=subprocess.DEVNULL,
                           stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=20)
        out = r.stdout.decode('utf-8', 'replace')
        ok = r.returncode == 0 and expect in out
        return ok, 'exit %d: %s' % (r.returncode, out.strip().splitlines()[0][:100] if out.strip() else '(no output)')
    except subprocess.TimeoutExpired:
        return False, 'timed out'
    except OSError as e:
        return False, str(e)


def critical(root, viol):
    """/bin/sh, /usr/lib/libc.so.6 and the dynamic loader: present (resolved
    inside the root) and working under chroot."""
    res = []
    shp = canon(root, 'bin/sh', True)
    libc = canon(root, 'usr/lib/libc.so.6', True)
    interp = None
    for label, p in (('/bin/sh', shp), ('/usr/lib/libc.so.6', libc)):
        full = os.path.join(root, p)
        if not os.path.isfile(full) or os.path.islink(full):
            viol.append('critical: %s (-> /%s) is missing' % (label, p))
        elif label == '/bin/sh':
            interp = elf_interp(full)
    if interp:
        ldp = canon(root, interp, True)
        full = os.path.join(root, ldp)
        if not os.path.isfile(full) or os.path.islink(full):
            viol.append('critical: the dynamic loader %s (-> /%s) is missing' % (interp, ldp))
    else:
        interp = '/lib64/ld-linux-x86-64.so.2'
    runs = [
        (['/bin/sh', '-c', 'echo ok'], 'ok'),
        (['/usr/lib/libc.so.6'], 'GNU C Library'),
        ([interp, '--version'], 'ld.so'),
        ([interp, '--list', '/usr/bin/ls'], 'libc.so.6'),
        (['/usr/bin/ls', '/'], 'usr'),
        (['/bin/bash', '-c', 'echo bash $BASH_VERSION'], 'bash 5.'),
    ]
    for argv, expect in runs:
        ok, what = chroot_run(root, argv, expect)
        res.append('%s %s: %s' % ('ok  ' if ok else 'FAIL', ' '.join(argv), what))
        if not ok:
            viol.append('critical: chroot %s failed (%s)' % (' '.join(argv), what))
    return res


# ------------------------------------------------------------ checks

def phase(root):
    """Where the upgrade was when it stopped."""
    progs = os.path.join(root, 'Programs')
    cur_new = cur_old = partial = 0
    multi = 0
    for name in os.listdir(progs):
        d = os.path.join(progs, name)
        ents = os.listdir(d)
        partial += sum(e.endswith('.partial') for e in ents)
        vers = [e for e in ents if e != 'Current' and not e.startswith('.')]
        if len(vers) > 1:
            multi += 1
        c = os.path.join(d, 'Current')
        if os.path.islink(c):
            if os.readlink(c) == '9999-r0':
                cur_new += 1
            else:
                cur_old += 1
    j = os.path.exists(os.path.join(root, 'var/lib/octet/journal'))
    done = 0
    if j:
        with open(os.path.join(root, 'var/lib/octet/journal')) as f:
            done = sum(1 for l in f if l.startswith('done '))
    return 'journal=%d(done %d) current: %d new %d old; %d with two versions; %d .partial' % (
        j, done, cur_new, cur_old, multi, partial)


def crash(root, pre, ref):
    viol, notes = [], []
    PE, RE = pre['entries'], ref['entries']
    L = walk(root)
    checked = 0
    progress = {}  # package -> [paths still old, paths already new]
    for p in sorted(set(PE) | set(RE)):
        if p in VOLATILE:
            continue
        a, b, l = PE.get(p), RE.get(p), L.get(p)
        if l is None:
            if a is not None and b is not None:
                viol.append('missing: /%s (there before and after the upgrade; before: %s)' % (p, show(a)))
            continue
        checked += 1
        is_old = a is not None and same(l, a)
        is_new = b is not None and same(l, b)
        if l['t'] == 'f' and not p.startswith('etc/'):
            # the old file is the very inode that was there; the new one is
            # the inode of its copy in Programs/<owner>/<new version>
            if is_old and l['ino'] != a['ino']:
                is_old = False
            if is_new:
                own = ref['owned'].get(p)
                if own and own[3] in ('f', 'h'):
                    try:
                        pino = os.lstat(os.path.join(root, 'Programs', own[0], own[1], own[2])).st_ino
                    except OSError:
                        pino = None
                    if l['ino'] != pino:
                        is_new = False
                        if not is_old:
                            notes.append('/%s has the new content but is not a hard link to Programs/%s/%s' % (p, own[0], own[1]))
        if not (is_old or is_new):
            viol.append('neither old nor new: /%s\n      now    %s\n      before %s\n      after  %s' % (
                p, show(l), show(a), show(b)))
        elif is_old != is_new and p in ref['owned']:
            n = ref['owned'][p][0]
            progress.setdefault(n, [0, 0])[0 if is_old else 1] += 1
    # every Current names a complete version folder (else octet cannot open
    # the root)
    progs = os.path.join(root, 'Programs')
    for name in sorted(os.listdir(progs)):
        c = os.path.join(progs, name, 'Current')
        if os.path.islink(c):
            d = os.path.join(progs, name, os.readlink(c), '.octet')
            if not (os.path.isfile(os.path.join(d, 'info')) and os.path.isfile(os.path.join(d, 'files'))):
                viol.append('Programs/%s/Current -> %s is not a complete version' % (name, os.readlink(c)))
        elif name in pre['programs'] and 'current' in pre['programs'][name]:
            viol.append('Programs/%s/Current is missing' % name)
    for p, e in sorted(L.items()):
        if p in PE or p in RE or p in VOLATILE:
            continue
        if os.path.basename(p).startswith(TMP_PREFIX):
            notes.append('temporary file of the interrupted switch: /%s' % p)
        else:
            viol.append('unexpected new entry: /%s %s' % (p, show(e)))
    crit = critical(root, viol)
    mid = ['%s (%d old, %d new)' % (n, o, w) for n, (o, w) in sorted(progress.items()) if o and w]
    new_pkgs = sum(1 for o, w in progress.values() if w and not o)
    crit.insert(0, 'packages wholly new %d, half placed: %s' % (new_pkgs, ', '.join(mid) or 'none'))
    return viol, notes, checked, crit


def final(root, ref):
    viol, notes = [], []
    now = snap(root)
    E, RE = now['entries'], ref['entries']
    for p in sorted(set(E) | set(RE)):
        if p in VOLATILE:
            continue
        a, b = E.get(p), RE.get(p)
        if a is None or b is None or not same(a, b):
            viol.append('differs from the clean upgrade: /%s\n      now   %s\n      clean %s' % (p, show(a), show(b)))
    if now['owned'] != ref['owned']:
        ks = sorted(k for k in set(now['owned']) | set(ref['owned']) if now['owned'].get(k) != ref['owned'].get(k))
        viol.append('owners differ for %d paths, e.g. %s' % (len(ks), ', '.join(
            '/%s: %s vs %s' % (k, now['owned'].get(k), ref['owned'].get(k)) for k in ks[:5])))
    for name in sorted(set(now['programs']) | set(ref['programs'])):
        a, b = now['programs'].get(name), ref['programs'].get(name)
        if a != b:
            viol.append('Programs/%s differs: %s vs clean %s' % (name, a, b))
    if now['world'] != ref['world']:
        viol.append('world differs: %r vs %r' % (now['world'], ref['world']))
    a, b = now['index'], ref['index']
    if a is None:
        viol.append('no lookup index (var/lib/octet/index)')
    elif a != b:
        b = b or {}
        ks = sorted(k for k in set(a) | set(b) if a.get(k) != b.get(k))
        viol.append('the lookup index differs from the clean run in %d files, e.g. %s' % (len(ks), ', '.join(ks[:5])))
    # hard links
    nlinks = 0
    for p, (name, ver, path, kind) in sorted(now['owned'].items()):
        e = E.get(p)
        if kind not in ('f', 'h') or e is None or e['t'] != 'f':
            continue
        pst = os.lstat(os.path.join(root, 'Programs', name, ver, path))
        if p.startswith('etc/'):
            if e['ino'] == pst.st_ino or e['nlink'] != 1:
                viol.append('/%s is not a separate copy (nlink %d)' % (p, e['nlink']))
        elif e['ino'] != pst.st_ino:
            viol.append('/%s is not a hard link to Programs/%s/%s' % (p, name, ver))
        else:
            nlinks += 1
    st = os.path.join(root, 'var/lib/octet')
    for x in sorted(os.listdir(st)):
        if x in ('world', 'lock', 'index'):
            continue
        if x == 'exec' and not os.listdir(os.path.join(st, x)):
            continue
        # what each repository's last accepted index offered (rollback
        # protection): one file per repository, named by a SHA-256
        if x == 'indexes' and all(len(f) == 64 and all(c in '0123456789abcdef' for c in f)
                                  for f in os.listdir(os.path.join(st, x))):
            continue
        viol.append('left in var/lib/octet: %s' % x)
    for dp, dns, fns in os.walk(os.path.join(root, 'Programs')):
        for f in dns + fns:
            if f.startswith(TMP_PREFIX) or f.startswith('.Current') or f.endswith('.partial'):
                viol.append('left in Programs: %s' % os.path.join(dp, f))
        if os.path.basename(dp) == '.octet':
            dns[:] = []
    notes.append('%d files outside etc/ are hard links to their Programs copy' % nlinks)
    return viol, notes


def main():
    cmd = sys.argv[1]
    if cmd == 'snap':
        with open(sys.argv[3], 'w') as f:
            json.dump(snap(sys.argv[2]), f)
        return 0
    if cmd == 'phase':
        print(phase(sys.argv[2]))
        return 0
    if cmd == 'pickfile':
        # a random file outside etc/ of a random upgraded package with at
        # least two such files, not its last one (in placing order)
        import random
        ref = json.load(open(sys.argv[2]))
        random.seed(sys.argv[3])
        files = {}
        for p, (name, ver, path, kind) in ref['owned'].items():  # in record order
            if kind in ('f', 'h') and not p.startswith('etc/') and ver == sys.argv[4]:
                files.setdefault(name, []).append(p)
        name = random.choice(sorted(n for n, f in files.items() if len(f) >= 2))
        print(random.choice(files[name][:-1]))
        return 0
    if cmd == 'crit':
        viol = []
        for l in critical(sys.argv[2], viol):
            print('  ' + l)
        print('RESULT ' + ('ok' if not viol else 'FAIL %d' % len(viol)))
        return 0 if not viol else 1
    root = sys.argv[2]
    if cmd == 'crash':
        pre = json.load(open(sys.argv[3]))
        ref = json.load(open(sys.argv[4]))
        print('  stopped at: ' + phase(root))
        viol, notes, checked, crit = crash(root, pre, ref)
        print('  %d paths checked; %d violations' % (checked, len(viol)))
        for l in crit:
            print('  ' + l)
    else:
        ref = json.load(open(sys.argv[3]))
        viol, notes = final(root, ref)
    for n in notes[:10]:
        print('  note: ' + n)
    if len(notes) > 10:
        print('  note: ... %d more notes' % (len(notes) - 10))
    for v in viol[:25]:
        print('  VIOLATION ' + v)
    if len(viol) > 25:
        print('  ... %d more violations' % (len(viol) - 25))
    print('RESULT ' + ('ok' if not viol else 'FAIL %d' % len(viol)))
    return 0 if not viol else 1


if __name__ == '__main__':
    sys.exit(main())
