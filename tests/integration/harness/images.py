"""The scenarios' installed images; add a spec here to cover another
machine family or profile (install.py drives the install)."""
from harness.scenario import ImageSpec

ORIGIN200_MINIMAL = ImageSpec(preset="origin200-1", profile="minimal")
