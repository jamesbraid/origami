"""Tests for `tools/screenshots compare`: run with `uv run --with pillow --with pytest pytest tools/screenshots_test.py`."""
import importlib.machinery, importlib.util, pathlib

from PIL import Image

path = pathlib.Path(__file__).with_name("screenshots")
spec = importlib.util.spec_from_loader("screenshots", importlib.machinery.SourceFileLoader("screenshots", str(path)))
tool = importlib.util.module_from_spec(spec)
spec.loader.exec_module(tool)


def test_a_small_drift_passes_and_a_changed_window_fails():
    base = Image.new("RGB", (200, 100), (30, 30, 40))
    near = base.copy()
    near.paste((33, 30, 40), (0, 0, 200, 100))  # a few levels: anti-aliasing, not a change
    moved = base.copy()
    moved.paste((250, 250, 250), (0, 0, 60, 100))  # 30% of the window differs
    assert tool.judge(near, base)[0] == 0
    assert tool.judge(moved, base)[0] > tool.BAD


def test_a_different_size_is_a_mismatch():
    assert tool.judge(Image.new("RGB", (10, 10)), Image.new("RGB", (11, 10))) is None
