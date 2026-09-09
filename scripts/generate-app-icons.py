#!/usr/bin/env python3
"""Generate Windows icon assets from assets/app-icon.png. Requires Pillow."""

from pathlib import Path

from PIL import Image


def main():
    assets = Path(__file__).resolve().parent.parent / "assets"
    with Image.open(assets / "app-icon.png") as source:
        if source.width != source.height or source.width < 1024:
            raise ValueError("The application icon must be square and at least 1024px")
        icon = source.convert("RGBA")
        icon.resize((512, 512), Image.Resampling.LANCZOS).save(
            assets / "windows/app-512.png", optimize=True
        )
        icon.save(
            assets / "windows/app.ico",
            format="ICO",
            sizes=[(size, size) for size in (16, 20, 24, 32, 40, 48, 64, 96, 128, 256)],
        )


if __name__ == "__main__":
    main()
