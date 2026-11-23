#!/usr/bin/env python3
"""BW-00-05 — extract the reference build manifest.

Every CMake target the original stack defines is one row: `target`, `type`,
`dir` (the source directory, relative to programming/bigworld), the
`CMakeLists.txt` that declares it, and the `enable_condition` — the IF
predicates enclosing the declaration, so the ledger can say when the target
is actually built. The manifest is the "构建清册" that BW-00-05-02..04 index.

Usage:
    extract_targets.py <reference_tree> > manifest.tsv
"""

import re
import sys

# Macro name -> build-artifact kind. The original stack declares targets with
# its own macros rather than the raw CMake commands; both are captured.
KINDS = {
    "BW_ADD_LIBRARY": "LIB",
    "add_library": "LIB",
    "BW_ADD_EXECUTABLE": "EXE",
    "add_executable": "EXE",
    "BW_ADD_TOOL_EXE": "TOOL_EXE",
    "BW_ADD_ASSETPIPELINE_EXE": "ASSET_EXE",
    "BW_ADD_TEST": "TEST",
    "BW_ADD_TOOL_TEST": "TOOL_TEST",
}

DECL = re.compile(r"^\s*(BW_ADD_LIBRARY|add_library|BW_ADD_EXECUTABLE|"
                  r"add_executable|BW_ADD_TOOL_EXE|BW_ADD_ASSETPIPELINE_EXE|"
                  r"BW_ADD_TEST|BW_ADD_TOOL_TEST)\s*\(")
IF_RE = re.compile(r"^\s*(IF|ELSEIF)\s*\(\s*(.*?)\s*\)")
ELSE_RE = re.compile(r"^\s*ELSE\s*\(")
ENDIF_RE = re.compile(r"^\s*ENDIF\s*\(")


def _balanced_end(text, start):
    """Index just past the closing paren of the call beginning at start."""
    depth = 0
    i = start
    while i < len(text):
        if text[i] == "(":
            depth += 1
        elif text[i] == ")":
            depth -= 1
            if depth == 0:
                return i + 1
        i += 1
    return None


def _target_name(call):
    """First token of the declaration, with CMake variable indirection kept."""
    token = call.strip().split()[0] if call.strip() else ""
    return token.strip()


def _normalize_condition(pred):
    """Shorten a predicate so the ledger stays readable.

    `BW_PLATFORM_WINDOWS`, `WIN32`, `${BW_PLATFORM} STREQUAL "win32"` all mean
    the Windows build; the platform family and the feature flags are what the
    ledger distinguishes.
    """
    p = " ".join(pred.split())
    p = p.replace("${BW_PLATFORM}", "BW_PLATFORM")
    p = p.replace('STREQUAL "win32"', "=win32")
    p = p.replace('STREQUAL "linux"', "=linux")
    p = p.replace('STREQUAL "macosx"', "=macosx")
    return p


def parse_cmakelists(path):
    """Yield (target, kind, enable_condition) for one CMakeLists.txt."""
    try:
        with open(path, encoding="utf-8") as handle:
            text = handle.read()
    except UnicodeDecodeError:
        # One manifest in the reference tree carries non-UTF-8 bytes; the
        # declarations are ASCII, so a latin-1 read preserves every token.
        with open(path, encoding="latin-1") as handle:
            text = handle.read()
    lines = text.splitlines()
    cond_stack = []
    i = 0
    while i < len(lines):
        line = lines[i]
        if ENDIF_RE.match(line):
            if cond_stack:
                cond_stack.pop()
            i += 1
            continue
        if ELSE_RE.match(line):
            if cond_stack:
                cond_stack[-1] = "(not " + cond_stack[-1].rstrip() + ")"
            i += 1
            continue
        m = IF_RE.match(line)
        if m:
            cond_stack.append(_normalize_condition(m.group(2)))
            i += 1
            continue
        decl = DECL.match(line)
        if decl:
            kind = KINDS[decl.group(1)]
            joined = line
            end = _balanced_end(line, line.index("("))
            j = i + 1
            while end is None and j < len(lines):
                joined += "\n" + lines[j]
                end = _balanced_end(joined, joined.index("("))
                j += 1
            if end is not None:
                call = joined[:end]
                target = _target_name(call[len(decl.group(0)):])
                yield target, kind, "; ".join(cond_stack)
            i = j
            continue
        i += 1


def main():
    if len(sys.argv) != 2:
        print("usage: extract_targets.py <reference_tree>", file=sys.stderr)
        return 2
    ref = sys.argv[1]
    import os
    base = os.path.join(ref, "programming", "bigworld")
    if not os.path.isdir(base):
        print(f"no reference source tree at {base}", file=sys.stderr)
        return 2

    rows = []
    for root, _dirs, files in os.walk(base):
        if "CMakeLists.txt" not in files:
            continue
        cmake = os.path.join(root, "CMakeLists.txt")
        rel_dir = os.path.relpath(root, base)
        if rel_dir == ".":
            rel_dir = "."
        for target, kind, cond in parse_cmakelists(cmake):
            rows.append((target, kind, rel_dir, os.path.relpath(cmake, base), cond))

    # Deterministic order: directory, then target.
    rows.sort(key=lambda r: (r[2], r[0]))
    print("target\ttype\tdir\tcmakelist\tenable_condition")
    for target, kind, rel_dir, cmake, cond in rows:
        print(f"{target}\t{kind}\t{rel_dir}\t{cmake}\t{cond}")
    return 0


# --- the BW_CMAKE_TARGET configurations ---------------------------------------
#
# The per-build-target configurations (build/cmake/BWConfiguration_*.cmake)
# list the libraries and executables each BW_CMAKE_TARGET builds, as
# `name<TAB>path` pairs. This is the authoritative enable condition: a target
# is enabled in the `server` build iff its directory appears in the server
# configuration. The IF predicates inside a CMakeLists.txt are secondary.

CONFIG_RE = re.compile(r"^\s*SET\(\s*(BW_LIBRARY_PROJECTS|BW_BINARY_PROJECTS|"
                       r"BW_SERVER_UNIT_TEST_LIBRARIES|BW_SERVER_UNIT_TEST_BINARIES)",
                       re.M)


def parse_configs(base):
    """{(name, dir): sorted([build_target, ...])} for every configuration."""
    import os
    result = {}
    config_dir = os.path.join(base, "build", "cmake")
    if not os.path.isdir(config_dir):
        return result
    for fname in sorted(os.listdir(config_dir)):
        if not fname.startswith("BWConfiguration_") or not fname.endswith(".cmake"):
            continue
        build_target = fname[len("BWConfiguration_"):-len(".cmake")]
        try:
            with open(os.path.join(config_dir, fname), encoding="utf-8") as handle:
                text = handle.read()
        except UnicodeDecodeError:
            with open(os.path.join(config_dir, fname), encoding="latin-1") as handle:
                text = handle.read()
        for match in CONFIG_RE.finditer(text):
            # The opening paren is SET's own, before the matched name.
            end = _balanced_end(text, match.start() + match.group(0).index("("))
            if end is None:
                continue
            block = text[match.end():end]
            for name, path in re.findall(r"^\s*(\S+)\s+(\S+)\s*$", block, re.M):
                path = path.replace("\\", "/")
                key = (name, path)
                result.setdefault(key, set()).add(build_target)
    return {k: sorted(v) for k, v in result.items()}


def enable_for(configs, target, dirname):
    """'; '.join of the build targets that include (target, dir)."""
    hits = configs.get((target, dirname))
    return "; ".join(hits) if hits else "not-in-config"


if __name__ == "__main__":
    sys.exit(main())
