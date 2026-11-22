#!/usr/bin/env python3
"""Probe the Windows client / tools baseline for BW-00-02-05.

Acceptance: one result or gap per target. A target only "has a result" when a
Windows artifact exists somewhere in the reference tree, so this scan walks the
tree once per artifact class and then reports, for every CMake target recorded
in the BW-00-01-02 inventory, either the artifact paths it found or the reason
there is nothing to run:

  * game/bin/<target>.exe|dll|pdb, plus the flat game/bin/server win32/win64
    directories the CMake build outputs into;
  * the packaging scripts that would produce them (package-daily-windows.ps1)
    and the CI job that builds them (ci-windows.yml);
  * whether this host can build or run Windows binaries at all
    (msbuild/cl/where.exe vs wine), which is what makes the remaining targets a
    gap rather than a failure.

Usage: windows_baseline_scan.py BIGWORLD_ROOT CMAKE_TARGETS_JSON OUTPUT_JSON
"""
import json
import os
import re
import subprocess
import sys
import time

# CMake output directory the Windows build is told to use (build-windows-server.ps1
# configures -DBUILD_RUNTIME_OUTPUT_DIRECTORY=<root>/game/bin/server and the CI
# root job builds into build/win_root_build, both staging there).
WIN_BIN_DIRS = ("win32", "win64", "server")
# Host toolchains that could produce or execute Windows binaries here.
HOST_TOOLS = ("cl.exe", "msbuild.exe", "where.exe", "wine", "wine64", "powershell.exe")


def run(cmd, timeout=20):
    try:
        return subprocess.run(cmd, capture_output=True, text=True,
                              timeout=timeout).stdout.strip()
    except Exception:
        return None


def find_files(root, exts, limit=40):
    """All files under root with one of exts, skipping the cross-built python host."""
    out = []
    for base, dirs, files in os.walk(root):
        # The python interpreter is cross-compiled here (build/*/bwpython313,
        # build/el7/third_party/python) and is the only win32 binary on a Linux
        # tree; it is not an artifact of any CMake target.
        if "bwpython" in base or "third_party/python" in base:
            dirs[:] = []
            continue
        for f in files:
            if f.lower().endswith(exts):
                p = os.path.relpath(os.path.join(base, f), root)
                if len(out) < limit:
                    out.append(p)
    return sorted(out)


def tool_present(name):
    return subprocess.run(["which", name], capture_output=True).returncode == 0


TRIGGERS = ("push", "pull_request", "workflow_dispatch", "schedule",
            "workflow_call", "pull_request_target", "issues", "discussion")
JOB_RE = re.compile(r"^  (?P<job>[a-z0-9_-]+):$")


def workflow_facts(root):
    path = os.path.join(root, ".github", "workflows", "ci-windows.yml")
    facts = {"path": path, "exists": os.path.isfile(path), "jobs": [],
             "runner": None, "build_dir": None}
    if not facts["exists"]:
        return facts
    text = open(path, errors="replace").read()
    jobs = []
    for line in text.splitlines():
        m = JOB_RE.match(line)
        if m and m.group("job") not in TRIGGERS:
            jobs.append(m.group("job"))
    m = re.search(r"runs-on: (?P<os>.*)$", text, re.M)
    runner = m.group("os").strip() if m else None
    facts["jobs"] = [{"job": j, "runner": runner} for j in jobs]
    facts["runner"] = runner
    if "win_root_build" in text:
        facts["build_dir"] = "build/win_root_build"
    facts["builds_hybrid_all_targets"] = ("Build root superproject (Hybrid, all targets)"
                                          in text)
    facts["library_matrix_targets"] = []
    for t in re.finditer(r"--target\s+(?P<t>[a-z0-9_]+)", text):
        if t.group("t") not in facts["library_matrix_targets"]:
            facts["library_matrix_targets"].append(t.group("t"))
    return facts


def main():
    if len(sys.argv) < 4:
        print(__doc__)
        return 1
    root, targets_json, out_json = sys.argv[1], sys.argv[2], sys.argv[3]

    with open(targets_json, encoding="utf-8") as fh:
        targets = json.load(fh)

    bin_root = os.path.join(root, "game", "bin")
    win_dirs = {}
    for d in WIN_BIN_DIRS:
        p = os.path.join(bin_root, d)
        win_dirs[d] = os.path.isdir(p)
    artifacts = {ext: find_files(root, ("." + ext,)) for ext in ("exe", "dll", "pdb")}

    host = {t: tool_present(t) for t in HOST_TOOLS}
    wf = workflow_facts(root)

    pkg = os.path.join(root, "scripts", "package-daily-windows.ps1")
    pkg_facts = {"path": pkg, "exists": os.path.isfile(pkg),
                 "zip": None, "layout": None}
    if pkg_facts["exists"]:
        text = open(pkg, errors="replace").read()
        m = re.search(r"\$(?P<zip>bigwindows|bigworld-windows-server)[^\s\"]*", text)
        m2 = re.search(r"\-BuildDir\s+(?P<dir>\$?\S+)", text)
        pkg_facts["zip"] = ("bigworld-windows-server-$ver.zip"
                            if "bigworld-windows-server" in text else None)
        pkg_facts["build_dir"] = m2.group("dir") if m2 else None
        m3 = re.search(r"Copy-Item[\s\S]{0,400}?-Destination[^\n]*", text)
        pkg_facts["layout"] = "flat (no subdirectories)" if m3 else None

    rows = []
    for name, info in targets.items():
        projs = info.get("projects") or []
        found = []
        for ext in ("exe", "dll", "pdb"):
            found += [p for p in artifacts[ext]
                      if name in os.path.splitext(os.path.basename(p))[0].lower()]
        rows.append({
            "target": name,
            "linux_projects": len(projs),
            "consumer_release": bool(info.get("consumer_release")),
            "artifacts": found,
            "result": "artifact" if found else "gap",
        })

    data = {
        "source_commit": run(["git", "-C", root, "rev-parse", "--short", "HEAD"]),
        "captured_at": time.strftime("%Y-%m-%d %H:%M:%S %z"),
        "game_bin_dirs": {"server": os.path.isdir(os.path.join(bin_root, "server")),
                          **win_dirs},
        "artifacts_found": artifacts,
        "host_tools": host,
        "windows_workflow": wf,
        "package_script": pkg_facts,
        "targets": rows,
    }
    with open(out_json, "w", encoding="utf-8") as fh:
        json.dump(data, fh, indent=1, sort_keys=True)

    print("commit %s  targets %d  artifacts: "
          "exe=%d dll=%d pdb=%d" % (
              data["source_commit"], len(rows), len(artifacts["exe"]),
              len(artifacts["dll"]), len(artifacts["pdb"])))
    print("game/bin dirs: %s" % ", ".join(
        "%s=%s" % (k, v) for k, v in data["game_bin_dirs"].items()))
    print("wine=%s msbuild=%s cl=%s" % (host["wine"], host["msbuild.exe"],
                                        host["cl.exe"]))
    for r in rows:
        print("  %-14s %-3s projects=%-3s artifacts=%d" % (
            r["target"], r["result"], r["linux_projects"], len(r["artifacts"])))
    return 0


if __name__ == "__main__":
    sys.exit(main())