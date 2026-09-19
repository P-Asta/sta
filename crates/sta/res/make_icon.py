#!/usr/bin/env python3
"""Regenerates sta's application icon — every size, in one run.

    python crates/sta/res/make_icon.py crates/sta/res

Writes, into the directory given (default: this file's own):

    sta.ico           16, 32, 48, 64, 128, 256 — every size Windows asks for, drawn, never upscaled
    icon-16/32/48/64.png  the runtime window icon (`crates/sta/src/window.rs` embeds these four)
    preview-256.png   what the icon looks like, for docs and for a glance in an image viewer

The ICO's entries are the **same bytes** as the PNG files (an ICO entry may be a whole PNG, which
is what every size here is), so `tools/check-icon.mjs` can prove the two came from one run of this
script rather than from an edit of one and not the other.

The artwork is the mark in `res/icon.svg`: an eight-pointed star with a smaller eight-pointed star
cut out of its middle, white on a black rounded square. The two polygons below are that file's two
paths, verbatim, in its own 64x64 viewBox — **edit them together with the SVG**, and re-run this
script afterwards (`node tools/check-icon.mjs` fails while the committed PNGs are from an older
run, but nothing can check the artwork against the SVG for you).

Needs Pillow (`pip install pillow`). Everything is drawn at 8x and resampled down, which is what
keeps the 16 px star from turning into a smudge.
"""

from __future__ import annotations

import struct
import sys
from io import BytesIO
from pathlib import Path

from PIL import Image, ImageDraw

# --------------------------------------------------------------------------------- the artwork

#: `res/icon.svg`, the outer path of the `<g mask=…>` group (viewBox units).
STAR = [
    (31.8126, 8.23438), (34.0767, 26.7215), (48.7501, 15.2501), (37.2787, 29.9235),
    (55.7658, 32.1876), (37.2787, 34.4517), (48.7501, 49.1251), (34.0767, 37.6537),
    (31.8126, 56.1408), (29.5485, 37.6537), (14.8751, 49.1251), (26.3465, 34.4517),
    (7.85938, 32.1876), (26.3465, 29.9235), (14.8751, 15.2501), (29.5485, 26.7215),
]
#: …and the black path inside its luminance mask: the hole in the middle of the star.
HOLE = [
    (27.2037, 19.9451), (32.2586, 28.0671), (37.3135, 19.9451), (35.1448, 29.2626),
    (44.4623, 27.0938), (36.3402, 32.1487), (44.4623, 37.2037), (35.1448, 35.0349),
    (37.3135, 44.3524), (32.2586, 36.2304), (27.2037, 44.3524), (29.3725, 35.0349),
    (20.0549, 37.2037), (28.177, 32.1487), (20.0549, 27.0938), (29.3725, 29.2626),
]

#: The mark's bounding box in those units (the star's four tips), and its centre.
MARK_BOX = (7.85938, 8.23438, 55.7658, 56.1408)
MARK_SPAN = MARK_BOX[2] - MARK_BOX[0]
MARK_CENTRE = ((MARK_BOX[0] + MARK_BOX[2]) / 2, (MARK_BOX[1] + MARK_BOX[3]) / 2)

#: Fractions of the icon's side: how wide the mark stands, and the rounded square's corner radius.
MARK_FRACTION = 182.5 / 256
RADIUS_FRACTION = 24.4 / 256

BACKGROUND = (0, 0, 0, 255)
FOREGROUND = (255, 255, 255, 255)

#: Drawn this many times larger, then resampled down — the only antialiasing in here.
SUPERSAMPLE = 8

#: Sizes the ICO carries. Windows picks from these for Explorer (16/32/48/256), the Start menu and
#: jump lists (32/48), Alt+Tab and the taskbar at 150-200 % DPI (64/128).
ICO_SIZES = (16, 32, 48, 64, 128, 256)
#: The sizes `window.rs` embeds as PNGs for `WM_SETICON`.
RUNTIME_SIZES = (16, 32, 48, 64)
PREVIEW_SIZE = 256


def render(size: int) -> Image.Image:
    """One icon, `size` x `size`, RGBA."""
    big = size * SUPERSAMPLE
    scale = (MARK_FRACTION * big) / MARK_SPAN
    ox = big / 2 - MARK_CENTRE[0] * scale
    oy = big / 2 - MARK_CENTRE[1] * scale
    place = lambda points: [(x * scale + ox, y * scale + oy) for x, y in points]

    icon = Image.new('RGBA', (big, big), (0, 0, 0, 0))
    ImageDraw.Draw(icon).rounded_rectangle(
        (0, 0, big - 1, big - 1), radius=RADIUS_FRACTION * big, fill=BACKGROUND
    )
    # The mark as a mask of its own (star minus hole), so it is one shape however it is filled.
    mask = Image.new('L', (big, big), 0)
    pen = ImageDraw.Draw(mask)
    pen.polygon(place(STAR), fill=255)
    pen.polygon(place(HOLE), fill=0)
    icon.paste(Image.new('RGBA', (big, big), FOREGROUND), (0, 0), mask)
    return icon.resize((size, size), Image.LANCZOS)


def png_bytes(image: Image.Image) -> bytes:
    out = BytesIO()
    image.save(out, format='PNG', optimize=True)
    return out.getvalue()


def ico_bytes(blobs: dict[int, bytes]) -> bytes:
    """An ICO holding each PNG blob as it is — no re-encoding, so the files stay byte-identical."""
    sizes = sorted(blobs)
    header = struct.pack('<HHH', 0, 1, len(sizes))
    offset = len(header) + 16 * len(sizes)
    directory, images = b'', b''
    for size in sizes:
        blob = blobs[size]
        directory += struct.pack(
            '<BBBBHHII', size % 256, size % 256, 0, 0, 1, 32, len(blob), offset
        )  # width/height 0 means 256; 1 plane, 32 bpp
        images += blob
        offset += len(blob)
    return header + directory + images


def main(argv: list[str]) -> int:
    out = Path(argv[1]) if len(argv) > 1 else Path(__file__).resolve().parent
    out.mkdir(parents=True, exist_ok=True)
    blobs = {size: png_bytes(render(size)) for size in sorted({*ICO_SIZES, *RUNTIME_SIZES, PREVIEW_SIZE})}
    for size in RUNTIME_SIZES:
        (out / f'icon-{size}.png').write_bytes(blobs[size])
    (out / 'preview-256.png').write_bytes(blobs[PREVIEW_SIZE])
    (out / 'sta.ico').write_bytes(ico_bytes({size: blobs[size] for size in ICO_SIZES}))
    written = ', '.join(str(s) for s in ICO_SIZES)
    print(f'make_icon: sta.ico ({written}), icon-16/32/48/64.png and preview-256.png in {out}')
    return 0


if __name__ == '__main__':
    raise SystemExit(main(sys.argv))
