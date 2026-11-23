S=$(cd "$(dirname "$(readlink -f "$0")")" && pwd)   # this folder
[ -e $S/catalyst-prof ] && { echo "catalyst-prof exists"; exit 1; }
cp -r /root/catalyst-test/bench/src-head $S/catalyst-prof
ls $S/catalyst-prof $S/catalyst-prof/src
grep -n 'pub fn sync_fs' -A4 $S/catalyst-prof/src/sys.rs
grep -n 'mod ' $S/catalyst-prof/src/lib.rs
grep -n 'fn main\|engine::' $S/catalyst-prof/src/main.rs | head
