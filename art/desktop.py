#!/usr/bin/env python3
"""Valence's desktop pictures and colours, made from the logo (art/logo.py): the grey V and its greys.

  desktop.py --export [black|gate]          writes the pictures into recipes/valence-desktop (the wallpaper in three
                                            sizes, the splash picture, the boot menu's background and boxes) and
                                            the logo into the icons and the installer's pictures
  desktop.py --preview <dir> [black|gate]   the wallpaper and the boot menu's background as small PNGs, to look at

The wallpaper is plain black, and so are the pictures made from it: the login and lock screens show the wallpaper
itself, the loading screen fades into it, and the boot menu's background goes with them. The owner, 20 November
2026: "make it a pure black wallpaper actually". The logo is still on the desktop as the launcher's button, the
icons and the installer's pictures.

"gate" is a drawing that is kept in case it is wanted again: a lattice gate made of the logo's V. The owner had
chosen it from previews an hour before choosing black ("imagine it as like a fence gate where the next row is
flipped compared to the other row", "too dense reduce the amount of v's by like half", "normal gate no logo"). Rows
of V's joined arm to arm alternate with rows of V's upside down. A flipped V sits straight under a V, point on
point, so one arm of the V and the opposite arm of the flipped V make one straight slat. The slats cross in X's and
leave black diamonds, and they are woven: over at one crossing, under at the next. No V is lit.

Needs Pillow, and cairosvg for the logo's copies and the gate. Pictures are drawn at every size, not scaled.
What this replaced was Melon's: a planet, and a green and orange scheme (ROADMAP.md, phase 6). The colour tables
below turned Melon's colours into the logo's greys, once. --export leaves a colour that is already converted alone
and stops at one it does not know, so to change a colour now, change it in the table and in the files it names.
"""
import io, os, re, shutil, sys
HERE = os.path.dirname(os.path.abspath(__file__)); M = os.path.dirname(HERE)
sys.path.insert(0, HERE)
import logo

BLACK = (0, 0, 0)
G1, G2, G3, G4, G5 = [tuple(int(h[i:i + 2], 16) for i in (1, 3, 5)) for h in logo.GRAPHITE]   # lightest to darkest
STEP = 290                 # the next V in a row shares an arm's top with this one
ACROSS = 5.66              # V's across the screen (8 was "too dense": half as many V's is each one 1.41 times larger)
LEVELS = 12                # steps of brightness between the edge of the picture and its brightest place


# ------------------------------------------------------------------------------------------------ pictures
def shade(c, f): return '#%02x%02x%02x' % tuple(min(255, int(int(c[i:i + 2], 16) * f)) for i in (1, 3, 5))


def arm(side, x, y, s, k, flip=False, clip=''):
    """One arm of a V whose 512 box starts at (x, y), in brightness step k. flip turns the V upside down in place:
    its drawing runs from 92 to 446 in the box either way."""
    t = f'translate({x:.2f} {y + 538 * s:.2f}) scale({s:.5f} {-s:.5f})' if flip else f'translate({x:.2f} {y:.2f}) scale({s:.5f})'
    return f'<polygon transform="{t}" fill="url(#{"a" if side == "l" else "b"}{k})"{clip} points="{logo.LEFT if side == "l" else logo.RIGHT}"/>'


def picture(w, h, across=ACROSS, focus=(0.66, 0.50), dim=1.0):
    """The lattice gate (the "gate" wallpaper). dim < 1 makes it quieter (the boot menu puts text on it).

    The numbers come from the V itself. Its arms meet in a fold from 318 to 446, so a flipped V 226 below a V (the
    depth of the notch) has its fold exactly on the V's fold, and the arm of one runs on into the opposite arm of
    the other. The flipped V's feet are then 580 below the V's tops, where the next row of V's starts.
    The weave: at a fold the slat that runs down to the right is on top (the V's left arm over the flipped V's left
    arm, the flipped V's right arm over the V's right arm); where arms share an end the other slat is (the V's right
    arm over the next V's left, the next flipped V's left foot over this one's right). That order goes round in a
    circle, so the flipped V's left arm is drawn twice: whole at the bottom of the pile, and its foot again on top."""
    s = w / (STEP * across)
    P, H = STEP * s, 580 * s
    ox, oy = focus[0] * w - 256 * s, focus[1] * h - 269 * s          # the V in the brightest place
    defs = ''.join(logo.gradients(tuple(shade(c, (0.13 + 0.50 * k / (LEVELS - 1)) * dim) for c in logo.ON_DARK), ids=(f'a{k}', f'b{k}'))
                   for k in range(LEVELS)) + '<clipPath id="foot"><rect x="0" y="60" width="512" height="170"/></clipPath>'

    def step(x, y):                                                   # brightness by distance from that V, in V's
        d = (((x - ox) / P) ** 2 + ((y - oy) / (350 * s)) ** 2) ** 0.5
        return max(0, min(LEVELS - 1, round(2.718281828 ** (-d / 1.7) * (LEVELS - 1))))
    under, vl, vr, fr, feet = [], [], [], [], []
    for j in range(int(-oy // H) - 2, int((h - oy) // H) + 2):
        for i in range(int(-ox // P) - 3, int((w - ox) // P) + 3):
            x, y = ox + i * P, oy + j * H
            k = step(x, y); vl.append(arm('l', x, y, s, k)); vr.append(arm('r', x, y, s, k))
            yf = y + 226 * s; k = step(x, yf)
            under.append(arm('l', x, yf, s, k, True)); fr.append(arm('r', x, yf, s, k, True))
            feet.append(arm('l', x, yf, s, k, True, ' clip-path="url(#foot)"'))
    return (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {w} {h}" width="{w}" height="{h}"><defs>{defs}</defs>'
            f'<rect width="{w}" height="{h}" fill="#000000"/>' + ''.join(under + vl + vr + fr + feet) + '</svg>')


def wall(w, h, variant, dim=1.0, out=None):
    """The wallpaper as an image of w by h (or of the size `out`, drawn as for w by h)."""
    from PIL import Image
    if variant == 'black': return Image.new('RGB', out or (w, h), (0, 0, 0))
    return render(picture(w, h, dim=dim), *(out or (w, h)))


def render(svg, w, h):
    import cairosvg
    from PIL import Image
    return Image.open(io.BytesIO(cairosvg.svg2png(bytestring=svg.encode(), output_width=w, output_height=h))).convert('RGB')


def nine(folder, name, radius, fill, edge=None):
    """A rounded box as GRUB wants it: nine PNGs, <name>_nw.png ... <name>_c.png."""
    from PIL import Image, ImageDraw
    n = radius * 3; ss = 4
    big = Image.new('RGBA', (n * ss, n * ss), (0, 0, 0, 0))
    ImageDraw.Draw(big).rounded_rectangle((0, 0, n * ss - 1, n * ss - 1), radius * ss, fill=fill, outline=edge, width=ss if edge else 0)
    box = big.resize((n, n), Image.LANCZOS)
    r, m = radius, radius + 1
    parts = {'nw': (0, 0, r, r), 'n': (r, 0, m, r), 'ne': (n - r, 0, n, r), 'w': (0, r, r, m), 'c': (r, r, m, m),
             'e': (n - r, r, n, m), 'sw': (0, n - r, r, n), 's': (r, n - r, m, n), 'se': (n - r, n - r, n, n)}
    for k, b in parts.items(): box.crop(b).save(os.path.join(folder, f'{name}_{k}.png'))


# ------------------------------------------------------------------------------------------------ colours
# Plasma's colour scheme, by what each of Melon's colours was for. Graphite: near-black surfaces with the logo's
# cool tint, its greys for text, focus and selection; only "good", "bad" and "careful" keep a hue.
TEXT, QUIET = (236, 238, 244), G3
SCHEME = {
    (35, 40, 36): (40, 42, 50), (90, 110, 90): (100, 104, 118),                    # disabled, inactive effects
    (36, 44, 38): (34, 36, 45), (32, 52, 36): (44, 47, 60),                        # buttons
    (25, 32, 27): (20, 21, 27), (30, 38, 32): (26, 27, 35),                        # windows
    (19, 24, 21): (13, 14, 18), (24, 30, 26): (18, 19, 25),                        # views (lists, text)
    (26, 33, 28): (24, 25, 32), (28, 34, 30): (28, 29, 37),                        # tooltips
    (16, 21, 18): (9, 10, 13), (22, 28, 24): (15, 16, 21),                         # panels ("complementary")
    (30, 40, 33): (25, 27, 35),                                                    # title bars and headers
    (233, 245, 225): TEXT, (150, 165, 150): QUIET,                                 # text, quiet text
    (111, 191, 74): (150, 160, 190), (240, 163, 94): G2,                           # focus, hover
    (84, 150, 56): (72, 79, 104), (74, 130, 50): (62, 68, 92),                     # selection
    (210, 230, 200): (200, 205, 220),                                              # quiet text on a selection
    (129, 203, 96): (143, 180, 255), (155, 89, 182): (180, 160, 230),              # links, visited links
    (218, 68, 83): (224, 90, 104), (246, 116, 0): (222, 160, 70), (39, 174, 96): (88, 186, 140),   # bad, careful, good
    (253, 188, 75): (245, 220, 150), (176, 55, 69): (255, 180, 188),               # the same on a selection
    (198, 92, 0): (255, 214, 150), (23, 104, 57): (170, 236, 200),
    (252, 252, 252): (252, 252, 252),
}
ACCENT = (112, 122, 156)
KONSOLE = {(16, 21, 18): (8, 9, 12), (22, 30, 25): (20, 21, 27), (230, 241, 222): (228, 231, 238), (24, 30, 26): (20, 21, 27),
           (72, 86, 76): G4, (111, 191, 74): (98, 200, 140), (150, 220, 110): (140, 228, 176),
           (208, 222, 200): G2, (248, 252, 244): G1}
def hexs(c): return '#%02x%02x%02x' % tuple(c)


def recolour(path, table, strict=True):
    """Every 'Key=r,g,b' in an ini file through the table."""
    missing, done = set(), set(table.values())            # (a colour that is already a new one stays)
    def one_(m):
        c = tuple(int(x) for x in m.group(2).split(','))
        if c not in table:
            if c not in done: missing.add(c)
            return m.group(0)
        return m.group(1) + ','.join(map(str, table[c]))
    s = open(path, encoding='utf-8').read()
    s2 = re.sub(r'^(\w+=)(\d+,\d+,\d+)$', one_, s, flags=re.M)
    if strict and missing: sys.exit(f'{path}: no new colour for {sorted(missing)} (is this the file as Melon had it?)')
    open(path, 'w', encoding='utf-8').write(s2)


def swap(path, pairs):
    """Exact strings in a text file; a string that is already the new one counts as done."""
    s = open(path, encoding='utf-8').read()
    for old, new in pairs:
        if old in s: s = s.replace(old, new)
        elif new not in s: sys.exit(f'{path}: neither {old!r} nor its replacement is there')
    open(path, 'w', encoding='utf-8').write(s)


def export(variant='black'):
    logo.check()
    D = f'{M}/recipes/valence-desktop/files/usr/share'
    E = f'{M}/recipes/valence-desktop/files/etc'
    C = f'{M}/recipes/calamares-valence'
    # ---- pictures
    wp = f'{D}/wallpapers/valence/contents'
    for w, h in ((1920, 1080), (2560, 1440), (3840, 2160)):
        wall(w, h, variant).save(f'{wp}/images/{w}x{h}.jpg', quality=92, optimize=True)
    wall(1600, 900, variant, out=(400, 225)).save(f'{wp}/screenshot.jpg', quality=90, optimize=True)
    wall(1920, 1080, variant).save(f'{D}/plasma/look-and-feel/org.valence.desktop/contents/splash/images/splash.jpg', quality=92, optimize=True)
    g = f'{D}/valence/grub/themes/valence'
    wall(1920, 1080, variant, dim=0.55).save(f'{g}/background.png', optimize=True)
    nine(g, 'menu', 14, (10, 11, 15, 220), (255, 255, 255, 30))
    nine(g, 'select', 8, G2 + (255,))
    # ---- the logo, wherever a copy of it is shipped
    for size in (32, 48, 64, 128, 256):
        logo.png(logo.DRAWINGS['valence-logo.svg'], size, f'{D}/icons/hicolor/{size}x{size}/apps/valence.png')
        logo.png(logo.DRAWINGS['valence-logo.svg'], size, f'{C}/files/usr/share/icons/hicolor/{size}x{size}/apps/valence-installer.png')
    for path in (f'{D}/icons/hicolor/scalable/apps/valence.svg', f'{C}/files/usr/share/icons/hicolor/scalable/apps/valence-installer.svg'):
        open(path, 'w', encoding='utf-8').write(logo.DRAWINGS['valence-logo.svg'])
    logo.png(logo.DRAWINGS['valence-logo.svg'], 256, f'{D}/pixmaps/valence.png')
    logo.png(logo.DRAWINGS['valence-logo.svg'], 256, f'{C}/branding/valence/logo.png')              # on the dark sidebar
    logo.png(logo.DRAWINGS['valence-logo.svg'], 64, f'{C}/branding/valence/icon.png')
    logo.png(logo.DRAWINGS['valence-logo-on-light.svg'], 512, f'{C}/branding/valence/welcome.png')  # on the installer's light pages
    # ---- colours
    recolour(f'{D}/color-schemes/ValenceDark.colors', SCHEME)
    recolour(f'{D}/plasma/desktoptheme/valence/colors', SCHEME)
    recolour(f'{D}/konsole/Valence.colorscheme', KONSOLE, strict=False)
    acc = ','.join(map(str, ACCENT))
    swap(f'{E}/xdg/kdeglobals', [('AccentColor=111,191,74', f'AccentColor={acc}')])
    swap(f'{D}/plasma/look-and-feel/org.valence.desktop/contents/defaults', [('AccentColor=111,191,74', f'AccentColor={acc}')])
    swap(f'{D}/sddm/themes/valence/theme.conf', [('accent=#6fbf4a', f'accent={hexs(G2)}')])
    swap(f'{D}/sddm/themes/valence/Main.qml', [
        ("the wallpaper's V's,", "the wallpaper,"), ("the Valence planet,", "the wallpaper,"),
        ('color: "#0a0e0c"', 'color: "#000000"'), ('config.accent || "#6fbf4a"', f'config.accent || "{hexs(G2)}"'),
        ('readonly property color text: "#eef6e8"', f'readonly property color text: "{hexs(G1)}"'),
        ('"#b0070a08"', '"#b0000000"'), ('"#60070a08"', '"#60000000"'), ('"#10070a08"', '"#10000000"'),
        ('placeholderTextColor: "#99b0a0"', f'placeholderTextColor: "{hexs(G3)}"'),
        ('color: "#99101612"', 'color: "#99101116"'), ('color: "#66101612"', 'color: "#66101116"'),
        ('font: go.font; color: "#0b150c"', 'font: go.font; color: "#0b0c10"')])
    sp = f'{D}/plasma/look-and-feel/org.valence.desktop/contents/splash/Splash.qml'
    swap(sp, [("// Valence's loading screen: the planet from the default wallpaper eases up into place while Plasma starts,",
               "// Valence's loading screen: the default wallpaper eases into place while Plasma starts,"),
              ('color: "#0a0e0c"', 'color: "#000000"'), ('color: "#d6ecc8"', f'color: "{hexs(G2)}"'), ('color: "#8fd46a"', f'color: "{hexs(G2)}"')])
    s = open(sp, encoding='utf-8').read()
    open(sp, 'w', encoding='utf-8').write(re.sub(r'\bplanet\b', 'picture', s))
    swap(f'{g}/theme.txt', [
        ('desktop-color: "#0a0e0c"', 'desktop-color: "#000000"'), ('item_color = "#d6e8cc"', f'item_color = "{hexs(G2)}"'),
        ('selected_item_color = "#0b150c"', 'selected_item_color = "#0b0c10"'),
        ('color = "#a8c2a8"', f'color = "{hexs(G3)}"'), ('color = "#7f9a82"', 'color = "#6f7690"')])
    swap(f'{C}/branding/valence/branding.desc', [
        ('SidebarBackground:        "#16301c"', 'SidebarBackground:        "#101116"'),
        ('SidebarText:              "#e9f5e1"', f'SidebarText:              "{hexs(G2)}"'),
        ('SidebarTextCurrent:       "#16301c"', 'SidebarTextCurrent:       "#0b0c10"'),
        ('SidebarBackgroundCurrent: "#f0a35e"', f'SidebarBackgroundCurrent: "{hexs(G2)}"')])
    swap(f'{C}/branding/valence/show.qml', [
        ('text: "Valence is being installed."; font.pixelSize: 20; color: "#1d3b22"', f'text: "Valence is being installed."; font.pixelSize: 20; color: "{hexs(G5)}"'),
        ('text: "musl, BusyBox, runit and apk underneath.\\nKDE Plasma on Wayland on top.\\nFlatpak for games."; font.pixelSize: 22; color: "#1d3b22"',
         'text: "glibc, the GNU tools, runit and apk underneath.\\nKDE Plasma on Wayland on top.\\nA snapshot before every update, so a bad one can be undone."; '
         f'font.pixelSize: 22; color: "{hexs(G5)}"')])
    swap(f'{M}/recipes/valence-desktop/files/usr/bin/valence-first-boot', [
        ("O=$(printf '\\033[38;5;215m') G=$(printf '\\033[38;5;114m')", "O=$(printf '\\033[38;5;255m') G=$(printf '\\033[38;5;146m')"),
        ("""       from Flathub); the desktop itself can't use it (NVIDIA's libraries need glibc), so it keeps running
       on the laptop's other graphics chip, or in software on a PC with only this card
""", """       from Flathub); the desktop itself does not use it (Valence has no package of NVIDIA's own
       libraries yet), so it keeps running on the laptop's other graphics chip, or in software on a PC
       with only this card
""")])
    print('written')


if __name__ == '__main__':
    a = sys.argv[1:]
    variant = a.pop() if a and a[-1] in ('black', 'gate') else 'black'
    if len(a) == 2 and a[0] == '--preview':
        os.makedirs(a[1], exist_ok=True)
        wall(1920, 1080, variant, out=(1280, 720)).save(os.path.join(a[1], 'wallpaper.png'))
        wall(1920, 1080, variant, dim=0.55, out=(1280, 720)).save(os.path.join(a[1], 'bootmenu.png'))
    elif a == ['--export']:
        export(variant)
    else:
        sys.exit(__doc__)
