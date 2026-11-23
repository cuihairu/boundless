#!/usr/bin/env python3
"""Report the differences between two captured evidence runs (BW-00-04-06).

The differ is generic over *record sequences*: both the callback fixture
(`callbacks` with order/name/args/ignorable) and the stack comparison
(`roles` with role/rc/timed_out) normalise to a common record shape
`{key, order, value, ignorable, timed_out}`, so one differ reports all three
difference classes:

  * field - the record's value (args / rc) differs between the two runs and
    the record is not marked ignorable (an ignorable record that differs is
    reported separately as `ignored`, i.e. the expected-to-differ fields);
  * timeout - one side timed out (timed_out differs);
  * ordering - the records are present in both runs but their relative order
    differs (the divergence point is reported).

Each difference carries its record key, so a reader can locate it.

Usage: diff_report.py REF_JSON NEW_JSON [--out OUT_JSON]
"""
import json
import sys


def load_records(source):
    """Normalise an evidence file to the common record shape (path or dict)."""
    d = json.load(open(source, encoding="utf-8")) \
        if isinstance(source, (str, bytes)) else source
    if "callbacks" in d:
        return [{"key": c["name"], "order": c["order"], "value": c.get("args"),
                 "ignorable": c.get("ignorable") or [],
                 "timed_out": bool(c.get("timed_out", False))}
                for c in d["callbacks"]]
    if "roles" in d:
        def reach(r):
            # One comparable "how far this role got" value per side: the
            # reference side reports presence/steady marker, the new side
            # reports acceptance/exit code.
            if "marker_found" in r:
                if r["marker_found"]:
                    return "running"
                return "present" if r["process_present"] else "absent"
            if not r.get("accepted"):
                return "rejected"
            return "started" if r.get("rc") == 0 else "failed"
        return [{"key": r["role"], "order": i, "value": reach(r),
                 "ignorable": [],
                 "timed_out": bool(r.get("timed_out", False))}
                for i, r in enumerate(d["roles"])]
    raise ValueError("no callbacks/roles in %s" % source)


def diff(ref_recs, new_recs):
    """The three difference classes, each located by record key."""
    by_key = lambda recs: {r["key"]: r for r in recs}
    ref, new = by_key(ref_recs), by_key(new_recs)
    keys = list(ref.keys())

    field, ignored, timeout, ordering = [], [], [], []
    for key in keys:
        if key not in new:
            continue
        r, n = ref[key], new[key]
        if r["timed_out"] != n["timed_out"]:
            timeout.append({"key": key, "ref": r["timed_out"],
                            "new": n["timed_out"],
                            "ref_order": r["order"], "new_order": n["order"]})
        if r["value"] != n["value"]:
            entry = {"key": key, "ref": r["value"], "new": n["value"],
                     "ref_order": r["order"], "new_order": n["order"]}
            if r["ignorable"] or n["ignorable"]:
                ignored.append(entry)
            else:
                field.append(entry)

    # Ordering: records whose relative position differs between the runs.
    ref_seq = [r["key"] for r in ref_recs]
    new_seq = [r["key"] for r in new_recs]
    ref_pos = {k: i for i, k in enumerate(ref_seq)}
    new_pos = {k: i for i, k in enumerate(new_seq)}
    for k in ref_pos:
        if k in new_pos and ref_pos[k] != new_pos[k]:
            ordering.append({"key": k, "ref": ref_pos[k], "new": new_pos[k]})

    return {"field": sorted(field, key=lambda e: e["key"]),
            "ignored": sorted(ignored, key=lambda e: e["key"]),
            "timeout": sorted(timeout, key=lambda e: e["key"]),
            "ordering": sorted(ordering, key=lambda e: e["key"])}


def main():
    args = sys.argv[1:]
    out = None
    if "--out" in args:
        i = args.index("--out")
        out = args[i + 1]
        args = args[:i] + args[i + 2:]
    if len(args) < 2:
        print(__doc__)
        return 1
    ref, new = args[:2]
    report = diff(load_records(ref), load_records(new))
    if out:
        with open(out, "w", encoding="utf-8") as fh:
            json.dump(report, fh, indent=1, sort_keys=True)
    print("field %d   ignored %d   timeout %d   ordering %d" % (
        len(report["field"]), len(report["ignored"]),
        len(report["timeout"]), len(report["ordering"])))
    for e in report["field"]:
        print("  FIELD   %s  %r -> %r" % (e["key"], e["ref"], e["new"]))
    for e in report["ignored"]:
        print("  IGNORED %s  %r -> %r" % (e["key"], e["ref"], e["new"]))
    for e in report["timeout"]:
        print("  TIMEOUT %s  ref=%s new=%s" % (e["key"], e["ref"], e["new"]))
    for e in report["ordering"]:
        print("  ORDER   %s  ref#%d new#%d" % (e["key"], e["ref"], e["new"]))
    return 0


if __name__ == "__main__":
    sys.exit(main())
