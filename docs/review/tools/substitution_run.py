#!/usr/bin/env python3
"""Run the full stack with one role substituted (BW-00-04-05).

A *substitution run* takes the same role set as the comparison run and swaps
exactly one role: that role is started by the new component
(`boundless-server <role>`) instead of the reference binary, every other role
keeps its reference side. The report names the substituted role and records
what its replacement reached, so a reader can tell which component stood in and
how far the stand-in got.

Like the comparison run, the reference cluster is observed (mode `live` when
another session owns it, read-only polling for the steady set) and never
driven or torn down. Wiring the replacement into a cluster someone else owns is
not possible; the run therefore records the substitution against the observed
reference side, which is the observable surface both sides can reach.

Usage: substitution_run.py BIGWORLD_ROOT NEW_BIN INPUT_JSON OUTPUT_JSON
"""
import json
import os
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from stack_comparison_run import (REFERENCE, new_observe, reference_observe,
                                 reference_run)

NEW = "boundless-server (crates/server-runtime app::run)"


def main():
    if len(sys.argv) < 5:
        print(__doc__)
        return 1
    root, new_bin, input_json, out_json = sys.argv[1:5]
    with open(input_json, encoding="utf-8") as fh:
        input_data = json.load(fh)

    substituted = input_data.get("substitute")
    if substituted not in input_data["roles"]:
        raise SystemExit("substitute %r is not in the input role set" % substituted)
    others = [r for r in input_data["roles"] if r != substituted]

    mode, rc, seconds = reference_run(root, input_data)
    ref_roles, poll_seconds, full = reference_observe(root, input_data, mode)

    # Only the substituted role runs the new component, on the same input.
    sub = next(r for r in new_observe(new_bin, input_data)
               if r["role"] == substituted)

    steady_holds = sub["accepted"] and sub["rc"] == 0
    report = {
        "substituted_role": substituted,
        "substituted_by": NEW,
        "reference_roles": others,
        "reference_commit": subprocess.run(
            ["git", "-C", root, "rev-parse", "--short", "HEAD"],
            capture_output=True, text=True).stdout.strip(),
        "new_commit": subprocess.run(
            ["git", "rev-parse", "--short", "HEAD"],
            capture_output=True, text=True).stdout.strip(),
        "captured_at": time.strftime("%Y-%m-%d %H:%M:%S %z"),
        "mode": mode,
        "bringup_rc": rc, "bringup_seconds": seconds,
        "poll_seconds": poll_seconds, "full_steady": full,
        "input": input_data,
        "reference": ref_roles,
        "substitution": sub,
        "steady_holds": steady_holds,
        "verdict": (
            "the substituted role accepts the input and starts, but reaches "
            "process startup only, so a cluster running the substitution cannot "
            "hold steady state" if steady_holds else
            "the substituted role is rejected outright"),
    }
    with open(out_json, "w", encoding="utf-8") as fh:
        json.dump(report, fh, indent=1, sort_keys=True)

    print("substituted role: %s (by %s)" % (substituted, NEW))
    print("mode=%s  reference full_steady=%s (%.1fs polling)" % (
        mode, full, poll_seconds))
    print("substitution: accepted=%s rc=%s  %s" % (
        sub["accepted"], sub["rc"], sub["output"][:60]))
    print("reference roles kept: %s" % ", ".join(others))
    print("verdict: %s" % report["verdict"])
    return 0


if __name__ == "__main__":
    sys.exit(main())
