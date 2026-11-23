#!/bin/bash
# Builds the kill test's upgrade repository: ~27 packages of valence-base
# rebuilt as version 9999-r0 (same files, a few deliberate changes), signed
# with the throwaway key /root/catalyst-test/keys/catalyst-test.rsa.
#
# Deliberate changes in the 9999-r0 versions (beyond every file getting a
# new inode, since every package is replaced):
#   valence-base:   etc/fstab changed (the test's "admin" changes it too, so
#                   it is kept and fstab.catalyst-new written); a second etc file
#                   changed (replaced); one usr/share file removed; a new
#                   directory usr/share/catalyst-kt with a file added
#   valence-layout -> glibc: the symlink lib64 -> usr/lib (the path of the
#                   dynamic loader, /lib64/ld-linux-x86-64.so.2) moves from
#                   valence-layout to glibc
#   gzip:           usr/bin/zless content changed; usr/bin/zcmp turns from a
#                   regular file into a symlink (zdiff)
#   scripts:        post-upgrade scripts that log to /var/log/catalyst-kt.log in
#                   glibc, coreutils, util-linux, shadow, valence-base, gzip,
#                   and a pre-upgrade one in ncurses-libs (more scripts =
#                   more syncfs calls = a longer switch to kill in)
set -eu
APK=/root/valence/hosttools/bin/apk
V=${KT_OCT:-/root/catalyst-target/release/oct}   # the binary under test (only its dump-index is used)
R=/root/valence/repo/x86_64
K=/root/valence/keys/trusted
KD=/root/catalyst-test/keys
W=/root/catalyst-test/kt-build
R3=/root/catalyst-test/kt-repo/x86_64
S=$(cd "$(dirname "$(readlink -f "$0")")" && pwd)   # this folder
NEWVER=9999-r0
PKGS="glibc busybox bash coreutils valence-base valence-layout util-linux shadow ncurses-libs ncurses-terminfo-base
libgcc pcre2 grep sed findutils diffutils gzip tar less procps-ng libproc2 runit zlib libacl libattr iproute2 libmnl"
LOGGED="glibc coreutils util-linux shadow valence-base gzip"

for d in $W /root/catalyst-test/kt-repo; do
    grep -q " $d" /proc/mounts && { echo "MOUNTED $d"; exit 1; }
    [ -e $d ] && rm -rf --one-file-system $d
done
mkdir -p $KD/trusted $W/pkgs $W/scripts $R3
if [ ! -f $KD/catalyst-test.rsa ]; then
    openssl genrsa -out $KD/catalyst-test.rsa 2048 2>/dev/null
    openssl rsa -in $KD/catalyst-test.rsa -pubout -out $KD/trusted/catalyst-test.rsa.pub 2>/dev/null
fi
[ -f $KD/trusted/valence-signing.rsa.pub ] || cp $K/valence-signing.rsa.pub $KD/trusted/

$V --keys-dir $K dump-index --json $R/Packages.adb > $W/index.json
LIST=$(echo $PKGS | tr ' ' ',')
# versions in the original index; pinned dependencies of other packages on
# the rebuilt ones would make the upgrade unsolvable
python3 -I - $W/index.json "$LIST" > $W/versions <<'EOF'
import json, re, sys
pk = json.load(open(sys.argv[1]))['packages']
rebuilt = set(sys.argv[2].split(','))
ver = {}
for p in pk:
    ver.setdefault(p['name'], p['version'])
for n in sorted(rebuilt):
    print(n, ver[n])
for p in pk:
    if p['name'] in rebuilt:
        continue
    for dep in p.get('depends', []):
        m = re.match(r'^!?([^<>=~!]+)(=|<|<=|~)', dep)
        if m and m.group(1) in rebuilt:
            print('# pinned by', p['name'], dep, file=sys.stderr)
EOF
cat $W/versions

pkgfile() { echo "$R/$1-$(awk -v n=$1 '$1==n{print $2}' $W/versions).apk"; }

for n in $PKGS; do
    f=$(pkgfile $n)
    mkdir $W/pkgs/$n
    $APK extract --keys-dir $K --destination $W/pkgs/$n $f
done

# --- deliberate changes
VB=$W/pkgs/valence-base
ETC_B=$(cd $VB && ls etc | grep -vx 'fstab\|passwd\|group\|shadow\|shells\|apk\|profile.d\|hostname' | while read f; do [ -f etc/$f ] && echo etc/$f; done | head -1)
RM_FILE=$(cd $VB && find usr/share -type f | sort | head -1)
echo "valence-base: changed etc/fstab and $ETC_B; removed $RM_FILE; added usr/share/catalyst-kt/added"
echo "# valence-base $NEWVER: new default" >> $VB/etc/fstab
echo "# valence-base $NEWVER: new line" >> $VB/$ETC_B
rm $VB/$RM_FILE
mkdir -p $VB/usr/share/catalyst-kt
echo "added by $NEWVER" > $VB/usr/share/catalyst-kt/added
echo "$ETC_B" > $W/etc_b; echo "$RM_FILE" > $W/rm_file

GL=$W/pkgs/glibc; VL=$W/pkgs/valence-layout
[ "$(readlink $VL/lib64)" = "usr/lib" ] || { echo "valence-layout's lib64 is not -> usr/lib: $(readlink $VL/lib64)"; exit 1; }
rm $VL/lib64
ln -s usr/lib $GL/lib64
touch -h -r $VL/usr $GL/lib64

GZ=$W/pkgs/gzip
[ -f $GZ/usr/bin/zless ] && [ -f $GZ/usr/bin/zcmp ] && [ -f $GZ/usr/bin/zdiff ] || { echo "gzip: zless/zcmp/zdiff missing"; ls $GZ/usr/bin; exit 1; }
echo "# gzip $NEWVER" >> $GZ/usr/bin/zless
rm $GZ/usr/bin/zcmp
ln -s zdiff $GZ/usr/bin/zcmp

# --- build
for n in $PKGS; do
    f=$(pkgfile $n)
    $V --keys-dir $K dump --json $f | python3 -I $S/catalyst-kt-pkgargs.py $NEWVER $W/scripts/$n "$LIST" > $W/args.$n
    case " $LOGGED " in *" $n "*)
        if ! grep -q '^--script=post-upgrade:' $W/args.$n; then
            printf '#!/bin/sh\nmkdir -p /var/log 2>/dev/null\necho "$APK_PACKAGE $APK_SCRIPT $*" >> /var/log/catalyst-kt.log\nexit 0\n' > $W/scripts/$n/post-upgrade
            chmod 755 $W/scripts/$n/post-upgrade
            echo "--script=post-upgrade:$W/scripts/$n/post-upgrade" >> $W/args.$n
        fi;;
    esac
    if [ $n = ncurses-libs ]; then
        mkdir -p $W/scripts/$n
        printf '#!/bin/sh\nmkdir -p /var/log 2>/dev/null\necho "$APK_PACKAGE $APK_SCRIPT $*" >> /var/log/catalyst-kt.log\nexit 0\n' > $W/scripts/$n/pre-upgrade
        chmod 755 $W/scripts/$n/pre-upgrade
        echo "--script=pre-upgrade:$W/scripts/$n/pre-upgrade" >> $W/args.$n
    fi
    mapfile -t A < $W/args.$n
    $APK mkpkg --sign-key $KD/catalyst-test.rsa "${A[@]}" --files $W/pkgs/$n --output $R3/$n-$NEWVER.apk
done
$APK mkndx --keys-dir $KD/trusted --sign-key $KD/catalyst-test.rsa --output $R3/Packages.adb $R3/*.apk
ls -la $R3 | head -40
$V --keys-dir $KD/trusted verify $R3/Packages.adb $R3/*.apk | tail -3
echo "scripts in the new packages:"
grep -h '^--script\|^--trigger' $W/args.* | sed 's|/root/catalyst-test/kt-build/scripts/||'
