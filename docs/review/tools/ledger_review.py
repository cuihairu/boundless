#!/usr/bin/env python3
"""Review the coverage ledger for completeness (BW-00-05-06).

Cross-checks the ledger against the original BW-00-01-01 component scan TSV:

  1. every inventoried component has exactly one ledger row (keyed by
     name+dir+type);
  2. the in_build counts agree;
  3. every ENABLED target (in_build=1) carries either run evidence (covered /
     partial with a fixture) or an explicit gap (a concrete note, i.e. not the
     generic unmapped note);
  4. the CMake rows all carry the Windows gap with task + fixture + note.

Emits a review JSON with the verdict; exit code 1 if any check fails.

Usage: ledger_review.py COMPONENT_TSV LEDGER_JSON OUTPUT_JSON
"""
import csv
import json
import sys

GENERIC_GAP = "尚无对应实现与证据（未映射任务）"


def main():
    if len(sys.argv) < 4:
        print(__doc__)
        return 1
    tsv, ledger_json, out = sys.argv[1:4]

    with open(tsv, encoding="utf-8") as fh:
        tsv_rows = list(csv.DictReader(fh, delimiter="\t"))
    ledger = json.load(open(ledger_json, encoding="utf-8"))
    make_rows = ledger["make"]

    checks = []

    def check(name, ok, detail):
        checks.append({"check": name, "ok": ok, "detail": detail})
        return ok

    # 1. one ledger row per inventoried component, keyed name+dir+type.
    tsv_keys = {(r["name"], r["dir"], r["type"]) for r in tsv_rows}
    led_keys = {(r["target"], r["dir"], r["type"]) for r in make_rows}
    check("components_1to1", tsv_keys == led_keys and len(tsv_keys) == len(
        tsv_rows) and len(led_keys) == len(make_rows),
        "tsv %d 行 / ledger %d 行 / 去重后 tsv %d、ledger %d" % (
            len(tsv_rows), len(make_rows), len(tsv_keys), len(led_keys)))

    # 2. in_build counts agree.
    tsv_in_build = sum(1 for r in tsv_rows if r["in_build"] == "1")
    led_in_build = sum(1 for r in make_rows if r["in_build"] == "1")
    check("in_build_agrees", tsv_in_build == led_in_build,
          "tsv in_build %d / ledger in_build %d" % (tsv_in_build,
                                                    led_in_build))

    # 3. every enabled target has run evidence or an explicit gap.
    unindexed = []
    for r in make_rows:
        if r["in_build"] != "1":
            continue
        if r["status"] in ("covered", "partial") and r["fixture"] not in (
                None, "", "—"):
            continue
        if r["status"] == "gap" and r["note"] and r["note"] != GENERIC_GAP:
            continue
        unindexed.append(r["dir"])
    enabled = sum(1 for r in make_rows if r["in_build"] == "1")
    check("enabled_have_evidence_or_gap", not unindexed,
          "启用 %d / 有证据 %d / 显式缺口 %d / 未索引 %d%s" % (
              enabled,
              sum(1 for r in make_rows if r["in_build"] == "1"
                  and r["status"] in ("covered", "partial")),
              sum(1 for r in make_rows if r["in_build"] == "1"
                  and r["status"] == "gap"),
              len(unindexed),
              "" if not unindexed else "：" + ", ".join(unindexed)))

    # 4. CMake rows all carry the Windows gap with task+fixture+note.
    cmake_rows = ledger["cmake"]
    cmake_bad = [r["target"] for r in cmake_rows
                 if r["task"] == "—" or r["fixture"] == "—" or not r["note"]]
    check("cmake_indexed", not cmake_bad,
          "cmake %d 组 / 未索引 %d%s" % (
              len(cmake_rows), len(cmake_bad),
              "" if not cmake_bad else "：" + ", ".join(cmake_bad)))

    all_ok = all(c["ok"] for c in checks)
    data = {"all_ok": all_ok, "checks": checks,
            "counts": {
                "components": len(tsv_rows),
                "ledger_rows": len(make_rows),
                "enabled": enabled,
                "enabled_with_evidence": sum(
                    1 for r in make_rows if r["in_build"] == "1"
                    and r["status"] in ("covered", "partial")),
                "enabled_explicit_gap": sum(
                    1 for r in make_rows if r["in_build"] == "1"
                    and r["status"] == "gap"),
                "enabled_unindexed": len(unindexed),
                "cmake_groups": len(cmake_rows),
            }}
    with open(out, "w", encoding="utf-8") as fh:
        json.dump(data, fh, indent=1, sort_keys=True)

    for c in checks:
        print("%-32s %s  %s" % (c["check"], "PASS" if c["ok"] else "FAIL",
                                c["detail"]))
    print("all_ok=%s" % all_ok)
    return 0 if all_ok else 1


if __name__ == "__main__":
    sys.exit(main())
