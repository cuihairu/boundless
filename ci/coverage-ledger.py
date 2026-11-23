#!/usr/bin/env python3
"""BW-00-05-01: the coverage ledger fields.

The ledger records, per reference build target, the five fields the coverage
review needs — the original target, its enable condition, the task that maps
it, the fixture that verifies it, and its coverage status. This parser
enumerates the reference's CMake build targets (BW_ADD_LIBRARY/EXECUTABLE/TEST/
TOOL_EXE/TOOL_TEST/ASSETPIPELINE_DLL/ASSETPIPELINE_EXE) under lib/, server/,
client/, tools/, common/ and examples/, and derives per target:

  * original_path — the target's source directory (relative to the bigworld
    source root) and its name, so a reader can find the build.
  * enable_condition — the innermost IF(...) that gates the target, or "always"
    when the target builds unconditionally.
  * task — the BW-00-05-xx item whose acceptance maps this category
    (lib+server -> 02, client+tools -> 03, common+examples -> 04).
  * fixture — the verification fixture, "pending" until the mapping item lands.
  * status — "unmapped" until the mapping item lands.

The distribution/SDK and missing-resource gaps are not CMake targets; they are
game/bin artifacts recorded by the BW-00-05-05 item, so they are out of scope
here.

Usage:
  ci/coverage-ledger.py REFERENCE_SOURCE_ROOT OUT_DIR [--self-check]

Exits 2 when the reference source it needs is missing; --self-check exits 3 on
a hollow ledger (no targets, or a row missing a field).
"""

import json
import os
import re
import sys

ROOTS = ("lib", "server", "client", "tools", "common", "examples")
TARGET_RE = re.compile(
    r"BW_ADD_(LIBRARY|EXECUTABLE|TEST|TOOL_EXE|TOOL_TEST|ASSETPIPELINE_DLL|"
    r"ASSETPIPELINE_EXE)\s*\(\s*([A-Za-z0-9_]+)")
IF_RE = re.compile(r"^\s*IF\s*\(\s*(.*?)\s*\)", re.I)
ENDIF_RE = re.compile(r"^\s*ENDIF\s*\(", re.I)

KIND_LABEL = {
    "LIBRARY": "library", "EXECUTABLE": "executable", "TEST": "test",
    "TOOL_EXE": "tool", "TOOL_TEST": "tool test",
    "ASSETPIPELINE_DLL": "asset pipeline dll",
    "ASSETPIPELINE_EXE": "asset pipeline exe",
}


def category_task(rel_dir):
    """The BW-00-05-xx item whose acceptance maps this category."""
    if rel_dir.startswith("lib") or rel_dir.startswith("server"):
        return "BW-00-05-02"
    if rel_dir.startswith("client") or rel_dir.startswith("tools"):
        return "BW-00-05-03"
    if rel_dir.startswith("common") or rel_dir.startswith("examples"):
        return "BW-00-05-04"
    return "BW-00-05-06"


def targets_in_cmakelist(text, rel_dir):
    """Yield {name, kind, dir, enable_condition} for each BW_ADD_* target.

    The enable condition is the innermost enclosing IF(...) text, tracked by
    scanning the IF/ELSE/ENDIF nesting; an unconditional target is "always".
    """
    out = []
    stack = []  # active IF conditions ("" for ELSE branches)
    for raw in text.splitlines():
        line = raw.strip()
        if ENDIF_RE.match(line):
            if stack:
                stack.pop()
            continue
        m = IF_RE.match(line)
        if m:
            cond = m.group(1).strip()
            if not cond:
                cond = "if (empty condition)"
            stack.append(cond)
            continue
        if re.match(r"^\s*ELSEIF\s*\(\s*(.*?)\s*\)", line, re.I):
            m = re.match(r"^\s*ELSEIF\s*\(\s*(.*?)\s*\)", line, re.I)
            if stack:
                stack[-1] = m.group(1).strip() or "else branch"
            continue
        if re.match(r"^\s*ELSE\s*\(", line, re.I):
            if stack:
                stack[-1] = "else branch"
            continue
        m = TARGET_RE.search(line)
        if m:
            kind, name = m.group(1), m.group(2)
            cond = stack[-1] if stack else "always"
            out.append({"name": name, "kind": kind,
                        "dir": rel_dir, "enable_condition": cond})
    return out


def enumerate_targets(src_root):
    """The reference's build targets across the six source roots."""
    if not os.path.isdir(src_root):
        return None
    rows = []
    for root in ROOTS:
        root_dir = os.path.join(src_root, root)
        if not os.path.isdir(root_dir):
            continue
        for dirpath, _dirs, files in os.walk(root_dir):
            if "CMakeLists.txt" not in files:
                continue
            rel = os.path.relpath(dirpath, src_root)
            with open(os.path.join(dirpath, "CMakeLists.txt"),
                      encoding="utf-8", errors="replace") as f:
                text = f.read()
            for t in targets_in_cmakelist(text, rel):
                rows.append({
                    "original_path": "%s (%s)" % (t["name"], t["dir"]),
                    "kind": KIND_LABEL.get(t["kind"], t["kind"].lower()),
                    "enable_condition": t["enable_condition"],
                    "task": category_task(t["dir"]),
                    "fixture": "pending",
                    "status": "unmapped",
                })
    return rows


def emit(src_root, out_dir, self_check=False):
    rows = enumerate_targets(src_root)
    if rows is None:
        sys.stderr.write("missing source: no reference source root at %s\n" % src_root)
        sys.exit(2)
    if not rows:
        sys.stderr.write("no build targets enumerated — a hollow ledger is not a record\n")
        sys.exit(2)

    if self_check:
        problems = []
        if not rows:
            problems.append("no targets")
        for r in rows:
            for field in ("original_path", "enable_condition", "task",
                          "fixture", "status"):
                if not r[field]:
                    problems.append("a row missing %s" % field)
        if problems:
            sys.stderr.write("hollow ledger: %s\n" % "; ".join(problems))
            sys.exit(3)

    os.makedirs(out_dir, exist_ok=True)
    with open(os.path.join(out_dir, "coverage-ledger.json"), "w") as f:
        json.dump({"rows": rows, "count": len(rows),
                   "schema": ["original_path", "enable_condition",
                              "task", "fixture", "status"]}, f, indent=2)
        f.write("\n")
    with open(os.path.join(out_dir, "coverage-ledger.tsv"), "w") as f:
        f.write("original_path\tenable_condition\ttask\tfixture\tstatus\n")
        for r in rows:
            f.write("%s\t%s\t%s\t%s\t%s\n" % (
                r["original_path"], r["enable_condition"].replace("\t", " "),
                r["task"], r["fixture"], r["status"]))
    return rows


if __name__ == "__main__":
    args = sys.argv[1:]
    self_check = "--self-check" in args
    if self_check:
        args.remove("--self-check")
    src = args[0] if len(args) > 0 else os.getcwd()
    out_dir = args[1] if len(args) > 1 else "."
    rows = emit(src, out_dir, self_check)
    kinds = {}
    for r in rows:
        kinds[r["kind"]] = kinds.get(r["kind"], 0) + 1
    print("targets=%d (%s)" % (len(rows),
          ", ".join("%s=%d" % (k, v) for k, v in sorted(kinds.items()))))
