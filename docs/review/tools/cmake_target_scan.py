#!/usr/bin/env python3
"""Scan the reference BigWorld CMake build for targets (BW-00-01-02).

The root CMakeLists.txt picks a project from BW_CMAKE_TARGET and then includes
build/cmake/BWConfiguration_<target>.cmake, which fills BW_LIBRARY_PROJECTS /
BW_BINARY_PROJECTS and the compile defines. This script parses every
BWConfiguration_*.cmake, resolves the per-file variable references, and records
which libraries and executables each target configures, together with the make
conditional that gates them.

Usage: cmake_target_scan.py BIGWORLD_ROOT [OUTPUT_JSON]
"""
import json
import os
import re
import sys

COMMAND_RE = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")
PROJECT_VARS = ("BW_LIBRARY_PROJECTS", "BW_BINARY_PROJECTS")


def tokenize(text):
    """Split a cmake file into (command, args-text) pairs with nested parens."""
    tokens = []
    i = 0
    n = len(text)
    while i < n:
        m = COMMAND_RE.match(text, i)
        if not m:
            i += 1  # skip one char, whatever it is
            continue
        name = m.group(0)
        p = m.end()
        while p < n and text[p].isspace():
            p += 1
        if p >= n or text[p] != "(":
            # a bare token (e.g. `ENDIF`), no parenthesized arguments
            tokens.append((name, ""))
            i = m.end()
            continue
        j = p
        # balanced paren scan
        depth = 0
        p = j
        while p < n:
            if text[p] == "(":
                depth += 1
            elif text[p] == ")":
                depth -= 1
                if depth == 0:
                    break
            p += 1
        args = text[j + 1 : p]
        tokens.append((name, args))
        i = p + 1
    return tokens


def strip_comments(body):
    return "\n".join(line.split("#", 1)[0] for line in body.splitlines())


def parse_set_body(body):
    """Return the argument words of a SET(var body...) body, comments removed."""
    words = []
    for line in strip_comments(body).splitlines():
        words.extend(line.split())
    return words


def resolve(words, variables):
    """Substitute ${VAR} references with their defined words."""
    out = []
    for w in words:
        m = re.fullmatch(r"\$\{(\w+)\}", w)
        out.extend(variables.get(m.group(1), [w]) if m else [w])
    return out


def pairs(words):
    """Group a flat word list into (name, path) pairs, dropping odd stragglers."""
    out = []
    i = 0
    while i + 1 < len(words):
        out.append((words[i], words[i + 1]))
        i += 2
    if i < len(words):
        out.append((words[i], ""))
    return out


def parse_file(path):
    with open(path, encoding="utf-8", errors="replace") as fh:
        text = fh.read()
    tokens = tokenize(text)
    defines = []
    flags = {}
    variables = {}
    projects = []  # (kind, name, path, guard)
    consumer_release = False
    includes = []

    stack = []  # (index of list of guard tokens, guard text)

    def current_guard():
        return "; ".join(g for g in stack if g) if stack else ""

    def collect_projects(kind, var, body, guard):
        words = parse_set_body(body)
        # `SET(var ...)` has the variable name as its first word, and an
        # append `SET(var ${var} ...)` re-states it plus a self-placeholder;
        # drop both instead of expanding the placeholder (which would
        # duplicate the accumulated list and shift the pairs).
        words = [w for w in words if w not in (var, "${%s}" % var)]
        words = resolve(words, variables)
        for name, path in pairs(words):
            projects.append((kind, name, path, guard))

    for name, body in tokens:
        upper = name.upper()
        if upper == "ADD_DEFINITIONS":
            for w in parse_set_body(body):
                if w.startswith("-D"):
                    defines.append(w[2:])
        elif upper == "BW_ADD_CONSUMER_RELEASE_CONFIG":
            consumer_release = True
        elif upper == "INCLUDE":
            w = parse_set_body(body)
            if w:
                includes.append(w[0])
        elif upper == "IF":
            cond = strip_comments(body).strip().strip("()")
            stack.append(cond.upper())
        elif upper == "ENDIF":
            if stack:
                stack.pop()
        elif upper == "SET":
            guard = current_guard()
            w = parse_set_body(body)
            if not w:
                continue
            var = w[0].upper()
            if var in PROJECT_VARS:
                collect_projects(var, var, body, guard)
            elif len(w) > 1:
                variables.setdefault(var, []).extend(w[1:])
                if re.fullmatch(r"(BW_[A-Z0-9_]+_SUPPORT|USE_[A-Z0-9_]+)", var):
                    flags[var] = w[1]
    return {
        "defines": defines,
        "flags": flags,
        "projects": projects,
        "consumer_release": consumer_release,
        "includes": includes,
        "variables": variables,
    }


def scan(root):
    cmake_dir = os.path.join(root, "build", "cmake")
    targets = {}
    for fn in sorted(os.listdir(cmake_dir)):
        m = re.fullmatch(r"BWConfiguration_(\w+)\.cmake", fn)
        if not m:
            continue
        targets[m.group(1)] = parse_file(os.path.join(cmake_dir, fn))
    return targets


def main():
    if len(sys.argv) < 2:
        print(__doc__)
        return 1
    root = sys.argv[1]
    targets = scan(root)
    if len(sys.argv) >= 3:
        with open(sys.argv[2], "w", encoding="utf-8") as fh:
            json.dump(targets, fh, indent=1, sort_keys=True)
    for name in sorted(targets):
        t = targets[name]
        libs = [p for p in t["projects"] if p[0] == "BW_LIBRARY_PROJECTS"]
        bins = [p for p in t["projects"] if p[0] == "BW_BINARY_PROJECTS"]
        print(
            f"{name:16s} libs={len(libs):3d} bins={len(bins):3d} "
            f"consumer_release={t['consumer_release']} defines={t['defines']}"
        )
    return 0


if __name__ == "__main__":
    sys.exit(main())
