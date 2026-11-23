"""Runs a command and kills it (SIGKILL to its whole process group, so a
package script it is running dies with it, as on a crash).

usage: oct-kt-kill.py MODE PARAM ROOT LOG -- CMD...

MODE (PARAM):
  measure (-)        no kill; reports when ROOT/var/lib/catalyst/journal appeared
                     and disappeared (the switch), how many steps it marked
                     done, and the total time
  time (D)           kill D seconds after the start
  journal (D)        kill D seconds after the journal appeared (in the switch)
  step (K/D)         kill D seconds after the journal holds K "done" marks
  gone (D)           kill D seconds after the journal disappeared again
                     (while the old versions are deleted)
  file (PATH/D)      kill D seconds after the live ROOT/PATH was replaced
                     (its inode changed): with a file in the middle of a
                     package, the package is half placed at the kill
  script (K/D)       kill D seconds after the K-th package script was
                     written to ROOT/var/lib/catalyst/exec (catalyst runs it next):
                     the script is killed while it runs, or just before
A MODE ending in "+throttle" throttles the command once the journal
appears: it is let run for 30 us, then stopped (SIGSTOP) for 0.5 ms, and so
on; the kill condition is checked while it is stopped and the SIGKILL then
reaches it stopped, so a kill lands within ~30 us of work after the
condition (the switch's file placing takes only a few ms in all, between
syncfs calls, and would otherwise rarely be hit). The command itself is
not changed; package scripts it runs are not throttled.
Prints one line of key=value pairs.
"""
import os, signal, subprocess, sys, time

mode, param, root, log = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]
throttle = mode.endswith('+throttle')
mode = mode.replace('+throttle', '')
cmd = sys.argv[sys.argv.index('--') + 1:]
J = os.path.join(root, 'var/lib/catalyst/journal')
EXEC = os.path.join(root, 'var/lib/catalyst/exec')
RUN, PAUSE = 0.00003, 0.0005
scripts_seen = set()
K = watch = None
if mode == 'measure':
    delay = 0.0
elif mode in ('step', 'script'):
    K, delay = param.split('/')
    K, delay = int(K), float(delay)
elif mode == 'file':
    watch, delay = param.rsplit('/', 1)
    watch, delay = os.path.join(root, watch), float(delay)
    ino0 = os.lstat(watch).st_ino
else:
    delay = float(param)
out = open(log, 'wb')
t0 = time.monotonic()
p = subprocess.Popen(cmd, stdout=out, stderr=subprocess.STDOUT, start_new_session=True)
tj0 = tj1 = tcond = None
jsize = -1
done = 0
done_max = 0
stopped = False


def now():
    return time.monotonic() - t0


def poll_journal():
    """One look at the journal: when it appeared/disappeared, its marks."""
    global tj0, tj1, jsize, done, done_max
    try:
        sz = os.stat(J).st_size
    except FileNotFoundError:
        if tj0 is not None and tj1 is None:
            tj1 = now()
        return
    if tj0 is None:
        tj0 = now()
    if mode in ('script', 'measure'):
        try:
            scripts_seen.update(os.listdir(EXEC))
        except FileNotFoundError:
            pass
    if sz != jsize:
        jsize = sz
        try:
            with open(J, 'rb') as f:
                done = sum(1 for l in f if l.startswith(b'done '))
            done_max = max(done_max, done)
        except FileNotFoundError:
            pass


def condition():
    """Has the moment to start the delay come?"""
    if mode == 'time':
        return True
    if mode == 'journal':
        return tj0 is not None
    if mode == 'gone':
        return tj1 is not None
    if mode == 'step':
        return tj0 is not None and (done >= K or tj1 is not None)
    if mode == 'script':
        return len(scripts_seen) >= K
    if mode == 'file':
        try:
            return os.lstat(watch).st_ino != ino0
        except FileNotFoundError:
            return False
    return False


def spin(seconds):
    end = time.perf_counter() + seconds
    while time.perf_counter() < end:
        pass


def is_stopped():
    try:
        with open('/proc/%d/stat' % p.pid) as f:
            return f.read().rsplit(')', 1)[1].split()[0] in ('T', 't')
    except (FileNotFoundError, IndexError):
        return True


def step_throttled():
    """One run/stop cycle; returns with the process stopped."""
    global stopped
    try:
        os.kill(p.pid, signal.SIGCONT)
        stopped = False
        spin(RUN)
        os.kill(p.pid, signal.SIGSTOP)
        stopped = True
    except ProcessLookupError:
        return
    # a stop takes effect when a syscall in progress (syncfs...) returns
    while p.poll() is None and not is_stopped():
        pass


deadline = None
while p.poll() is None:
    poll_journal()
    if deadline is None and mode != 'measure' and condition():
        deadline = (t0 if mode == 'time' else time.monotonic()) + delay
        tcond = now()
    if deadline is not None and time.monotonic() >= deadline:
        break
    if throttle and tj0 is not None and tj1 is None:
        step_throttled()
        spin(PAUSE)
    elif stopped:
        os.kill(p.pid, signal.SIGCONT)
        stopped = False

killed = False
at = None
j_at_kill = None
if p.poll() is None:
    at = now()
    j_at_kill = os.path.exists(J)
    try:
        os.killpg(p.pid, signal.SIGKILL)
        killed = True
    except ProcessLookupError:
        pass
p.wait()
total = now()
# the whole group must be gone before anything looks at the root
for _ in range(2000):
    try:
        os.killpg(p.pid, signal.SIGKILL if killed else 0)
    except ProcessLookupError:
        break
    time.sleep(0.005)
else:
    print('WARNING: processes of group %d still alive' % p.pid, file=sys.stderr)
if tj0 is not None and tj1 is None and not os.path.exists(J):
    tj1 = total
fmt = lambda x: 'none' if x is None else '%.4f' % x
print('killed=%d at=%s rc=%s journal_at_kill=%s marks_at_kill=%s condition_at=%s tj0=%s tj1=%s marks_max=%d scripts=%d total=%.4f' % (
    killed, fmt(at), p.returncode, {None: '-', True: 1, False: 0}[j_at_kill], done if killed else '-',
    fmt(tcond), fmt(tj0), fmt(tj1), done_max, len(scripts_seen), total))
