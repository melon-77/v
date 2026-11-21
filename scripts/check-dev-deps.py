#!/usr/bin/env python3
"""check-dev-deps.py [-v] [PACKAGE...]

Does every package that ships CMake package files bring along what those files name?

A project that says find_package(Plasma) gets PlasmaConfig.cmake, which says find_dependency(KF6KirigamiPlatform),
and PlasmaTargets.cmake, which names include directories and library files. If the package holding those is not
among what libplasma-dev depends on, the project fails to configure in the builder's clean sysroot, far from the
recipe that is wrong. This reads the built packages in repo/x86_64 and reports, for each package:

  - find_dependency(X) where no package in its dependencies ships X's CMake files, and a pkg-config module asked
    for there (pkg_check_modules) whose .pc file none of them ships,
  - an include directory or an imported file (a library, a program) that no package in its dependencies ships,
  - and, for every package, a library link (usr/lib/libx.so -> libx.so.1) whose library is in a package it does
    not depend on: the linker follows the link and finds nothing.

What no built package ships at all (find modules such as Threads or ZLIB that come with CMake, static libraries that
are not packaged) is listed only with -v: the check cannot tell whether those are needed.

The builder already adds the -dev packages of the libraries a recipe's libraries link, and pkg-config Requires.
What is left is what nothing links: headers used alone, and libraries that live in a sister package. Those go in
the recipe (depends_<name>_dev=) or the generator's table (DEV_MORE in gen-kde-recipes.py).

Exit status 1 if anything is reported without -v.
"""
import concurrent.futures, glob, importlib.machinery, importlib.util, os, re, shutil, subprocess, sys, tempfile

M = os.path.dirname(os.path.dirname(os.path.abspath(__file__))) if os.path.basename(os.path.dirname(os.path.abspath(__file__))) == 'scripts' else '/root/valence'
APK = f'{M}/hosttools/bin/apk'
REPO = f'{M}/repo/x86_64'
CMAKE_ROOTS = ('usr/lib/cmake/', 'usr/lib64/cmake/', 'usr/share/cmake/')


def dump(path):
    """name, version, origin, depends, provides, directories and files of one package file"""
    out = subprocess.run([APK, 'adbdump', path], capture_output=True, text=True, errors='replace').stdout
    p = {'file': path, 'depends': [], 'provides': [], 'files': set(), 'dirs': set(), 'links': {}, 'name': '', 'origin': ''}
    section = lst = None; d = last = ''
    for line in out.split('\n'):
        if line.startswith('info:'): section = 'info'; continue
        if line.startswith('paths:'): section = 'paths'; continue
        if line and not line.startswith(' ') and not line.startswith('#'): section = None; continue
        if section == 'info':
            m = re.match(r'  ([a-z-]+): ?(.*)$', line)
            if m:
                lst = m.group(1) if m.group(1) in ('depends', 'provides') else None
                if m.group(1) in ('name', 'version', 'origin'): p[m.group(1)] = m.group(2)
            elif lst and line.startswith('    - '): p[lst].append(line[6:].strip())
        elif section == 'paths':
            if line.startswith('  - name: '): d = line[10:].strip(); p['dirs'].add(d)
            elif line.startswith('  - '): d = ''
            elif line.startswith('      - name: '): last = (d + '/' if d else '') + line[14:].strip(); p['files'].add(last)
            elif line.startswith('        target: ') and last:
                try: p['links'][last] = bytes.fromhex(line[16:].strip())[2:].decode()      # two bytes of file type, then the path
                except ValueError: pass
    return p


def norm(path):
    """a path in a CMake file -> a path in a package, or None when it does not point into the system"""
    path = path.strip().strip('"')
    path = re.sub(r'^\$\{[A-Za-z_]+\}/', 'usr/', path)                     # ${_IMPORT_PREFIX}/, ${PACKAGE_PREFIX_DIR}/
    path = re.sub(r'^/root/valence/sysroot/', '', path)                    # a path found in the sysroot at build time
    path = re.sub(r'^/usr/', 'usr/', path)
    if not path.startswith('usr/') or '$' in path: return None
    return os.path.normpath(path.replace('usr/lib64/', 'usr/lib/'))


def live(text):
    """the text without the blocks a shared build never reaches: if(NOT ON) ... endif(), with the ifs inside them"""
    out, pos, depth, dead = [], 0, 0, None
    for m in re.finditer(r'(?im)^[ \t]*(if|else|elseif|endif)[ \t]*\(([^\n]*)\)[ \t]*$', text):
        word, cond = m.group(1).lower(), m.group(2).strip()
        if dead is None: out.append(text[pos:m.end()])
        pos = m.end()
        if word == 'if':
            depth += 1
            if dead is None and re.fullmatch(r'(?i)NOT\s+(ON|TRUE|1)|OFF|FALSE|0', cond): dead = depth
        elif word == 'endif':
            if dead == depth: dead = None
            depth -= 1
        elif dead == depth: dead = None                # the else of a dead if is live
    if dead is None: out.append(text[pos:])
    return ''.join(out)


def main():
    verbose = '-v' in sys.argv[1:]
    only = set(a for a in sys.argv[1:] if a != '-v')
    newest = {}
    for f in glob.glob(f'{REPO}/*.apk'):
        base = re.sub(r'-[0-9][^-]*-r[0-9]+\.apk$', '', os.path.basename(f))
        if base not in newest or os.path.getmtime(f) > os.path.getmtime(newest[base]): newest[base] = f
    with concurrent.futures.ThreadPoolExecutor(12) as ex: pkgs = {p['name']: p for p in ex.map(dump, newest.values()) if p['name']}
    provider = {}
    for p in pkgs.values():
        for pr in p['provides']: provider.setdefault(re.split(r'[<>=~]', pr)[0], p['name'])
    cmake_owner = {}
    for p in pkgs.values():
        for d in p['dirs']:
            for root in CMAKE_ROOTS:
                if d.startswith(root) and '/' not in d[len(root):] and d[len(root):]: cmake_owner.setdefault(d[len(root):].lower(), set()).add(p['name'])
            m = re.match(r'usr/share/([^/]+)/cmake$', d)                    # usr/share/ECM/cmake
            if m: cmake_owner.setdefault(m.group(1).lower(), set()).add(p['name'])
    loader = importlib.machinery.SourceFileLoader('sm', f'{M}/scripts/suggest-makedepends.py')
    spec = importlib.util.spec_from_loader('sm', loader); sm = importlib.util.module_from_spec(spec)
    try: loader.exec_module(sm); find_modules = {k.lower(): v for k, v in sm.CMAKE_NAMES.items()}
    except Exception: find_modules = {}

    def closure(name):
        seen, todo = set(), [name]
        while todo:
            n = todo.pop()
            if n in seen or n not in pkgs: continue
            seen.add(n)
            for d in pkgs[n]['depends']:
                d = re.split(r'[<>=~]', d.lstrip('!'))[0]
                todo.append(d if d in pkgs else provider.get(d, ''))
        return seen

    def holders(path, kind):
        return sorted(p['name'] for p in pkgs.values() if path in p[kind])

    tmp = tempfile.mkdtemp(prefix='check-dev-', dir=f'{M}/work')
    report = {}
    try:
        for name in sorted(pkgs):
            p = pkgs[name]
            if only and name not in only: continue
            cm = [f for f in p['files'] if f.endswith('.cmake') and f.startswith(CMAKE_ROOTS)]
            links = {f: t for f, t in p['links'].items() if re.match(r'usr/lib/[^/]+\.so$', f)}
            if not cm and not links: continue
            have = closure(name)
            dirs = set().union(*(pkgs[n]['dirs'] for n in have)); files = set().union(*(pkgs[n]['files'] for n in have))
            miss = report.setdefault(name, {})
            for f, t in links.items():
                target = os.path.normpath(os.path.join(os.path.dirname(f), t)) if not t.startswith('/') else t.lstrip('/')
                if target not in files:
                    h = holders(target, 'files')
                    miss.setdefault(' or '.join(h) if h else f'?? {target}', set()).add(f'link {f} -> {t}')
            if not cm: continue
            dest = f'{tmp}/{name}'; os.makedirs(dest)
            subprocess.run([APK, 'extract', '--allow-untrusted', '--destination', dest, p['file']], capture_output=True)
            for f in cm:
                try: text = open(f'{dest}/{f}', encoding='utf-8', errors='replace').read()
                except OSError: continue
                text = re.sub(r'(?m)#.*$', '', text)
                text = live(text)
                short = os.path.basename(f)
                if short.startswith('Find'): continue          # a find module (CMake's own FindPkgConfig.cmake): not a package's own files
                if short.endswith('Config.cmake') or short.endswith('-config.cmake') or 'Dependencies' in short:
                    for m in re.finditer(r'find_dependency\s*\(\s*([A-Za-z0-9_+.-]+)([^)]*)\)', text):
                        pk, rest = m.group(1), m.group(2)
                        if re.search(r'\bQUIET\b', rest): continue       # asked for quietly: used if there, not needed
                        names = [pk]
                        cmpts = re.search(r'COMPONENTS\s+([A-Za-z0-9_ \n]+)', rest)
                        if cmpts and pk in ('Qt6', 'KF6'): names = [pk + c for c in cmpts.group(1).split() if c not in ('REQUIRED', 'OPTIONAL_COMPONENTS', 'CONFIG', 'QUIET')]
                        for n in names:
                            owners = cmake_owner.get(n.lower())
                            if owners is None and n.lower() in find_modules:
                                fm = find_modules[n.lower()]; owners = set(fm.split()) if isinstance(fm, str) else set(fm)
                            if owners is None: miss.setdefault(f'?? {n}', set()).add(f'find_dependency in {short}: nothing built ships it (a find module?)'); continue
                            if not (owners & have): miss.setdefault(' or '.join(sorted(owners)), set()).add(f'find_dependency({n}) in {short}')
                    # pkg_check_modules(X REQUIRED libnm>=1.4.0) in a Config file: the module's .pc file must come along
                    for m in re.finditer(r'pkg_(?:check_modules|search_module)\s*\(\s*\w+\s+([^)]*)\)', text):
                        words = m.group(1).split()
                        if 'QUIET' in words and 'REQUIRED' not in words: continue
                        for w in words:
                            if w in ('REQUIRED', 'QUIET', 'IMPORTED_TARGET', 'GLOBAL', 'NO_CMAKE_PATH', 'NO_CMAKE_ENVIRONMENT_PATH'): continue
                            mod = re.split(r'[<>=]', w.strip('"'))[0]
                            if not mod or '$' in mod: continue
                            owner = provider.get('pc:' + mod)
                            if owner is None: miss.setdefault(f'?? pc:{mod}', set()).add(f'pkg-config module in {short}')
                            elif owner not in have: miss.setdefault(owner, set()).add(f'pkg-config module {mod} in {short}')
                for m in re.finditer(r'INTERFACE_INCLUDE_DIRECTORIES\s+"([^"]*)"', text):
                    for part in m.group(1).split(';'):
                        d = norm(part)
                        if d and d not in dirs:
                            h = holders(d, 'dirs')
                            miss.setdefault(' or '.join(h) if h else f'?? {d}', set()).add(f'include directory {d} in {short}')
                for m in re.finditer(r'(?:IMPORTED_LOCATION[_A-Z]*|IMPORTED_SONAME[_A-Z]*)\s+"([^"]*)"|"((?:\$\{[A-Za-z_]+\}|/usr|/root/valence/sysroot/usr)/lib(?:64)?/lib[^";]+\.so[0-9.]*)"', text):
                    raw = m.group(1) or m.group(2)
                    if 'SONAME' in m.group(0)[:20]: continue
                    fpath = norm(raw)
                    if fpath and fpath not in files:
                        h = holders(fpath, 'files')
                        miss.setdefault(' or '.join(h) if h else f'?? {fpath}', set()).add(f'file {fpath} in {short}')
    finally:
        shutil.rmtree(tmp, ignore_errors=True)
    bad = unknown = 0
    for name in sorted(report):
        needs = sorted(n for n in report[name] if verbose or not n.startswith('?? '))
        unknown += sum(1 for n in report[name] if n.startswith('?? '))
        if not needs: continue
        print(f'{name}  (recipe {pkgs[name]["origin"]})')
        for need in needs:
            bad += not need.startswith('?? ')
            print(f'    needs {need}:  ' + '; '.join(sorted(report[name][need]))[:260])
    print(f'{len(report)} packages looked at: {bad} things named that the package does not bring'
          + (f', and {unknown} that no built package ships' + ('' if verbose else ' (-v lists them)') if unknown else ''))
    return 1 if bad else 0


if __name__ == '__main__': sys.exit(main())
