#!/usr/bin/env python3
"""suggest-makedepends.py: what a recipe's makedepends probably are, from records of how it was built before.

Clean builds give a recipe only what it declares (AGENTS.md, "Building"), and most of the recipes that came from
Melon declare little or nothing, because Melon built them one after another in one sysroot. Two records say what
they found there:

  - A package index (Melon's published Packages.adb, or Valence's own repo/<arch>/Packages.adb). The libraries a
    recipe's packages link, their so: dependencies, name the recipes that provide them, and so their -dev packages.
  - For KDE's sources, the .kde-ci.yml in the tarball: the KDE projects the source is built against. Only those
    that gen-kde-recipes.py builds earlier count, since those are the ones Melon's build had.
  - KDE sources with a poqm/ directory install Qt translations and need Qt's lrelease: qt6-qttools-dev.
  - For KDE's and Qt's sources, the find_package() calls in their CMake files: the Qt modules and KDE libraries
    they look for, linked or not (Qt's shader tools and KDE's protocol files are used without being linked, so
    no index shows them). A name is matched to the recipe whose source ships <Name>Config.cmake.in, or to a Qt
    module through QT_COMPONENTS. Again only what gen-kde-recipes.py builds earlier counts.

usage: scripts/suggest-makedepends.py INDEX [RECIPE...]   what each recipe (default: all) should add to its makedepends
       scripts/suggest-makedepends.py INDEX --kde-json FILE   the whole lists for gen-kde-recipes.py's recipes, as JSON

What this cannot see is anything used without being linked: header-only libraries, protocol descriptions, build
tools. A clean build fails on those, or quietly leaves a feature out: compare the built package's dependencies with
the index's (scripts/compare-with-index.py) afterwards.
"""
import importlib.machinery, importlib.util, json, os, re, subprocess, sys, tarfile

HERE = os.path.dirname(os.path.realpath(__file__))
M = os.path.dirname(HERE)

def builder():
    loader = importlib.machinery.SourceFileLoader('valence_build', f'{HERE}/valence-build')
    spec = importlib.util.spec_from_loader('valence_build', loader); mod = importlib.util.module_from_spec(spec)
    loader.exec_module(mod); return mod

def read_index(path):
    """{package: {origin, depends, provides}} from apk adbdump's text"""
    out = subprocess.run([f'{M}/hosttools/bin/apk', 'adbdump', path], capture_output=True, text=True).stdout
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

# what the C library and the compiler bring, and what only a musl system needed
NOT_A_DEPENDENCY = {'musl', 'glibc', 'gcc-runtime', 'libxcrypt-compat', 'musl-fts', 'musl-obstack', 'argp-standalone', 'gettext'}

def kde_project_recipe(project, recipes):
    group, _, name = project.partition('/')
    for cand in ([f'kf6-{name}'] if group == 'frameworks' else []) + [name]:
        if cand in recipes: return cand
    return None

# which Qt module a component named in find_package(Qt6 COMPONENTS ...) comes from
QT_COMPONENTS = {
    'qt6-qtbase': 'Core Gui Widgets DBus Network Xml Test Concurrent Sql PrintSupport OpenGL OpenGLWidgets',
    'qt6-qtdeclarative': 'Qml Quick QuickControls2 QuickWidgets QmlModels QuickTest QmlCore QuickDialogs2 QuickLayouts QuickTemplates2 '
                         'QmlIntegration QuickShapes LabsQmlModels QmlWorkerScript QmlNetwork QuickEffects',
    'qt6-qtsvg': 'Svg SvgWidgets', 'qt6-qtwayland': 'WaylandClient WaylandCompositor', 'qt6-qtshadertools': 'ShaderTools',
    'qt6-qttools': 'LinguistTools UiTools UiPlugin Designer Help', 'qt6-qtmultimedia': 'Multimedia MultimediaWidgets SpatialAudio',
    'qt6-qtpositioning': 'Positioning', 'qt6-qtlocation': 'Location', 'qt6-qt5compat': 'Core5Compat', 'qt6-qtspeech': 'TextToSpeech',
    'qt6-qtwebview': 'WebView WebViewQuick',
}
QT_OF = {c: r for r, cs in QT_COMPONENTS.items() for c in cs.split()}
# CMake package names whose recipe cannot be told from a <Name>Config.cmake.in in its source
CMAKE_NAMES = {'ECM': 'extra-cmake-modules', 'Wayland': 'wayland', 'WaylandProtocols': 'wayland-protocols',
               'PlasmaWaylandProtocols': 'plasma-wayland-protocols', 'PolkitQt6-1': 'polkit-qt-1', 'QCoro6': 'qcoro',
               'Qca-qt6': 'qca', 'Qca': 'qca', 'KF6BreezeIcons': 'kf6-breeze-icons'}
_KEYWORDS = {'REQUIRED', 'COMPONENTS', 'OPTIONAL_COMPONENTS', 'CONFIG', 'NO_MODULE', 'QUIET', 'EXACT', 'MODULE', 'GLOBAL', 'NO_POLICY_SCOPE'}

def find_packages(text):
    """CMake package names a file asks for: find_package(Foo), and Qt6<C>/KF6<C> for the components of Qt6 and KF6"""
    out = set()
    text = re.sub(r'#[^\n]*', '', text)
    for m in re.finditer(r'find_(?:package|dependency)\s*\(([^)]*)\)', text):
        tok = m.group(1).split()
        if not tok: continue
        name = re.sub(r'\$\{(?:QT_MAJOR_VERSION|KF_MAJOR_VERSION|QT_VERSION_MAJOR)\}', '6', tok[0]).strip('"')
        comps = [t for t in tok[1:] if t not in _KEYWORDS and not t.startswith('$') and not t[0].isdigit() and t.isidentifier()]
        if name in ('Qt6', 'KF6'): out |= {name + c for c in comps}
        elif '$' not in name: out.add(name)
    return out

def kde_ci(tarball):
    """From a source tarball: (projects under Dependencies in .kde-ci.yml, whether it has a poqm/ directory,
    the CMake packages it ships a <Name>Config.cmake.in for, the CMake packages its CMake files look for)"""
    projects, poqm, ships, wants = [], False, set(), set()
    try:
        with tarfile.open(tarball) as t:
            for mem in t:
                parts = mem.name.split('/'); base = parts[-1]
                if len(parts) >= 2 and parts[1] == 'poqm': poqm = True
                if base.endswith('Config.cmake.in'): ships.add(base[:-len('Config.cmake.in')])
                if len(parts) == 2 and parts[1] == '.kde-ci.yml':
                    text = t.extractfile(mem).read().decode('utf-8', 'replace')
                    sect = re.split(r'^(?:RuntimeDependencies|Options|Environment)\s*:', text, flags=re.M)[0]
                    projects = re.findall(r"^\s*'?([a-z0-9-]+/[A-Za-z0-9_.+-]+)'?\s*:", sect, flags=re.M)
                elif mem.isfile() and base == 'CMakeLists.txt' and mem.size < 400000 and not any(d in parts for d in ('autotests', 'tests', 'examples', 'test', 'templates', '3rdparty')):
                    wants |= find_packages(t.extractfile(mem).read().decode('utf-8', 'replace'))
    except (OSError, tarfile.TarError):
        pass
    return projects, poqm, ships, wants

def main():
    args = sys.argv[1:]
    if not args or args[0].startswith('-'): sys.exit(__doc__)
    index = read_index(args.pop(0))
    kde_json = None
    if '--kde-json' in args: i = args.index('--kde-json'); kde_json = args[i + 1]; del args[i:i + 2]
    vb = builder(); recipes, origin = vb.load()
    order = subprocess.run([sys.executable, f'{HERE}/gen-kde-recipes.py'], capture_output=True, text=True).stdout.split()
    pos = {n: i for i, n in enumerate(order)}
    name = lambda n: n.replace('melon', 'valence')
    so_owner, by_origin = {}, {}
    for p, d in index.items():
        o = name(d['origin']); by_origin.setdefault(o, []).append(d)
        for pr in d['provides']:
            if pr.startswith('so:'): so_owner[pr[3:].split('=')[0]] = o
    def dev_of(r):
        rec = recipes[r]; devs = [p for p in rec.packages if p.endswith('-dev')]
        if f'{rec.pkgname}-dev' in devs: return f'{rec.pkgname}-dev'
        return devs[0] if devs else rec.pkgname
    def source_of(r):
        src = re.search(r'^source=\(\s*([^\s)]+)', open(f'{M}/recipes/{r}/VALENCEBUILD', encoding='utf-8').read(), re.M)
        return f'{M}/sources/{src.group(1)}' if src and os.path.exists(f'{M}/sources/{src.group(1)}') else None
    scanned = {r: kde_ci(source_of(r)) for r in order if r in recipes and source_of(r)}
    cmake_owner = dict(CMAKE_NAMES)
    for r, (_, _, ships, _) in scanned.items():
        for n in ships: cmake_owner.setdefault(n, r)
    for c, r in QT_OF.items(): cmake_owner['Qt6' + c] = r
    def suggest(r):
        out = set()
        for d in by_origin.get(r, []):
            for dep in d['depends']:
                if not dep.startswith('so:'): continue
                o = so_owner.get(re.split(r'[<>=~]', dep[3:])[0])
                if o and o != r and o not in NOT_A_DEPENDENCY and o in recipes: out.add(dev_of(o))
        if r in scanned:
            projects, poqm, _, wants = scanned[r]
            for pr in projects:
                k = kde_project_recipe(pr, recipes)
                if k and k != r and (k not in pos or pos[k] < pos[r]): out.add(dev_of(k))
            for n in wants:
                k = cmake_owner.get(n)
                if k and k in recipes and k != r and (k not in pos or pos[k] < pos[r]): out.add(dev_of(k))
            if poqm and 'qt6-qttools' in recipes and r != 'qt6-qttools' and pos.get('qt6-qttools', 0) < pos[r]: out.add(dev_of('qt6-qttools'))
        return out
    if kde_json:
        table = {r: sorted(suggest(r)) for r in order if r in recipes}
        json.dump(table, open(kde_json, 'w'), indent=0, sort_keys=True); open(kde_json, 'a').write('\n')
        print(f'{kde_json}: {len(table)} recipes, {sum(map(len, table.values()))} dependencies'); return
    for r in (args or sorted(recipes)):
        if r not in recipes: print(f'{r}: no such recipe'); continue
        if r not in by_origin:
            if args: print(f'{r}: not in the index')
            continue
        add = sorted(suggest(r) - set(recipes[r].makedepends))
        if add or args: print(f'{r}: +{" ".join(add) if add else "(nothing)"}   [declares: {" ".join(recipes[r].makedepends) or "nothing"}]')

if __name__ == '__main__':
    main()
