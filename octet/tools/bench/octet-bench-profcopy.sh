S=$(cd "$(dirname "$(readlink -f "$0")")" && pwd)   # this folder
[ -e $S/octet-prof ] && { echo "octet-prof exists"; exit 1; }
cp -r /root/octet-test/bench/src-head $S/octet-prof
ls $S/octet-prof $S/octet-prof/src
grep -n 'pub fn sync_fs' -A4 $S/octet-prof/src/sys.rs
grep -n 'mod ' $S/octet-prof/src/lib.rs
grep -n 'fn main\|engine::' $S/octet-prof/src/main.rs | head
