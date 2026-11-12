"""How a scenario is declared.

A scenario is a module in `scenarios/` that defines `SCENARIO`. It names the
installed image it needs, the lab it wants, a timeout and a function that
does the work and returns a dict of what it measured. Raising fails it.
The runner owns everything else: installing the image once, a fresh
container and network namespace per scenario, results and evidence.
"""
import hashlib
import importlib
import pkgutil
from dataclasses import dataclass
from pathlib import Path
from typing import Callable

HERE = Path(__file__).resolve().parent.parent


@dataclass(frozen=True)
class ImageSpec:
    """An installed system, identified by what went into installing it."""
    preset: str
    profile: str
    disk_mb: int = 4096

    @property
    def key(self):
        """Changes with the machine, the media profile and the install
        driver, so editing install.py or console.py re-installs."""
        h = hashlib.sha256()
        for part in (self.preset, self.profile, str(self.disk_mb), "1",
                     (HERE / "install.py").read_bytes().decode()):
            h.update(part.encode() + b"\0")
        return h.hexdigest()[:16]

    @property
    def dirname(self):
        return f"irix-{self.preset}-{self.profile}-{self.key}"


@dataclass(frozen=True)
class Scenario:
    name: str
    summary: str
    image: ImageSpec
    run: Callable  # run(ctx) -> dict
    taps: int = 0          # tap devices the lab creates, tap0 .. tapN-1
    timeout: int = 3600    # seconds, for the whole scenario container


def load_all():
    """Every scenario in `scenarios/`, by name."""
    import scenarios
    found = {}
    for mod in pkgutil.iter_modules(scenarios.__path__):
        s = importlib.import_module(f"scenarios.{mod.name}").SCENARIO
        found[s.name] = s
    return found
