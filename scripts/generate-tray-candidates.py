#!/usr/bin/env python3
"""Stage-1 tray icon candidates from the new bird art (art/tray-source-bird.png).

The source is a 1536x1024 render: stylized bird + green rising arrow + glowing
orange dot on an OPAQUE dark gray background (no alpha). Every element bleeds a
soft element-colored glow halo into the background, so there is no global
saturation or highpass cut that separates mark from glow. Instead the mark is
reconstructed per element (the repo's tuned-constants pattern):
crisp flat-color elements (wing, belly, beak, eye ring,
arrow) are extracted by seeded flood fills over color distance to seed colors,
clamped to per-element boxes; the soft-shaded head dome is approximated with a
drawn ellipse; the glowing dot is a fitted disc. The cast shadow, the bright
glow streak, the branch, and the legs (which are color-identical to the glow
streak they stand in) are deliberately excluded.

Outputs (art/tray-candidates/), all RGBA, height 64px (= 18pt tray height @2x):
- template-full.png   black+alpha silhouette of bird + arrow + dot (eye punched)
- template-bird.png   black+alpha silhouette of the bird only (eye punched)
- color.png           full-color mark, glow clipped, transparent background
- preview.png         simulated macOS menu bar strips (light + dark) for each

The CHOSEN candidate (template-bird) is also written to the shipped path,
src-tauri/icons/tray-icon.png (82x64), which tray.rs embeds via
`include_bytes!` with `icon_as_template(true)`. This script owns that file;
scripts/generate-icons.py no longer writes a tray glyph.

Deterministic, PIL-only. Tuned constants are specific to this source art.

Usage: python3 scripts/generate-tray-candidates.py [--debug]
"""

from __future__ import annotations

import pathlib
import sys
from collections import deque

from PIL import Image, ImageChops, ImageDraw, ImageFilter, ImageFont

ROOT = pathlib.Path(__file__).resolve().parent.parent
SRC = ROOT / "art" / "tray-source-bird.png"
OUT = ROOT / "art" / "tray-candidates"
# Shipped tray glyph (the chosen candidate: template-bird), embedded by
# src-tauri/src/tray.rs via include_bytes!.
SHIPPED = ROOT / "src-tauri" / "icons" / "tray-icon.png"

# --- extraction tuning (coordinates are within MARK_REGION) ------------------

# Crop around the mark (saturation bbox + 60px margin) in source coordinates.
MARK_REGION = (474, 184, 1154, 804)

# Flood-fill elements: seeds sit in each element's flat-color core; a pixel
# joins if its sum-abs RGB distance to ANY seed's color is <= tol and it is
# 4-connected to a seed within box. Tolerances are tuned so the flood stops
# inside the element-colored halo instead of leaking through it.
ELEMENTS = {
    "wing": dict(seeds=[(220, 310), (250, 290), (300, 300)], tol=45, box=(150, 250, 420, 420)),
    "tail": dict(
        seeds=[(200, 350), (220, 355), (210, 365), (230, 345)], tol=35, box=(140, 320, 290, 400)
    ),
    "belly": dict(seeds=[(300, 370), (260, 395)], tol=80, box=(200, 300, 420, 430)),
    "beak": dict(seeds=[(395, 235), (410, 230)], tol=35, box=(375, 215, 440, 255)),
    "eye": dict(seeds=[(344, 237)], tol=80, box=(335, 218, 378, 258)),
    "arrow": dict(
        seeds=[(455, 300), (430, 330), (480, 265), (445, 315)], tol=35, box=(360, 245, 515, 420)
    ),
}
BIRD_ELEMENTS = ("wing", "tail", "belly", "beak", "eye")
# The head is a soft-shaded dome with no flat core (any color cut of it is
# arbitrary), so it is approximated geometrically.
HEAD_ELLIPSE = (340 - 56, 237 - 62, 340 + 56, 237 + 62)
# The glowing dot's ring fades radially; a fitted disc is the clean cut.
# Center from the centroid of the bright cream core.
DOT_CENTER, DOT_RADIUS = (453, 206), 33
# The arrow flood's tight tolerance shaves its halo a little thin; plump it.
ARROW_DILATE = 3
# Bird eye (cream ring + pupil) punched as a hole in the template variants so
# the head reads as a bird instead of a blob at 18pt.
EYE_CENTER, EYE_RADIUS = (356, 237), 15
# Post-pass: close seams between adjacent elements, fill interior holes
# (pupil, AA seams), then round off fringe lumps with blur + re-threshold.
CLOSE_KERNEL = 5
MAX_HOLE = 4000
SMOOTH_BLUR = 3
MIN_COMPONENT = 400

TRAY_HEIGHT = 64  # 18pt @2x

# --- preview tuning -----------------------------------------------------------

BAR_W, BAR_H = 800, 56  # menu bar strip @2x
ICON_H = 36  # 18pt tray render height @2x
TITLE = " $74.78"
LIGHT_BG, LIGHT_FG = (245, 245, 247), (0, 0, 0)
DARK_BG, DARK_FG = (38, 38, 40), (255, 255, 255)
CAPTION_H = 26
FONT = "/System/Library/Fonts/Helvetica.ttc"


def _flood(crop: Image.Image, spec: dict) -> Image.Image:
    px = crop.load()
    w, h = crop.size
    x0, y0, x1, y1 = spec["box"]
    tol = spec["tol"]
    colors = [px[s] for s in spec["seeds"]]

    def ok(p: tuple[int, int, int]) -> bool:
        r, g, b = p
        return any(abs(r - cr) + abs(g - cg) + abs(b - cb) <= tol for cr, cg, cb in colors)

    mask = Image.new("L", (w, h), 0)
    mp = mask.load()
    queue: deque[tuple[int, int]] = deque()
    for seed in spec["seeds"]:
        if ok(px[seed]) and mp[seed] == 0:
            mp[seed] = 255
            queue.append(seed)
    while queue:
        x, y = queue.popleft()
        for nx, ny in ((x + 1, y), (x - 1, y), (x, y + 1), (x, y - 1)):
            if x0 <= nx < x1 and y0 <= ny < y1 and mp[nx, ny] == 0 and ok(px[nx, ny]):
                mp[nx, ny] = 255
                queue.append((nx, ny))
    return mask


def _components(mask: Image.Image) -> list[tuple[int, list[tuple[int, int]]]]:
    """Connected components (4-neighborhood) of mask > 0 as (area, pixels)."""
    px = mask.load()
    w, h = mask.size
    seen = bytearray(w * h)
    out = []
    for sy in range(h):
        for sx in range(w):
            if px[sx, sy] > 0 and not seen[sy * w + sx]:
                comp = [(sx, sy)]
                seen[sy * w + sx] = 1
                queue: deque[tuple[int, int]] = deque([(sx, sy)])
                while queue:
                    x, y = queue.popleft()
                    for nx, ny in ((x + 1, y), (x - 1, y), (x, y + 1), (x, y - 1)):
                        if 0 <= nx < w and 0 <= ny < h and px[nx, ny] > 0 and not seen[ny * w + nx]:
                            seen[ny * w + nx] = 1
                            comp.append((nx, ny))
                            queue.append((nx, ny))
                out.append((len(comp), comp))
    return out


def _fill_holes(mask: Image.Image, max_hole: int) -> Image.Image:
    """Fill enclosed holes up to max_hole px (border-connected space stays)."""
    inv = ImageChops.invert(mask)
    px = inv.load()
    w, h = inv.size
    exterior = Image.new("L", mask.size, 0)
    xp = exterior.load()
    queue: deque[tuple[int, int]] = deque()
    for x in range(w):
        for y in (0, h - 1):
            if px[x, y] > 0 and xp[x, y] == 0:
                xp[x, y] = 255
                queue.append((x, y))
    for y in range(h):
        for x in (0, w - 1):
            if px[x, y] > 0 and xp[x, y] == 0:
                xp[x, y] = 255
                queue.append((x, y))
    while queue:
        x, y = queue.popleft()
        for nx, ny in ((x + 1, y), (x - 1, y), (x, y + 1), (x, y - 1)):
            if 0 <= nx < w and 0 <= ny < h and px[nx, ny] > 0 and xp[nx, ny] == 0:
                xp[nx, ny] = 255
                queue.append((nx, ny))
    holes = ImageChops.subtract(inv, exterior)
    out = mask.copy()
    op = out.load()
    for area, comp in _components(holes):
        if area <= max_hole:
            for x, y in comp:
                op[x, y] = 255
    return out


def _post(mask: Image.Image) -> Image.Image:
    """Seal seams, fill holes, round off fringe lumps, drop speckle."""
    m = mask.filter(ImageFilter.MaxFilter(CLOSE_KERNEL)).filter(
        ImageFilter.MinFilter(CLOSE_KERNEL)
    )
    m = _fill_holes(m, MAX_HOLE)
    m = m.filter(ImageFilter.GaussianBlur(SMOOTH_BLUR)).point(lambda v: 255 if v >= 128 else 0)
    out = Image.new("L", m.size, 0)
    op = out.load()
    for area, comp in _components(m):
        if area >= MIN_COMPONENT:
            for x, y in comp:
                op[x, y] = 255
    return out


def build_masks(crop: Image.Image, debug: bool) -> tuple[Image.Image, Image.Image]:
    """Returns (full mark mask, bird-only mask)."""
    floods = {name: _flood(crop, spec) for name, spec in ELEMENTS.items()}

    bird = Image.new("L", crop.size, 0)
    ImageDraw.Draw(bird).ellipse(HEAD_ELLIPSE, fill=255)
    for name in BIRD_ELEMENTS:
        bird = ImageChops.lighter(bird, floods[name])
    bird = _post(bird)

    arrow = _post(floods["arrow"].filter(ImageFilter.MaxFilter(ARROW_DILATE)))
    full = ImageChops.lighter(bird, arrow)
    draw = ImageDraw.Draw(full)
    cx, cy = DOT_CENTER
    draw.ellipse((cx - DOT_RADIUS, cy - DOT_RADIUS, cx + DOT_RADIUS, cy + DOT_RADIUS), fill=255)

    if debug:
        for name, m in floods.items():
            m.save(f"/tmp/dbg-el-{name}.png")
        bird.save("/tmp/dbg-bird.png")
        full.save("/tmp/dbg-full.png")
    return full, bird


def punch_eye(mask: Image.Image) -> Image.Image:
    out = mask.copy()
    draw = ImageDraw.Draw(out)
    ex, ey = EYE_CENTER
    draw.ellipse((ex - EYE_RADIUS, ey - EYE_RADIUS, ex + EYE_RADIUS, ey + EYE_RADIUS), fill=0)
    return out


def to_tray(mask: Image.Image, color_src: Image.Image | None = None) -> Image.Image:
    """Crop to content, scale to TRAY_HEIGHT, return RGBA.

    Template (color_src None): pure black + AA alpha. Otherwise full color
    with the mask as alpha.
    """
    bbox = mask.getbbox()
    if bbox is None:
        raise RuntimeError("empty mask")
    glyph_mask = mask.crop(bbox)
    scale = TRAY_HEIGHT / glyph_mask.height
    size = (max(1, round(glyph_mask.width * scale)), TRAY_HEIGHT)
    if color_src is None:
        alpha = glyph_mask.resize(size, Image.LANCZOS)
        out = Image.new("RGBA", size, (0, 0, 0, 255))
        out.putalpha(alpha)
        return out
    rgba = color_src.convert("RGBA").crop(bbox)
    rgba.putalpha(glyph_mask)
    return rgba.resize(size, Image.LANCZOS)


def _recolor(icon: Image.Image, fg: tuple[int, int, int]) -> Image.Image:
    out = Image.new("RGBA", icon.size, fg + (255,))
    out.putalpha(icon.getchannel("A"))
    return out


def preview_strip(
    icon: Image.Image, template: bool, bg: tuple[int, int, int], fg: tuple[int, int, int]
) -> Image.Image:
    strip = Image.new("RGBA", (BAR_W, BAR_H), bg + (255,))
    scale = ICON_H / icon.height
    rendered = icon.resize((max(1, round(icon.width * scale)), ICON_H), Image.LANCZOS)
    if template:
        rendered = _recolor(rendered, fg)
    x = 16
    strip.alpha_composite(rendered, (x, (BAR_H - ICON_H) // 2))
    x += rendered.width
    draw = ImageDraw.Draw(strip)
    font = ImageFont.truetype(FONT, 27)
    asc, desc = font.getmetrics()
    draw.text((x, (BAR_H - asc - desc) // 2), TITLE, font=font, fill=fg)
    return strip


def build_preview(candidates: list[tuple[str, Image.Image, bool]]) -> Image.Image:
    rows = []
    cap_font = ImageFont.truetype(FONT, 16)
    for label, icon, template in candidates:
        for mode, bg, fg in (("light", LIGHT_BG, LIGHT_FG), ("dark", DARK_BG, DARK_FG)):
            caption = Image.new("RGBA", (BAR_W, CAPTION_H), (255, 255, 255, 255))
            ImageDraw.Draw(caption).text(
                (16, 4), f"{label}  -  {mode} menu bar", font=cap_font, fill=(90, 90, 90)
            )
            rows.append(caption)
            rows.append(preview_strip(icon, template, bg, fg))
    total_h = sum(r.height for r in rows) + 8
    out = Image.new("RGBA", (BAR_W, total_h), (255, 255, 255, 255))
    y = 4
    for r in rows:
        out.alpha_composite(r, (0, y))
        y += r.height
    return out


def main() -> None:
    debug = "--debug" in sys.argv
    OUT.mkdir(parents=True, exist_ok=True)
    src = Image.open(SRC).convert("RGB")
    crop = src.crop(MARK_REGION)

    full, bird = build_masks(crop, debug)

    tpl_full = to_tray(punch_eye(full))
    tpl_bird = to_tray(punch_eye(bird))
    color = to_tray(full, color_src=crop)

    for name, img in (
        ("template-full.png", tpl_full),
        ("template-bird.png", tpl_bird),
        ("color.png", color),
    ):
        img.save(OUT / name)
        print(f"wrote {OUT / name} {img.size}")

    # The chosen candidate ships as the embedded tray glyph (see tray.rs).
    tpl_bird.save(SHIPPED)
    print(f"wrote {SHIPPED} {tpl_bird.size}")

    preview = build_preview(
        [
            ("TEMPLATE-FULL (icon_as_template=true)", tpl_full, True),
            ("TEMPLATE-BIRD (icon_as_template=true)", tpl_bird, True),
            ("COLOR (icon_as_template=false)", color, False),
        ]
    )
    preview.convert("RGB").save(OUT / "preview.png")
    print(f"wrote {OUT / 'preview.png'} {preview.size}")


if __name__ == "__main__":
    main()
