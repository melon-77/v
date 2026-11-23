"""apk mkpkg arguments that rebuild a package as another version.

usage: oct dump --json PKG.apk | python3 -I catalyst-kt-pkgargs.py NEWVER SCRIPTDIR NAME1,NAME2,...

Prints one argument per line: --info=KEY:VALUE for the package's metadata
(version replaced by NEWVER; a dependency "X=OLD" on another rebuilt package
X becomes "X=NEWVER"), --script TYPE:FILE for each of its scripts (written
to SCRIPTDIR) and --trigger GLOB for each trigger.
"""
import json, os, re, sys

newver, sdir, rebuilt = sys.argv[1], sys.argv[2], set(sys.argv[3].split(','))
d = json.load(sys.stdin)
info = d['info']
out = []
for k in ('name', 'arch', 'description', 'license', 'origin', 'maintainer', 'url'):
    if info.get(k):
        out.append('--info=%s:%s' % (k, info[k]))
out.append('--info=version:%s' % newver)
if info.get('provider-priority'):
    out.append('--info=provider-priority:%d' % info['provider-priority'])
if d.get('replaces-priority'):
    out.append('--info=replaces-priority:%d' % d['replaces-priority'])


def fix(dep):
    m = re.match(r'^(!?)([^<>=~!]+)(=|>=|<=|<|>|~|><)?(.*)$', dep)
    if m and m.group(2) in rebuilt and m.group(3) in ('=', '~'):
        return '%s%s=%s' % (m.group(1), m.group(2), newver)
    return dep


for k in ('depends', 'provides', 'replaces', 'install-if'):
    if info.get(k):
        out.append('--info=%s:%s' % (k, ' '.join(fix(x) if k == 'depends' else x for x in info[k])))
os.makedirs(sdir, exist_ok=True)
for typ, body in sorted(d.get('scripts', {}).items()):
    p = os.path.join(sdir, '%s.%s' % (info['name'], typ))
    with open(p, 'w') as f:
        f.write(body)
    os.chmod(p, 0o755)
    out.append('--script=%s:%s' % (typ, p))
for t in d.get('triggers', []):
    out.append('--trigger=%s' % t)
print('\n'.join(out))
