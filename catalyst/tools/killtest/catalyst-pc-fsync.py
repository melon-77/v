"""For catalyst-powercut.sh: renames and fsyncs a file of its own, every few
milliseconds, until it is killed.

usage: catalyst-pc-fsync.py DIRECTORY SEED

Each round writes the file, renames it (a change of the directory) and
fsyncs the file and the directory. On ext4 and XFS that commits the file
system's journal: every name made so far on that file system, by any
process (links, renames, removals), goes to disk, and the contents of other
files that are still in the page cache do not. A power cut after such a
commit is the case in which a file has its name and not its content. (On
btrfs an fsync logs the one file only; the test does not rely on this
process there.)
"""
import os, random, sys, time

d = sys.argv[1]
names = [os.path.join(d, '.fsync-file-a'), os.path.join(d, '.fsync-file-b')]
fd = os.open(names[0], os.O_WRONLY | os.O_CREAT, 0o600)
dfd = os.open(d, os.O_RDONLY | os.O_DIRECTORY)
random.seed(sys.argv[2])
n = 0
while True:
    n += 1
    os.pwrite(fd, b'%d\n' % n, 0)
    os.rename(names[(n + 1) % 2], names[n % 2])
    os.fsync(fd)
    os.fsync(dfd)
    time.sleep(random.uniform(0, 0.004))
