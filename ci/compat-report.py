#!/usr/bin/env python3
"""BW-00-04-06: the compatibility differential report.

The comparison lane (BW-00-04-04) records, per compatibility boundary, the
original and new values and whether they agree. This report consumes that
record and locates every difference by kind — a field value, a timeout, or an
ordering — so a reader can find each one instead of staring at a flat diff.

Each non-agreeing boundary is one row of the report, classified:

  * order   — the boundary is about the sequence of events (callback order).
  * timeout — the boundary or its note is about a timeout value.
  * field   — anything else: a message field, a length parameter, a seed.

Classification is name-driven, so a boundary that changes kind as the port
grows is still located by its own text. A boundary that agrees has no
difference to locate and is not listed.

Usage:
  ci/compat-report.py SUMMARY_JSON OUT_DIR [--self-check]

Exits 2 when the summary it needs is missing; --self-check exits 3 when a
hollow report (no rows, or a difference without a category) is produced.
"""

import json
import os
import sys

CATEGORIES = ("timeout", "order", "field")


def classify(boundary, note):
    """Classify a boundary's difference by its own name and note."""
    text = ("%s %s" % (boundary, note or "")).lower()
    if "timeout" in text or "timed out" in text:
        return "timeout"
    if "order" in text or "sequence" in text:
        return "order"
    return "field"


def load(summary_path):
    if not os.path.isfile(summary_path):
        return None
    with open(summary_path) as f:
        return json.load(f)


def emit(summary, out_dir, self_check=False):
    rows = summary.get("comparison") if isinstance(summary, dict) else None
    if not rows:
        sys.stderr.write("no comparison rows in the summary — nothing to locate\n")
        sys.exit(2)

    located = []
    for row in rows:
        if row.get("agree"):
            continue
        located.append({
            "category": classify(row.get("boundary", ""), row.get("note", "")),
            "boundary": row.get("boundary", ""),
            "original": row.get("original"),
            "new": row.get("new"),
            "note": row.get("note", ""),
        })

    if self_check:
        # Zero differences is a clean comparison, not a hollow report — only a
        # malformed row (no category or boundary) is hollow.
        problems = []
        for r in located:
            if not r["category"]:
                problems.append("a difference with no category")
            if not r["boundary"]:
                problems.append("a difference with no boundary")
        if problems:
            sys.stderr.write("hollow report: %s\n" % "; ".join(problems))
            sys.exit(3)

    os.makedirs(out_dir, exist_ok=True)
    counts = {c: sum(1 for r in located if r["category"] == c) for c in CATEGORIES}
    doc = {
        "differences": located,
        "counts": counts,
        "located": len(located),
        "source": "compare-summary.json (BW-00-04-04)",
    }
    with open(os.path.join(out_dir, "compat-report.json"), "w") as f:
        json.dump(doc, f, indent=2)
        f.write("\n")
    with open(os.path.join(out_dir, "compat-report.tsv"), "w") as f:
        f.write("category\tboundary\toriginal\tnew\tnote\n")
        for r in located:
            f.write("%s\t%s\t%s\t%s\t%s\n" % (
                r["category"], r["boundary"],
                json.dumps(r["original"], ensure_ascii=False),
                json.dumps(r["new"], ensure_ascii=False),
                r["note"].replace("\t", " ").replace("\n", " ")))
    return doc


if __name__ == "__main__":
    args = sys.argv[1:]
    self_check = "--self-check" in args
    if self_check:
        args.remove("--self-check")
    summary_path = args[0] if args else "tests/baseline/compare-summary.json"
    out_dir = args[1] if len(args) > 1 else "."
    summary = load(summary_path)
    if summary is None:
        sys.stderr.write("summary not found: %s\n" % summary_path)
        sys.exit(2)
    doc = emit(summary, out_dir, self_check)
    print("differences=%d located (timeout=%d order=%d field=%d)" % (
        doc["located"], doc["counts"]["timeout"],
        doc["counts"]["order"], doc["counts"]["field"]))
