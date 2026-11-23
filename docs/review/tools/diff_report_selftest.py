#!/usr/bin/env python3
"""Inject a field, a timeout and an ordering difference, each separately
locatable by `diff_report.py` (BW-00-04-06 acceptance).

Each case starts from the real BW-00-04-02 callback fixture and mutates one
copy, then asks the differ to locate the injected difference. A case passes
when the injected kind is reported and the other two kinds are not — so a
field injection is not mistaken for a timeout or an ordering change.

Usage: diff_report_selftest.py BASELINE_FIXTURE_JSON OUTPUT_JSON
"""
import copy
import json
import sys

sys.path.insert(0, __import__("os").path.dirname(__import__("os").path.abspath(__file__)))
from diff_report import load_records, diff


def case(baseline, mutate, injected_kind):
    ref = copy.deepcopy(baseline)
    new = copy.deepcopy(baseline)
    mutate(new["callbacks"])
    report = diff(load_records(ref), load_records(new))
    counts = {"field": len(report["field"]), "timeout": len(report["timeout"]),
              "ordering": len(report["ordering"])}
    ok = counts[injected_kind] >= 1 and \
        all(counts[k] == 0 for k in counts if k != injected_kind)
    return {"injected": injected_kind, "ok": ok, "counts": counts,
            "report": report}


def main():
    if len(sys.argv) < 3:
        print(__doc__)
        return 1
    baseline = json.load(open(sys.argv[1], encoding="utf-8"))
    out = sys.argv[2]

    def find(callbacks, name):
        return next(c for c in callbacks if c["name"] == name)

    # 1. field: a non-ignorable callback's args change (only its value differs).
    def inject_field(callbacks):
        c = find(callbacks, "Proxy::addCreateBasePlayerToChannelBundle")
        c["args"] = c["args"] + " (edited)"

    # 2. timeout: one callback times out on the new side only.
    def inject_timeout(callbacks):
        find(callbacks, "ClientAvatar.__init__")["timed_out"] = True

    # 3. ordering: two callbacks swap position in the sequence. The indices
    # must be precomputed: `l[i], l[l.index(x)] = ...` re-evaluates index(x)
    # after the first store and undoes the swap.
    def inject_ordering(callbacks):
        a = find(callbacks, "Proxy::enableEntities")
        b = find(callbacks, "Proxy::addCreateBasePlayerToChannelBundle")
        ia, ib = callbacks.index(a), callbacks.index(b)
        callbacks[ia], callbacks[ib] = callbacks[ib], callbacks[ia]

    cases = [case(baseline, inject_field, "field"),
             case(baseline, inject_timeout, "timeout"),
             case(baseline, inject_ordering, "ordering")]
    data = {"baseline": sys.argv[1], "cases": cases,
            "all_ok": all(c["ok"] for c in cases)}
    with open(out, "w", encoding="utf-8") as fh:
        json.dump(data, fh, indent=1, sort_keys=True)

    for c in cases:
        print("%-8s ok=%-5s %s" % (c["injected"], c["ok"], c["counts"]))
    print("all_ok=%s" % data["all_ok"])
    return 0 if data["all_ok"] else 1


if __name__ == "__main__":
    sys.exit(main())
