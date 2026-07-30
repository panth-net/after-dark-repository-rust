#!/usr/bin/env python3
"""Draw the two app icons.

    tools/package/make_icons.py [output-dir]

Writes `after-dark.icns` and `lunatic-fringe.icns`, which `make_app.sh` copies
into the bundles. They are committed, so packaging works on a machine with no
Python; this script is how they are *changed*.

# Why they are drawn rather than painted

The icons are 1-bit Macintosh artwork on a 32x32 grid, which is the size and
the discipline the originals were drawn at. Everything here is that grid: the
crescent is two overlapping circles, the ship is four points. Nothing is
sampled from After Dark itself — the modules and their artwork are Berkeley
Systems' and are not in this repository, and an icon lifted from them would be
the one piece of their work that shipped.

The colours are the ones the launcher already draws with (`colour::SELECTED` and
`colour::SELECTED_INK` in crates/ad-player/src/ui.rs): white on the deep navy
of a selected row. Dark, and the same dark the application itself uses.

# Why the shape is smooth and the art is not

The rounded square is anti-aliased at every size, because a staircase silhouette
in the Dock reads as a broken icon rather than a retro one. What is inside it is
never interpolated: the 32x32 grid is scaled by whole numbers only, so a 512px
icon is the same drawing with 16px pixels. That contrast is the point — a
Macintosh bitmap, presented properly.
"""

import os
import struct
import subprocess
import sys
import zlib

# The launcher's palette, as ARGB in ui.rs: a selected row and its text.
FIELD = (0x00, 0x00, 0x66)
MARK = (0xFF, 0xFF, 0xFF)

# The corner radius as a fraction of the icon, matching the grid it was drawn
# for: 6px of rounding on a 32px square.
RADIUS = 6 / 32


def circle(cx, cy, r):
    """A test for "inside this circle", in grid units."""
    return lambda x, y: (x + 0.5 - cx) ** 2 + (y + 0.5 - cy) ** 2 <= r * r


def polygon(points):
    """A test for "inside this polygon", by the even-odd rule."""

    def inside(x, y):
        px, py, hit = x + 0.5, y + 0.5, False
        for i, (ax, ay) in enumerate(points):
            bx, by = points[i - 1]
            if (ay > py) != (by > py) and px < (bx - ax) * (py - ay) / (by - ay) + ax:
                hit = not hit
        return hit

    return inside


def sparkle(cx, cy, arm):
    """A four-pointed star: the cross a 1-bit grid can actually draw."""
    return lambda x, y: (x == cx and abs(y - cy) <= arm) or (y == cy and abs(x - cx) <= arm)


def after_dark(g):
    """A crescent moon and three stars, on a `g`-unit grid.

    A moon rather than a toaster. The toaster is theirs; the night is not.
    """
    s = g / 32
    # Two circles of the same radius, offset along the horizontal: the crescent
    # is then symmetric about that line, with the horns level. Offsetting on a
    # diagonal instead gives a lopsided shape that reads as a letter C.
    disc = circle(15 * s, 16 * s, 11 * s)
    bite = circle(21 * s, 16 * s, 11 * s)
    stars = [
        sparkle(round(23 * s), round(8 * s), max(1, round(2 * s))),
        sparkle(round(27 * s), round(16 * s), max(1, round(1 * s))),
        sparkle(round(23 * s), round(24 * s), max(1, round(1 * s))),
    ]
    return lambda x, y: (disc(x, y) and not bite(x, y)) or any(st(x, y) for st in stars)


def lunatic_fringe(g):
    """A ship and a starfield.

    The shape is the one every vector shooter has drawn since 1979 — a triangle
    with the back kicked in — and deliberately not the sprite Lunatic Fringe
    flies, which is Berkeley Systems' drawing and stays in their file.
    """
    s = g / 32
    # Tall and narrow, with the fins swept below the tail. A wide delta with a
    # shallow notch is the same four points and reads as a badge, not a ship —
    # the height is what makes it fly.
    ship = polygon([
        (15.5 * s, 3.5 * s), (16.5 * s, 3.5 * s),   # nose
        (22 * s, 26.5 * s),                         # right fin
        (16 * s, 21.5 * s),                         # the kicked-in tail
        (10 * s, 26.5 * s),                         # left fin
    ])
    stars = [
        sparkle(round(6 * s), round(9 * s), max(1, round(2 * s))),
        sparkle(round(26 * s), round(8 * s), max(1, round(1 * s))),
        sparkle(round(25 * s), round(25 * s), max(1, round(1 * s))),
    ]
    return lambda x, y: ship(x, y) or any(st(x, y) for st in stars)


def coverage(size, x, y, samples=4):
    """How much of pixel (x, y) the rounded square covers, 0.0 to 1.0.

    Anti-aliasing by counting sub-samples inside the shape. Only the silhouette
    gets this; see the module docstring for why the artwork does not.
    """
    r = RADIUS * size
    inside = 0
    for i in range(samples):
        for j in range(samples):
            px, py = x + (i + 0.5) / samples, y + (j + 0.5) / samples
            # Distance to the nearest corner centre, clamped into the middle
            # band where the shape is a plain rectangle.
            cx = min(max(px, r), size - r)
            cy = min(max(py, r), size - r)
            if (px - cx) ** 2 + (py - cy) ** 2 <= r * r:
                inside += 1
    return inside / (samples * samples)


def render(size, motif):
    """One RGBA icon: smooth outside, whole pixels inside."""
    # 16px is drawn on its own 16-unit grid; everything else is the 32-unit
    # drawing at a whole-number scale.
    grid = 16 if size <= 16 else 32
    art = motif(grid)
    step = size // grid
    rows = []
    for y in range(size):
        row = bytearray()
        for x in range(size):
            a = coverage(size, x, y)
            r, g, b = MARK if art(x // step, y // step) else FIELD
            row += bytes((r, g, b, round(a * 255)))
        rows.append(bytes(row))
    return rows


def write_png(path, size, rows):
    """A PNG, written with the standard library and nothing else."""
    raw = b"".join(b"\x00" + row for row in rows)

    def chunk(tag, data):
        c = tag + data
        return struct.pack(">I", len(data)) + c + struct.pack(">I", zlib.crc32(c))

    with open(path, "wb") as f:
        f.write(b"\x89PNG\r\n\x1a\n")
        f.write(chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)))
        f.write(chunk(b"IDAT", zlib.compress(raw, 9)))
        f.write(chunk(b"IEND", b""))


# The slots macOS asks for, as (pixels, filename).
SLOTS = [
    (16, "icon_16x16.png"),
    (32, "icon_16x16@2x.png"),
    (32, "icon_32x32.png"),
    (64, "icon_32x32@2x.png"),
    (128, "icon_128x128.png"),
    (256, "icon_128x128@2x.png"),
    (256, "icon_256x256.png"),
    (512, "icon_256x256@2x.png"),
    (512, "icon_512x512.png"),
    (1024, "icon_512x512@2x.png"),
]


def build(out_dir, name, motif):
    iconset = os.path.join(out_dir, name + ".iconset")
    os.makedirs(iconset, exist_ok=True)
    drawn = {}
    for size, filename in SLOTS:
        if size not in drawn:
            drawn[size] = render(size, motif)
        write_png(os.path.join(iconset, filename), size, drawn[size])
    icns = os.path.join(out_dir, name + ".icns")
    subprocess.run(["iconutil", "-c", "icns", iconset, "-o", icns], check=True)
    for filename in os.listdir(iconset):
        os.remove(os.path.join(iconset, filename))
    os.rmdir(iconset)
    print(f"    {icns}")


if __name__ == "__main__":
    out = sys.argv[1] if len(sys.argv) > 1 else os.path.dirname(os.path.abspath(__file__))
    build(out, "after-dark", after_dark)
    build(out, "lunatic-fringe", lunatic_fringe)
