"""Builds every app icon from source.png (the hematite render on a flat grey background).

    python3 -m venv .venv && .venv/bin/pip install pillow numpy scipy
    .venv/bin/python packaging/icons/build.py

Writes, next to this script:
    rsmol.png                     1024px, transparent: Linux, Flathub, Homebrew, README
    rsmol-macos.png               1024px, crystal on a rounded-square tile (macOS 26 puts icons
                                  that aren't a tile inside a grey one, so macOS gets its own)
    rsmol.icns                    built from rsmol-macos.png with iconutil (macOS only)
    hicolor/NxN/apps/<APP_ID>.png freedesktop icon theme sizes for the tarball and Flatpak

The cut-out relies on the source's background being flat neutral grey and the crystal being
blue-tinted metal; a new source render with a different background needs the thresholds revisited.
"""

import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw, ImageFilter
from scipy import ndimage as ndi
from scipy.spatial import ConvexHull

APP_ID = "io.github.joseribon.rsmol"
HERE = Path(__file__).resolve().parent
HICOLOR_SIZES = [16, 24, 32, 48, 64, 128, 256, 512]
TILE_TOP, TILE_BOTTOM = (74, 82, 96), (28, 31, 38)  # dark slate; (232,232,236)->(196,196,202) for light


def cut_out(src: Path) -> Image.Image:
    """The crystal with its soft shadow, on transparency."""
    a = np.asarray(Image.open(src).convert("RGB")).astype(float)
    r, g, b = a[..., 0], a[..., 1], a[..., 2]
    lum = (r + g + b) / 3
    bg_level = float(np.median(np.concatenate([lum[0], lum[-1], lum[:, 0], lum[:, -1]])))
    smooth = ndi.gaussian_filter(lum, 1)
    grad = np.hypot(ndi.sobel(smooth, 0), ndi.sobel(smooth, 1)) / 8

    # Background and shadow: neutral grey, no brighter than the backdrop, changing gently,
    # and connected to the border. Bright silver facets and the crystal's sharp outline stop it.
    neutral = (b - r < 5) & (np.abs(r - g) < 5) & (a.max(-1) <= bg_level + 6) & (grad < 6)
    lab, _ = ndi.label(neutral)
    border = np.unique(np.concatenate([lab[0], lab[-1], lab[:, 0], lab[:, -1]]))
    crystal = ndi.binary_fill_holes(~np.isin(lab, border[border > 0]))

    # The near-black contact shadow under the base is neutral too: give it back to the shadow,
    # keep the largest blob, and take the lower outline from the convex hull (it is straight).
    rows = np.arange(lum.shape[0])[:, None] * np.ones_like(lum)
    low = rows > lum.shape[0] * 0.76
    crystal &= ~((b - r < 4) & (np.abs(r - g) < 4) & (lum < 90) & low)
    crystal = ndi.binary_opening(crystal, iterations=3)
    lab, n = ndi.label(crystal)
    if n > 1:
        crystal = lab == (np.argmax(ndi.sum(crystal, lab, range(1, n + 1))) + 1)
    crystal = ndi.binary_fill_holes(crystal)
    pts = np.argwhere(crystal)[:, ::-1]
    hull_img = Image.new("L", (lum.shape[1], lum.shape[0]))
    ImageDraw.Draw(hull_img).polygon([tuple(map(float, v)) for v in pts[ConvexHull(pts).vertices]], fill=255)
    crystal |= (np.asarray(hull_img) > 0) & low
    crystal = ndi.binary_erosion(crystal, iterations=1)

    cover = ndi.gaussian_filter(crystal.astype(float), 0.7)
    shadow = ndi.gaussian_filter(np.where(crystal, 1.0, np.clip((bg_level - lum) / bg_level, 0, 1) * 1.5), 1.5)
    alpha = cover + (1 - cover) * shadow.clip(0, 1)
    # Edge pixels: remove the grey backdrop mixed into them, so no halo on dark backgrounds.
    c = np.clip(cover, 1e-3, 1)[..., None]
    rgb = np.where(cover[..., None] > 0.97, a, np.clip((a - (1 - c) * bg_level) / c, 0, 255))
    rgb *= np.where(alpha > 0, cover / np.maximum(alpha, 1e-6), 0)[..., None]  # the shadow is black
    img = Image.fromarray(np.dstack([rgb, alpha * 255]).round().astype(np.uint8), "RGBA")
    ys, xs = np.where(np.asarray(img)[..., 3] > 8)
    return img.crop((xs.min(), ys.min(), xs.max() + 1, ys.max() + 1))


def place(canvas: Image.Image, art: Image.Image, x0: int, y0: int, size: int) -> None:
    s = size / max(art.size)
    a = art.resize((round(art.width * s), round(art.height * s)), Image.LANCZOS)
    canvas.alpha_composite(a, (x0 + (size - a.width) // 2, y0 + (size - a.height) // 2))


def squircle(size: int, n: float = 5.0, ss: int = 4) -> Image.Image:
    t = (np.arange(size * ss) + 0.5) / (size * ss) * 2 - 1
    x, y = np.meshgrid(t, t)
    mask = ((np.abs(x) ** n + np.abs(y) ** n) <= 1).astype(np.uint8) * 255
    return Image.fromarray(mask).resize((size, size), Image.LANCZOS)


def macos_tile(art: Image.Image) -> Image.Image:
    """Apple's icon grid: an 824px tile centred on the 1024px canvas, with a soft drop shadow."""
    tile, off = 824, 100
    t = np.linspace(0, 1, tile)[:, None, None]
    fill = (np.array(TILE_TOP) * (1 - t) + np.array(TILE_BOTTOM) * t) * np.ones((tile, tile, 1))
    face = Image.fromarray(fill.astype(np.uint8), "RGB").convert("RGBA")
    face.putalpha(squircle(tile))
    canvas = Image.new("RGBA", (1024, 1024))
    mask = Image.new("L", (1024, 1024))
    mask.paste(squircle(tile), (off, off + 10))
    drop = Image.new("RGBA", (1024, 1024))
    drop.putalpha(mask.point(lambda v: int(v * 0.3)))
    canvas.alpha_composite(drop.filter(ImageFilter.GaussianBlur(10)))
    canvas.alpha_composite(face, (off, off))
    place(canvas, art, off + 112, off + 112, tile - 224)
    return canvas


def icns(master: Image.Image, out: Path) -> None:
    if not shutil.which("iconutil"):
        print("iconutil not found (it ships with macOS): skipped rsmol.icns", file=sys.stderr)
        return
    with tempfile.TemporaryDirectory() as tmp:
        iconset = Path(tmp) / "rsmol.iconset"
        iconset.mkdir()
        for pt in [16, 32, 128, 256, 512]:
            master.resize((pt, pt), Image.LANCZOS).save(iconset / f"icon_{pt}x{pt}.png")
            master.resize((pt * 2, pt * 2), Image.LANCZOS).save(iconset / f"icon_{pt}x{pt}@2x.png")
        subprocess.run(["iconutil", "-c", "icns", str(iconset), "-o", str(out)], check=True)


def main() -> None:
    art = cut_out(HERE / "source.png")

    linux = Image.new("RGBA", (1024, 1024))
    place(linux, art, 52, 52, 920)
    linux.save(HERE / "rsmol.png", optimize=True)
    for n in HICOLOR_SIZES:
        d = HERE / "hicolor" / f"{n}x{n}" / "apps"
        d.mkdir(parents=True, exist_ok=True)
        linux.resize((n, n), Image.LANCZOS).save(d / f"{APP_ID}.png", optimize=True)

    mac = macos_tile(art)
    mac.save(HERE / "rsmol-macos.png", optimize=True)
    icns(mac, HERE / "rsmol.icns")
    print("icons written to", HERE)


if __name__ == "__main__":
    main()
