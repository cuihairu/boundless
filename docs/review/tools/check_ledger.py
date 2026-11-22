#!/usr/bin/env python3
"""BW-00-05-06 — verify a coverage ledger is complete.

The ledger (build_ledger.py) indexes every original build target. This checker
enforces the acceptance of BW-00-05-01 (every row carries the original target,
its enable condition, the owning feature id, the fixture and the status) and of
BW-00-05-06 (every enabled target has run evidence or an explicit gap).

    check_ledger.py <ledger.tsv>
    check_ledger.py <ledger.tsv> --regenerate <reference_tree>

The second mode also rebuilds the ledger from the reference tree and fails when
it differs — the committed artifact must be the one the generator produces, so
the index cannot silently drift. Exits 0 when the ledger is complete and, in
regenerate mode, byte-identical; 1 otherwise.
"""

import csv
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import build_ledger  # noqa: E402

HEADER = ["target", "type", "dir", "enable_condition", "feature_id",
          "fixture", "status"]
STATUSES = ("covered", "partial", "gap", "not-enabled")


def _problems(rows):
    """Yield one message per violation."""
    if rows[0] != HEADER:
        yield f"header is {rows[0]!r}, expected {HEADER!r}"
    data = rows[1:]
    if not data:
        yield "the ledger has no rows"
        return
    seen = set()
    for n, row in enumerate(data, 2):
        if len(row) != len(HEADER):
            yield f"row {n} has {len(row)} fields, expected {len(HEADER)}"
            continue
        target, kind, dirname, enable, task, fixture, status = row
        for field, value in (("target", target), ("dir", dirname),
                             ("enable_condition", enable),
                             ("feature_id", task), ("fixture", fixture),
                             ("status", status)):
            if value == "":
                yield f"row {n} ({dirname}/{target}): empty {field}"
        if status not in STATUSES:
            yield f"row {n} ({dirname}/{target}): unknown status {status!r}"
        key = (dirname, target)
        if key in seen:
            yield f"duplicate row for {dirname}/{target}"
        seen.add(key)
        if status in ("covered", "partial"):
            if not fixture or fixture.startswith("gap:"):
                yield f"row {n} ({dirname}/{target}): {status} needs run evidence"
        elif status == "gap":
            if not fixture.startswith("gap: "):
                yield f"row {n} ({dirname}/{target}): a gap records its blocker"
        elif status == "not-enabled":
            if task != "-" or fixture != "-":
                yield (f"row {n} ({dirname}/{target}): not-enabled has no "
                       "porting task or fixture ('-')")
    # BW-00-05-06: every enabled target has evidence or an explicit gap. An
    # area is off the Linux build only for the always-off areas or when no
    # Linux configuration includes it; everything else must be covered,
    # partial, or a recorded gap.
    for n, row in enumerate(data, 2):
        if len(row) != len(HEADER):
            continue
        dirname, enable, status = row[2], row[3], row[6]
        area = build_ledger._area(dirname)
        if area in build_ledger.ALWAYS_OFF:
            continue
        if build_ledger._linux_enabled(enable) and status not in (
                "covered", "partial", "gap"):
            yield (f"row {n} ({dirname}/{row[0]}): enabled in a Linux build "
                   f"but {status} — needs evidence or an explicit gap")


def main():
    if len(sys.argv) not in (2, 4):
        print(__doc__, file=sys.stderr)
        return 2
    ledger_path = sys.argv[1]
    regenerate = None
    if len(sys.argv) == 4:
        if sys.argv[2] != "--regenerate":
            print(f"unknown option {sys.argv[2]!r}", file=sys.stderr)
            return 2
        regenerate = sys.argv[3]
    if not os.path.isfile(ledger_path):
        print(f"no ledger at {ledger_path}", file=sys.stderr)
        return 2

    with open(ledger_path, encoding="utf-8") as handle:
        rows = list(csv.reader(handle, delimiter="\t"))

    problems = list(_problems(rows))
    if problems:
        print(f"ledger {ledger_path} is incomplete:", file=sys.stderr)
        for problem in problems:
            print(f"  - {problem}", file=sys.stderr)
        return 1

    if regenerate is not None:
        if not os.path.isdir(regenerate):
            print(f"no reference tree at {regenerate}", file=sys.stderr)
            return 2
        import subprocess
        fresh = subprocess.run(
            [sys.executable,
             os.path.join(os.path.dirname(os.path.abspath(__file__)),
                          "build_ledger.py"),
             regenerate],
            capture_output=True, text=True, check=True).stdout
        committed = open(ledger_path, encoding="utf-8").read()
        if fresh != committed:
            print("ledger differs from a fresh build of the reference tree",
                  file=sys.stderr)
            return 1

    print(f"ledger {ledger_path} is complete "
          f"({len(rows) - 1} targets)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
