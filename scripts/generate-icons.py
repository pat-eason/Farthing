#!/usr/bin/env python3
"""Generate placeholder icon assets (task 6.1).

Deterministic, PIL-only. Produces:

- art/app-icon.png            1024x1024 app icon master. Feed it to
                              `pnpm tauri icon art/app-icon.png` to regenerate
                              every size in src-tauri/icons/ (icns, ico, pngs).
- src-tauri/icons/tray-icon.png
                              32x32 macOS menu bar *template* image: pure black
                              glyph, alpha-only shape. macOS recolors template
                              images for dark/light menu bars, so no color and
                              no @2x variant is needed here.

The mark is a three-bar ascending "usage" chart: legible at 16px, and honest
about being a placeholder until the final name/brand lands.

Usage: python3 scripts/generate-icons.py
"""

from __future__ import annotations

import pathlib

from PIL import Image, ImageDraw

ROOT = pathlib.Path(__file__).resolve().parent.parent

# Render at SS x the target size, downscale with Lanczos for clean edges
# (PIL's draw primitives are not antialiased).
SS = 4

# Palette: warm dark ground + coral bars. Coral is in the Claude-tooling
# neighborhood without copying Anthropic's trade dress.
BG_TOP = (38, 31, 24, 255)
BG_BOTTOM = (24, 19, 15, 255)
CORAL = (224, 122, 90, 255)
CORAL_LIGHT = (240, 160, 128, 255)


def _bars(draw: ImageDraw.ImageDraw, box: tuple[float, float, float, float],
          colors: tuple, gap_ratio: float = 0.18) -> None:
    """Draw three ascending rounded bars filling `box` (left, top, right, bottom)."""
    left, top, right, bottom = box
    width = right - left
    height = bottom - top
    heights = (0.40, 0.66, 1.00)
    gap = width * gap_ratio
    bar_w = (width - 2 * gap) / 3
    radius = bar_w * 0.28
    for i, h in enumerate(heights):
        x0 = left + i * (bar_w + gap)
        y0 = bottom - height * h
        draw.rounded_rectangle((x0, y0, x0 + bar_w, bottom), radius=radius,
                               fill=colors[i])


def app_icon(size: int = 1024) -> Image.Image:
    big = size * SS
    img = Image.new("RGBA", (big, big), (0, 0, 0, 0))
    draw = ImageDraw.Draw(img)

    # macOS icon grid: the rounded square occupies ~824/1024 centered.
    margin = round(big * 100 / 1024)
    corner = round(big * 185 / 1024)
    plate = (margin, margin, big - margin, big - margin)

    # Vertical gradient on the plate, clipped by a rounded-rect mask.
    grad = Image.new("RGBA", (big, big), (0, 0, 0, 0))
    gd = ImageDraw.Draw(grad)
    top_y, bot_y = plate[1], plate[3]
    for y in range(top_y, bot_y):
        t = (y - top_y) / max(1, bot_y - top_y)
        color = tuple(
            round(BG_TOP[c] + (BG_BOTTOM[c] - BG_TOP[c]) * t) for c in range(4)
        )
        gd.line([(plate[0], y), (plate[2], y)], fill=color)
    mask = Image.new("L", (big, big), 0)
    ImageDraw.Draw(mask).rounded_rectangle(plate, radius=corner, fill=255)
    img.paste(grad, (0, 0), mask)

    # Glyph: ascending bars in the middle ~46% of the plate.
    plate_w = plate[2] - plate[0]
    inset_x = plate[0] + plate_w * 0.27
    glyph = (inset_x, plate[1] + plate_w * 0.30,
             plate[2] - plate_w * 0.27, plate[3] - plate_w * 0.26)
    _bars(draw, glyph, (CORAL, CORAL, CORAL_LIGHT))

    return img.resize((size, size), Image.LANCZOS)


def tray_icon(size: int = 32) -> Image.Image:
    big = size * SS
    img = Image.new("RGBA", (big, big), (0, 0, 0, 0))
    draw = ImageDraw.Draw(img)
    # Slight inset so the glyph doesn't touch the menu bar bounds.
    inset = big * 0.10
    black = (0, 0, 0, 255)
    _bars(draw, (inset, inset, big - inset, big - inset),
          (black, black, black), gap_ratio=0.16)
    return img.resize((size, size), Image.LANCZOS)


def main() -> None:
    art = ROOT / "art"
    art.mkdir(exist_ok=True)
    app_path = art / "app-icon.png"
    app_icon().save(app_path)
    print(f"wrote {app_path}")

    tray_path = ROOT / "src-tauri" / "icons" / "tray-icon.png"
    tray_icon().save(tray_path)
    print(f"wrote {tray_path}")
    print("now run: pnpm tauri icon art/app-icon.png")


if __name__ == "__main__":
    main()
