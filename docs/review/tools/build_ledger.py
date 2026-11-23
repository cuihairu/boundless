#!/usr/bin/env python3
"""BW-00-05 — build the feature-coverage ledger.

One row per original build target: `target`, `type`, `dir`, `enable_condition`,
and the coverage columns `feature_id` (the BW lane that owns the target), the
`fixture` (the run evidence), and `status`. The manifest comes from
extract_targets.py; the coverage columns come from the mapping below. The lane
ids are the task-book index (todo.md section 1).

Usage:
    build_ledger.py <reference_tree> > ledger.tsv
"""

import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import extract_targets  # noqa: E402

# status vocabulary:
#   covered      the owning lane's run evidence exists (the fixture is real)
#   partial      a part is covered, the rest has a recorded blocker
#   gap          enabled in the reference but not ported; blocker recorded
#   not-enabled  platform/feature-gated away on the Linux server build

# dir -> (status, feature_id, fixture, blocker). A missing dir falls through to
# the area default. The dir is the source directory the original CMakeLists
# lives in, relative to programming/bigworld.
COVERAGE = {
    # --- engine libraries with a ported crate -------------------------------
    "lib/cstdmf": ("covered", "BW-01-01", "crates/foundation tests", ""),
    "lib/cstdmf/unit_test": ("covered", "BW-01-01", "crates/foundation tests", ""),
    "lib/math": ("covered", "BW-01-01-04", "crates/foundation math tests", ""),
    "lib/math/unit_test": ("covered", "BW-01-01-04", "crates/foundation math tests", ""),
    "lib/resmgr": ("covered", "BW-01-03", "crates/resources tests", ""),
    "lib/resmgr/unit_test": ("covered", "BW-01-03", "crates/resources tests", ""),
    "lib/entitydef": ("covered", "BW-04-01", "docs/review/fixtures/full_def", ""),
    "lib/entitydef/unit_test": ("covered", "BW-04-01", "crates/entitydef tests", ""),
    "lib/entitydef_script": ("covered", "BW-03-02", "crates/python-host tests", ""),
    "lib/network": ("covered", "BW-02-01", "crates/mercury tests", ""),
    "lib/network/unit_test": ("covered", "BW-02-01", "crates/mercury tests", ""),
    "lib/pyscript": ("covered", "BW-03-01", "crates/python-host tests", ""),
    "lib/pyscript/unit_test": ("covered", "BW-03-01", "crates/python-host tests", ""),
    "lib/script": ("covered", "BW-03-03", "crates/python-host tests", ""),
    "lib/script/unit_test": ("covered", "BW-03-03", "crates/python-host tests", ""),
    "lib/server": ("covered", "BW-01-04", "crates/server-runtime tests", ""),
    "lib/server/unit_test": ("covered", "BW-01-04", "crates/server-runtime tests", ""),
    "lib/sqlite": ("covered", "BW-01-03-03", "crates/resources tests", ""),
    # --- the one tool the new stack already ships ----------------------------
    "tools/process_defs": ("covered", "BW-00-04",
                           "docs/review/fixtures/full_def + oracles", ""),
    # --- server apps: the process/config/tick layer is ported in server-runtime
    #     (BW-01-04); the app-specific logic is the owning lane's gap ---------
    "server/baseapp": ("partial", "BW-07", "crates/server-runtime tests (BW-01-04)",
                       "baseapp entity/message logic not implemented"),
    "server/baseapp/unit_test": ("partial", "BW-07", "crates/server-runtime tests (BW-01-04)",
                                 "baseapp entity/message logic not implemented"),
    "server/baseappmgr": ("partial", "BW-05", "crates/server-runtime tests (BW-01-04)",
                          "baseappmgr manager logic not implemented"),
    "server/baseappmgr/unit_test": ("partial", "BW-05", "crates/server-runtime tests (BW-01-04)",
                                    "baseappmgr manager logic not implemented"),
    "server/cellapp": ("partial", "BW-08", "crates/server-runtime tests (BW-01-04)",
                       "cellapp entity/message logic not implemented"),
    "server/cellapp/unit_test": ("partial", "BW-08", "crates/server-runtime tests (BW-01-04)",
                                 "cellapp entity/message logic not implemented"),
    "server/cellappmgr": ("partial", "BW-05", "crates/server-runtime tests (BW-01-04)",
                          "cellappmgr manager logic not implemented"),
    "server/cellappmgr/unit_test": ("partial", "BW-05", "crates/server-runtime tests (BW-01-04)",
                                    "cellappmgr manager logic not implemented"),
    "server/dbapp": ("partial", "BW-09", "crates/server-runtime tests (BW-01-04)",
                     "dbapp database logic not implemented"),
    "server/dbapp/unit_test": ("partial", "BW-09", "crates/server-runtime tests (BW-01-04)",
                               "dbapp database logic not implemented"),
    "server/dbappmgr": ("partial", "BW-05", "crates/server-runtime tests (BW-01-04)",
                        "dbappmgr manager logic not implemented"),
    "server/dbappmgr/unit_test": ("partial", "BW-05", "crates/server-runtime tests (BW-01-04)",
                                  "dbappmgr manager logic not implemented"),
    "server/loginapp": ("partial", "BW-06", "crates/server-runtime tests (BW-01-04)",
                        "loginapp login logic not implemented"),
    "server/loginapp/unit_test": ("partial", "BW-06", "crates/server-runtime tests (BW-01-04)",
                                  "loginapp login logic not implemented"),
    "server/reviver": ("partial", "BW-05", "crates/server-runtime tests (BW-01-04)",
                       "reviver process-watchdog logic not implemented"),
    "server/reviver/unit_test": ("partial", "BW-05", "crates/server-runtime tests (BW-01-04)",
                                 "reviver process-watchdog logic not implemented"),
    # --- database layer: DBApp 实体读写、MySQL/XML、整理、快照和同步工具 (BW-09) --
    "server/dbapp_extensions/bwengine_mysql": (
        "gap", "BW-09", "", "MySQL engine binding not ported"),
    "server/dbapp_extensions/bwengine_xml": (
        "gap", "BW-09", "", "XML engine binding not ported"),
    # --- server tools: not ported --------------------------------------------
    "server/tools/bwmachined": ("gap", "BW-05", "", "bwmachined daemon not ported"),
    "server/tools/bots": ("gap", "BW-17", "", "bots load-generator not ported"),
    "server/tools/bw_profile": ("gap", "BW-12", "", "bw_profile not ported"),
    "server/tools/clear_auto_load": ("gap", "BW-09", "", "clear_auto_load not ported"),
    "server/tools/consolidate_dbs": ("gap", "BW-09", "", "consolidate_dbs not ported"),
    "server/tools/snapshot_helper": ("gap", "BW-09", "", "snapshot_helper not ported"),
    "server/tools/sync_db": ("gap", "BW-09", "", "sync_db not ported"),
    "server/tools/transfer_db": ("gap", "BW-09", "", "transfer_db not ported"),
    "lib/db": ("gap", "BW-09", "", "entity database layer not ported"),
    "lib/db/unit_test": ("gap", "BW-09", "", "entity database layer not ported"),
    "lib/db_storage": ("gap", "BW-09", "", "database storage abstraction not ported"),
    "lib/db_storage_mysql": ("gap", "BW-09", "", "MySQL storage backend not ported"),
    "lib/db_storage_xml": ("gap", "BW-09", "", "XML storage backend not ported"),
    # --- build system --------------------------------------------------------
    "lib/build": ("gap", "BW-00-03", "", "build tooling not ported"),
    # --- examples: the minimal client's login flow is evidenced --------------
    "examples/client_integration/python/simple": (
        "partial", "BW-14", "docs/review/bw00-02-04-minimal-client-login.md",
        "example script integration not ported"),
}

# dir -> owning lane for gap rows whose area default is too coarse. The ids are
# the task-book index: BW-10 map/nav, BW-11 client render/input/connection,
# BW-13 resource pipeline, BW-14/19 examples and demo.
GAP_TASK = {
    "lib/chunk": "BW-10", "lib/chunk_loading": "BW-10",
    "lib/chunk_scene_adapter": "BW-10", "lib/space": "BW-10",
    "lib/scene": "BW-10", "lib/scene/unit_test": "BW-10",
    "lib/terrain": "BW-10", "lib/terrain/unit_test": "BW-10",
    "lib/waypoint": "BW-10", "lib/waypoint_generator": "BW-10",
    "lib/physics2": "BW-11", "lib/physics2/unit_test": "BW-11",
    "lib/moo": "BW-11", "lib/model": "BW-11", "lib/romp": "BW-11",
    "lib/particle": "BW-11", "lib/speedtree": "BW-11",
    "lib/fmodsound": "BW-11", "lib/ashes": "BW-11", "lib/duplo": "BW-11",
    "lib/input": "BW-11",
    "lib/connection": "BW-11", "lib/connection_model": "BW-11",
    "lib/urlrequest": "BW-11", "lib/delegate_interface": "BW-11",
    "lib/asset_pipeline": "BW-13", "lib/bwentity": "BW-13",
    "tools/navgen": "BW-10", "tools/assetprocessor": "BW-13",
    "tools/res_packer": "BW-13",
    "examples/cellapp_extension": "BW-19",
    "fantasydemo": "BW-14",
}

# Top-level area -> (status, feature_id, blocker) for targets not in COVERAGE.
# The client-side rendering libraries and the editors are Windows-only in the
# reference, so they are recorded as not-enabled on the Linux server build;
# "-" is the explicit "no porting task / no fixture".
AREA = {
    "client": ("not-enabled", "-", "Windows client; not enabled on the Linux server build"),
    "tools": ("gap", "BW-13", "Windows editor/export tools; not enabled on the Linux server build"),
    "third_party": ("not-enabled", "-", "vendored third-party; not part of the ported stack"),
    "cmake": ("not-enabled", "-", "CMake build-system samples; not a production target"),
    "examples": ("gap", "BW-19", "example extensions not ported"),
    "common": ("gap", "BW-00-05", "shared server/client code not ported"),
    "server": ("gap", "BW-01-04", "server target not in the coverage map"),
    "lib": ("gap", "BW-00-05", "engine library not ported"),
    ".": ("not-enabled", "-", "root meta-target; not a production target"),
}

# Areas that are unconditionally off the Linux build, whatever the
# configurations say: the Windows client, the vendored third-party trees, the
# CMake samples and the root meta-target. `tools` is deliberately NOT here:
# the editor/exporters live on Windows, but the reference also builds some of
# them (navgen, assetprocessor, res_packer, process_defs) in Linux
# configurations, so their enable condition decides.
ALWAYS_OFF = {"client", "third_party", "cmake", "."}


def _area(dirname):
    return dirname.split("/", 1)[0]


# The builds the Linux server port actually compiles. The reference also has
# client/tools/exporters configurations, which build Windows-only binaries.
LINUX_BUILDS = ("server", "extras", "navgen", "assetprocessor", "bwentity")


def _linux_enabled(enable):
    return any(b in enable for b in LINUX_BUILDS)


def status_for(target, kind, dirname, condition, enable):
    """(status, feature_id, fixture, blocker) for one target."""
    if dirname in COVERAGE:
        return COVERAGE[dirname]
    area = _area(dirname)
    status, task, blocker = AREA.get(area, AREA["lib"])
    if area in ALWAYS_OFF:
        return (status, task, "-", blocker)
    # Anything not in a Linux build is Windows-only, a vendored dependency, or
    # a CMake sample — recorded, not ported.
    if not _linux_enabled(enable):
        return ("not-enabled", "-", "-", blocker)
    return ("gap", GAP_TASK.get(dirname, task), "", blocker)


# Non-CMake ledger entries: parts of the original stack that have no build
# target of their own (common/ is compiled into other targets; FantasyDemo is
# content) or are distribution artifacts (the shipped server binaries). They
# are ledger rows all the same — BW-00-05-04/05 index them.
NON_CMAKE = [
    ("common", "-", "common", "compiled into other targets (no own target)",
     "BW-03-03", "crates/python-host tests (py_* bindings) "
                  "| remaining shared code not ported", "partial"),
    ("fantasydemo", "RES", "fantasydemo", "game content (res/scripts/web)",
     "BW-14", "gap: demo scripts and resources not ported", "gap"),
    # The shipped Linux server distribution (game/bin/server/el7).
    ("baseapp", "DIST", "game/bin/server/el7/server", "shipped binary",
     "BW-07", "crates/server-runtime tests (BW-01-04) | no shipped binary; app logic not implemented", "partial"),
    ("baseappmgr", "DIST", "game/bin/server/el7/server", "shipped binary",
     "BW-05", "crates/server-runtime tests (BW-01-04) | no shipped binary; app logic not implemented", "partial"),
    ("bots", "DIST", "game/bin/server/el7/server", "shipped binary",
     "BW-17", "gap: bots load-generator not ported", "gap"),
    ("cellapp", "DIST", "game/bin/server/el7/server", "shipped binary",
     "BW-08", "crates/server-runtime tests (BW-01-04) | no shipped binary; app logic not implemented", "partial"),
    ("cellappmgr", "DIST", "game/bin/server/el7/server", "shipped binary",
     "BW-05", "crates/server-runtime tests (BW-01-04) | no shipped binary; app logic not implemented", "partial"),
    ("dbapp", "DIST", "game/bin/server/el7/server", "shipped binary",
     "BW-09", "crates/server-runtime tests (BW-01-04) | no shipped binary; app logic not implemented", "partial"),
    ("dbappmgr", "DIST", "game/bin/server/el7/server", "shipped binary",
     "BW-05", "crates/server-runtime tests (BW-01-04) | no shipped binary; app logic not implemented", "partial"),
    ("loginapp", "DIST", "game/bin/server/el7/server", "shipped binary",
     "BW-06", "crates/server-runtime tests (BW-01-04) | no shipped binary; app logic not implemented", "partial"),
    ("reviver", "DIST", "game/bin/server/el7/server", "shipped binary",
     "BW-05", "crates/server-runtime tests (BW-01-04) | no shipped binary; app logic not implemented", "partial"),
    ("serviceapp", "DIST", "game/bin/server/el7/server", "shipped binary",
     "BW-07", "gap: service app not ported", "gap"),
    ("bwmachined2", "DIST", "game/bin/server/el7/tools", "shipped binary",
     "BW-05", "gap: bwmachined daemon not ported", "gap"),
    ("message_logger", "DIST", "game/bin/server/el7/tools", "shipped binary",
     "BW-12", "gap: message logger not ported", "gap"),
    ("process_defs", "DIST", "game/bin/server/el7/tools", "shipped binary",
     "BW-00-04", "docs/review/fixtures/full_def + oracles (target/debug/process_defs)", "covered"),
    ("res_packer", "DIST", "game/bin/server/el7/tools", "shipped binary",
     "BW-13", "gap: res packer not ported", "gap"),
]


def _prefer(new, old):
    """True when the new duplicate row should win over the old one."""
    if _linux_enabled(new[3]) != _linux_enabled(old[3]):
        return _linux_enabled(new[3])
    # A Windows-DLL (SHARED) declaration loses to the static server build.
    new_shared = "SHARED" in new[2] or "SHARED" in new[3]
    old_shared = "SHARED" in old[2] or "SHARED" in old[3]
    if new_shared != old_shared:
        return not new_shared
    return False


def main():
    if len(sys.argv) != 2:
        print("usage: build_ledger.py <reference_tree>", file=sys.stderr)
        return 2
    ref = sys.argv[1]
    base = os.path.join(ref, "programming", "bigworld")
    if not os.path.isdir(base):
        print(f"no reference source tree at {base}", file=sys.stderr)
        return 2

    # The build-target membership is the authoritative enable condition; the
    # IF predicates inside a CMakeLists are secondary, kept as a fallback.
    configs = extract_targets.parse_configs(base)

    rows = {}
    for root, _dirs, files in os.walk(base):
        if "CMakeLists.txt" not in files:
            continue
        cmake = os.path.join(root, "CMakeLists.txt")
        rel_dir = os.path.relpath(root, base)
        if rel_dir == ".":
            rel_dir = "."
        for target, kind, cond in extract_targets.parse_cmakelists(cmake):
            enable = extract_targets.enable_for(configs, target, rel_dir)
            if enable == "not-in-config" and cond:
                enable = cond
            status, task, fixture, blocker = status_for(target, kind, rel_dir, cond, enable)
            if status in ("covered", "partial"):
                note = "" if status == "covered" else f" | {blocker}"
                fixture = f"{fixture}{note}"
            elif status == "gap":
                fixture = f"gap: {blocker}"
            row = (rel_dir, target, kind, enable, task, fixture, status)
            # One build target per directory: the reference declares some
            # targets twice (a Windows DLL and the static server build, or the
            # executable and its BW_ADD_TEST registration). Prefer the row the
            # Linux build actually compiles.
            key = (rel_dir, target)
            if key not in rows or _prefer(row, rows[key]):
                rows[key] = row

    rows = sorted(rows.values(), key=lambda r: (r[0], r[1]))
    for target, kind, rel_dir, enable, task, fixture, status in NON_CMAKE:
        rows.append((rel_dir, target, kind, enable, task, fixture, status))
    rows.sort(key=lambda r: (r[0], r[1]))
    print("target\ttype\tdir\tenable_condition\tfeature_id\tfixture\tstatus")
    for rel_dir, target, kind, enable, task, fixture, status in rows:
        print(f"{target}\t{kind}\t{rel_dir}\t{enable}\t{task}\t{fixture}\t{status}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
