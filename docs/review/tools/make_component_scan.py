#!/usr/bin/env python3
"""Scan the reference BigWorld Make build for declared components (BW-00-01-01).

Each component is declared by a makefile that sets `name :=` and `type :=`
(the standard Makefile.rules, or a custom makefile such as
Makefile.message_logger). A target can also register itself directly against
`BW_BINS` without a `type` (the once-off remove_db script copy); those are
classified as `script_copy`.

The `subdirs = ...` lists drive the include recursion, so the scan computes
reachability from the bigworld root and marks every component `in_build`
(1 = reachable through the static subdirs graph, 0 = present in the tree but
not built by Make, e.g. the leftover third_party/png and third_party/zip
component stubs). Conditional guards (the `ifeq` line nearest to the name
declaration) are kept verbatim in the `guard` column.

Usage: make_component_scan.py PROGRAMMING_BIGWORLD_ROOT [OUTPUT_TSV]
"""
import os
import re
import sys
from collections import deque

COMPONENT_TYPES = {
    "library",
    "binary",
    "unit_test",
    "shared_library",
    "copy_of_binary",
    "copy_of_shared_library",
}

FLAG_RE = re.compile(r"^\s*(use\w+)\s*:?=\s*(\d+)\s*(?:#.*)?$")
NAME_RE = re.compile(r"^\s*name\s*:?=\s*(\S+)\s*(?:#.*)?$")
TYPE_RE = re.compile(r"^\s*type\s*:?=\s*(\S+)\s*(?:#.*)?$")
BINDIR_RE = re.compile(r"^\s*bindir\s*:?=\s*(\S+)\s*(?:#.*)?$")
EXPORT_RE = re.compile(r"^\s*exportFor\s*:?=\s*(\S.*)$")
DEPENDS_RE = re.compile(r"^\s*dependsOn\s*:?=\s*(\S.*)$")
GUARD_RE = re.compile(r"^\s*(if(eq|neq|def|ndef))\b")
CONT_RE = re.compile(r"\\\s*$")
SUBDIRS_RE = re.compile(r"^\s*subdirs\s*:?=")
SUBDIRS_PLUS_RE = re.compile(r"^\s*subdirs\s*\+=")
MAKEFILE_RE = re.compile(r"^Makefile(?:\.[A-Za-z0-9_.-]+)?$")


def continuation_values(lines, start):
    r"""Collect `\`-continued words from a subdirs assignment at `start`."""
    words = []
    i = start
    while i < len(lines):
        line = lines[i]
        line = CONT_RE.sub("", line)
        parts = line.split(":=", 1) if ":=" in line else line.split("=", 1)
        seg = parts[1] if len(parts) > 1 else line
        words.extend(seg.split())
        if not CONT_RE.search(lines[i]):
            break
        i += 1
    return words


def parse_makefile(path):
    with open(path, encoding="utf-8", errors="replace") as fh:
        lines = fh.read().splitlines()
    name = type_ = None
    bindir = export_for = guard = None
    flags = {}
    depends = ""
    src_count = 0
    src_kinds = set()
    raw_bins = False
    subdirs = []
    for idx, line in enumerate(lines):
        if name is None:
            m = NAME_RE.match(line)
            if m:
                name = m.group(1)
                for j in range(max(0, idx - 20), idx):
                    if GUARD_RE.match(lines[j]):
                        guard = lines[j].strip()
                        break
        if type_ is None:
            m = TYPE_RE.match(line)
            if m:
                type_ = m.group(1)
        if bindir is None:
            m = BINDIR_RE.match(line)
            if m:
                bindir = m.group(1)
        if export_for is None:
            m = EXPORT_RE.match(line)
            if m:
                export_for = m.group(1).strip()
        if depends == "":
            m = DEPENDS_RE.match(line)
            if m:
                depends = m.group(1).strip()
        m = FLAG_RE.match(line)
        if m:
            flags[m.group(1)] = m.group(2)
        m = re.match(r"^\s*(cxx|cc|c)Source\s*:?=", line)
        if m:
            kind = m.group(1)
            src_count += len(continuation_values(lines, idx))
            src_kinds.add(kind)
        # The first assignment only: some rules files reassign subdirs with
        # `:=` plus a $(filter-out ...) override (server's LICENSE_SERVER arm).
        if not subdirs and SUBDIRS_RE.match(line):
            subdirs = continuation_values(lines, idx)
        if SUBDIRS_PLUS_RE.match(line):
            subdirs += continuation_values(lines, idx)
        if re.match(r"^\s*BW_BINS\s*\+=", line):
            raw_bins = True
    return {
        "name": name,
        "type": type_,
        "bindir": bindir or "",
        "export_for": export_for or "",
        "flags": flags,
        "depends": depends.replace("\\", "").strip(),
        "guard": guard or "",
        "sources": src_count,
        "raw_bins": raw_bins,
        "subdirs": subdirs,
    }


def scan(root):
    """Walk the tree collecting components and the subdirs include graph."""
    components = []  # list of (dir_rel, info, rules_relpath)
    graph = {}  # dir_rel -> [subdir names]
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames[:] = sorted(d for d in dirnames if d not in (".git", "__pycache__"))
        for fn in sorted(filenames):
            if not MAKEFILE_RE.match(fn):
                continue
            path = os.path.join(dirpath, fn)
            info = parse_makefile(path)
            dirrel = os.path.relpath(dirpath, root)
            if info["subdirs"]:
                graph[dirrel] = info["subdirs"]
            if info["name"] is None:
                continue
            if info["type"] is None and not info["raw_bins"]:
                continue
            components.append((dirrel, info, os.path.relpath(path, root)))

    # Reachability from the bigworld root through the static subdirs graph.
    reachable = set()
    queue = deque([root and "."])
    while queue:
        cur = queue.popleft()
        if cur in reachable:
            continue
        reachable.add(cur)
        for name in graph.get(cur, []):
            queue.append(os.path.normpath(os.path.join(cur, name)))

    rows = []
    seen = set()
    for dirrel, info, rules in components:
        key = (info["name"], info["type"] or "script_copy")
        if key in seen:
            continue
        seen.add(key)
        parent = os.path.dirname(dirrel)
        # A component is in the build when its own dir is reachable; a
        # unit_test child is pulled in by its parent's unitTestIncludeWorkAround.
        if os.path.basename(dirrel) == "unit_test":
            in_build = "1" if parent in reachable else "0"
        else:
            in_build = "1" if dirrel in reachable else "0"
        has_unit_tests = os.path.isdir(os.path.join(root, dirrel, "unit_test"))
        rows.append(
            {
                "type": info["type"] or "script_copy",
                "name": info["name"],
                "dir": dirrel,
                "in_build": in_build,
                "sources": info["sources"],
                "bindir": info["bindir"],
                "flags": " ".join(f"{k}={v}" for k, v in sorted(info["flags"].items())),
                "depends": info["depends"],
                "export_for": info["export_for"],
                "guard": info["guard"],
                "unit_test": "1" if has_unit_tests else "0",
                "rules": rules,
            }
        )
    return rows


def main():
    if len(sys.argv) < 2:
        print(__doc__)
        return 1
    root = sys.argv[1]
    rows = scan(root)
    header = [
        "type", "name", "dir", "in_build", "sources", "bindir", "flags",
        "depends", "export_for", "guard", "unit_test", "rules",
    ]
    if len(sys.argv) >= 3:
        with open(sys.argv[2], "w", encoding="utf-8") as fh:
            fh.write("\t".join(header) + "\n")
            for r in rows:
                fh.write("\t".join(str(r[h]) for h in header) + "\n")
    by_type = {}
    for r in rows:
        by_type.setdefault(r["type"], []).append(r)
    print(f"components: {len(rows)}")
    for t in sorted(by_type):
        inb = sum(1 for r in by_type[t] if r["in_build"] == "1")
        print(f"  {t:24s} {len(by_type[t]):3d}  (in_build {inb})  e.g. "
              + ", ".join(r["name"] for r in by_type[t][:4]))
    return 0


if __name__ == "__main__":
    sys.exit(main())
