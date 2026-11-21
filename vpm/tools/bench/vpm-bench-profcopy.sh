S=$(cd "$(dirname "$(readlink -f "$0")")" && pwd)   # this folder
[ -e $S/vpm-prof ] && { echo "vpm-prof exists"; exit 1; }
cp -r /root/vpm-test/bench/src-head $S/vpm-prof
ls $S/vpm-prof $S/vpm-prof/src
grep -n 'pub fn sync_fs' -A4 $S/vpm-prof/src/sys.rs
grep -n 'mod ' $S/vpm-prof/src/lib.rs
grep -n 'fn main\|engine::' $S/vpm-prof/src/main.rs | head
