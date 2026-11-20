"""tools/screenshots save() must keep every pixel: small red and green UI dots were once palettised to grey."""
import importlib.machinery
import importlib.util
from pathlib import Path
import random
import tempfile
import unittest

from PIL import Image

ROOT = Path(__file__).resolve().parents[2]


class ScreenshotsSave(unittest.TestCase):
    def test_small_colours_survive(self):
        loader = importlib.machinery.SourceFileLoader("screenshots", str(ROOT / "tools" / "screenshots"))
        mod = importlib.util.module_from_spec(importlib.util.spec_from_loader("screenshots", loader))
        loader.exec_module(mod)
        # Random noise is over the size where the old code palettised; the dots are a few pixels.
        rng = random.Random(1)
        im = Image.new("RGB", (400, 300))
        im.putdata([tuple(rng.randrange(256) for _ in range(3)) for _ in range(400 * 300)])
        for x in range(10):
            im.putpixel((x, 0), (255, 59, 48))
            im.putpixel((x, 1), (40, 200, 64))
        with tempfile.TemporaryDirectory() as t:
            mod.OUT = Path(t)
            src = Path(t) / "in.png"
            im.save(src)
            mod.save(src, "out")
            got = Image.open(Path(t) / "out.png").convert("RGB")
        self.assertEqual(got.tobytes(), im.tobytes())
        self.assertEqual(got.getpixel((3, 0)), (255, 59, 48))
        self.assertEqual(got.getpixel((3, 1)), (40, 200, 64))


if __name__ == "__main__":
    unittest.main()
