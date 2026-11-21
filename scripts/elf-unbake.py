#!/usr/bin/env python3
"""elf-unbake.py [--check] SYSROOT INTERP DIR...

For recipes built with options=(runhere) (AGENTS.md, rule 18). Their programs were linked to start through the
sysroot's loader and to look for libraries in the sysroot, so that the build could run them on the build machine:
the loader's path under SYSROOT is written into them as the program interpreter, and SYSROOT/usr/lib as an RPATH.
Neither may reach a package. For every ELF file under DIR this puts INTERP (the system's loader, for instance
/lib64/ld-linux-x86-64.so.2) in place of an interpreter under SYSROOT, and takes every RPATH and RUNPATH entry under
SYSROOT out (the whole tag, when nothing else is in it).

--check changes nothing and exits 1 if a file still has either: the builder runs it on every package it makes.
64-bit little-endian ELF only; anything else that would need changing is an error, not a silent pass.
"""
import os, struct, sys

PT_LOAD, PT_DYNAMIC, PT_INTERP = 1, 2, 3
DT_NULL, DT_STRTAB, DT_RPATH, DT_RUNPATH = 0, 5, 15, 29


def cstr(data, off):
    end = data.index(b'\0', off)
    return bytes(data[off:end])


def work(path, sysroot, interp, check):
    """returns a list of what was (or, with check, would have to be) changed"""
    with open(path, 'rb') as f: data = bytearray(f.read())
    if data[:4] != b'\x7fELF': return []
    sr = sysroot.encode()
    if sr not in data: return []                       # nothing of the sysroot's anywhere in the file
    if data[4] != 2 or data[5] != 1: raise SystemExit(f'{path}: names {sysroot} and is not 64-bit little-endian ELF')
    e_phoff, e_shoff = struct.unpack_from('<QQ', data, 32)
    e_phentsize, e_phnum, e_shentsize, e_shnum = struct.unpack_from('<HHHH', data, 54)
    ph = [(e_phoff + i * e_phentsize,) + struct.unpack_from('<IIQQQQQQ', data, e_phoff + i * e_phentsize) for i in range(e_phnum)]
    done = []

    def offset_of(vaddr):
        for _, t, _, off, va, _, filesz, _, _ in ph:
            if t == PT_LOAD and va <= vaddr < va + filesz: return vaddr - va + off
        raise SystemExit(f'{path}: address {vaddr:#x} is in no loaded segment')

    for pos, t, flags, off, va, pa, filesz, memsz, align in ph:
        if t == PT_INTERP:
            cur = bytes(data[off:off + filesz]).rstrip(b'\0')
            if cur.startswith(sr):
                done.append(f'interpreter {cur.decode()}')
                if check: continue
                new = interp.encode() + b'\0'
                if len(new) > filesz: raise SystemExit(f'{path}: {interp} does not fit where {cur.decode()} is')
                data[off:off + filesz] = new + b'\0' * (filesz - len(new))
                struct.pack_into('<QQ', data, pos + 32, len(new), len(new))          # p_filesz, p_memsz
                for i in range(e_shnum if e_shoff else 0):                         # the .interp section, if the file has sections
                    sh = e_shoff + i * e_shentsize
                    if struct.unpack_from('<Q', data, sh + 24)[0] == off and struct.unpack_from('<Q', data, sh + 32)[0] == filesz:
                        struct.pack_into('<Q', data, sh + 32, len(new))
        elif t == PT_DYNAMIC:
            n = filesz // 16
            dyn = [list(struct.unpack_from('<qQ', data, off + i * 16)) for i in range(n)]
            strtab = next((v for tag, v in dyn if tag == DT_STRTAB), None)
            if strtab is None: continue
            stroff = offset_of(strtab)
            keep = []
            for tag, val in dyn:
                if tag in (DT_RPATH, DT_RUNPATH):
                    cur = cstr(data, stroff + val); parts = cur.split(b':')
                    left = [p for p in parts if not p.startswith(sr)]
                    if left != parts:
                        done.append(f'{"RPATH" if tag == DT_RPATH else "RUNPATH"} {cur.decode()}')
                        if check: keep.append([tag, val]); continue
                        if not left: continue                                      # the whole tag goes
                        new = b':'.join(left)                                      # shorter: written over the old string
                        data[stroff + val:stroff + val + len(cur)] = new + b'\0' * (len(cur) - len(new))
                keep.append([tag, val])
            if not check and len(keep) != len(dyn):
                keep += [[DT_NULL, 0]] * (len(dyn) - len(keep))
                for i, (tag, val) in enumerate(keep): struct.pack_into('<qQ', data, off + i * 16, tag, val)
    if done and not check:
        st = os.stat(path)
        with open(path, 'wb') as f: f.write(data)
        os.chmod(path, st.st_mode & 0o7777)
    return done


def main():
    a = sys.argv[1:]; check = bool(a) and a[0] == '--check'
    if check: a = a[1:]
    if len(a) < 3: sys.exit(__doc__)
    sysroot, interp, dirs = a[0].rstrip('/'), a[1], a[2:]
    found = 0
    for d in dirs:
        for root, _, files in os.walk(d):
            for name in files:
                p = os.path.join(root, name)
                if os.path.islink(p) or not os.path.isfile(p): continue
                for what in work(p, sysroot, interp, check):
                    found += 1
                    print(('still there: ' if check else 'taken out: ') + f'{os.path.relpath(p, d)}: {what}')
    return 1 if check and found else 0


if __name__ == '__main__': sys.exit(main())
