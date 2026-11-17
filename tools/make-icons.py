"""Draws Veil's mark and writes every icon file the app and the installers use.

Purpose: one place for the logo's geometry, so the README logo, the window icon and the
installer icons always match. The mark is the shipped icon (design concept 33b) without its
rounded teal square: two rings around a disc, with the V cut out of the disc (transparent).
The mark is plain white.

Writes:
- crates/veil-app/assets/veil-logo.svg  the README logo (vector)
- crates/veil-app/assets/veil-256.png   window icon; macOS and Linux packages draw from it
- crates/veil-app/assets/veil.ico       exe icon, 16 to 256 px

Sizes of 32 px and below use the small form (the disc and V only, filling the square),
because the rings blur into noise that small.

Run with Python 3 and Pillow:
  <python with Pillow> tools/make-icons.py
The geometry here must match crates/veil-app/src/logo.rs, which draws the same mark live.
"""

from pathlib import Path

from PIL import Image, ImageChops, ImageDraw

ROOT = Path(__file__).resolve().parent.parent
ASSETS = ROOT / "crates" / "veil-app" / "assets"

WHITE = (0xFF, 0xFF, 0xFF)

# Geometry in units of the mark's half-width (the outer ring's outer edge is 1.0), centre 0,0.
FULL = {
    "rings": [(0.739, 0.109, 0.65), (0.946, 0.109, 0.38)],  # (radius, width, opacity)
    "disc": 0.533,
    "v": [(-0.207, -0.152), (0.0, 0.207), (0.207, -0.152)],
    "v_width": 0.185,
}
SMALL = {
    "rings": [],
    "disc": 1.0,
    "v": [(-0.388, -0.286), (0.0, 0.388), (0.388, -0.286)],
    "v_width": 0.373,
}

PAD = 0.03  # empty margin each side, as a share of the square's half-width
SUPERSAMPLE = 8


def render(size, form):
    """The mark as an RGBA image `size` pixels square."""
    big = size * SUPERSAMPLE
    half = big / 2
    unit = half * (1 - PAD)

    def at(x, y):
        return (half + x * unit, half + y * unit)

    def circle(draw, r, **kw):
        draw.ellipse((half - r * unit, half - r * unit, half + r * unit, half + r * unit), **kw)

    alpha = Image.new("L", (big, big), 0)
    for radius, width, opacity in form["rings"]:
        ring = Image.new("L", (big, big), 0)
        outer = radius + width / 2
        circle(ImageDraw.Draw(ring), outer, outline=round(255 * opacity), width=round(width * unit))
        alpha = ImageChops.lighter(alpha, ring)

    disc = Image.new("L", (big, big), 0)
    circle(ImageDraw.Draw(disc), form["disc"], fill=255)
    v = Image.new("L", (big, big), 0)
    draw = ImageDraw.Draw(v)
    points = [at(x, y) for x, y in form["v"]]
    w = form["v_width"] * unit
    draw.line(points, fill=255, width=round(w), joint="curve")
    for px, py in (points[0], points[-1]):  # round caps
        draw.ellipse((px - w / 2, py - w / 2, px + w / 2, py + w / 2), fill=255)
    alpha = ImageChops.lighter(alpha, ImageChops.subtract(disc, v))

    image = Image.new("RGBA", (big, big), WHITE)
    image.putalpha(alpha)
    return image.resize((size, size), Image.LANCZOS)


def svg():
    """The full mark as SVG, for the README. The V is a mask, so it is a true cut-out."""
    s = 256 * (1 - PAD)

    def n(value):
        return f"{value * s:.1f}"

    rings = "".join(
        f'<circle r="{n(r)}" fill="none" stroke="#FFFFFF" stroke-width="{n(w)}" '
        f'stroke-opacity="{o}"/>'
        for r, w, o in FULL["rings"]
    )
    v = " ".join(f"{n(x)},{n(y)}" for x, y in FULL["v"])
    return (
        '<svg xmlns="http://www.w3.org/2000/svg" width="512" height="512" '
        'viewBox="-256 -256 512 512">'
        '<defs><mask id="v"><rect x="-256" y="-256" width="512" height="512" fill="#fff"/>'
        f'<polyline points="{v}" fill="none" stroke="#000" stroke-width="{n(FULL["v_width"])}" '
        'stroke-linecap="round" stroke-linejoin="round"/></mask></defs>'
        f'{rings}<circle r="{n(FULL["disc"])}" fill="#FFFFFF" mask="url(#v)"/></svg>\n'
    )


def main():
    (ASSETS / "veil-logo.svg").write_text(svg(), encoding="utf-8", newline="\n")
    render(256, FULL).save(ASSETS / "veil-256.png", optimize=True)
    sizes = [256, 128, 64, 48, 32, 24, 16]
    frames = [render(s, FULL if s > 32 else SMALL) for s in sizes]
    frames[0].save(
        ASSETS / "veil.ico",
        format="ICO",
        sizes=[(s, s) for s in sizes],
        append_images=frames[1:],
    )
    print("wrote veil-logo.svg, veil-256.png, veil.ico")


if __name__ == "__main__":
    main()
