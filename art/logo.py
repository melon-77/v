#!/usr/bin/env python3
"""Valence logo: a bismuth hopper crystal seen straight down (vector, SVG). Bismuth grows as stepped square
spirals with an iridescent oxide skin: gold, rose, violet, blue, teal.
  logo.py --export <dir>           writes valence-logo.svg and valence-logo-<size>.png (needs cairosvg)
  logo.py <concept> <out.svg>      one drawing; "logo" is the chosen mark, the rest are earlier concepts"""
import sys

GOLD, ROSE, VIOLET, BLUE, TEAL = "#F5C535", "#EE5D8C", "#8A5CF0", "#3A86F2", "#18B9A7"
STOPS = [GOLD, ROSE, VIOLET, BLUE, TEAL]


def rgb(h): return tuple(int(h[i:i + 2], 16) for i in (1, 3, 5))
def hexc(c): return "#%02X%02X%02X" % tuple(max(0, min(255, round(v))) for v in c)
def mix(a, b, t): return hexc(tuple(x + (y - x) * t for x, y in zip(rgb(a), rgb(b))))
def shade(c, k): return mix(c, "#0B0B1E", k) if k > 0 else mix(c, "#FFFFFF", -k)


def ramp(t):
    """Colour along the oxide ramp, t in 0..1."""
    t = max(0.0, min(1.0, t)) * (len(STOPS) - 1)
    i = min(int(t), len(STOPS) - 2)
    return mix(STOPS[i], STOPS[i + 1], t - i)


def pts(p): return " ".join(f"{x:g},{y:g}" for x, y in p)


def svg(body, defs=""):
    return (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 512 512" width="512" height="512">'
            f'<defs>{defs}</defs>{body}</svg>\n')


# ------------------------------------------------------------------ hopper: looking down into the stepped pit
DEEP = "#1A1240"
SHEEN = ('<linearGradient id="sheen" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="#FFFFFF" stop-opacity="0.26"/>'
         '<stop offset="0.5" stop-color="#FFFFFF" stop-opacity="0"/></linearGradient>')


def hopper_parts(frames, gaps, shade_tl=True, crisp=0, first=48, size=416):
    """Nested square terraces going down to a floor. frames: the width of each terrace (the last entry is the
    floor). gaps: per step, how far the next terrace sits in on the (top/left, bottom/right) sides; equal
    numbers look straight down, unequal ones tilt the view so the far walls show. The walls on one side are
    in shade (top/left when shade_tl) and the others catch the light. crisp: line width for the lit crests,
    the creases at the foot of the shaded walls and a soft sheen (0 = none). Returns (defs, body)."""
    defs, body, lines = [], [], []
    x0 = y0 = first
    s = size
    n = len(frames)
    for k, th in enumerate(frames):
        a, b = ramp(k / n), ramp((k + 1) / n)
        defs.append(f'<linearGradient id="t{k}" gradientUnits="userSpaceOnUse" x1="{x0:g}" y1="{y0:g}" x2="{x0 + s:g}" y2="{y0 + s:g}">'
                    f'<stop offset="0" stop-color="{shade(a, -0.10)}"/><stop offset="1" stop-color="{b}"/></linearGradient>')
        x1, y1 = x0 + s, y0 + s
        if k == n - 1 or th * 2 >= s:   # the floor of the pit
            body.append(f'<rect x="{x0:g}" y="{y0:g}" width="{s:g}" height="{s:g}" fill="url(#t{k})"/>')
            break
        ix0, iy0, ix1, iy1 = x0 + th, y0 + th, x1 - th, y1 - th
        body.append(f'<path fill-rule="evenodd" fill="url(#t{k})" d="M{x0:g},{y0:g}H{x1:g}V{y1:g}H{x0:g}Z M{ix0:g},{iy0:g}V{iy1:g}H{ix1:g}V{iy0:g}Z"/>')
        tl, br = gaps[k]
        nx0, ny0, nx1, ny1 = ix0 + tl, iy0 + tl, ix1 - br, iy1 - br
        deep, deeper = mix(b, DEEP, 0.50), mix(b, DEEP, 0.66)
        pale, paler = mix(b, "#FFFFFF", 0.38), mix(b, "#FFFFFF", 0.58)
        top, left, right, bottom = ((deep, deeper, paler, pale) if shade_tl else (paler, pale, deep, deeper))
        body.append(f'<polygon fill="{top}" points="{pts([(ix0, iy0), (ix1, iy0), (nx1, ny0), (nx0, ny0)])}"/>')
        body.append(f'<polygon fill="{left}" points="{pts([(ix0, iy0), (nx0, ny0), (nx0, ny1), (ix0, iy1)])}"/>')
        body.append(f'<polygon fill="{right}" points="{pts([(ix1, iy0), (ix1, iy1), (nx1, ny1), (nx1, ny0)])}"/>')
        body.append(f'<polygon fill="{bottom}" points="{pts([(ix0, iy1), (nx0, ny1), (nx1, ny1), (ix1, iy1)])}"/>')
        if crisp:
            h = crisp / 2
            crest = ([(ix1 + h, iy0), (ix1 + h, iy1 + h), (ix0, iy1 + h)] if shade_tl
                     else [(ix0 - h, iy1), (ix0 - h, iy0 - h), (ix1, iy0 - h)])
            foot = ([(nx1, ny0 - h), (nx0 - h, ny0 - h), (nx0 - h, ny1)] if shade_tl
                    else [(nx0, ny1 + h), (nx1 + h, ny1 + h), (nx1 + h, ny0)])
            lines.append(f'<polyline fill="none" stroke="#FFFFFF" stroke-opacity="0.85" stroke-width="{crisp:g}" points="{pts(crest)}"/>')
            lines.append(f'<polyline fill="none" stroke="{DEEP}" stroke-opacity="0.55" stroke-width="{crisp:g}" points="{pts(foot)}"/>')
        x0, y0, s = nx0, ny0, nx1 - nx0
    if crisp:
        defs.append(SHEEN)
        lines.append(f'<rect x="{first}" y="{first}" width="{size}" height="{size}" fill="url(#sheen)"/>')
    return "".join(defs), "".join(body + lines)


def hopper():
    """Straight down: the terraces are centred and every wall is the same width."""
    d, b = hopper_parts([44, 40, 36, 0], [(24, 24)] * 3)
    return svg(b, d)


def hopper_crisp():
    """The same three steps with lit crests, creases and a sheen."""
    d, b = hopper_parts([44, 40, 36, 0], [(24, 24)] * 3, crisp=4)
    return svg(b, d)


def hopper_fine():
    """Six finer steps, closer to a real crystal: more of the oxide rainbow, crisper edges."""
    frames = [30, 27, 24, 21, 19, 17, 0]
    gaps = [(g, g) for g in (13, 12, 11, 10, 9, 8)]
    d, b = hopper_parts(frames, gaps, crisp=2.5)
    return svg(b, d)


def hopper_angled():
    """The first version: seen from an angle, so the far walls are wider."""
    d, b = hopper_parts([44, 40, 36, 0], [(36, 12)] * 3)
    return svg(b, d)


def diamond():
    """The same crystal turned on its corner and seen from the other side, so the shaded walls nest as Vs."""
    d, b = hopper_parts([44, 40, 36, 0], [(12, 38)] * 3, shade_tl=False)
    return svg(f'<g transform="translate(256 256) rotate(45) scale(0.82) translate(-256 -256)">{b}</g>', d)


def hopper_spiral(w=40, g=22, crisp=3):
    """Straight down, and the steps are one staircase winding to the floor, the way bismuth really grows."""
    pitch, h = w + g, w / 2
    lo, hi = 48 + h, 464 - h
    span = hi - lo
    runs = [span, span, span]
    while span - pitch >= 2 * pitch:
        span -= pitch
        runs += [span, span]
    runs.append(span - pitch)             # the last step runs into the wall above it
    P, d, dirs = [(lo, lo)], (1, 0), []
    for r in runs:
        x, y = P[-1]
        P.append((x + d[0] * r, y + d[1] * r))
        dirs.append(d)
        d = (-d[1], d[0])                 # turn right
    turns = (len(runs) - 1) / 4
    inset = round(turns) * pitch
    defs = [f'<linearGradient id="fl" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="{mix(TEAL, DEEP, 0.62)}"/>'
            f'<stop offset="1" stop-color="{mix(TEAL, DEEP, 0.40)}"/></linearGradient>', SHEEN]
    walls, steps, lines = [], [], []
    for i, ((ax, ay), (bx, by)) in enumerate(zip(P, P[1:])):
        ux, uy = dirs[i]
        t0, t1 = i / len(runs), (i + 1) / len(runs)   # colour follows the turns, not the distance
        stops = ([(0.0, ramp(t0))] + [((k / 4 - t0) / (t1 - t0), STOPS[k]) for k in range(5) if t0 < k / 4 < t1]
                 + [(1.0, ramp(t1))])
        defs.append(f'<linearGradient id="s{i}" gradientUnits="userSpaceOnUse" x1="{ax:g}" y1="{ay:g}" x2="{bx:g}" y2="{by:g}">'
                    + "".join(f'<stop offset="{o:.4f}" stop-color="{c}"/>' for o, c in stops) + '</linearGradient>')
        nx, ny = -uy, ux                   # unit normal towards the centre
        first, last = i == 0, i == len(runs) - 1

        def corner(px, py, off, back):     # a point `off` towards the centre, `back` along the run
            return (px + nx * off + ux * back, py + ny * off + uy * back)
        a_out, b_out = corner(ax, ay, -h, -h), corner(bx, by, -h, h)
        a_in = corner(ax, ay, h, -h if first else h)
        b_in = corner(bx, by, h, h if last else -h)
        steps.append(f'<polygon fill="url(#s{i})" stroke="url(#s{i})" stroke-width="0.8" stroke-linejoin="round" points="{pts([a_out, b_out, b_in, a_in])}"/>')
        c = ramp(min(1.0, (t0 + t1) / 2 + 1 / 6))
        shaded = (ux, uy) in ((1, 0), (0, -1))          # walls under the top and left arms face away from the light
        tone = {(1, 0): mix(c, DEEP, 0.50), (0, -1): mix(c, DEEP, 0.66),
                (0, 1): mix(c, "#FFFFFF", 0.58), (-1, 0): mix(c, "#FFFFFF", 0.38)}[(ux, uy)]
        k = h + g
        w_a = corner(ax, ay, k, -h if first else k)
        w_b = corner(bx, by, k, h if last else -k)
        walls.append(f'<polygon fill="{tone}" stroke="{tone}" stroke-width="0.8" stroke-linejoin="round" points="{pts([a_in, b_in, w_b, w_a])}"/>')
        if crisp:
            e = crisp / 2
            if shaded:   # crease at the foot of the wall
                lines.append(f'<line x1="{w_a[0] + nx * e:g}" y1="{w_a[1] + ny * e:g}" x2="{w_b[0] + nx * e:g}" y2="{w_b[1] + ny * e:g}" stroke="{DEEP}" stroke-opacity="0.55" stroke-width="{crisp:g}"/>')
            else:        # lit crest where the step drops away
                lines.append(f'<line x1="{a_in[0] - nx * e:g}" y1="{a_in[1] - ny * e:g}" x2="{b_in[0] - nx * e:g}" y2="{b_in[1] - ny * e:g}" stroke="#FFFFFF" stroke-opacity="0.85" stroke-width="{crisp:g}"/>')
    base = f'<rect x="{48 + inset:g}" y="{48 + inset:g}" width="{416 - 2 * inset:g}" height="{416 - 2 * inset:g}" fill="url(#fl)"/>'
    sheen = '<rect x="48" y="48" width="416" height="416" fill="url(#sheen)"/>' if crisp else ""
    return svg(base + "".join(walls) + "".join(steps) + "".join(lines) + sheen, "".join(defs))


# ------------------------------------------------------------------ spiral: the growth staircase, flat
def spiral():
    w, gap = 50, 22
    pitch = w + gap
    lo, hi = 48 + w / 2, 464 - w / 2
    span = hi - lo
    runs = [span, span, span]
    while span - pitch > w:
        span -= pitch
        runs += [span, span]
    P, d = [(lo, lo)], (1, 0)
    for r in runs:
        x, y = P[-1]
        P.append((x + d[0] * r, y + d[1] * r))
        d = (-d[1], d[0])                 # turn right (clockwise on screen)
    total = sum(runs)
    defs, body, done, h = [], [], 0.0, w / 2
    for i, ((ax, ay), (bx, by)) in enumerate(zip(P, P[1:])):
        length = runs[i]
        ux, uy = (bx - ax) / length, (by - ay) / length
        t0, t1 = done / total, (done + length) / total
        stops = ([(0.0, ramp(t0))] + [((k / 4 - t0) / (t1 - t0), STOPS[k]) for k in range(5) if t0 < k / 4 < t1]
                 + [(1.0, ramp(t1))])
        defs.append(f'<linearGradient id="s{i}" gradientUnits="userSpaceOnUse" x1="{ax:g}" y1="{ay:g}" x2="{bx:g}" y2="{by:g}">'
                    + "".join(f'<stop offset="{o:.4f}" stop-color="{c}"/>' for o, c in stops) + '</linearGradient>')
        nx, ny, ex, ey = -uy * h, ux * h, ux * h, uy * h        # n: towards the centre, e: along the run
        first, last = i == 0, i == len(runs) - 1
        # mitred at the corners so neighbouring runs meet on the diagonal; square at the two free ends
        a_out = (ax - nx - ex, ay - ny - ey)
        a_in = (ax + nx - ex, ay + ny - ey) if first else (ax + nx + ex, ay + ny + ey)
        b_out = (bx - nx + ex, by - ny + ey)
        b_in = (bx + nx + ex, by + ny + ey) if last else (bx + nx - ex, by + ny - ey)
        body.append(f'<polygon fill="url(#s{i})" stroke="url(#s{i})" stroke-width="0.8" stroke-linejoin="round" points="{pts([a_out, b_out, b_in, a_in])}"/>')
        done += length
    return svg("".join(body), "".join(defs))


CONCEPTS = {"logo": hopper_crisp, "hopper": hopper, "hopper-crisp": hopper_crisp, "hopper-fine": hopper_fine,
            "hopper-spiral": hopper_spiral, "hopper-angled": hopper_angled, "spiral": spiral, "diamond": diamond}

SIZES = (16, 32, 48, 64, 128, 256, 512)


def export(folder):
    import os
    import cairosvg
    os.makedirs(folder, exist_ok=True)
    art = CONCEPTS["logo"]()
    open(os.path.join(folder, "valence-logo.svg"), "w", encoding="utf-8").write(art)
    for size in SIZES:
        cairosvg.svg2png(bytestring=art.encode(), write_to=os.path.join(folder, f"valence-logo-{size}.png"),
                         output_width=size, output_height=size)


if __name__ == "__main__":
    if sys.argv[1] == "--export":
        export(sys.argv[2])
    else:
        concept, out = sys.argv[1], sys.argv[2]
        open(out, "w", encoding="utf-8").write(CONCEPTS[concept]())
