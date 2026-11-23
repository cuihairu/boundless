#!/usr/bin/env python3
"""Re-run the reference unit-test suite (BW-00-02-02).

Mirrors the Make build's `bw-run-all-unit-tests` recipe
(build/make/Makefile:382-407): each test binary is run from its component
source directory with `--root $(BW_ROOT)`, where BW_ROOT resolves to the
`programming` directory. A bounded timeout replaces the recipe's 1800s so a
stuck test does not stall the lane.

Usage: unit_test_run.py BIGWORLD_ROOT OUTPUT_JSON
"""
import json
import os
import subprocess
import sys
import time

# name -> component source dir (relative to programming/bigworld), from the
# BW-00-01-01 unit_test inventory.
UNITS = {
    "baseapp": "server/baseapp/unit_test",
    "baseappmgr": "server/baseappmgr/unit_test",
    "cellapp": "server/cellapp/unit_test",
    "cellappmgr": "server/cellappmgr/unit_test",
    "cstdmf": "lib/cstdmf/unit_test",
    "db": "lib/db/unit_test",
    "dbapp": "server/dbapp/unit_test",
    "dbappmgr": "server/dbappmgr/unit_test",
    "entitydef": "lib/entitydef/unit_test",
    "loginapp": "server/loginapp/unit_test",
    "math": "lib/math/unit_test",
    "network": "lib/network/unit_test",
    "physics2": "lib/physics2/unit_test",
    "pyscript": "lib/pyscript/unit_test",
    "resmgr": "lib/resmgr/unit_test",
    "reviver": "server/reviver/unit_test",
    "scene": "lib/scene/unit_test",
    "script": "lib/script/unit_test",
    "server": "lib/server/unit_test",
    "terrain": "lib/terrain/unit_test",
    "unit_test_lib": "lib/unit_test_lib/unit_test",
}

TIMEOUT_S = 180


def run_one(root, bin_root, name, src_dir):
    binary = os.path.join(bin_root, name + "_test")
    cwd = os.path.join(root, "programming", "bigworld", src_dir)
    cmd = [binary, "--root", os.path.join(root, "programming")]
    start = time.time()
    try:
        proc = subprocess.run(
            cmd, cwd=cwd, capture_output=True, text=True, timeout=TIMEOUT_S)
        rc, out = proc.returncode, proc.stdout + proc.stderr
        status = "pass" if rc == 0 else "fail"
    except subprocess.TimeoutExpired:
        rc, out = None, ""
        status = "timeout"
    return {
        "name": name,
        "src_dir": src_dir,
        "command": " ".join(cmd),
        "cwd": cwd,
        "status": status,
        "returncode": rc,
        "seconds": round(time.time() - start, 3),
        "output_tail": out[-4000:],
    }


def main():
    if len(sys.argv) < 3:
        print(__doc__)
        return 1
    root, out_json = sys.argv[1], sys.argv[2]
    bin_root = os.path.join(root, "game", "bin", "server", "el7", "unit_tests")
    results = [
        run_one(root, bin_root, name, src_dir)
        for name, src_dir in sorted(UNITS.items())
    ]
    with open(out_json, "w", encoding="utf-8") as fh:
        json.dump(results, fh, indent=1, sort_keys=True)
    passed = sum(1 for r in results if r["status"] == "pass")
    print(f"unit tests: {len(results)}  passed: {passed}  failed: "
          f"{len(results) - passed}")
    for r in results:
        print(f"  {r['status']:8s} {r['name']:16s} {r['seconds']:8.3f}s rc={r['returncode']}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
