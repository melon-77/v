# A dinit variant of Valence (not planned; notes for if it ever happens)

runit is Valence's init, and the owner decided (29 September 2026) that it stays. dinit may one day become an
*optional variant*, built by the owner, never a switch forced on the whole OS. This file keeps the scope that was
measured when a full switch was on the table, so a variant doesn't have to rediscover it.

## What a variant has to cover

- **Services:** every `/etc/sv/<name>/` in the recipes (33 at the time), including the ones shipped by packages
  (dbus, elogind, polkit, eudev's udevd, the VM guest agents). Each needs a dinit service file next to its run script.
  The `./check` readiness scripts map to dinit's readiness notification; the `sv check` waits map to `depends-on`.
- **Boot and shutdown:** the runit stages `/etc/runit/1` (mounts, device coldplug, swap/zram, hostname, the VM
  module blacklist) and `/etc/runit/3` (the explicit `/boot` unmount, rule 16) become dinit boot and shutdown services.
- **Valence's tools:** `valence-svc`, `halt`/`poweroff`/`reboot`, `valence-wifi`, `valencefetch` (shows the init).
- **Installers and image:** the service lists in `/usr/share/valence/profiles/*.services`, how `valence-install`,
  `cal-finish` and `mkiso.sh` enable services, and a choice between the two inits.
- **Tests:** `scripts/qemu-test.py` uses `sv status`, `sv check`, `sv down` and `sv up`.
- **Text that names runit:** AGENTS.md, README, `docs/config.txt`, `/etc/issue` and the installer slideshow.

## Keeping two formats in sync

A variant means two service definitions per service. Without something that generates one from the other, or a
test that boots both variants, they will drift. Decide that before writing the first dinit file.

## Reference

Chimera Linux runs musl, dinit, elogind and KDE Plasma (with turnstile for per-user services); its service files are
the closest model.
