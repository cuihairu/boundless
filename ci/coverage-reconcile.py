#!/usr/bin/env python3
"""BW-00-05-06: every enabled target has evidence or an explicit gap.

The ledger (BW-00-05-01) enumerates the reference's build targets and the
mappers (BW-00-05-02/03/04) give each one an index: a port crate that
implements it, or the BW-xx ticket whose source scope owns it. This lane
closes the ledger — it joins the two and proves the acceptance every target
meets: a runnable acceptance (a port index) or a traceable blocking reason (an
explicit gap). A target with neither is the record this lane exists to make
visible.

  * evidence — the port crate (mapped=True), or the ticket scope (a BW-xx);
  * gap — a bare target (no crate, no ticket), recorded with a curated
    blocking reason so the ledger has no silent hole.

Usage:
  ci/coverage-reconcile.py LEDGER MAPS_DIR OUT_DIR [--self-check]

Exits 2 when the ledger or the maps are missing; --self-check exits 3 when a
ledger row has neither evidence nor a gap, or a bare target has no curated
blocking reason.
"""

import json
import os
import sys

MAP_TAGS = ("BW000502", "BW000503", "BW000504")

# Curated blocking reasons for bare targets: why a target has neither a port
# crate nor a ticket, and what turns it on. urlrequest is the only bare target
# today — no port crate ships an HTTP/curl binding and no BW-xx source scope
# names lib/urlrequest (BW-02 owns lib/network and lib/connection, BW-01 owns
# lib/resmgr), so it is recorded as an explicit gap for the plan to scope.
GAP_REASON = {
    "urlrequest": "no port crate ships an HTTP/curl binding and no BW-xx"
                  " source scope names lib/urlrequest — recorded as an"
                  " explicit gap for the plan to scope (BW-02 owns"
                  " lib/network/lib/connection, BW-01 owns lib/resmgr)",
}


def ledger_rows(path):
    if not os.path.isfile(path):
        return None
    with open(path, encoding="utf-8") as f:
        return json.load(f)["rows"]


def map_rows(maps_dir):
    """{original_path: row} across the three category maps (first match wins)."""
    rows = {}
    for tag in MAP_TAGS:
        for cand in (os.path.join(maps_dir, "coverage-map-%s" % tag),
                     os.path.join(maps_dir, "coverage-map-%s.json" % tag)):
            path = cand if cand.endswith(".json") else os.path.join(cand, "coverage-map.json")
            if os.path.isfile(path):
                with open(path, encoding="utf-8") as f:
                    for r in json.load(f)["rows"]:
                        rows.setdefault(r["original_path"], r)
                break
    return rows


def reconcile(rows, maps):
    """One row per ledger target: evidence (crate/ticket) or an explicit gap."""
    out = []
    for r in rows:
        name = r["original_path"].split(" (", 1)[0]
        m = maps.get(r["original_path"])
        evidence = "none"
        if m and m.get("index") and m["index"] != "none yet":
            evidence = "crate"
        elif m and m.get("ticket") and m["ticket"] != "none":
            evidence = "ticket"
        gap = ""
        if evidence == "none":
            # A bare target needs a curated blocking reason — a generic
            # fallback would make the gap untraceable, which is the hole this
            # lane exists to close.
            gap = GAP_REASON.get(name, "")
        out.append({
            "original_path": r["original_path"],
            "kind": r["kind"],
            "evidence": evidence,
            "gap": gap,
            "resolved": evidence != "none",
        })
    return out


def emit(ledger_path, maps_dir, out_dir, self_check=False):
    rows = ledger_rows(ledger_path)
    if rows is None:
        sys.stderr.write("missing ledger: no coverage ledger at %s\n" % ledger_path)
        sys.exit(2)
    maps = map_rows(maps_dir)
    if not maps:
        sys.stderr.write("missing maps: no coverage maps under %s\n" % maps_dir)
        sys.exit(2)

    out = reconcile(rows, maps)
    unmapped = sum(1 for r in out if r["resolved"])
    gaps = sum(1 for r in out if not r["resolved"])

    if self_check:
        problems = []
        for r in out:
            if not r["resolved"] and not r["gap"]:
                problems.append("%s has neither evidence nor a gap"
                                % r["original_path"])
        if unmapped == 0:
            problems.append("no ledger row resolved")
        if problems:
            sys.stderr.write("hollow map: %s\n" % "; ".join(problems))
            sys.exit(3)

    os.makedirs(out_dir, exist_ok=True)
    with open(os.path.join(out_dir, "coverage-reconcile.json"), "w") as f:
        json.dump({"rows": out, "count": len(out), "evidence": unmapped,
                   "gaps": gaps,
                   "schema": ["original_path", "kind", "evidence", "gap",
                              "resolved"]}, f, indent=2)
        f.write("\n")
    with open(os.path.join(out_dir, "coverage-reconcile.tsv"), "w") as f:
        f.write("original_path\tkind\tevidence\tgap\n")
        for r in out:
            f.write("%s\t%s\t%s\t%s\n" % (
                r["original_path"], r["kind"], r["evidence"], r["gap"]))
    return out, unmapped, gaps


if __name__ == "__main__":
    args = sys.argv[1:]
    self_check = "--self-check" in args
    if self_check:
        args.remove("--self-check")
    if len(args) < 3:
        sys.stderr.write("usage: coverage-reconcile.py LEDGER MAPS_DIR"
                         " OUT_DIR [--self-check]\n")
        sys.exit(2)
    out, unmapped, gaps = emit(args[0], args[1], args[2], self_check)
    print("reconciled=%d (evidence=%d, gap=%d)" % (len(out), unmapped, gaps))
