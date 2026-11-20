#!/usr/bin/env python3
"""Boot-test Valence in QEMU over the serial console.

  qemu-test.py live  <iso> <disk.img> [--uefi]   boot the ISO, run the quick installer onto disk.img
  qemu-test.py disk  <disk.img> [--uefi]         boot the installed disk and check the system
  qemu-test.py rollback <disk.img> [--uefi]      on the installed disk: an update broken on purpose is rolled back with
                                                 valence-snapshot; then one that leaves the disk unable to boot: the
                                                 boot menu starts the snapshot from before it, and valence-snapshot
                                                 rollback there makes that the system again
  qemu-test.py toram <iso> <disk.img>            boot with "copy to RAM", eject the CD, then use and install
  qemu-test.py desktop <desktop-iso>             boot the desktop ISO with graphics: services up, Plasma running,
                                                 a USB stick mounts through UDisks2, screenshot in logs/qemu-desktop.png
  qemu-test.py desktop-install <desktop-iso> <disk.img>   install the desktop profile, boot the installed disk
                                                 with graphics, log in through SDDM: greeter stays up, Plasma starts
  qemu-test.py dualboot <iso> <disk.img>         a fake Windows disk (EFI partition, MSR, NTFS C:, free space): install
                                                 alongside on UEFI, boot Valence from the firmware's boot menu, Windows in
                                                 GRUB, and nothing of Windows' changed
  --luks   install with an encrypted root (and type the passphrase when the installed disk boots)
  --vmware VMware-style virtual hardware: PVSCSI disk, VMXNET3 network, VMware SVGA
"""
import sys, time, pexpect, os
import re as _re
import types, pexpect.expect, pexpect.pty_spawn, pexpect.utils
# pexpect times its timeouts with the wall clock, and WSL's clock jumps (hours at a time after the laptop sleeps,
# or back and forth while Windows and NTP disagree) would end them at once: give it the monotonic clock
_mono = types.SimpleNamespace(**{k: getattr(time, k) for k in dir(time) if not k.startswith('_')})
_mono.time = time.monotonic
for _m in (pexpect.expect, pexpect.pty_spawn, pexpect.utils): _m.time = _mono
M = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

mode = sys.argv[1]
uefi = '--uefi' in sys.argv
i686 = '--i686' in sys.argv          # 32-bit ISO / disk: run in qemu-system-i386
luks = '--luks' in sys.argv
vmware = '--vmware' in sys.argv
offline = '--offline' in sys.argv  # the VM keeps its network card but can't reach anything outside (no internet)
DISKNAME = 'sda' if vmware else 'vda'
args = [a for a in sys.argv[2:] if not a.startswith('--')]
log = open(f'{M}/logs/qemu-{mode}{"-uefi" if uefi else ""}{"-i686" if "--i686" in sys.argv else ""}{"-vmware" if vmware else ""}{"-offline" if offline else ""}.log', 'w')

cmd = ['qemu-system-i386' if i686 else 'qemu-system-x86_64', '-m', '1024' if i686 else '3072', '-smp', '2', '-nographic', '-no-reboot',
       '-audiodev', f'wav,id=snd0,path={M}/logs/audio-capture.wav',
       '-device', 'intel-hda', '-device', 'hda-duplex,audiodev=snd0',
       '-netdev', 'user,id=n0' + (',restrict=on' if offline else ''), '-device', ('vmxnet3' if vmware else 'virtio-net-pci') + ',netdev=n0',
       # QEMU guest agent channel (the host side is a socket the test talks to)
       '-device', 'virtio-serial', '-chardev', 'socket,path=/tmp/valence-qga.sock,server=on,wait=off,id=qga0',
       '-device', 'virtserialport,chardev=qga0,name=org.qemu.guest_agent.0',
       '-monitor', 'unix:/tmp/valence-qmon.sock,server,nowait']
def kvm_works():
    # /dev/kvm can exist and still be unusable (WSL2 on Windows 10 or on an AMD CPU without nesting: QEMU says
    # "failed to initialize kvm: No such device"), so ask the kernel: KVM_GET_API_VERSION must answer 12
    try:
        import fcntl
        fd = os.open('/dev/kvm', os.O_RDWR)
        try: return fcntl.ioctl(fd, 0xAE00) == 12
        finally: os.close(fd)
    except OSError:
        return False
# hardware acceleration when the build host has it (WSL2 on Windows 11 and most PCs do; the original build container didn't)
HAVE_KVM = kvm_works() and not i686
if HAVE_KVM:
    cmd += ['-enable-kvm', '-cpu', 'host']
elif i686:
    cmd += ['-cpu', 'n270']   # the Intel Atom N270 of the netbooks the 32-bit edition is for (MSI Wind U100): SSE2/SSSE3, no 64-bit
else:
    # software emulation: one thread per virtual CPU, and every CPU feature TCG has (Mesa and Qt want the newer ones)
    # No KVM: software emulation. glibc picks AVX2 versions of memcpy, strlen and friends when the CPU offers AVX2, and
    # QEMU emulates those instructions slowly: the install's copy step took 107 s with them and 74 s without (the same
    # step with musl, which has no such versions: 57 s). On real CPUs and under KVM they are the faster ones.
    cmd += ['-accel', 'tcg,thread=multi', '-cpu', 'max,avx=off,avx2=off']
if uefi:
    cmd += ['-bios', '/usr/share/ovmf/OVMF.fd']
if vmware:
    cmd += ['-vga', 'vmware', '-device', 'pvscsi,id=scsi0']
def disk_args(img):
    if vmware:
        return ['-drive', f'file={img},if=none,format=raw,id=d0', '-device', 'scsi-hd,drive=d0,bus=scsi0.0']
    return ['-drive', f'file={img},if=virtio,format=raw']
if mode == 'desktop':
    iso, = args
    # a USB stick like a real one: MBR partition table, one FAT32 partition labelled VALENCEUSB with a file on it
    import subprocess
    usb = '/tmp/valence-usb.img'
    open(usb, 'wb').truncate(64 << 20)
    subprocess.run(['sfdisk', '-q', usb], input=b'label: dos\nstart=2048, type=c\n', check=True)
    subprocess.run(['mkfs.vfat', '-F', '32', '-n', 'VALENCEUSB', '--offset', '2048', usb, str((64 << 20) // 1024 - 1024)],
                   check=True, stdout=subprocess.DEVNULL)
    subprocess.run(['mcopy', '-i', f'{usb}@@1M', '-', '::hello.txt'], input=b'hello from the stick\n', check=True)
    # a real graphics card for KWin; the serial port stays the test's console
    cmd = [c for c in cmd if c != '-nographic'] + ['-display', 'none', '-serial', 'stdio', '-device', 'virtio-vga', '-m', '4096',
                                                 '-cdrom', iso, '-boot', 'd', '-device', 'qemu-xhci,id=xhci',
                                                 '-drive', f'if=none,id=stick,format=raw,file={usb}', '-device', 'usb-storage,bus=xhci.0,drive=stick']
elif mode == 'desktop-install':
    iso, disk = args
    base = cmd + ['-m', '2048' if i686 else '4096']   # 32-bit: what the netbooks it is for can hold
    cmd = base + ['-cdrom', iso] + disk_args(disk) + ['-boot', 'd']
elif mode == 'dualboot':
    import subprocess, hashlib
    iso, disk = args
    def run(*c, **k): return subprocess.run(list(c), check=True, capture_output=True, text=True, **k).stdout
    # the fake Windows disk: 40 GiB GPT, 260 MiB EFI (Windows Boot Manager + fallback loader), 16 MiB MSR,
    # 15 GiB NTFS "C:", the rest unallocated (what Windows' "Shrink Volume" leaves)
    open(disk, 'wb').truncate(40 << 30)
    run('sfdisk', '-q', disk, input='label: gpt\nsize=260MiB, type=C12A7328-F81F-11D2-BA4B-00A0C93EC93B, name="EFI system partition"\n'
        'size=16MiB, type=E3C9E316-0B5C-4DB8-817D-F92DF00215AE, name="Microsoft reserved partition"\n'
        'size=15GiB, type=EBD0A0A2-B9E5-4433-87C0-68B6B72699C7, name="Basic data partition"\n')
    loop = run('losetup', '-P', '-f', '--show', disk).strip()
    try:
        run('mkfs.vfat', '-F', '32', '-n', 'SYSTEM', loop + 'p1'); run('mkntfs', '-Q', '-q', '-L', 'Windows', loop + 'p3')
        os.makedirs('/tmp/valence-esp', exist_ok=True); run('mount', loop + 'p1', '/tmp/valence-esp')
        for path, blob in (('EFI/Microsoft/Boot/bootmgfw.efi', b'fake windows boot manager\n' * 999),
                           ('EFI/Boot/bootx64.efi', b'fake windows fallback loader\n' * 999)):
            os.makedirs(os.path.dirname(f'/tmp/valence-esp/{path}'), exist_ok=True); open(f'/tmp/valence-esp/{path}', 'wb').write(blob)
        run('umount', '/tmp/valence-esp')
    finally:
        run('losetup', '-d', loop)
    def windows_state():
        """checksums of everything Windows owns: its partition table entries, MSR, NTFS C:, its files on the EFI partition"""
        loop = run('losetup', '-P', '-f', '--show', disk).strip()
        try:
            st = {'table': ''.join(l for l in run('sfdisk', '-d', disk).splitlines(True) if 'valence' not in l and not l.startswith(('last-lba', 'first-lba')))}
            for n in ('p2', 'p3'):
                st[n] = hashlib.sha256(open(loop + n, 'rb').read(64 << 20)).hexdigest()
            run('mount', '-o', 'ro', loop + 'p1', '/tmp/valence-esp')
            for path in ('EFI/Microsoft/Boot/bootmgfw.efi', 'EFI/Boot/bootx64.efi'):
                st[path] = hashlib.sha256(open(f'/tmp/valence-esp/{path}', 'rb').read()).hexdigest()
            run('umount', '/tmp/valence-esp')
        finally:
            run('losetup', '-d', loop)
        return st
    before = windows_state()
    # UEFI with a firmware boot menu that survives reboots (writable OVMF variables)
    run('cp', '/usr/share/OVMF/OVMF_VARS_4M.fd', '/tmp/valence-ovmf-vars.fd')
    uefi_args = ['-drive', 'if=pflash,format=raw,readonly=on,file=/usr/share/OVMF/OVMF_CODE_4M.fd',
                 '-drive', 'if=pflash,format=raw,file=/tmp/valence-ovmf-vars.fd']
    base = cmd + uefi_args
    cmd = base + ['-cdrom', iso] + disk_args(disk) + ['-boot', 'd']
elif mode in ('live', 'toram'):
    iso, disk = args
    cmd += ['-cdrom', iso] + disk_args(disk) + ['-boot', 'd']
else:
    disk, = args
    cmd += disk_args(disk) + ['-boot', 'c']

t0 = time.monotonic()
def step(s): print(f'[{time.monotonic()-t0:7.1f}s] {s}', flush=True)

def boot_serial_entry(p):
    """Boot the ISO's "serial console" menu entry. The tests used to press Down three times and Enter; a busy guest
    drops key presses now and then, one lost Down boots "safe graphics", which says nothing on the serial port, and
    the test waits ten minutes for a kernel it cannot hear. GRUB marks the highlighted entry with '*' on a serial
    terminal, so press Down until that entry is the marked one. (Only the start of the title is matched: GRUB
    sometimes repositions the cursor in the middle of a title.)"""
    p.expect('Valence live', timeout=300); time.sleep(1)
    for _ in range(12):
        p.send('\x1b[B')
        try:
            p.expect(_re.escape('*Valence live (ser'), timeout=2); break
        except pexpect.TIMEOUT:
            pass
    else:
        print('GRUB menu: the "serial console" entry never became the highlighted one'); sys.exit(1)
    p.send('\r')

p = pexpect.spawn(cmd[0], cmd[1:], encoding='utf-8', codec_errors='replace', timeout=600)
p.logfile_read = log
PROMPT = r'@valence[\w-]*.*[#$] '

def sh(c, timeout=300, expect=PROMPT):
    p.sendline(c)
    try:
        p.expect(expect, timeout=timeout)
    except pexpect.TIMEOUT:
        # a busy guest can drop characters typed at the serial console; a lost quote leaves bash waiting for more
        # input. Short commands get one more go (never long ones like the installer: they would run twice)
        if timeout > 300 or expect is not PROMPT: raise
        step(f'no prompt after {c[:50]!r}: interrupting and typing it again')
        p.sendcontrol('c'); p.expect(PROMPT, timeout=30)
        p.sendline(c); p.expect(expect, timeout=timeout)
    # without the terminal's echo of the command line, which contains every marker the command would print
    return p.before.split('\n', 1)[-1]

def spawn():
    """Start the VM again. QEMU runs with -no-reboot, so it ends when the guest restarts."""
    global p
    p = pexpect.spawn(cmd[0], cmd[1:], encoding='utf-8', codec_errors='replace', timeout=600); p.logfile_read = log

def boot_installed():
    """The installed disk, from power-on to a root shell on the serial console (typing the LUKS passphrase on the way)."""
    if luks:
        p.expect('unlocking the encrypted disk', timeout=900); p.expect('assphrase', timeout=120); p.sendline('valenceluks')
    p.expect('login:', timeout=900); seen = p.before
    p.sendline('root'); p.expect('assword:'); p.sendline('valenceroot'); p.expect(PROMPT, timeout=120)
    sh('stty cols 160 rows 50; export TERM=vt100')
    return seen          # what the console said on the way up

def grub_pick(title, presses=14):
    """Move the boot menu's highlight to the entry whose title starts with TITLE and press Enter. GRUB marks the
    highlighted entry with '*' on a serial terminal (see boot_serial_entry)."""
    for _ in range(presses):
        try:
            p.expect(_re.escape('*' + title), timeout=3); p.send('\r'); return True
        except pexpect.TIMEOUT:
            p.send('\x1b[B')
    return False

if mode == 'desktop':
    import socket
    step('waiting for GRUB')
    boot_serial_entry(p)
    p.expect(PROMPT, timeout=900); step('logged in on the serial console')
    ok = True
    for svc in ['dbus', 'elogind', 'polkitd', 'udevd', 'NetworkManager', 'sddm', 'avahi-daemon', 'cupsd']:
        for _ in range(30):                        # services settle in the first seconds after boot
            out = sh(f'doas sv check /var/service/{svc} >/dev/null && echo SVC-READY || echo SVC-WAIT')
            if 'SVC-READY' in out: break
            time.sleep(2)
        up = 'SVC-READY' in out; ok &= up; step(f'service {svc}: ' + ('ready' if up else 'NOT READY'))
    flapping = sh('sleep 5; doas sv status /var/service/* 2>&1 | grep "want up" | cut -d: -f2 | xargs echo FLAP=')
    flap = _re.search(r'FLAP=([^\r\n]*)', flapping); flap = flap.group(1).strip() if flap else '?'
    ok &= flap == ''; step('services: ' + ('none restarting in a loop' if flap == '' else f'RESTARTING: {flap}'))
    found = False
    for _ in range(40):                            # KWin and plasmashell after SDDM's autologin
        out = sh('ps -e -o comm | grep -qx kwin_wayland && ps -e -o comm | grep -qx plasmashell && echo PLASMA-UP || echo waiting')
        if 'PLASMA-UP' in out: found = True; break
        time.sleep(5)
    time.sleep(20)                                  # let the desktop finish drawing
    # still there after settling, and inside a real logind session (a crash loop can look alive for a moment)
    out = sh('echo SESSIONS=$(loginctl list-sessions --no-legend 2>/dev/null | grep -c seat0) '
             'KWIN=$(ps -e -o comm | grep -cx kwin_wayland) SHELL=$(ps -e -o comm | grep -cx plasmashell)')
    import re as _re
    v = dict(_re.findall(r'(SESSIONS|KWIN|SHELL)=(\d+)', out))
    stable = found and all(int(v.get(k, '0')) > 0 for k in ('SESSIONS', 'KWIN', 'SHELL')); ok &= stable
    step(f"Plasma after 20 s: sessions={v.get('SESSIONS')} kwin={v.get('KWIN')} plasmashell={v.get('SHELL')}")
    # Plasma's device list asks UDisks2 over D-Bus, which starts udisksd; then mount the stick the same way Dolphin does
    out = sh('echo UDISKSD=$(ps -e -o comm | grep -cx udisksd)')
    started = 'UDISKSD=1' in out; ok &= started; step('udisksd started by D-Bus activation: ' + ('yes' if started else 'NO'))
    out = sh('dev=$(doas blkid -L VALENCEUSB); echo "stick: $dev"; doas udisksctl mount -b $dev && '
             'cat /run/media/root/VALENCEUSB/hello.txt && doas udisksctl unmount -b $dev', timeout=120)
    mounted = 'hello from the stick' in out and 'Mounted' in out; ok &= mounted
    step('USB stick through UDisks2: ' + ('mounted, file read, unmounted' if mounted else 'FAILED'))
    if not mounted: print(out[-2000:])
    # codecs: a second of video through each encoder Spectacle's screen recorder (KPipeWire) asks ffmpeg for,
    # Opus audio, and PipeWire's Bluetooth codecs
    out = sh('cd /tmp; for e in libvpx-vp9:webm libx264:mp4 libwebp_anim:webp; do ffmpeg -v error -f lavfi '
             '-i testsrc=duration=1:size=320x240:rate=10 -c:v ${e%:*} -y rec.${e#*:} && [ -s rec.${e#*:} ] && echo ENC-OK ${e%:*}; done; '
             'ffmpeg -v error -f lavfi -i sine=duration=1 -c:a libopus -y rec.opus && echo ENC-OK libopus; cd; '
             'ls /usr/lib/spa-0.2/bluez5/ | grep -oE "codec-bluez5-[a-z0-9]+" | xargs', timeout=180)
    have = set(_re.findall(r'ENC-OK ([a-z0-9_-]+)', out)) | set(_re.findall(r'codec-bluez5-[a-z0-9]+', out))
    missing = sorted({'libvpx-vp9', 'libx264', 'libwebp_anim', 'libopus',
                      'codec-bluez5-sbc', 'codec-bluez5-opus', 'codec-bluez5-aptx', 'codec-bluez5-lc3'} - have); ok &= not missing
    step('codecs: ' + ('VP9, H.264, WebP and Opus encode; Bluetooth codecs present' if not missing else f'MISSING {missing}'))
    if missing: print(out[-2000:])
    # printing: CUPS' virtual IPP Everywhere printer that only takes PWG raster, a driverless queue for it, and a text
    # file through the whole filter chain (texttopdf, pdftopdf with QPDF, pdftoraster with Poppler, the ipp backend)
    out = sh('rm -rf /tmp/ippspool; mkdir -p /tmp/ippspool; (ippeveprinter -r off -p 8631 -f image/pwg-raster -k -d /tmp/ippspool '
             'ValenceTest >/tmp/ippeve.log 2>&1 &); sleep 3; lpadmin -p valencetest -E -v ipp://localhost:8631/ipp/print -m everywhere '
             '&& lp -d valencetest /etc/os-release; for i in $(seq 60); do [ -z "$(lpstat -o valencetest)" ] && break; sleep 1; done; '
             'lpstat -W completed -o valencetest | wc -l | sed "s/^/COMPLETED=/"; ls /tmp/ippspool | grep -c "pwg$" | sed "s/^/RASTER=/"', timeout=180)
    v = dict(_re.findall(r'(COMPLETED|RASTER)=(\d+)', out))
    printed = int(v.get('COMPLETED', '0')) >= 1 and int(v.get('RASTER', '0')) >= 1; ok &= printed
    step('printing: ' + ('a text file reached the IPP Everywhere printer as PWG raster' if printed else f'FAILED {v}'))
    if not printed: print(out[-1500:]); print(sh('tail -20 /var/log/cups/error_log; tail -5 /tmp/ippeve.log'))
    # nmcli: the text-mode way to reach the network when the desktop won't start
    out = sh('nmcli -c no -t -f STATE general; nmcli -c no -t -f DEVICE,STATE device')
    online = _re.search(r'(^|[\r\n])connected', out) is not None; ok &= online   # (bash's bracketed-paste codes end in \r)
    step('nmcli: ' + ('NetworkManager connected' if online else 'NOT CONNECTED'))
    if not online: print(repr(out[-600:]))
    # Wi-Fi the way Plasma's network applet does it (NetworkManager, which starts wpa_supplicant over D-Bus) on two
    # virtual radios: wlan1 is a WPA2 access point run by a separate wpa_supplicant, NetworkManager joins it with wlan0
    ap = ("printf '%s\\n' 'network={' 'ssid=\"valencetest\"' 'mode=2' 'frequency=2437' 'key_mgmt=WPA-PSK' 'proto=RSN' "
          "'pairwise=CCMP' 'psk=\"valencewifi\"' '}' > /tmp/ap.conf")
    # The radios appear while udevd is stopped, like a real card whose interface registers after its firmware loads,
    # in the gap between stage 1's udevd and the udevd service: nothing processes them and NetworkManager leaves them
    # unmanaged. Starting udevd must replay those events (recipes/eudev/udevd.run).
    out = sh('sv down /var/service/udevd; modprobe mac80211_hwsim radios=2; sleep 3; '
             'nmcli -c no -t -f DEVICE,STATE device | grep "^wlan0:" | sed "s/^/GAP=/"; sv up /var/service/udevd', timeout=60)
    gap = _re.search(r'GAP=wlan0:(\S+)', out); gap = gap.group(1) if gap else '?'
    out = sh('for i in $(seq 20); do nmcli -c no -t -f DEVICE,STATE device | grep -q "^wlan1:unmanaged" || break; sleep 1; done; '
             'nmcli -c no device set wlan1 managed no && ' + ap + ' && '
             'wpa_supplicant -B -i wlan1 -c /tmp/ap.conf >/tmp/ap.log 2>&1 && sleep 3 && '
             'nmcli -c no connection add type wifi ifname wlan0 con-name valencetest ssid valencetest wifi-sec.key-mgmt wpa-psk '
             'wifi-sec.psk valencewifi ipv4.method disabled ipv6.method link-local >/dev/null && '
             'nmcli -c no --wait 60 connection up valencetest >/dev/null 2>&1; '
             'nmcli -c no -t -f GENERAL.STATE device show wlan0 | sed "s/^GENERAL.STATE:/WIFI=/"', timeout=180)
    wifi = 'WIFI=100' in out; ok &= wifi
    step(f'Wi-Fi: radios found while udevd was down were {gap}; ' +
         ('after the replay NetworkManager joined a WPA2 network on one' if wifi else 'FAILED'))
    if not wifi: print(out[-1500:]); print(sh('cat /tmp/ap.log; nmcli -c no device; tail -20 /var/log/NetworkManager/current'))
    m = socket.socket(socket.AF_UNIX); m.connect('/tmp/valence-qmon.sock'); time.sleep(0.5)
    m.sendall(f'screendump {M}/logs/qemu-desktop.ppm\n'.encode()); time.sleep(3); m.close()
    step('screenshot: logs/qemu-desktop.ppm')
    sh('doas poweroff', expect=pexpect.EOF, timeout=300)
    step('desktop test: ' + ('OK' if ok else 'FAILED'))
    sys.exit(0 if ok else 1)

if mode == 'desktop-install':
    import socket, re as _re
    def mon(line):
        m = socket.socket(socket.AF_UNIX); m.connect('/tmp/valence-qmon.sock'); time.sleep(0.3)
        m.sendall(line.encode() + b'\n'); time.sleep(0.4); m.close()
    def count(c):                                   # run c, return the number it echoes as N=<n>
        m = _re.search(r'N=(\d+)', sh(f'echo N=$({c})')); return int(m.group(1)) if m else 0
    step('waiting for GRUB')
    boot_serial_entry(p)
    p.expect(PROMPT, timeout=900); step('logged in on the live desktop ISO')
    sh('stty cols 160 rows 50; export TERM=vt100')
    sh(f'export VALENCE_DISK={DISKNAME} VALENCE_HOSTNAME=valencedesk VALENCE_ROOTPW=valenceroot VALENCE_USER=jcole '
       'VALENCE_USERPW=valenceuser VALENCE_PROFILE=desktop VALENCE_YES=1 VALENCE_SERIAL=1 VALENCE_ENCRYPT=n')
    out = sh('valence-install', timeout=3600)
    ok = 'Valence is installed' in out
    step(f'desktop install: {"OK" if ok else "FAILED"}')
    if not ok: print(out[-3000:])
    p.sendline('poweroff'); p.expect(pexpect.EOF, timeout=300)
    if not ok: sys.exit(1)
    # second boot: the installed disk, with a graphics card for SDDM and KWin; the serial port stays the test's console
    cmd = [c for c in base if c != '-nographic'] + ['-display', 'none', '-serial', 'stdio', '-device', 'virtio-vga'] \
          + disk_args(disk) + ['-boot', 'c']
    p = pexpect.spawn(cmd[0], cmd[1:], encoding='utf-8', codec_errors='replace', timeout=600)
    p.logfile_read = log
    p.expect('login:', timeout=900); p.sendline('root'); p.expect('assword:'); p.sendline('valenceroot')
    p.expect(PROMPT, timeout=120); step('logged in on the installed desktop (serial console)')
    time.sleep(15)                                  # SDDM and KWin are starting: typing now can lose characters
    sh('stty cols 160 rows 50; export TERM=vt100')
    # (comm is cut to 15 characters: sddm-greeter-qt6 shows as sddm-greeter-qt)
    greeter = "ps -e -o pid,comm | awk '$2 ~ /^sddm-greeter/{print $1}' | head -1"
    for _ in range(60):
        pid = count(greeter)
        if pid: break
        time.sleep(2)
    step(f'greeter: {"running" if pid else "NOT RUNNING"}')
    time.sleep(20)                                  # a crash-looping greeter comes back with a new pid
    stable = pid and count(greeter) == pid and count("loginctl list-sessions --no-legend | awk '$3==\"sddm\" && $6==\"active\"' | wc -l") == 1
    ok &= bool(stable); step('greeter after 20 s: ' + ('same process, active session' if stable else 'RESTARTED OR NO SESSION'))
    mon(f'screendump {M}/logs/qemu-desktop-greeter.ppm'); time.sleep(3)
    if not HAVE_KVM: time.sleep(90)   # emulated CPU: the greeter's password field takes a while to take keys
    for ch in 'valenceuser':
        mon(f'sendkey {ch}'); time.sleep(0.15)
    mon('sendkey ret'); step('typed the password into the greeter')
    time.sleep(3); mon(f'screendump {M}/logs/qemu-desktop-splash.ppm')   # Plasma's loading screen
    # the desktop's shell: Plasma's plasmashell, or on the 32-bit edition LXQt's panel (on labwc)
    desk = 'lxqt-panel' if i686 else 'plasmashell'
    shell = f"ps -e -o user,comm | awk '$1==\"jcole\" && $2==\"{desk}\"' | wc -l"
    for _ in range(60):
        if count(shell): break
        time.sleep(3)
    time.sleep(20)                                  # let the desktop finish drawing
    sess = count("loginctl list-sessions --no-legend | awk '$3==\"jcole\" && $6==\"active\"' | wc -l")
    up = count(shell) == 1 and sess == 1; ok &= up
    if i686:
        lab = count("ps -e -o user,comm | awk '$1==\"jcole\" && $2==\"labwc\"' | wc -l"); up &= lab == 1; ok &= lab == 1
        step(f'LXQt for jcole after 20 s: lxqt-panel={count(shell)} labwc={lab} active sessions={sess}')
    else:
        step(f'Plasma for jcole after 20 s: plasmashell={count(shell)} active sessions={sess}')
    mime = count('[ -s /usr/share/mime/mime.cache ] && echo 1 || echo 0') == 1; ok &= mime
    # a service runit keeps restarting shows as "down: 1s, normally up, want up" (polkitd did, raced by D-Bus activation)
    flapping = sh('sv status /var/service/* 2>&1 | grep "want up" | cut -d: -f2 | xargs echo FLAP=')
    flap = _re.search(r'FLAP=([^\r\n]*)', flapping); flap = flap.group(1).strip() if flap else '?'
    ok &= flap == ''; step('services: ' + ('none restarting in a loop' if flap == '' else f'RESTARTING: {flap}'))
    step('MIME cache: ' + ('present' if mime else 'MISSING'))
    # Flatpak must hand X11 apps (Steam, VLC's interface) an Xauthority cookie, or they never open a window
    if not i686:                                    # no Flatpak on the 32-bit edition
        xau = count('ldd /usr/bin/flatpak | grep -c libXau')
        ok &= xau == 1; step('flatpak: ' + ('X11 authorization for sandboxed apps' if xau == 1 else 'BUILT WITHOUT libXau (X11 apps cannot open windows)'))
    # packaged files owned by the build machine's account arrive as nobody's (rule 45)
    nob = count('find /usr /etc -xdev \\( -user 65534 -o -group 65534 \\) | wc -l')
    ok &= nob == 0; step('system files owned by nobody: ' + ('none' if nob == 0 else f'{nob} FOUND'))
    # the desktop profile's services, and the user may manage printers
    svcs = count('n=0; for s in avahi-daemon cupsd; do sv check /var/service/$s >/dev/null 2>&1 && n=$((n+1)); done; echo $n')
    lpadm = count('id -Gn jcole | tr " " "\\n" | grep -cx lpadmin')
    ok &= svcs == 2 and lpadm == 1; step(f'printing on the installed system: {svcs}/2 services up, jcole in lpadmin: {lpadm == 1}')
    time.sleep(10); mon(f'screendump {M}/logs/qemu-desktop-installed.ppm'); time.sleep(3)
    step('screenshots: logs/qemu-desktop-greeter.ppm, -splash.ppm, -installed.ppm')
    p.sendline('poweroff'); p.expect(pexpect.EOF, timeout=300)
    step('installed desktop test: ' + ('OK' if ok else 'FAILED'))
    sys.exit(0 if ok else 1)

if mode == 'dualboot':
    step('waiting for GRUB (UEFI)')
    boot_serial_entry(p)
    p.expect(PROMPT, timeout=900); step('logged in on the live system')
    sh('stty cols 160 rows 50; export TERM=vt100')
    sh(f'export VALENCE_DISK={DISKNAME} VALENCE_HOSTNAME=valencedual VALENCE_ROOTPW=valenceroot VALENCE_USER=jcole VALENCE_USERPW=valenceuser '
       'VALENCE_PROFILE=base VALENCE_YES=1 VALENCE_SERIAL=1 VALENCE_ENCRYPT=n VALENCE_MODE=alongside')
    out = sh('valence-install', timeout=3600)
    ok = 'Valence is installed' in out and 'next to the other systems' in out
    step('install alongside: ' + ('OK' if ok else 'FAILED'))
    if not ok: print(out[-3000:])
    p.sendline('poweroff'); p.expect(pexpect.EOF, timeout=300)
    if not ok: sys.exit(1)
    # boot the disk with no boot order given: the firmware must pick Valence from its own boot menu
    cmd = base + disk_args(disk)
    p = pexpect.spawn(cmd[0], cmd[1:], encoding='utf-8', codec_errors='replace', timeout=600); p.logfile_read = log
    i = p.expect(['Valence Linux', 'fake windows', pexpect.TIMEOUT], timeout=300)
    menu = p.before + (p.after if isinstance(p.after, str) else '')
    ok &= i == 0; step('firmware boot menu: ' + ('started Valence\'s GRUB' if i == 0 else 'DID NOT START VALENCE'))
    try: p.expect('Windows', timeout=5); win_menu = True
    except pexpect.TIMEOUT: win_menu = 'Windows' in menu
    p.expect('login:', timeout=900); p.sendline('root'); p.expect('assword:'); p.sendline('valenceroot'); p.expect(PROMPT, timeout=120)
    sh('stty cols 160 rows 50; export TERM=vt100')
    out = sh("echo WIN=$(grep -c \"menuentry 'Windows'\" /boot/grub/grub.cfg) EFIMNT=$(awk '$2==\"/boot/efi\"{print $2}' /proc/mounts) "
             "NVRAM=$(efibootmgr | grep -c -i valence); ls /boot/efi/EFI | xargs echo ESP:")
    v = dict(_re.findall(r'(WIN|EFIMNT|NVRAM)=(\S*)', out))
    good = v.get('WIN') == '1' and v.get('EFIMNT') == '/boot/efi' and v.get('NVRAM', '0') != '0'; ok &= good
    step(f"installed system: Windows in GRUB {v.get('WIN')} (menu showed it: {win_menu}), EFI partition at {v.get('EFIMNT')}, "
         f"firmware entries for Valence {v.get('NVRAM')}")
    print(sh('ls /boot/efi/EFI; efibootmgr'))
    p.sendline('poweroff'); p.expect(pexpect.EOF, timeout=300)
    after = windows_state()
    same = after == before; ok &= same
    step('Windows untouched: ' + ('partition table entries, MSR, NTFS and its EFI files identical' if same else
         f'CHANGED: {[k for k in before if before[k] != after.get(k)]}'))
    step('dual boot test: ' + ('OK' if ok else 'FAILED'))
    sys.exit(0 if ok else 1)

if mode == 'rollback':
    # Phase 5's "done when": an update is broken on purpose, the system is rolled back, and it boots. Rollback works
    # the way NixOS's generations do, and the test goes both ways, on a disk the live test installed:
    #   1. An update that breaks one program (less). apk took a snapshot before it; "valence-snapshot rollback" and a
    #      restart bring the program and the package database back; /home and /var/log keep what was written since.
    #   2. An update that leaves the computer unable to start: init is replaced by a script that fails, and the
    #      package's install script overwrites the kernel on /boot, the way a kernel update that went wrong would.
    #      The disk must then NOT boot. The boot menu's "Valence snapshots" has an entry per snapshot; the one from
    #      before the update starts the system as it was, with the kernel the snapshot kept, and changes nothing else.
    #      In that session "valence-snapshot rollback" makes it the default, and the next start is that system.
    # The broken updates are real packages, made here with apk mkpkg, signed with the repository key and offered
    # to the VM over HTTP as version 9999 of an installed package.
    import subprocess, shutil, http.server, threading, functools
    arch = 'x86' if i686 else 'x86_64'
    APK = f'{M}/hosttools/bin/apk'; KEY = os.environ.get('VALENCE_SIGN_KEY', f'{M}/keys/valence-signing.rsa')
    T = f'{M}/work/tests/badrepo'; shutil.rmtree(T, ignore_errors=True)
    def bad_package(repo, name, files, post_upgrade=None):
        d = f'{T}/{repo}'; out = f'{d}/{arch}'; os.makedirs(out)
        for path, text in files.items():
            os.makedirs(os.path.dirname(f'{d}/root/{path}'), exist_ok=True)
            open(f'{d}/root/{path}', 'w').write(text); os.chmod(f'{d}/root/{path}', 0o755)
        c = [APK, '--sign-key', KEY, 'mkpkg', '-F', f'{d}/root', '-I', f'name:{name}', '-I', 'version:9999-r0',
             '-I', 'description:an update broken on purpose (qemu-test.py rollback)', '-I', f'arch:{arch}', '-I', 'license:MIT',
             '-I', f'origin:{name}', '-o', f'{out}/{name}-9999-r0.apk']
        if post_upgrade:
            open(f'{d}/post-upgrade', 'w').write(post_upgrade); c += ['-s', f'post-upgrade:{d}/post-upgrade']
        subprocess.run(c, check=True, capture_output=True)
        subprocess.run([APK, '--keys-dir', f'{M}/keys/trusted', '--sign-key', KEY, 'mkndx', '-d', 'broken updates', '-o', 'Packages.adb',
                        f'{name}-9999-r0.apk'], cwd=out, check=True, capture_output=True)
    bad_package('harmless', 'less', {'usr/bin/less': '#!/bin/sh\necho this-less-is-broken\n'})
    broken_init = '#!/bin/sh\necho "this init is broken on purpose"\nexit 1\n'
    bad_package('fatal', 'runit', {'usr/bin/runit': broken_init, 'usr/bin/runit-init': broken_init, 'usr/bin/init': broken_init},
                post_upgrade='#!/bin/sh\necho "not a kernel" > /boot/vmlinuz-valence\nexit 0\n')
    def serve(directory):
        s = http.server.ThreadingHTTPServer(('127.0.0.1', 0), functools.partial(http.server.SimpleHTTPRequestHandler, directory=directory))
        threading.Thread(target=s.serve_forever, daemon=True).start()
        return f'http://10.0.2.2:{s.server_address[1]}'
    badurl, repourl = serve(T), serve(f'{M}/repo')
    apkbad = lambda repo: f'apk --repositories-file /dev/null --repository {badurl}/{repo}/{arch}/Packages.adb'
    apkn = f'apk --repositories-file /dev/null --repository {repourl}/{arch}/Packages.adb'
    failed = []
    def check(cond, what):
        if not cond: failed.append(what)
        step(what + ': ' + ('OK' if cond else 'FAILED'))
        return cond
    def state():
        """what a rollback has to bring back, and what it has to leave alone"""
        out = sh('echo "LESS=$(less --version 2>&1 | head -n1 | cut -c1-8 | tr " " _)" "LESSPKG=$(apk list --installed less 2>/dev/null | cut -d" " -f1)" '
                 '"RUNIT=$(apk list --installed runit 2>/dev/null | cut -d" " -f1)" "KERNEL=$(sha256sum /boot/vmlinuz-valence | cut -c1-16)" '
                 '"ROOT=$(btrfs subvolume show / | head -n1)" "IDS=$(ls /.snapshots | grep -E "^[0-9]+$" | sort -n | xargs | tr " " ,)" '
                 '"ETC=$(cat /etc/rollback-test 2>/dev/null)" "HOME=$(cat /home/jcole/rollback-test 2>/dev/null)" '
                 '"LOG=$(cat /var/log/rollback-test 2>/dev/null)" "LEFT=$(btrfs subvolume list / | grep -c -E " @(old|new|prev|trial)$")" '
                 '"ROOTLINE=$(awk \'$2 == "/" {print $4}\' /etc/fstab | cut -d, -f1)" "state-$((40+2))"', timeout=120)
        s = dict(_re.findall(r'(\w+)=(\S*)', out.split('state-42')[0] if 'state-42' in out else ''))
        s['IDS'] = [int(x) for x in s.get('IDS', '').split(',') if x]
        return s
    def number(pattern, out):
        m = _re.search(pattern, out)
        return int(m.group(1)) if m else 0
    def stop(power_off=True):
        if power_off:
            try: p.sendline('poweroff'); p.expect(pexpect.EOF, timeout=300)
            except Exception: p.close(force=True)
        else:
            p.close(force=True)
        step('rollback test: FAILED: ' + '; '.join(failed)); sys.exit(1)

    step('waiting for the installed system'); boot_installed(); step('logged in')
    print(sh('findmnt -n -o TARGET,FSTYPE,FSROOT,OPTIONS -t btrfs; swapon --show; valence-snapshot list; cat /boot/grub/snapshots.cfg | cut -c1-240'))
    sh('rm -f /etc/rollback-test /home/jcole/rollback-test /var/log/rollback-test')
    s0 = state(); print('before:', s0)
    if not check(s0.get('ROOT') == '@' and s0.get('LESSPKG', '').startswith('less-') and len(s0.get('KERNEL', '')) == 16,
                 'the system runs from the subvolume @ and its state can be read'): stop()

    # ---- 1. a broken program, rolled back with the command
    out = sh(f'{apkbad("harmless")} upgrade; echo upgraded-$((40+2))', timeout=900); print(out)
    snap1 = number(r'snapshot (\d+) taken', out)
    sh('echo after-1 > /etc/rollback-test; echo after-1 > /home/jcole/rollback-test; echo after-1 > /var/log/rollback-test')
    s1 = state(); print('after the broken update:', s1)
    check(snap1 > 0 and snap1 == max(s1['IDS']) and snap1 not in s0['IDS'], f'apk took a snapshot before the update (number {snap1})')
    if not check(s1.get('LESS') == 'this-les' and s1.get('LESSPKG') == 'less-9999-r0', 'the update went in and less is broken'): stop()
    out = sh('valence-snapshot rollback -y; echo "rc=$?"', timeout=600); print(out)
    kept1 = number(r'it replaces is snapshot (\d+)', out)
    check(f'Rolled back to snapshot {snap1}.' in out and 'rc=0' in out and kept1 == snap1 + 1,
          f'valence-snapshot rollback, with no number, went to that snapshot and kept the present system as snapshot {kept1}')
    if not offline:
        out = sh(f'{apkn} add less-doc; echo "rc=$?"; ls /usr/share/man/man1/less.1* 2>/dev/null | wc -l | sed "s/^/manpage=/"', timeout=600); print(out)
        check('has not been restarted' in out and 'rc=0' not in out and 'manpage=0' in out, 'apk changes nothing between a rollback and the restart')
    p.sendline('reboot'); p.expect(pexpect.EOF, timeout=300)
    spawn(); boot_installed(); step('restarted')
    out = sh('sleep 5; echo "flag=$(ls /.snapshots/.cleanup 2>/dev/null | wc -l)" "entries=$(grep -c "menuentry .Snapshot " /boot/grub/snapshots.cfg)" '
             f'"kept=$(grep -c "before rolling back to {snap1}" /.snapshots/{kept1}/info)"; valence-snapshot list | tail -n 4'); print(out)
    s2 = state(); print('after the rollback:', s2)
    check(s2.get('LESS') == s0.get('LESS') and s2.get('LESSPKG') == s0.get('LESSPKG') and s2.get('ROOT') == '@' and s2.get('ETC') == ''
          and s2.get('ROOTLINE') == 'subvol=@', f"less works again ({s2.get('LESSPKG')}), the package database and /etc are the snapshot's")
    check(s2.get('HOME') == 'after-1' and s2.get('LOG') == 'after-1', '/home and /var/log kept what was written after the snapshot')
    check(s2.get('LEFT') == '0' and 'flag=0' in out, 'the replaced system (@old) was deleted after the restart')
    check(f"entries={len(s2['IDS'])}" in out and 'kept=1' in out and kept1 in s2['IDS'],
          f'the system as it was is kept as snapshot {kept1}, and the boot menu lists all {len(s2["IDS"])} snapshots')
    if failed: stop()

    # ---- 2. an update after which the computer does not start; the boot menu starts the snapshot from before it
    out = sh(f'{apkbad("fatal")} upgrade; echo upgraded-$((40+2))', timeout=900); print(out)
    snap2 = number(r'snapshot (\d+) taken', out)
    sh('echo after-2 > /etc/rollback-test; echo after-2 > /home/jcole/rollback-test; echo after-2 > /var/log/rollback-test')
    s3 = state(); print('after the fatal update:', s3)
    if not check(snap2 > kept1 and s3.get('RUNIT') == 'runit-9999-r0' and s3.get('KERNEL') != s0.get('KERNEL'),
                 f'the fatal update went in (snapshot {snap2} before it): init and the kernel are broken'): stop()
    print(sh('cat /boot/grub/snapshots.cfg | cut -c1-240 | head -n 8; ls -la /boot /boot/snapshots'))
    # (reboot itself goes through the init that was just broken: restart the hard way, after a sync)
    p.sendline('sync; sync; echo b > /proc/sysrq-trigger'); p.expect(pexpect.EOF, timeout=300)
    spawn()
    # (GRUB says "error: ...: premature end of file /vmlinuz-valence", wrapped after "of", then "Press any key to continue")
    i = p.expect(['login:', 'premature end of', 'invalid magic number', 'you need to load the kernel first', 'Press any key to continue',
                  'Kernel panic', pexpect.TIMEOUT, pexpect.EOF], timeout=240)
    reason = (['IT STARTED'] + ['GRUB cannot load the kernel'] * 4 + ['the kernel panics', 'nothing happens', 'the VM stops'])[i]
    check(i != 0, f'the disk does not boot after the fatal update ({reason})')
    if failed: stop(power_off=False)
    p.close(force=True)
    # switched off and on again. The boot menu is hidden on a disk with one system: Esc opens it. (Only once GRUB
    # is there: on UEFI an earlier Esc opens the firmware's own setup.)
    spawn()
    p.expect('GRUB', timeout=300)
    shown = False
    for _ in range(150):
        p.send('\x1b')
        try: p.expect('Valence snapshots', timeout=0.4); shown = True; break
        except pexpect.TIMEOUT: pass
    if not check(shown, 'Esc opens the boot menu, and it has "Valence snapshots"'): stop(power_off=False)
    time.sleep(1)
    if not check(grub_pick('Valence snap') and grub_pick(f'Snapshot {snap2}:'), f'the menu entry "Snapshot {snap2}" can be chosen'): stop(power_off=False)
    seen = boot_installed()
    check(f'as it was in snapshot {snap2}' in seen, 'the entry starts the system as it was in that snapshot, with the kernel the snapshot kept')
    s4 = state(); print('in the system the boot menu started:', s4)
    out = sh('echo "trial=$(cat /run/valence/trial)" "default=$(btrfs subvolume list / | grep -c -E " @$")"; . /etc/profile.d/valence-snapshot.sh; '
             'valence-snapshot list | tail -n 3'); print(out)
    check(s4.get('ROOT') == '@trial' and s4.get('RUNIT') == s0.get('RUNIT') and s4.get('ETC') == '' and s4.get('ROOTLINE') == 'subvol=@trial'
          and f'trial={snap2}' in out and 'started from the boot menu' in out,
          f"it runs from a copy of the snapshot (@trial), with the snapshot's init ({s4.get('RUNIT')}), and says so at login")
    check(s4.get('KERNEL') == s3.get('KERNEL') and 'default=1' in out and s4.get('HOME') == 'after-2' and s4.get('LOG') == 'after-2',
          'the default system is untouched (its broken kernel is still on /boot); /home and /var/log are as they were left')
    if not offline:
        out = sh(f'{apkn} add less-doc; echo "rc=$?"', timeout=600); print(out)
        check('started from the boot menu' in out and 'rc=0' not in out, 'apk changes nothing in a system started from the boot menu until it is kept')
    out = sh('valence-snapshot rollback -y; echo "rc=$?"', timeout=600); print(out)
    kept2 = number(r'What it replaced is snapshot (\d+)', out)
    s5 = state(); print('after keeping it:', s5)
    check(f'Snapshot {snap2} is the system now' in out and 'rc=0' in out and kept2 > snap2 and s5.get('ROOT') == '@' and s5.get('LEFT') == '0'
          and s5.get('KERNEL') == s0.get('KERNEL') and s5.get('ROOTLINE') == 'subvol=@',
          f'valence-snapshot rollback there made it the default at once: it is @, the kernel is back on /boot, the broken system is snapshot {kept2}')
    if failed: stop()
    p.sendline('reboot'); p.expect(pexpect.EOF, timeout=300)
    spawn(); boot_installed(); step('restarted, with nothing chosen in the boot menu')
    s6 = state(); print('after the restart:', s6)
    out = sh(f'sleep 5; echo "kept=$(grep -c "before rolling back to {snap2}" /.snapshots/{kept2}/info)" "flag=$(ls /.snapshots/.cleanup 2>/dev/null | wc -l)" '
             '"trial=$(ls /run/valence/trial 2>/dev/null | wc -l)" "sv=$(sv status /var/service/* 2>/dev/null | grep -c "^run:")"; valence-snapshot list'); print(out)
    check(s6.get('RUNIT') == s0.get('RUNIT') and s6.get('KERNEL') == s0.get('KERNEL') and s6.get('ROOT') == '@' and s6.get('ETC') == ''
          and s6.get('LESS') == s0.get('LESS'), f"the system boots by itself again, with the snapshot's init ({s6.get('RUNIT')}) and kernel")
    check(s6.get('HOME') == 'after-2' and s6.get('LOG') == 'after-2', '/home and /var/log kept what was written after the snapshot')
    check('kept=1' in out and 'flag=0' in out and 'trial=0' in out and s6.get('LEFT') == '0', f'the broken system is kept as snapshot {kept2}, and nothing is left over on the disk')
    sv = number(r'sv=(\d+)', out); check(sv >= 5, f'{sv} services are running')

    # ---- the rest of the command: a snapshot by hand, removing one, and how many apk keeps
    out = sh('valence-snapshot create by the test; valence-snapshot list | tail -n 3'); print(out)
    mine = number(r'snapshot (\d+) taken', out)
    out2 = sh(f'valence-snapshot delete {mine} {kept1}; echo "left=$(ls -d /.snapshots/{mine} /.snapshots/{kept1} 2>/dev/null | wc -l)" '
              '"copies=$(ls /boot/snapshots | wc -l)" "used=$(cat /.snapshots/*/info | grep "^boot=" | sort -u | wc -l)" '
              '"entries=$(grep -c "menuentry .Snapshot " /boot/grub/snapshots.cfg)" "snaps=$(ls /.snapshots | grep -c -E "^[0-9]+$")"'); print(out2)
    v = dict(_re.findall(r'(left|copies|used|entries|snaps)=(\d+)', out2))
    check(mine > 0 and 'by the test' in out and v.get('left') == '0' and v.get('copies') == v.get('used') and v.get('entries') == v.get('snaps'),
          'create and delete work; /boot/snapshots holds exactly the kernel files the snapshots use, the menu one entry per snapshot')
    if not offline:
        out = sh(f'mkdir -p /etc/valence; echo KEEP=2 > /etc/valence/snapshot.conf; {apkn} add less-doc >/dev/null; {apkn} del less-doc >/dev/null; '
                 'rm /etc/valence/snapshot.conf; echo "updates=$(grep -l "^type=update" /.snapshots/*/info | wc -l)" '
                 '"copies=$(ls /boot/snapshots | wc -l)" "used=$(cat /.snapshots/*/info | grep "^boot=" | sort -u | wc -l)"', timeout=900); print(out)
        v = dict(_re.findall(r'(updates|copies|used)=(\d+)', out))
        check(v.get('updates') == '2' and v.get('copies') == v.get('used'), 'with KEEP=2 apk keeps its two newest snapshots, and the kernel copies of the others go')
    p.sendline('poweroff'); p.expect(pexpect.EOF, timeout=300)
    step('rollback test: ' + ('OK' if not failed else 'FAILED: ' + '; '.join(failed)))
    sys.exit(0 if not failed else 1)

if mode == 'live':
    step('waiting for GRUB')
    boot_serial_entry(p)
    step('booting kernel')
    p.expect('Valence live: looking for boot medium', timeout=600)
    step('initramfs running')
    p.expect(PROMPT, timeout=900)
    step('logged in on live system')
    sh('stty cols 160 rows 50; export TERM=vt100')
    print(sh('valencefetch'))
    print(sh('cat /proc/asound/cards; ls /dev/snd'))
    step('running the console installer')
    sh(f'export VALENCE_DISK={DISKNAME} VALENCE_HOSTNAME=valencetest VALENCE_ROOTPW=valenceroot VALENCE_USER=jcole '
       'VALENCE_USERPW=valenceuser VALENCE_PROFILE=base VALENCE_YES=1 VALENCE_SERIAL=1'
       + (' VALENCE_ENCRYPT=y VALENCE_LUKSPW=valenceluks' if luks else ' VALENCE_ENCRYPT=n'))
    out = sh('valence-install', timeout=3600)
    print(out[-3000:])
    ok = 'Valence is installed' in out
    step(f'installer finished: {"OK" if ok else "FAILED"}')
    print(sh('ls /usr/bin | wc -l'))
    p.sendline('poweroff')
    p.expect(pexpect.EOF, timeout=300)
    sys.exit(0 if ok else 1)
elif mode == 'toram':
    step('waiting for GRUB')
    p.expect('Valence live', timeout=300); time.sleep(1)
    p.send('c'); p.expect('grub>', timeout=60)
    for line in ['linux /boot/vmlinuz toram console=tty0 console=ttyS0,115200', 'initrd /boot/initramfs.img', 'boot']:
        for ch in line: p.send(ch); time.sleep(0.08)   # GRUB drops keys that arrive too fast
        p.send('\r'); time.sleep(1)
    p.expect('copying .* MiB to RAM', timeout=600); step('copying to RAM')
    p.expect('you can remove the boot medium now', timeout=900); step('copy finished')
    p.expect(PROMPT, timeout=900); step('logged in')
    # pull the "USB stick": eject the CD through the QEMU monitor
    import socket
    m = socket.socket(socket.AF_UNIX); m.connect('/tmp/valence-qmon.sock'); time.sleep(0.5)
    m.sendall(b'eject -f ide1-cd0\n'); time.sleep(1); m.sendall(b'info block\n'); time.sleep(1)
    print(m.recv(65536).decode(errors='replace')); m.close()
    step('boot medium ejected')
    sh('stty cols 160 rows 50; export TERM=vt100')
    for c in ['grep -E "media|sr0" /proc/mounts', 'ls -la /media/valence/valence', 'dd if=/dev/sr0 of=/dev/null bs=2048 count=1', 'free -m',
              'valencefetch | tail -3', 'btrfs --version']:
        print(f'$ {c}'); print(sh(c))
    sh(f'export VALENCE_DISK={DISKNAME} VALENCE_HOSTNAME=ramtest VALENCE_ROOTPW=valenceroot VALENCE_USER=jcole '
       'VALENCE_USERPW=valenceuser VALENCE_PROFILE=base VALENCE_YES=1 VALENCE_SERIAL=1')
    out = sh('valence-install', timeout=3600)
    ok = 'Valence is installed' in out
    step(f'install with the medium removed: {"OK" if ok else "FAILED"}')
    if not ok: print(out[-3000:])
    p.sendline('poweroff'); p.expect(pexpect.EOF, timeout=300)
    sys.exit(0 if ok else 1)
else:
    step('waiting for GRUB on the installed disk')
    # The boot menu is hidden on a disk with no other system, and GRUB then prints nothing after its banner. The first
    # thing an encrypted disk says is the initramfs asking for the passphrase; "Valence Linux" (/etc/issue) only comes
    # once the root is unlocked. Waiting for the name first, as this did, could never succeed with an encrypted root.
    if luks:
        p.expect('unlocking the encrypted disk', timeout=900); step('initramfs asks for the passphrase')
        p.expect('assphrase', timeout=120); p.sendline('valenceluks'); step('passphrase sent')
    p.expect("Valence Linux", timeout=900)
    step('kernel booting')
    p.expect('login:', timeout=900)
    step('login prompt')
    p.sendline('root'); p.expect('assword:'); p.sendline('valenceroot')
    p.expect(PROMPT, timeout=120)
    step('logged in')
    sh('stty cols 160 rows 50; export TERM=vt100')
    for c in ['valencefetch', 'cat /etc/os-release', 'findmnt / /boot', 'valence-svc status',
              'apk list --installed | wc -l', 'apk update && apk search -q | wc -l',
              'ls /sys/firmware/efi >/dev/null 2>&1 && echo booted-UEFI || echo booted-BIOS',
              'id jcole', 'ip -4 addr show dev eth0 | grep inet; ping -c1 -W3 10.0.2.2 >/dev/null && echo net-ok',
              'ls /usr/libexec/valence', 'free -m; df -h / /boot', 'wc -l < /proc/modules; ls /usr/lib/modules',
              'lspci 2>/dev/null | head; cat /sys/bus/pci/devices/*/modalias | head -20',
              'modprobe -v snd_hda_intel; ls /dev/snd; dmesg | tail -5',
              'valence-detect-virt; lsmod | grep -E "vmw|vmxnet|pvscsi|virtio|hv_|vbox" ; ls /var/service',
              'sleep 3; sv status /var/service/qemu-ga /var/service/vmtoolsd /var/service/hv_kvp_daemon 2>&1']:
        print(f'$ {c}'); print(sh(c))
    # The GNU tools own the command names (BusyBox keeps /bin/sh and the commands nothing else provides), and the
    # passwords the installer set are yescrypt hashes made by shadow's chpasswd.
    out = sh('for c in ls cp mv cat sort find xargs grep sed tar gzip diff less ps top free pgrep sysctl ip login passwd su '
             'useradd findmnt mkswap findfs blkdiscard; do [ -L /usr/bin/$c ] && echo "LINK-$c"; [ -x /usr/bin/$c ] || echo "MISSING-$c"; done; '
             'echo tools-$((40+2))')
    bad = _re.findall(r'(?:LINK|MISSING)-\w+', out)
    ver = sh('ls --version | head -n1; grep --version | head -n1; sed --version | head -n1; tar --version | head -n1; '
             'find --version | head -n1; diff --version | head -n1; gzip --version | head -n1; ps --version; ip -V')
    print(ver)
    want = ['GNU coreutils', 'GNU grep', 'GNU sed', 'GNU tar', 'GNU findutils', 'GNU diffutils', 'gzip 1.', 'procps-ng', 'iproute2']
    bad += [w for w in want if w not in ver]
    hashes = sh("grep -E '^(root|jcole):' /etc/shadow | cut -d: -f1,2 | cut -c1-9")
    bad += [f'{u} password hash' for u in ('root', 'jcole') if f'{u}:$y$' not in hashes]
    if 'tools-42' not in out: bad.append('the check itself did not finish')
    # findmnt was asked for by the util-linux recipe and silently not built until pkgrel 1
    if not _re.search(r'rootfs=\w+', sh('echo "rootfs=$(findmnt -no FSTYPE /)"')): bad.append('findmnt /')
    step('GNU tools on the installed system: ' + ('OK' if not bad else 'FAILED: ' + ', '.join(bad)))
    # Phase 5: the root is Btrfs, laid out for snapshots (valence-install, valence-snapshot). The system is the
    # subvolume @; /home, /var/log, the snapshots and the swap file have their own, so a rollback leaves them alone.
    out = sh('for m in / /home /var/log /.snapshots /swap; do echo "SUBVOL$m=$(findmnt -n -o FSTYPE,FSROOT -M $m | xargs | tr " " :)"; done; '
             'echo "OPTS=$(findmnt -n -o OPTIONS -M /)"; echo "SWAP=$(swapon --noheadings --show=NAME | xargs)"; '
             'echo "snapshots=$(ls /.snapshots | grep -c -E "^[0-9]+$")"; echo layout-$((40+2))')
    print(out)
    want = {'/': 'btrfs:/@', '/home': 'btrfs:/@home', '/var/log': 'btrfs:/@log', '/.snapshots': 'btrfs:/@snapshots', '/swap': 'btrfs:/@swap'}
    got = dict(_re.findall(r'SUBVOL(/[\w./]*)=(\S*)', out.split('layout-42')[0] if 'layout-42' in out else ''))
    lbad = [f'{m} is {got.get(m)}' for m in want if got.get(m) != want[m]]
    opts = (_re.findall(r'OPTS=(\S+)', out) or [''])[-1]
    lbad += [f'/ is mounted without {o}' for o in ('noatime', 'compress=zstd:1') if o not in opts.split(',')]
    if not _re.search(r'SWAP=/swap/swapfile', out): lbad.append('the swap file is not in use')
    snaps_before = int((_re.findall(r'snapshots=(\d+)', out) or ['-1'])[-1])
    bad += lbad
    step('Btrfs root with subvolumes (@, @home, @log, @snapshots, @swap), swap file in use: ' + ('OK' if not lbad else 'FAILED: ' + ', '.join(lbad)))
    # Phase 3's "done when": the installed system carries no documentation; adding one -doc package makes man work for
    # that package; "docs" brings the -doc part of everything installed; taking them out again leaves nothing behind.
    # (Phase 2's "done when", a package added from a repository over the network, is the same step.) The repository
    # is this build's repo/, served by this script (10.0.2.2 is the host in QEMU's user network). Everything is taken
    # out again afterwards, so the next boot of this disk tests the same thing.
    nfiles = 'find /usr/share/man /usr/share/doc /usr/share/info /usr/share/locale -type f 2>/dev/null | wc -l'
    last = lambda name, text: int((_re.findall(name + r'=(\d+)', text) or ['-1'])[-1])
    out = sh(f'echo "docfiles=$({nfiles})"')
    if last('docfiles', out) != 0: bad.append(f"{last('docfiles', out)} documentation files on the base system")
    step('no documentation on the installed system: ' + ('OK' if last('docfiles', out) == 0 else 'FAILED'))
    if not offline:
        import http.server, threading, functools
        srv = http.server.ThreadingHTTPServer(('127.0.0.1', 0), functools.partial(http.server.SimpleHTTPRequestHandler, directory=f'{M}/repo'))
        threading.Thread(target=srv.serve_forever, daemon=True).start()
        apkn = f'apk --repositories-file /dev/null --repository http://10.0.2.2:{srv.server_address[1]}/{"x86" if i686 else "x86_64"}/Packages.adb'
        out = sh(f'{apkn} add less-doc; ls /usr/share/man/man1/less.1* && echo added-$((40+2)); '
                 'echo "manlines=$(man less 2>/dev/null | grep -c -i less)"; echo "apropos=$(apropos -s 1 less 2>/dev/null | grep -c "^less")"', timeout=600)
        print(out)
        ok = 'added-42' in out and last('manlines', out) > 0
        if not ok: bad.append('apk add less-doc over the network, then man less')
        if last('apropos', out) < 1: bad.append('apropos less (the index of man pages)')
        step('apk add less-doc from a repository over the network, man less, apropos less: ' + ('OK' if ok and last('apropos', out) > 0 else 'FAILED'))
        pages = ['1 ls', '1 grep', '1 bash', '5 passwd', '8 mount', '8 ip', '1 doas', '8 runsv']
        out = sh(f'{apkn} add docs >/dev/null; echo "docpkgs=$(apk list --installed 2>/dev/null | grep -c -- -doc-)"; '
                 'for q in ' + ' '.join(f'"{q}"' for q in pages) + '; do man $q 2>/dev/null | grep -q . || echo "NOPAGE-$q"; done; '
                 'echo alldocs-$((40+2))', timeout=1800)
        print(out)
        n = last('docpkgs', out); nop = _re.findall(r'NOPAGE-\d \w+', out)
        ok = n >= 20 and not nop and 'alldocs-42' in out
        if not ok: bad.append(f'apk add docs ({n} -doc packages' + (', no page for ' + ', '.join(x[7:] for x in nop) if nop else '') + ')')
        step(f'apk add docs: {n} -doc packages came with it, man finds {len(pages) - len(nop)} of {len(pages)} pages: ' + ('OK' if ok else 'FAILED'))
        # (mandoc came as a dependency and leaves with them; /usr/bin/man is then BusyBox's link again)
        out = sh(f'apk del docs less-doc >/dev/null; echo "docfiles=$({nfiles})"; '
                 'echo "docpkgs=$(apk list --installed 2>/dev/null | grep -c -E -- "-doc-|^mandoc-")"; '
                 '[ -L /usr/bin/man ]; echo "manlink=$?"', timeout=900)
        print(out)
        ok = last('docfiles', out) == 0 and last('docpkgs', out) == 0 and last('manlink', out) == 0
        if not ok: bad.append('apk del docs left documentation behind')
        step('apk del docs: nothing left behind: ' + ('OK' if ok else 'FAILED'))
        # each of those three changes had a snapshot taken before it (apk's commit hook), and the boot menu lists them
        out = sh('echo "snapshots=$(ls /.snapshots | grep -c -E "^[0-9]+$")" "entries=$(grep -c "menuentry .Snapshot " /boot/grub/snapshots.cfg)"; '
                 'valence-snapshot list | tail -n 5')
        print(out)
        n = int((_re.findall(r'snapshots=(\d+)', out) or ['-1'])[-1]); e = int((_re.findall(r'entries=(\d+)', out) or ['-1'])[-1])
        ok = n == min(snaps_before + 3, 10) and e == n and 'before apk del docs less-doc' in out
        if not ok: bad.append(f'snapshots before apk changes ({snaps_before} before, {n} after, {e} in the boot menu)')
        step(f'apk took a snapshot before each change ({snaps_before} -> {n}), the boot menu lists {e}: ' + ('OK' if ok else 'FAILED'))
    if bad:
        p.sendline('poweroff'); p.expect(pexpect.EOF, timeout=300); sys.exit(1)
    # sudo as the normal user: asks for the password once, then remembers it
    p.sendline('su - jcole'); p.expect(r'@valence[\w-]*.*\$ ', timeout=60)
    p.sendline('sudo id -u'); p.expect('assword', timeout=60); p.sendline('valenceuser')
    p.expect(r'\n\r*0\r*\n', timeout=60); step('sudo as jcole: OK (got uid 0)')
    p.sendline('sudo -i whoami'); i = p.expect([r'[\r\n]root\r', 'assword'], timeout=60)
    step('sudo -i without asking again: ' + ('OK' if i == 0 else 'asked again'))
    if i == 1: p.sendline('valenceuser'); p.expect('root', timeout=60)
    p.sendline('exit'); p.expect(PROMPT, timeout=60)
    print(sh('valence-svc status; ls /var/log'))
    # the host side of the guest agent: ask the VM for its OS info, then shut it down from the host
    import socket, json
    try:
        q = socket.socket(socket.AF_UNIX); q.connect('/tmp/valence-qga.sock'); q.settimeout(20)
        q.sendall(b'{"execute":"guest-sync","arguments":{"id":1234}}\n'); print('qga sync:', q.recv(4096).decode().strip())
        q.sendall(b'{"execute":"guest-get-osinfo"}\n'); print('qga osinfo:', q.recv(4096).decode().strip())
        q.sendall(b'{"execute":"guest-shutdown"}\n'); step('asked the guest agent to shut down')
        p.expect(pexpect.EOF, timeout=300); step('powered off by the host (guest agent)')
    except Exception as e:
        step(f'guest agent not answering ({e}), powering off from inside')
        p.sendline('poweroff'); p.expect(pexpect.EOF, timeout=300); step('powered off')
