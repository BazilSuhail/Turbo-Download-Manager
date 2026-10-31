"""Generate TDM's installer art and app icons from code (stdlib only).

The mark is ours: a bold orange T with three forward chevrons (a speed mark)
on a charcoal rounded tile. Everything is rasterised here, so the art is
reproducible and carries no third-party pixels. The same geometry is drawn in
src/ui/assets.rs (app_icon) and extension/tools/make-icons.mjs; change all three.

    python packaging/gen_art.py      # rewrites packaging/icons and packaging/windows/*.bmp
"""

import math
import struct
import zlib
from functools import lru_cache
from pathlib import Path

ROOT = Path(__file__).resolve().parent
ICONS = ROOT / "icons"
WIN = ROOT / "windows"

TILE_TOP = (46, 40, 35)
TILE_BOTTOM = (22, 19, 17)
MARK_TOP = (255, 184, 77)
MARK_BOTTOM = (255, 106, 26)
DARK = (28, 20, 16)
DARK2 = (46, 30, 22)
CREAM = (255, 247, 239)
ORANGE = (240, 106, 16)

CHEVRON_TIPS = (0.70, 0.79, 0.88)
CHEVRON_ARM = 0.10  # horizontal run of each arm
CHEVRON_RISE = 0.17  # vertical reach of each arm from the centre line
CHEVRON_HALF = 0.034  # half the stroke width


def seg_dist(px, py, ax, ay, bx, by):
    dx, dy = bx - ax, by - ay
    t = max(0.0, min(1.0, ((px - ax) * dx + (py - ay) * dy) / (dx * dx + dy * dy)))
    return math.hypot(px - ax - t * dx, py - ay - t * dy)


def in_chevron(x, y, tip):
    back = tip - CHEVRON_ARM
    d_up = seg_dist(x, y, back, 0.5 - CHEVRON_RISE, tip, 0.5)
    d_down = seg_dist(x, y, tip, 0.5, back, 0.5 + CHEVRON_RISE)
    return min(d_up, d_down) <= CHEVRON_HALF


def in_t(x, y):
    bar = 0.12 <= x <= 0.56 and 0.30 <= y <= 0.41
    stem = 0.29 <= x <= 0.39 and 0.30 <= y <= 0.72
    return bar or stem


def in_tile(u, v, radius=0.22):
    """Unit-square point inside the rounded tile (centred, half-size 0.5)."""
    dx, dy = abs(u - 0.5) - (0.5 - radius), abs(v - 0.5) - (0.5 - radius)
    if dx <= 0 or dy <= 0:
        return True
    return dx * dx + dy * dy <= radius * radius


def sample(u, v):
    """RGBA-ish colour at unit-square point (u, v), v down; None outside the tile."""
    if not in_tile(u, v):
        return None
    if in_t(u, v) or any(in_chevron(u, v, t) for t in CHEVRON_TIPS):
        t = max(0.0, min(1.0, (v - 0.30) / 0.42))
        return tuple(round(a + (b - a) * t) for a, b in zip(MARK_TOP, MARK_BOTTOM))
    t = v
    return tuple(round(a + (b - a) * t) for a, b in zip(TILE_TOP, TILE_BOTTOM))


@lru_cache(maxsize=None)
def render_icon(size):
    ss = 4 if size <= 256 else 2
    px = []
    for j in range(size):
        row = []
        for i in range(size):
            r = g = b = a = 0
            for sj in range(ss):
                for si in range(ss):
                    c = sample((i + (si + 0.5) / ss) / size, (j + (sj + 0.5) / ss) / size)
                    if c:
                        r, g, b, a = r + c[0], g + c[1], b + c[2], a + 1
            n = ss * ss
            row.append((r // a, g // a, b // a, 255 * a // n) if a else (0, 0, 0, 0))
        px.append(row)
    return tuple(tuple(row) for row in px)


def png(px):
    h, w = len(px), len(px[0])
    raw = b"".join(b"\0" + bytes(c for p in row for c in p) for row in px)

    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))

    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b""))


def ico(images):
    """ICO with one PNG frame per RGBA image (Windows Vista and later read these)."""
    out = struct.pack("<HHH", 0, 1, len(images))
    offset = 6 + 16 * len(images)
    blobs = []
    for px in images:
        s = len(px)
        blob = png(px)
        out += struct.pack("<BBBBHHII", s % 256, s % 256, 0, 0, 1, 32, len(blob), offset)
        offset += len(blob)
        blobs.append(blob)
    return out + b"".join(blobs)


def icns(entries):
    body = b"".join(k + struct.pack(">I", len(d) + 8) + d for k, d in entries)
    return b"icns" + struct.pack(">I", len(body) + 8) + body


def over(dst, src, x0, y0):
    """Alpha-composite RGBA `src` onto RGB `dst` at (x0, y0)."""
    for j, row in enumerate(src):
        for i, (r, g, b, a) in enumerate(row):
            if a:
                d = dst[y0 + j][x0 + i]
                dst[y0 + j][x0 + i] = tuple((c * a + o * (255 - a)) // 255 for c, o in zip((r, g, b), d))


def canvas(w, h, top, bottom):
    return [[tuple(round(a + (b - a) * j / max(h - 1, 1)) for a, b in zip(top, bottom))] * w
            for j in range(h)]


def bmp(px):
    h, w = len(px), len(px[0])
    pad = (4 - (w * 3) % 4) % 4
    data = b"".join(b"".join(bytes((p[2], p[1], p[0])) for p in row) + b"\0" * pad
                    for row in reversed(px))
    return (b"BM" + struct.pack("<IHHI", 54 + len(data), 0, 0, 54)
            + struct.pack("<IiiHHIIiiII", 40, w, h, 1, 24, 0, len(data), 2835, 2835, 0, 0) + data)


def bar(px, y, h, color):
    for j in range(y, y + h):
        px[j] = [color] * len(px[j])


def main():
    ICONS.mkdir(exist_ok=True)
    WIN.mkdir(exist_ok=True)
    art = {s: render_icon(s) for s in (16, 32, 48, 128, 256, 512)}
    for s in (32, 128, 256):
        (ICONS / f"{s}x{s}.png").write_bytes(png(art[s]))
    (ICONS / "icon.png").write_bytes(png(art[512]))
    (ICONS / "icon.ico").write_bytes(ico([art[s] for s in (16, 32, 48, 256)]))
    (ICONS / "icon.icns").write_bytes(icns([
        (b"ic07", png(art[128])), (b"ic08", png(art[256])), (b"ic09", png(art[512]))]))

    # NSIS welcome/finish sidebar, 164x314: dark, mark up top, orange rule.
    side = canvas(164, 314, DARK2, DARK)
    over(side, render_icon(96), 34, 40)
    bar(side, 300, 4, ORANGE)
    (WIN / "sidebar.bmp").write_bytes(bmp(side))

    # NSIS page header, 150x57: matches the dark MUI_BGCOLOR set in the template.
    head = canvas(150, 57, DARK2, DARK)
    over(head, render_icon(41), 101, 8)
    bar(head, 54, 3, ORANGE)
    (WIN / "header.bmp").write_bytes(bmp(head))

    # WiX draws black text on these, so they stay light where text goes.
    banner = canvas(493, 58, CREAM, CREAM)
    over(banner, render_icon(42), 440, 8)
    bar(banner, 56, 2, ORANGE)
    (WIN / "wix-banner.bmp").write_bytes(bmp(banner))
    dialog = canvas(493, 312, CREAM, CREAM)
    strip = canvas(164, 312, DARK2, DARK)
    for j in range(312):
        dialog[j][:164] = strip[j]
    over(dialog, render_icon(96), 34, 48)
    for j in range(312):
        dialog[j][164:167] = [ORANGE] * 3
    (WIN / "wix-dialog.bmp").write_bytes(bmp(dialog))


if __name__ == "__main__":
    main()
