"""BW-00-03-03: the Python module builds and imports.

The acceptance is that a minimal module is importable after install. The
installed copy is exercised by the venv smoke test recorded in the todo
tick; these cases pin the import face and the one invariant a package
ships with -- its version tracks the workspace it is built with, so a
crate bump can never silently drift from the module an interpreter sees.
"""

from pathlib import Path
import re

import boundless

_REPO_ROOT = Path(__file__).resolve().parents[2]


def test_package_imports_with_a_version():
    assert isinstance(boundless.__version__, str)
    assert re.fullmatch(r"\d+\.\d+\.\d+", boundless.__version__)


def test_version_tracks_the_workspace():
    """The module version must equal ``[workspace.package] version``.

    The package is built from the same tree as the crates, so a release
    that bumps one side and not the other is a packaging defect the suite
    should name rather than ship.
    """
    text = (_REPO_ROOT / "Cargo.toml").read_text(encoding="utf-8")
    section = text.split("[workspace.package]", 1)[1].split("\n[", 1)[0]
    pin = re.search(r'^version\s*=\s*"([^"]+)"', section, re.MULTILINE)
    assert pin is not None, "Cargo.toml lost its [workspace.package] version"
    assert boundless.__version__ == pin.group(1)
