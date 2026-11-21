"""Put the tree's Python package in front of the suite.

The suite lives under ``tests/python`` and imports the engine's own
package from ``python/``. ``pytest`` inserts this file's directory into
``sys.path``, not the repo root, so the package would not be found
without this hook. ``ci/check-python.sh`` runs the suite straight from a
checkout, where no ``pip install`` has happened; the installed-copy face
(the acceptance of BW-00-03-03) is proven separately and recorded in the
todo tick.
"""

from pathlib import Path
import sys

_PACKAGE_ROOT = Path(__file__).resolve().parents[2] / "python"
if str(_PACKAGE_ROOT) not in sys.path:
    sys.path.insert(0, str(_PACKAGE_ROOT))
