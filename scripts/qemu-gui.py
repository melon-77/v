#!/usr/bin/env python3
"""Drive a Valence VM's screen by hand: look at it, click, type. For what the serial-console tests cannot reach,
which is the graphical installer (AGENTS.md, "Testing", has the Calamares run).

  qemu-gui.py start cd [--uefi]     boot the newest desktop ISO in out/ with an empty 24 GiB disk (work/tests/gui.img);
                                    QEMU stays in the foreground, so start it in a second terminal or in the background
                                    (ISO=path in the environment: that ISO, for example the one built with catalyst)
  qemu-gui.py start disk [--uefi]   boot that disk
  qemu-gui.py shot NAME             screenshot -> logs/gui/NAME.png (or $OUT/NAME.png); prints the path and the size
  qemu-gui.py click X Y [right]     move the pointer there (a USB tablet: absolute positions) and click
  qemu-gui.py move X Y
  qemu-gui.py key K[+K...] [...]    QEMU's key names: alt+n  ret  tab  down  esc  ctrl+alt+f2
  qemu-gui.py type 'text'           types it (US layout)
  qemu-gui.py wait SECONDS
  qemu-gui.py quit                  stop the VM (power the guest off first when its disk matters)
Several commands in one call are separated by a lone comma:  qemu-gui.py click 57 45 , click 57 45 , wait 25 , shot a

Without KVM the guest is emulated (as in qemu-test.py): give a click time to show before the next screenshot, and
look at a screenshot before typing into a window that should have opened. Positions are in the screenshot's pixels.
"""
import json, os, socket, subprocess, sys, time
M = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SOCK = os.environ.get('QMP', '/tmp/valence-gui.qmp')
OUT = os.environ.get('OUT', f'{M}/logs/gui')
IMG = f'{M}/work/tests/gui.img'
W = H = 0


def start(what, uefi):
    import glob
    isos = [os.environ['ISO']] if os.environ.get('ISO') else sorted(glob.glob(f'{M}/out/valence-desktop-2*-x86_64.iso'), key=os.path.getmtime)
    os.makedirs(os.path.dirname(IMG), exist_ok=True)
    if what == 'cd':
        if not isos: sys.exit('no desktop ISO in out/ (VALENCE_EDITION=desktop scripts/mkiso.sh)')
        if os.path.exists(IMG): os.remove(IMG)
        subprocess.run(['qemu-img', 'create', '-q', '-f', 'raw', IMG, '24G'], check=True)
    elif not os.path.exists(IMG): sys.exit(f'{IMG} is not there: "start cd" and an install make it')
    if os.path.exists(SOCK): os.remove(SOCK)
    # the machine qemu-test.py's desktop modes use (software emulation, a real graphics card for KWin), with a
    # tablet for absolute pointer positions and the monitor as a QMP socket
    cmd = ['qemu-system-x86_64', '-m', '4096', '-smp', '4', '-accel', 'tcg,thread=multi', '-cpu', 'max,avx=off,avx2=off',
           '-display', 'none', '-device', 'virtio-vga', '-device', 'qemu-xhci', '-device', 'usb-tablet', '-device', 'usb-kbd',
           '-qmp', f'unix:{SOCK},server,nowait', '-netdev', 'user,id=n0', '-device', 'virtio-net-pci,netdev=n0',
           '-drive', f'file={IMG},if=virtio,format=raw']
    if uefi: cmd += ['-bios', '/usr/share/ovmf/OVMF.fd']
    cmd += ['-cdrom', isos[-1], '-boot', 'd'] if what == 'cd' else ['-boot', 'c']
    print('starting', 'the desktop ISO' if what == 'cd' else 'the disk', '(UEFI)' if uefi else '(BIOS)', flush=True)
    os.execvp(cmd[0], cmd)


class Qmp:
    def __init__(self):
        self.s = socket.socket(socket.AF_UNIX); self.s.connect(SOCK); self.f = self.s.makefile('rw')
        self.f.readline(); self.cmd('qmp_capabilities')

    def cmd(self, name, **args):
        self.f.write(json.dumps({'execute': name, 'arguments': args} if args else {'execute': name}) + '\n'); self.f.flush()
        while True:
            line = self.f.readline()
            if not line: return {}
            r = json.loads(line)
            if 'return' in r or 'error' in r:
                if 'error' in r: print('QMP error:', r['error'])
                return r


SHIFT = {'!': '1', '@': '2', '#': '3', '$': '4', '%': '5', '^': '6', '&': '7', '*': '8', '(': '9', ')': '0', '_': 'minus', '+': 'equal',
         ':': 'semicolon', '"': 'apostrophe', '<': 'comma', '>': 'dot', '?': 'slash', '{': 'bracket_left', '}': 'bracket_right',
         '|': 'backslash', '~': 'grave_accent'}
PLAIN = {' ': 'spc', '-': 'minus', '=': 'equal', ';': 'semicolon', "'": 'apostrophe', ',': 'comma', '.': 'dot', '/': 'slash',
         '[': 'bracket_left', ']': 'bracket_right', '\\': 'backslash', '`': 'grave_accent', '\n': 'ret', '\t': 'tab'}


def keys_for(ch):
    if ch.isalpha() and ch.isascii(): return (['shift'] if ch.isupper() else []) + [ch.lower()]
    if ch.isdigit(): return [ch]
    if ch in PLAIN: return [PLAIN[ch]]
    if ch in SHIFT: return ['shift', SHIFT[ch]]
    sys.exit(f'no key for {ch!r}')


def screen(q, path):
    from PIL import Image
    q.cmd('screendump', filename=path, format='png'); time.sleep(0.8)
    return Image.open(path)


def run(q, a):
    global W, H
    c = a[0]
    if c == 'shot':
        os.makedirs(OUT, exist_ok=True); p = f'{OUT}/{a[1]}.png'
        im = screen(q, f'/tmp/valence-gui-{a[1]}.png'); im.save(p); print(f'{p} {im.size[0]}x{im.size[1]}')
    elif c in ('click', 'move'):
        if not W: W, H = screen(q, '/tmp/valence-gui-size.png').size
        x, y = int(a[1]), int(a[2])
        q.cmd('input-send-event', events=[{'type': 'abs', 'data': {'axis': 'x', 'value': int(x * 32767 / (W - 1))}},
                                          {'type': 'abs', 'data': {'axis': 'y', 'value': int(y * 32767 / (H - 1))}}])
        time.sleep(0.15)
        if c == 'click':
            b = 'right' if len(a) > 3 and a[3] == 'right' else 'left'
            for down in (True, False):
                q.cmd('input-send-event', events=[{'type': 'btn', 'data': {'down': down, 'button': b}}]); time.sleep(0.08)
    elif c == 'key':
        for combo in a[1:]:
            q.cmd('send-key', keys=[{'type': 'qcode', 'data': k} for k in combo.split('+')]); time.sleep(0.15)
    elif c == 'type':
        for ch in a[1]:
            q.cmd('send-key', keys=[{'type': 'qcode', 'data': k} for k in keys_for(ch)]); time.sleep(0.12)
    elif c == 'wait':
        time.sleep(float(a[1]))
    elif c == 'quit':
        q.cmd('quit')
    else:
        sys.exit(__doc__)


if __name__ == '__main__':
    args = sys.argv[1:]
    if not args or args[0] in ('-h', '--help'): sys.exit(__doc__)
    if args[0] == 'start':
        if len(args) < 2 or args[1] not in ('cd', 'disk'): sys.exit(__doc__)
        start(args[1], '--uefi' in args)
    groups, cur = [], []
    for x in args:
        if x == ',': groups.append(cur); cur = []
        else: cur.append(x)
    groups.append(cur)
    q = Qmp()
    for g in groups:
        if g: run(q, g)
