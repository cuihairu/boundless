"""BW-00-03-03 — the Python module builds and imports after install.

The acceptance is the install -> import round trip, so the test installs the
package into a throwaway site directory (never the interpreter's own site) and
imports it in a subprocess with that directory on PYTHONPATH.
"""
import os
import subprocess
import sys

REPO_ROOT = os.path.dirname(
    os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
PKG_DIR = os.path.join(REPO_ROOT, "python")


def _install(tmp_path):
    site = tmp_path / "site"
    subprocess.run(
        [sys.executable, "-m", "pip", "install", "--quiet",
         "--no-deps", "--target", str(site), PKG_DIR],
        check=True,
    )
    return site


def test_module_imports_after_install(tmp_path):
    site = _install(tmp_path)
    out = subprocess.run(
        [sys.executable, "-c",
         "import boundless; print(boundless.__version__); "
         "print(','.join(boundless.COMPONENTS))"],
        env={**os.environ, "PYTHONPATH": str(site)},
        capture_output=True, text=True, check=True,
    )
    lines = out.stdout.splitlines()
    assert lines[0] == "0.1.0"
    assert lines[1] == "cell,base,client,database,bot,editor"
