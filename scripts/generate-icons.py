#!/usr/bin/env python3
"""Derive shipped icon assets from the Farthing coin master art.

Deterministic, PIL-only. The master is `art/farthing-icon.png` (full-color
coin-with-bird mark, 781x768). This script derives:

- art/app-icon.png            1024x1024 app icon master: the coin scaled to
                              the macOS icon grid (~824/1024) and centered on
                              a transparent canvas. Feed it to
                              `pnpm tauri icon art/app-icon.png` to regenerate
                              every size in src-tauri/icons/ (icns, ico, pngs).
- src-tauri/icons/tray-icon.png
                              80x64 macOS menu bar *template* image: pure
                              black glyph, alpha-only shape. macOS recolors
                              template images for dark/light menu bars; the
                              tray renders it 18pt tall (width proportional),
                              so the 64px height bakes in @2x retina detail.
- src/lib/assets/farthing-icon.png
                              128px full-color asset for the desktop app's
                              sidebar wordmark (imported via Vite, displayed
                              at ~22px, so 128px covers retina with room to
                              spare without shipping the full master).

Tray glyph: the full coin silhouette is an illegible disc at menu bar sizes,
so the template image is a bird-only silhouette (no coin ring, no FARTHING
banner, no legs/branch). It is extracted from the master by color
classification (the bird's plum/brown/orange against the teal sky and
lighter grounds), isolated with an erode -> seeded flood fill -> dilate pass
that breaks thin background connections (chart grid lines, twigs), then
solidified, smoothed with a morphological close, and given a punched eye.
The tuned constants below are specific to the current master art; re-tune if
the art changes.

Usage: python3 scripts/generate-icons.py
"""

from __future__ import annotations

import pathlib
from collections import deque

from PIL import Image, ImageChops, ImageDraw, ImageFilter

ROOT = pathlib.Path(__file__).resolve().parent.parent
MASTER = ROOT / "art" / "farthing-icon.png"

# --- tray glyph tuning (coordinates are within BIRD_CROP) -------------------

# Region of the master containing the bird (excludes most of the coin ring,
# the FARTHING banner, and the ground).
BIRD_CROP = (80, 150, 650, 560)
# Everything below this row (legs, twigs, ground) is dropped: the glyph is
# the bird body only.
BIRD_BOTTOM = 312
# Seed inside the bird's head for the flood fills.
BIRD_SEED = (360, 80)
# Bird darks (plum head/wing/tail outline) stay under this luminance; the
# ground brown sits just above it (~83), which is what separates them.
DARK_LUM = 80
# Erode/dilate kernel that disconnects thin background features (grid lines,
# twigs) without losing the beak.
ISOLATE_KERNEL = 9
# Morphological close that fills the throat notch left by the background
# shadow under the beak.
CLOSE_KERNEL = 23
# The eye is re-punched as a clean hole (its whites are the same color as
# the cream back, so it cannot be separated by color): center + radius.
EYE_CENTER = (421, 82)
EYE_RADIUS = 16

# Tray canvas: aspect-matched to the bird so the 18pt-tall rendered glyph is
# ~22pt wide instead of floating small inside a square.
TRAY_SIZE = (80, 64)
TRAY_MARGIN = 2


def _flood_component(img: Image.Image, seeds: list[tuple[int, int]]) -> Image.Image:
    """Connected component (4-neighborhood) of `img > 0` reachable from seeds."""
    px = img.load()
    out = Image.new("L", img.size, 0)
    op = out.load()
    w, h = img.size
    queue: deque[tuple[int, int]] = deque()
    for seed in seeds:
        x, y = seed
        if px[x, y] > 0 and op[x, y] == 0:
            op[x, y] = 255
            queue.append(seed)
    while queue:
        x, y = queue.popleft()
        for nx, ny in ((x + 1, y), (x - 1, y), (x, y + 1), (x, y - 1)):
            if 0 <= nx < w and 0 <= ny < h and px[nx, ny] > 0 and op[nx, ny] == 0:
                op[nx, ny] = 255
                queue.append((nx, ny))
    return out


def _solidify(mask: Image.Image) -> Image.Image:
    """Fill every enclosed hole: anything not flood-reachable from the border."""
    px = mask.load()
    w, h = mask.size
    exterior = Image.new("L", mask.size, 0)
    xp = exterior.load()
    queue: deque[tuple[int, int]] = deque()
    for x in range(w):
        for y in (0, h - 1):
            if px[x, y] == 0 and xp[x, y] == 0:
                xp[x, y] = 255
                queue.append((x, y))
    for y in range(h):
        for x in (0, w - 1):
            if px[x, y] == 0 and xp[x, y] == 0:
                xp[x, y] = 255
                queue.append((x, y))
    while queue:
        x, y = queue.popleft()
        for nx, ny in ((x + 1, y), (x - 1, y), (x, y + 1), (x, y - 1)):
            if 0 <= nx < w and 0 <= ny < h and px[nx, ny] == 0 and xp[nx, ny] == 0:
                xp[nx, ny] = 255
                queue.append((nx, ny))
    return ImageChops.invert(exterior)


def bird_silhouette(master: Image.Image) -> Image.Image:
    """Black+alpha bird-only template glyph derived from the coin art."""
    crop = master.crop(BIRD_CROP)
    w, h = crop.size
    px = crop.load()

    # Classify bird pixels: darks (plum/brown/rust) and the orange beak;
    # reject the teal sky, the green arrow, and anything translucent.
    mask = Image.new("L", (w, h), 0)
    mp = mask.load()
    for y in range(min(h, BIRD_BOTTOM)):
        for x in range(w):
            r, g, b, a = px[x, y]
            if a < 128:
                continue
            if (g > r and b > r and g > 120) or (g > r + 30 and g > b + 30):
                continue  # teal sky / green arrow
            lum = 0.299 * r + 0.587 * g + 0.114 * b
            if lum < DARK_LUM or (r > 180 and (r - b) > 90 and (r - g) > 60):
                mp[x, y] = 255

    # Isolate the bird: erosion breaks thin connections to background
    # features, the seeded flood keeps only the bird, dilation restores the
    # original boundary (clipped back to the classified mask).
    eroded = mask.filter(ImageFilter.MinFilter(ISOLATE_KERNEL))
    component = _flood_component(eroded, [BIRD_SEED])
    bird = ImageChops.multiply(component.filter(ImageFilter.MaxFilter(ISOLATE_KERNEL)), mask)

    solid = _solidify(bird)
    solid = solid.filter(ImageFilter.MaxFilter(CLOSE_KERNEL)).filter(
        ImageFilter.MinFilter(CLOSE_KERNEL)
    )
    solid = _flood_component(solid, [BIRD_SEED])  # drop smoothing debris

    draw = ImageDraw.Draw(solid)
    ex, ey = EYE_CENTER
    draw.ellipse((ex - EYE_RADIUS, ey - EYE_RADIUS, ex + EYE_RADIUS, ey + EYE_RADIUS), fill=0)
    return solid


def tray_icon(master: Image.Image) -> Image.Image:
    silhouette = bird_silhouette(master)
    bbox = silhouette.getbbox()
    if bbox is None:
        raise RuntimeError("bird silhouette extraction produced an empty mask")
    glyph = silhouette.crop(bbox)

    cw, ch = TRAY_SIZE
    scale = min(
        (cw - 2 * TRAY_MARGIN) / glyph.width,
        (ch - 2 * TRAY_MARGIN) / glyph.height,
    )
    nw, nh = round(glyph.width * scale), round(glyph.height * scale)
    alpha = Image.new("L", (cw, ch), 0)
    alpha.paste(glyph.resize((nw, nh), Image.LANCZOS), ((cw - nw) // 2, (ch - nh) // 2))

    out = Image.new("RGBA", (cw, ch), (0, 0, 0, 0))
    out.paste(Image.new("RGBA", (cw, ch), (0, 0, 0, 255)), (0, 0), alpha)
    return out


def app_icon(master: Image.Image, size: int = 1024) -> Image.Image:
    """Square master for `pnpm tauri icon`: coin on the macOS icon grid."""
    # macOS icon artwork occupies ~824/1024 of the canvas, centered.
    target = round(size * 824 / 1024)
    scale = target / max(master.size)
    scaled = master.resize(
        (round(master.width * scale), round(master.height * scale)), Image.LANCZOS
    )
    out = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    out.paste(scaled, ((size - scaled.width) // 2, (size - scaled.height) // 2), scaled)
    return out


def sidebar_icon(master: Image.Image, size: int = 128) -> Image.Image:
    scale = size / max(master.size)
    return master.resize(
        (round(master.width * scale), round(master.height * scale)), Image.LANCZOS
    )


def main() -> None:
    master = Image.open(MASTER).convert("RGBA")

    app_path = ROOT / "art" / "app-icon.png"
    app_icon(master).save(app_path)
    print(f"wrote {app_path}")

    tray_path = ROOT / "src-tauri" / "icons" / "tray-icon.png"
    tray_icon(master).save(tray_path)
    print(f"wrote {tray_path}")

    sidebar_path = ROOT / "src" / "lib" / "assets" / "farthing-icon.png"
    sidebar_path.parent.mkdir(parents=True, exist_ok=True)
    sidebar_icon(master).save(sidebar_path)
    print(f"wrote {sidebar_path}")

    print("now run: pnpm tauri icon art/app-icon.png")


if __name__ == "__main__":
    main()
