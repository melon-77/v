#!/usr/bin/env python3
"""Valence's logo: a folded V in greys (the owner's choice, 19 November 2026; it replaces the bismuth crystal).

Two drawings, 512 by 512 with no background: one for dark backgrounds and a darker run of the same greys for
light ones. The greys ("Graphite"): #F2F4F8 #C3C9D6 #8D95A8 #59607A #2E3350. No wordmark: its typeface is not
chosen yet.

  logo.py --export <dir>   writes valence-logo.svg, valence-logo-on-light.svg and their PNGs (needs cairosvg)
  logo.py --check          the two drawings against the checksums of the files the owner approved
"""
import hashlib, os, sys

# The V in the 512 box: 408 wide (52..460) and 354 high (92..446). Each arm is 118 wide at the top; they meet in a
# fold that runs from the bottom of the notch (256,318) to the tip (256,446).
LEFT, RIGHT = "52,92 170,92 256,318 256,446", "460,92 342,92 256,318 256,446"
ON_DARK = ("#C3C9D6", "#59607A", "#F2F4F8", "#8D95A8")     # left arm from/to, right arm from/to
ON_LIGHT = ("#A2A9BC", "#4A5069", "#C9CEDB", "#747C93")
GRAPHITE = ("#F2F4F8", "#C3C9D6", "#8D95A8", "#59607A", "#2E3350")
SIZES = (16, 32, 48, 64, 128, 256, 512)
SHA256 = {"valence-logo.svg": "7b3c5f66d45da7f2aefe1ddbad9afadbb39b1e66ba591214a3b426ef1263c0e0",
          "valence-logo-on-light.svg": "9ca021197b7ea6193742b8e8fb190147cb6a518a5686fd0666869260ab056fde"}


def gradients(greys, ids=("g0", "g1")):
    """The two arms' fills: the left one runs down to the right, the right one down to the left."""
    a0, a1, b0, b1 = greys
    return (f'<linearGradient id="{ids[0]}" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="{a0}"/><stop offset="1" stop-color="{a1}"/></linearGradient>'
            f'<linearGradient id="{ids[1]}" x1="1" y1="0" x2="0" y2="1"><stop offset="0" stop-color="{b0}"/><stop offset="1" stop-color="{b1}"/></linearGradient>')


def svg(greys):
    # (g2 is not used by anything; it is in the approved files, and these are kept the same to the byte)
    return ('<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 512 512" width="512" height="512"><defs>' + gradients(greys) +
            '<linearGradient id="g2" x1="0" y1="0" x2="1" y2="0"><stop offset="0" stop-color="#05060F" stop-opacity="0.5"/>'
            '<stop offset="1" stop-color="#05060F" stop-opacity="0"/></linearGradient></defs>'
            f'<polygon fill="url(#g0)" points="{LEFT}"/><polygon fill="url(#g1)" points="{RIGHT}"/></svg>\n')


DRAWINGS = {"valence-logo.svg": svg(ON_DARK), "valence-logo-on-light.svg": svg(ON_LIGHT)}


def check():
    bad = [n for n, text in DRAWINGS.items() if hashlib.sha256(text.encode()).hexdigest() != SHA256[n]]
    if bad: sys.exit("not the approved drawing any more: " + ", ".join(bad))


def png(text, size, path):
    import cairosvg
    cairosvg.svg2png(bytestring=text.encode(), write_to=path, output_width=size, output_height=size)


def export(folder):
    check()
    os.makedirs(folder, exist_ok=True)
    for name, text in DRAWINGS.items():
        open(os.path.join(folder, name), "w", encoding="utf-8").write(text)
        for size in SIZES:
            png(text, size, os.path.join(folder, f"{name[:-4]}-{size}.png"))


if __name__ == "__main__":
    if sys.argv[1:2] == ["--export"] and len(sys.argv) == 3:
        export(sys.argv[2])
    elif sys.argv[1:] == ["--check"]:
        check(); print("both drawings are the approved ones")
    else:
        sys.exit(__doc__)
