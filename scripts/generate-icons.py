#!/usr/bin/env python3
"""Derive shipped icon assets from the Farthing coin master art.

Deterministic, PIL-only. The master is `art/farthing-icon.png` (full-color
coin-with-bird mark, 781x768). This script derives:

- art/app-icon.png            1024x1024 app icon master: the coin scaled to
                              the macOS icon grid (~824/1024) and centered on
                              a transparent canvas. Feed it to
                              `pnpm tauri icon art/app-icon.png` to regenerate
                              every size in src-tauri/icons/ (icns, ico, pngs).
- src/lib/assets/farthing-icon.png
                              128px full-color asset for the desktop app's
                              sidebar wordmark (imported via Vite, displayed
                              at ~22px, so 128px covers retina with room to
                              spare without shipping the full master).

The menu bar tray glyph (src-tauri/icons/tray-icon.png) is NOT derived from
the coin master anymore: it comes from the dedicated bird art
(art/tray-source-bird.png) via scripts/generate-tray-candidates.py, which
owns that file.

Usage: python3 scripts/generate-icons.py
"""

from __future__ import annotations

import pathlib

from PIL import Image

ROOT = pathlib.Path(__file__).resolve().parent.parent
MASTER = ROOT / "art" / "farthing-icon.png"


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

    sidebar_path = ROOT / "src" / "lib" / "assets" / "farthing-icon.png"
    sidebar_path.parent.mkdir(parents=True, exist_ok=True)
    sidebar_icon(master).save(sidebar_path)
    print(f"wrote {sidebar_path}")

    print("now run: pnpm tauri icon art/app-icon.png")


if __name__ == "__main__":
    main()
