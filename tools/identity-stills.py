#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["pillow", "numpy"]
# ///
"""Regenerate resources/identity/stills/ from machine-models' product stills.

    tools/identity-stills.py HINV_ARCHIVE_CHECKOUT

Reads the pinned hinv-archive commits with `git`, copies the raw stills
unchanged, applies the small-size treatment from the design's identity
addendum (drop the baked shadow, tight crop, equal weight, Lanczos, unsharp,
lift dark bodies, 1 px rim), frames the card and hero stills at equal weight
and writes manifest.json. Output is deterministic.
"""
import io, json, pathlib, subprocess, sys

import numpy as np
from PIL import Image, ImageFilter

# front34 stills (all machines) and the front34-high additions (flat machines).
STILLS = "28b4ceac034825fed0d30020b788d1c62de24ab9"  # origami/product-stills
HIGH = "8b5b40fa449534ec15516ac579531b25049fc890"  # stills-front34-high
BASE = "art/publish/stills"

# Art ids Origami uses, all hinv ids. Flat machines draw their small sizes
# from front34-high so the lid shows.
ARTS = [
    "fuel", "octane", "octane2", "o2", "indy", "indigo2-impact", "tezro-tower",
    "origin200-tower", "origin2000-deskside", "origin2000-rack",
    "onyx2-deskside", "onyx2-rack", "origin300", "origin350", "onyx350",
    "origin3000", "origin3200", "origin3900", "onyx3000",
]
FLAT = {"origin350", "onyx350", "indy", "origin300"}

# Origami topology (or, failing that, product) -> art id.
TOPOLOGIES = {
    "fuel": "fuel", "octane": "octane", "octane2": "octane2", "o2": "o2",
    "indy": "indy", "indigo2": "indigo2-impact",
    "tezro-ip53": "tezro-tower", "tezro-ip59": "tezro-tower",
    "tezro-one-processor": "tezro-tower",
    "origin200": "origin200-tower", "origin200-dual": "origin200-tower",
    "origin200-gigachannel": "origin200-tower",
    "origin200-gigachannel-loopback": "origin200-tower",
    "origin2000-deskside": "origin2000-deskside", "origin2000-rack": "origin2000-rack",
    "onyx2-deskside": "onyx2-deskside", "onyx2-rack": "onyx2-rack",
    "origin300": "origin300", "origin350": "origin350",
    "origin3200": "origin3200", "origin3400": "origin3000",
    "origin3900-ip53": "origin3900", "origin3900-ip59": "origin3900",
    "onyx350-ip53": "onyx350", "onyx350-ip59": "onyx350", "onyx3000": "onyx3000",
}
PRODUCTS = {
    "fuel": "fuel", "octane": "octane", "octane2": "octane2", "o2": "o2",
    "indy": "indy", "indigo2": "indigo2-impact", "tezro": "tezro-tower",
    "origin200": "origin200-tower", "origin2000": "origin2000-deskside",
    "onyx2": "onyx2-deskside", "origin300": "origin300", "origin350": "origin350",
    "onyx350": "onyx350",
}
POINTS = (20, 34, 48, 72)
MODES = {"light": (0, 0, 0, 40), "dark": (255, 255, 255, 60)}  # 16% black, 24% white


def blob(hinv, commit, path):
    return subprocess.run(["git", "-C", hinv, "cat-file", "blob", f"{commit}:{path}"],
                          check=True, capture_output=True).stdout


def treated(im, px, rim, cover=0.55):
    a = np.array(im)
    a[..., 3] = np.where(a[..., 3] / 255 > 0.6, a[..., 3], 0)  # drops the baked shadow
    im = Image.fromarray(a)
    im = im.crop(im.getbbox())
    solid = np.array(im)[..., 3] > 8
    side = px - 2  # room for the rim
    s = min(side * 0.96 / im.width, side * 0.96 / im.height, (cover * side * side / solid.sum()) ** 0.5)
    im = im.resize((max(1, round(im.width * s)), max(1, round(im.height * s))), Image.LANCZOS)
    rgb = im.convert("RGB").filter(ImageFilter.UnsharpMask(radius=0.6, percent=80, threshold=2))
    if np.array(rgb).astype(float).mean() < 70:  # navy and graphite bodies
        rgb = Image.fromarray(np.clip(np.array(rgb).astype(float) * 1.18 + 8, 0, 255).astype(np.uint8))
    im = Image.merge("RGBA", (*rgb.split(), im.split()[3]))
    canvas = Image.new("RGBA", (px, px))
    ox, oy = (px - im.width) // 2, px - im.height - max(1, px // 14)
    if rim:
        al = Image.new("L", (px, px))
        al.paste(im.split()[3], (ox, oy))
        halo = al.filter(ImageFilter.MaxFilter(3))
        r = Image.new("RGBA", (px, px), rim[:3] + (0,))
        r.putalpha(halo.point(lambda v: v * rim[3] // 255))
        canvas.alpha_composite(r)
    canvas.alpha_composite(im, (ox, oy))
    return canvas


def framed(im, width, height):
    """Equal visual weight in a slot, shadow kept, floor line 7% up; never upscaled."""
    a = np.array(im)
    alpha = a[..., 3].astype(float)
    # The baked shadow runs off the still's edges at about 18% alpha: fade it to nothing there.
    def ramp(n):
        t = np.clip(np.minimum(np.arange(n), np.arange(n)[::-1]) / (0.15 * n), 0, 1)
        return t * t * (3 - 2 * t)
    window = ramp(a.shape[0])[:, None] * ramp(a.shape[1])[None, :]
    alpha = np.where(alpha < 153, alpha * window, alpha)
    a[..., 3] = np.clip(alpha - 6, 0, 255).astype(np.uint8)
    im = Image.fromarray(a.astype(np.uint8))
    im = im.crop(im.getbbox())
    solid = np.array(im)[..., 3] > 8
    fit = min(width * 0.86 / im.width, height * 0.86 / im.height)
    s = min(fit, (0.40 * width * height / solid.sum()) ** 0.5, 1.0)
    im = im.resize((max(1, round(im.width * s)), max(1, round(im.height * s))), Image.LANCZOS)
    canvas = Image.new("RGBA", (width, height))
    canvas.alpha_composite(im, ((width - im.width) // 2, height - im.height - round(height * 0.07)))
    return canvas


def body_colour(im):
    """Median of the opaque pixels between the 40th and 80th luminance percentile."""
    arr = np.array(im).astype(float)
    px = arr[arr[..., 3] > 250][:, :3]
    lum = px.mean(1)
    sel = px[(lum > np.percentile(lum, 40)) & (lum < np.percentile(lum, 80))]
    return "#%02x%02x%02x" % tuple(int(v) for v in np.median(sel, 0))


def png(im):
    b = io.BytesIO()
    im.save(b, "PNG", optimize=True)
    return b.getvalue()


def main():
    hinv = sys.argv[1]
    out = pathlib.Path(__file__).resolve().parent.parent / "resources/identity/stills"
    src = json.loads(blob(hinv, STILLS, f"{BASE}/manifest.json"))
    stills = {}
    for art in ARTS:
        d = out / art
        d.mkdir(parents=True, exist_ok=True)
        for size in (512, 256, 128):
            (d / f"front34-{size}.png").write_bytes(blob(hinv, STILLS, f"{BASE}/{art}/front34-{size}.png"))
        raw = Image.open(io.BytesIO(blob(hinv, STILLS, f"{BASE}/{art}/front34-512.png"))).convert("RGBA")
        small = raw
        if art in FLAT:
            small = Image.open(io.BytesIO(blob(hinv, HIGH, f"{BASE}/{art}/front34-high-512.png"))).convert("RGBA")
            for size in (512, 256, 128):
                (d / f"front34-high-{size}.png").write_bytes(
                    blob(hinv, HIGH, f"{BASE}/{art}/front34-high-{size}.png"))
        for pt in POINTS:
            for mode, rim in MODES.items():
                for scale, suffix in ((1, ""), (2, "@2x")):
                    (d / f"{pt}-{mode}{suffix}.png").write_bytes(png(treated(small, pt * scale, rim)))
        # Card (120 x 90 pt) and hero (240 x 180 pt), from the next larger still.
        for name, (w, h), source in (("card", (120, 90), "256"), ("hero", (240, 180), "512")):
            big = Image.open(io.BytesIO(blob(hinv, STILLS, f"{BASE}/{art}/front34-{source}.png"))).convert("RGBA")
            for scale, suffix in ((1, ""), (2, "@2x")):
                (d / f"{name}{suffix}.png").write_bytes(png(framed(big, w * scale, h * scale)))
        m = src[art]
        stills[art] = {
            "name": m["name"], "colour": body_colour(raw), "small": "front34-high" if art in FLAT else "front34",
            "hinv": {"status": m["status"], "check_id": m["check_id"], "source_commit": m["source_commit"]},
        }
    manifest = {
        "hinv": {"stills_commit": STILLS, "high_commit": HIGH},
        "files": {
            "raw": "<art>/front34-{512,256,128}.png",
            "small": "<art>/<pt>-<light|dark>[@2x].png, pt in 20, 34, 48, 72",
            "framed": "<art>/{card,hero}[@2x].png, 120 x 90 pt and 240 x 180 pt, equal weight, shadow kept",
        },
        "topologies": dict(sorted(TOPOLOGIES.items())),
        "products": dict(sorted(PRODUCTS.items())),
        "stills": stills,
    }
    (out / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")


main()
