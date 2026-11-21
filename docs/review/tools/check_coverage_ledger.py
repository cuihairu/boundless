#!/usr/bin/env python3
"""Validate the BW-00-05 coverage ledger (BW-00-05-01).

The ledger maps every reference build target to a Boundless task, its
verification fixture and its coverage status. One TSV row per target:

    target   reference component name (Makefile.rules `name` / CMake target)
    type     component kind (see TYPE vocab below)
    dir      reference source dir, relative to programming/ (or `-`)
    origin   where the row was registered from: make|cmake|release|resource|gap
    enable   enable condition: `-` (unconditional) or comma-joined feature
             tokens (usePython / useOpenSSL / ... or BW_* CMake macros)
    task     todo item(s): `BW-xx[-yy[-zz]]` comma-joined, `pending`, or `n/a`
    fixture  repo-relative path(s) to the runnable acceptance or the record
             that evidences coverage (`-` when none yet)
    status   covered|partial|pending|blocked|excluded
    note     free text; required when status is blocked/excluded (`-`)

Every `origin=make` row is cross-checked against a fresh `make_inventory`
run: each declared component (by name+dir) appears exactly once, and no
`make` row invents a component the inventory does not declare. Rows with
another origin are exempt from the inventory match (they register the
CMake/Windows surface, release artifacts, resource entries or gaps).

Run:
    python3 check_coverage_ledger.py [--schema-only] [--ledger path]
    python3 check_coverage_ledger.py --scaffold   # list unregistered components

`--scaffold` prints the inventory components missing from the ledger (one
TSV row each, all fields at their pending defaults) so a later mapping
item can append them; it does not write anything.
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

TOOLS_DIR = Path(__file__).resolve().parent
REPO_ROOT = TOOLS_DIR.parent.parent.parent
sys.path.insert(0, str(TOOLS_DIR))
import make_inventory  # noqa: E402  (same-tool sibling)

DEFAULT_LEDGER = TOOLS_DIR.parent / "data" / "bw00-05-coverage-ledger.tsv"

COLUMNS = ("target", "type", "dir", "origin", "enable", "task",
           "fixture", "status", "note")

ORIGIN = ("make", "cmake", "release", "resource", "gap")
STATUS = ("covered", "partial", "pending", "blocked", "excluded")
# The Make component kinds (BW-00-01-01): 6 declared types + the one-off
# copy (remove_db) which the inventory records with an empty type.
MAKE_TYPES = {"binary", "library", "shared_library", "unit_test",
              "copy_of_binary", "oneoff"}

_TASK = r"BW-\d{2}(-\d{2}){0,2}"
_ENABLE = r"[A-Za-z_][A-Za-z0-9_]*"


def _is_task(value: str) -> bool:
    import re

    for part in value.split(","):
        part = part.strip()
        if part in ("pending", "n/a"):
            continue
        if not re.fullmatch(_TASK, part):
            return False
    return True


def _is_enable(value: str) -> bool:
    import re

    if value == "-":
        return True
    for tok in value.split(","):
        if not re.fullmatch(_ENABLE, tok.strip()):
            return False
    return True


def _is_fixture(value: str) -> bool:
    return value == "-" or all(p.strip() for p in value.split(","))


def parse_ledger(path: Path) -> tuple[list[dict], list[str]]:
    """Return (rows, errors). `rows` preserve insertion order."""
    rows: list[dict] = []
    errors: list[str] = []
    if not path.exists():
        return rows, [f"ledger not found: {path}"]
    for lineno, line in enumerate(path.read_text().splitlines(), 1):
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        fields = line.split("\t")
        if lineno == 1:
            if fields != list(COLUMNS):
                errors.append(f"line 1: header {fields} != {list(COLUMNS)}")
            continue
        if len(fields) != len(COLUMNS):
            errors.append(f"line {lineno}: {len(fields)} fields, "
                          f"want {len(COLUMNS)}: {line!r}")
            continue
        rows.append(dict(zip(COLUMNS, fields)))
    return rows, errors


def validate(rows: list[dict], ledger: Path) -> list[str]:
    errors: list[str] = []
    seen: dict[tuple[str, str], int] = {}
    for i, r in enumerate(rows, 1):
        where = f"row {i} ({r['target']} {r['dir']})"
        if any(not r[c] for c in COLUMNS):
            errors.append(f"{where}: empty field "
                          f"(use `-` for no value)")
        if r["origin"] not in ORIGIN:
            errors.append(f"{where}: origin {r['origin']!r} not in {ORIGIN}")
        if r["status"] not in STATUS:
            errors.append(f"{where}: status {r['status']!r} not in {STATUS}")
        if not _is_task(r["task"]):
            errors.append(f"{where}: task {r['task']!r} not "
                          f"BW-xx[-yy[-zz]]/pending/n/a (comma-joined)")
        if not _is_enable(r["enable"]):
            errors.append(f"{where}: enable {r['enable']!r} not `-` or "
                          f"comma-joined feature tokens")
        if not _is_fixture(r["fixture"]):
            errors.append(f"{where}: fixture has an empty path in "
                          f"{r['fixture']!r}")
        if r["origin"] == "make" and r["type"] not in MAKE_TYPES:
            errors.append(f"{where}: make type {r['type']!r} not in "
                          f"{sorted(MAKE_TYPES)}")
        if r["status"] in ("blocked", "excluded") and r["note"] == "-":
            errors.append(f"{where}: {r['status']} requires a note")
        if r["status"] in ("covered", "partial") and r["fixture"] == "-":
            errors.append(f"{where}: {r['status']} requires a fixture")
        # The fixture must be a real repo-relative path (when given).
        if r["fixture"] != "-":
            for f in r["fixture"].split(","):
                f = f.strip()
                if not (REPO_ROOT / f).exists():
                    errors.append(f"{where}: fixture path not found: {f}")
        key = (r["target"], r["dir"])
        seen[key] = seen.get(key, 0) + 1
    dupes = [k for k, n in seen.items() if n > 1]
    if dupes:
        errors.append(f"duplicate (target, dir) rows: {dupes}")
    return errors


def inventory_components(root: Path) -> list[dict]:
    return [r for r in make_inventory.build(root) if r["kind"] == "component"]


def inventory_match(rows: list[dict], root: Path) -> tuple[list[str], list[dict]]:
    """Cross-check origin=make rows against the Make inventory."""
    errors: list[str] = []
    comp = inventory_components(root)
    inv_keys = {(c["name"], c["dir"]) for c in comp}
    make_keys = {(r["target"], r["dir"]) for r in rows if r["origin"] == "make"}

    for c in comp:
        key = (c["name"], c["dir"])
        if key not in make_keys:
            errors.append(f"inventory component not in ledger: "
                          f"{c['name']} {c['dir']}")
    for key in sorted(make_keys - inv_keys):
        errors.append(f"make row not in inventory: {key[0]} {key[1]}")

    # Type agreement for make rows.
    inv_by_key = {(c["name"], c["dir"]): c for c in comp}
    for r in rows:
        if r["origin"] != "make":
            continue
        c = inv_by_key.get((r["target"], r["dir"]))
        if c is None:
            continue
        want = c["type"] or "oneoff"
        if r["type"] != want:
            errors.append(f"row {r['target']} {r['dir']}: type {r['type']!r} "
                          f"!= inventory {want!r}")
    return errors, comp


def scaffold(rows: list[dict], root: Path) -> list[dict]:
    """Rows the ledger still lacks (inventory components, pending defaults)."""
    have = {(r["target"], r["dir"]) for r in rows}
    out = []
    for c in inventory_components(root):
        if (c["name"], c["dir"]) in have:
            continue
        out.append({
            "target": c["name"],
            "type": c["type"] or "oneoff",
            "dir": c["dir"],
            "origin": "make",
            "enable": ",".join(c["use_flags"]) or "-",
            "task": "pending",
            "fixture": "-",
            "status": "pending",
            "note": "-",
        })
    return out


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--ledger", default=str(DEFAULT_LEDGER))
    ap.add_argument("--root", default=make_inventory.default_reference_root())
    ap.add_argument("--schema-only", action="store_true",
                    help="skip the make_inventory cross-check")
    ap.add_argument("--scaffold", action="store_true",
                    help="print inventory components missing from the ledger")
    args = ap.parse_args()

    ledger = Path(args.ledger)
    rows, errors = parse_ledger(ledger)

    if args.scaffold:
        if not Path(args.root).is_dir():
            print(f"reference tree not a directory: {args.root}", file=sys.stderr)
            return 2
        for r in scaffold(rows, Path(args.root)):
            print("\t".join(r[c] for c in COLUMNS))
        return 0

    errors += validate(rows, ledger)
    if not args.schema_only:
        if not Path(args.root).is_dir():
            errors.append(f"reference tree not a directory: {args.root} "
                          f"(pass --schema-only to skip the inventory "
                          f"cross-check)")
        else:
            inv_errors, comp = inventory_match(rows, Path(args.root))
            errors += inv_errors
            print(f"inventory components checked : {len(comp)}")

    if errors:
        for e in errors:
            print(f"ERROR {e}", file=sys.stderr)
        print(f"ledger has {len(rows)} rows, {len(errors)} error(s)",
              file=sys.stderr)
        return 1

    print(f"ledger OK: {len(rows)} rows, all fields valid")
    return 0


if __name__ == "__main__":
    sys.exit(main())
