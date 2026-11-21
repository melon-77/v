#!/usr/bin/env python3
"""qemu-test-rpi.py IMAGE: the Raspberry Pi edition's SD card image in QEMU's Raspberry Pi 3B (ROADMAP.md, phase 9).

QEMU starts a kernel itself and does not run the Pi's GPU firmware, so the test takes the card's own kernel8.img,
its device tree for the Pi 3B and its cmdline.txt from the card's boot partition, and gives QEMU the card as the
SD card (a copy, made 4 GB: QEMU wants a power of two, and the first start grows the system into the rest). On the
serial console (the mini UART, which the Pi's serial0 is): the system starts to a login prompt, root logs in, it is
the 64-bit ARM system on Btrfs with /boot on the FAT partition, the first start grew it to the card, a USB network
card gets an address, and apk installs a package from this build's repository (served here over HTTP) after taking
a snapshot. Software emulation: several minutes. Log: logs/qemu-rpi.log. (apk adds nano from the small repository the
card carries, the way someone without the online repository would.)
"""
import sys, os, subprocess, shutil, time, re, functools, threading, http.server
import pexpect
M = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
img, = [a for a in sys.argv[1:] if not a.startswith('--')]
T = f'{M}/work-aarch64/tests'; os.makedirs(T, exist_ok=True)
t0 = time.monotonic()
def step(s): print(f'[{time.monotonic()-t0:7.1f}s] {s}', flush=True)
failed = []
def check(cond, what):
    if not cond: failed.append(what)
    step(what + ': ' + ('OK' if cond else 'FAILED'))
    return cond

disk = f'{T}/rpi-card.img'; shutil.copyfile(img, disk)
subprocess.run(['qemu-img', 'resize', '-q', '-f', 'raw', disk, '4G'], check=True)
def mcopy(name, dest): subprocess.run(['mcopy', '-n', '-o', '-i', f'{disk}@@1M', f'::{name}', dest], check=True, stdin=subprocess.DEVNULL)
# The card's own device tree for the Pi 3B (bcm2710-rpi-3-b.dtb, the one the firmware picks on a real board), with
# one change for QEMU: on a Pi 3 the PL011 belongs to Bluetooth (its node has a "bluetooth" child, so the kernel gives
# the port to the Bluetooth driver and makes no ttyAMA0), and the board's console is the mini UART, whose clock QEMU's
# missing GPU firmware would have to provide. Without the child node the PL011 is a plain serial port. (The mainline
# tree, bcm2837-rpi-3-b.dtb, has a console in QEMU but its SD card driver does not read the card there.)
mcopy('kernel8.img', f'{T}/kernel8.img'); mcopy('bcm2710-rpi-3-b.dtb', f'{T}/rpi-3-b.dtb'); mcopy('cmdline.txt', f'{T}/cmdline.txt')
subprocess.run(['fdtput', '-r', f'{T}/rpi-3-b.dtb', '/soc/serial@7e201000/bluetooth'], check=True)
# The Pi's firmware turns console=serial0 into the board's serial port before the kernel sees it; QEMU has no such
# firmware, so the test names the port itself: the PL011, QEMU's first serial port (the image has a login there too)
# (and the serial console last, so that it is /dev/console and the start's own messages show on it; the kernel numbers
# the PL011 after the device tree's alias for it, serial1, so it is ttyAMA1)
cmdline = 'earlycon=pl011,mmio32,0x3f201000 ' + open(f'{T}/cmdline.txt').read().strip().replace('console=serial0,115200 ', '') + ' console=ttyAMA1,115200'
step(f'kernel command line (the card\'s, with the serial port named): {cmdline}')
srv = http.server.ThreadingHTTPServer(('127.0.0.1', 0), functools.partial(http.server.SimpleHTTPRequestHandler, directory=f'{M}/repo'))
threading.Thread(target=srv.serve_forever, daemon=True).start()
repo = f'http://10.0.2.2:{srv.server_address[1]}/aarch64/Packages.adb'
cmd = ['qemu-system-aarch64', '-M', 'raspi3b', '-m', '1G', '-kernel', f'{T}/kernel8.img', '-dtb', f'{T}/rpi-3-b.dtb',
       '-append', cmdline, '-drive', f'file={disk},if=sd,format=raw', '-display', 'none', '-no-reboot',
       '-serial', 'stdio',                                         # the PL011 (ttyAMA1)
       '-netdev', 'user,id=n0', '-device', 'usb-net,netdev=n0']
log = open(f'{M}/logs/qemu-rpi.log', 'w')
p = pexpect.spawn(cmd[0], cmd[1:], encoding='utf-8', codec_errors='replace', timeout=1800); p.logfile_read = log
PROMPT = r'@valence[\w-]*.*[#$] '
def sh(c, timeout=300):
    p.sendline(c); p.expect(PROMPT, timeout=timeout); return p.before.split('\n', 1)[-1]
step('starting the card in a Raspberry Pi 3B')
p.expect(r'Linux version \S+', timeout=600); step('kernel: ' + p.after)
p.expect('login:', timeout=1800); step('login prompt')
# A busy emulated board loses characters typed at its serial port (the first start is growing the card meanwhile):
# let the first start settle, type slowly, and try the login again if it was garbled (only a lost character can
# make "valence" fail: the hash on the card is checked separately)
def slowly(text):
    for ch in text: p.send(ch); time.sleep(0.15)
    p.send('\r')
NEWPW = 'Pi-test-Valence-9'
time.sleep(15); p.sendline(''); p.expect('login:', timeout=120)
for attempt in range(3):
    slowly('root'); p.expect('assword:', timeout=120); time.sleep(1.5); slowly('valence')
    i = p.expect([r'(?i)current password:', r'\nNew password:', PROMPT, 'incorrect'], timeout=120)
    if i != 3: break
    step('login refused (characters lost on the serial port?), trying again'); p.expect('login:', timeout=120); time.sleep(3)
if i == 0: time.sleep(1); slowly('valence'); i = p.expect([r'\nNew password:', PROMPT, 'incorrect'], timeout=60) + 1
asked = i == 1
if asked:
    time.sleep(1); slowly(NEWPW); p.expect(r'Re-enter new password:', timeout=60); time.sleep(1); slowly(NEWPW)
    i = p.expect([PROMPT, r'\nNew password:', 'incorrect', 'do not match'], timeout=120)
check(asked and i == 0, 'root logs in with "valence" and has to choose a new password at once')
if p.after != '' and not re.search(PROMPT, p.after or ''): p.expect(PROMPT, timeout=60)
sh('stty cols 160 rows 50; export TERM=vt100')
out = sh('uname -m; uname -r; findmnt -n -o FSTYPE,OPTIONS /; findmnt -n -o FSTYPE /boot; ls /boot/kernel8.img /boot/start4.elf /boot/config.txt /boot/bcm2712-rpi-5-b.dtb'); print(out)
check('aarch64' in out and 'valence-rpi' in out, 'the 64-bit ARM system, with the Pi kernel')
check('btrfs' in out and 'subvol=/@' in out and 'vfat' in out, 'Btrfs root in @, /boot on the FAT partition')
check(all(f in out for f in ('kernel8.img', 'start4.elf', 'config.txt', 'bcm2712-rpi-5-b.dtb')) and 'No such' not in out, '/boot has the kernel, the firmware, config.txt and the Pi 5\'s device tree')
size = 0
for _ in range(30):
    out = sh('lsblk -b -n -d -o SIZE /dev/mmcblk0p2; btrfs filesystem show / | grep -o "size [0-9.]*GiB" | head -n1')
    m = re.search(r'size ([0-9.]+)GiB', out)
    if m and float(m.group(1)) > 3.5: size = float(m.group(1)); break
    time.sleep(10)
check(size > 3.5, f'the first start grew / to the 4 GB card ({size} GiB)')
out = sh('for i in 1 2 3 4 5 6 7 8 9 10 11 12; do ip -4 addr | grep -q "inet 10\\." && break; sleep 5; done; ip -4 addr | grep inet', timeout=200); print(out)
net = check('inet 10.0.2.' in out, 'a USB network card gets an address over DHCP')
if not net: print(sh('ls /sys/class/net; ls /usr/lib/modules/*/modules.alias; lsmod | grep -i -E "rndis|cdc|usbnet"; dmesg | grep -i -E "rndis|cdc_ether|usb0" | tail -n 5'))
out = sh('apk add nano 2>&1 | tail -n 4; nano --version | head -n1', timeout=900); print(out)
check('GNU nano' in out, 'apk adds a package (nano, from the card\'s own repository)')
out = sh('valence-snapshot list 2>&1 | tail -n 3'); print(out)
check('apk add nano' in out, 'with a snapshot taken before it')
if net:
    # (a package whose dependencies are built for aarch64: the -doc packages need mandoc, which is not yet)
    out = sh(f'apk --repositories-file /dev/null --repository {repo} add zstd-dev 2>&1 | tail -n 3; ls /usr/include/zstd.h', timeout=900); print(out)
    check('ERROR' not in out and 'No such file' not in out, 'and over the network, from this build\'s repository (zstd-dev)')
p.sendline('poweroff'); p.expect(pexpect.EOF, timeout=300)
step('rpi test: ' + ('OK' if not failed else 'FAILED: ' + '; '.join(failed)))
sys.exit(0 if not failed else 1)
