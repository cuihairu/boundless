#!/usr/bin/env python3
"""Extract the BigWorld Make component inventory (BW-00-01-01).

Parses every Makefile.rules under the reference `programming/` tree and
emits one record per declared component: name, type, output dir, the
source list, the components it depends on, and the feature switches
(usePython / useOpenSSL / ...). The goal is an *exactly* complete
inventory -- the acceptance criterion is that every binary and library the
Linux Make build declares appears here with its owning rule.

Run:
    python3 make_inventory.py [--root <programming dir>] [--json out.json]

The parser only reads top-level `key = value`, `key := value` and
`key += value` assignments (with GNU make line continuations) plus the
`$(call includeMakefilesFrom, subdirs)` aggregation lines. It is
intentionally dumb: BigWorld's Makefiles are declarative, so a flat parse
is both complete and easy to audit.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
from pathlib import Path

# Feature switches that gate third-party components / code paths.
USE_FLAGS = (
    "usePython",
    "useSystemPython",
    "useOpenSSL",
    "useMySQL",
    "useMongoDB",
    "useSQLite",
    "usePyURLRequest",
    "useCurl",
    "usePNG",
    "useCustomBillingSystem",
    "useBigWorld",
)

# Plain assignments: `name := value`, `name = value`, `name += value`.
ASSIGN = re.compile(r"^\s*([A-Za-z_][A-Za-z0-9_]*)\s*(\+?=|:=)\s*(.*)$")

# Aggregation of a subdirectory's Makefile.
INCLUDE_SUB = re.compile(
    r"\$\(call\s+includeMakefilesFrom\s*,\s*(\$\([a-zA-Z]+\)|\S+)\s*\)"
)


def _continuation(lines: list[str]) -> list[str]:
    """Join GNU make backslash continuations into single logical lines."""
    joined: list[str] = []
    buf = ""
    for raw in lines:
        line = raw.rstrip("\n")
        if line.endswith("\\"):
            buf += line[:-1] + " "
            continue
        joined.append(buf + line)
        buf = ""
    if buf:
        joined.append(buf)
    return joined


def parse_rules_file(path: Path) -> dict:
    """Return one record for a single Makefile.rules (or None if a bare
    subdirectory aggregator with no name/type)."""
    text = path.read_text(errors="replace")
    assigns: dict[str, list[str]] = {}
    aggregators: list[str] = []

    for line in _continuation(text.splitlines()):
        if line.lstrip().startswith("#"):
            continue
        m = INCLUDE_SUB.search(line)
        if m:
            aggregators.append(m.group(1).strip())
            continue
        m = ASSIGN.match(line)
        if m:
            key, _op, value = m.group(1), m.group(2), m.group(3).strip()
            assigns.setdefault(key, []).append(value)

    name = assigns.get("name", [""])[0]
    type_ = assigns.get("type", [""])[0]

    if not name and not type_:
        # Subdirectory aggregator: `subdirs = a b c`. Record the dir.
        return {"kind": "subdirs", "dir": "", "subdirs": assigns.get("subdirs", [])}

    def field(*keys: str) -> str:
        for k in keys:
            if k in assigns:
                return " ".join(assigns[k])
        return ""

    record: dict = {
        "kind": "component",
        "name": name,
        "type": type_,
        "dir": "",
        "bindir": field("bindir"),
        "build_time_file": field("buildTimeFile"),
        "cxxSource": field("cxxSource", "cxxSource_$(binName)"),
        "dependsOn": field("dependsOn"),
        "postBuildTargets": field("postBuildTargets"),
        "use_flags": sorted(
            f for f in USE_FLAGS if field(f) == "1"
        ),
        "aggregators": aggregators,
    }
    return record


def build(root: Path) -> list[dict]:
    """Scan every `Makefile*` (rules and the non-standard companions such as
    `Makefile.serviceapp` / `Makefile.message_logger`). `Makefile.rules`
    files recurse through their `subdirs`; the companions declare extra
    targets (copy_of_binary, extra binaries/shared modules) that the
    component framework does not funnel through a `Makefile.rules`."""
    seen: set[str] = set()
    records: list[dict] = []

    def add(record: dict, path: Path) -> None:
        record["path"] = str(path.relative_to(root))
        if record["kind"] == "component":
            record["dir"] = str(path.parent.relative_to(root))
        records.append(record)

    for path in sorted(root.rglob("Makefile.rules")):
        record = parse_rules_file(path)
        add(record, path)
        if record["kind"] == "component" and record["name"]:
            seen.add(record["name"])

    # Companion makefiles: declare name/type outside Makefile.rules. Avoid
    # re-recording an aggregator Makefile.rules (no name) and dedupe names.
    for path in sorted(root.rglob("Makefile.*")):
        if path.name == "Makefile.rules":
            continue
        record = parse_rules_file(path)
        if record["kind"] == "component" and record["name"] and record["name"] not in seen:
            seen.add(record["name"])
            add(record, path)
    return records


def default_reference_root() -> str:
    """The reference tree, environment-declared like ``ci/parity.sh``.

    ``CI_PARITY_REFERENCE`` / ``BW_REFERENCE`` name a tree root (the same
    variables parity.sh reads); the home-relative checkout is the last
    resort. No single machine's absolute path is baked in (BW-00-03-04):
    each environment points at its own copy of the original artifacts, so
    two environments never resolve one another's tree.
    """
    override = os.environ.get("CI_PARITY_REFERENCE") or os.environ.get("BW_REFERENCE")
    tree = Path(override) if override else Path.home() / "workspaces" / "BigWorld"
    return str(tree / "programming")


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument(
        "--root",
        default=default_reference_root(),
        help="the reference programming/ tree (default: the BW_REFERENCE / "
        "CI_PARITY_REFERENCE tree, or ~/workspaces/BigWorld)",
    )
    ap.add_argument("--json", default="", help="optional JSON output path")
    args = ap.parse_args()

    root = Path(args.root)
    if not root.is_dir():
        print(f"not a directory: {root}", file=sys.stderr)
        return 2

    records = build(root)

    components = [r for r in records if r["kind"] == "component"]
    by_type: dict[str, int] = {}
    for r in components:
        by_type[r["type"]] = by_type.get(r["type"], 0) + 1

    print(f"Makefile.rules scanned : {len(records)}")
    print(f"components declared    : {len(components)}")
    for t in sorted(by_type):
        print(f"  type {t:20s}: {by_type[t]}")
    print()
    for r in components:
        print(f"[{r['type']:12s}] {r['name']:28s} {r['dir'] or '.'}")

    if args.json:
        Path(args.json).write_text(
            json.dumps(records, indent=2, ensure_ascii=False)
        )
        print(f"\nwrote {args.json}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
