#!/usr/bin/env python3
"""Emit an index view of the coverage ledger for a dir scope (BW-00-05-02..04).

A row is *indexed* when it carries a concrete index entry: a task number plus
the fixture that exercises it, or - for a gap - a specific gap source (what the
target is, what the new side is missing, the blocker). A row still holding the
generic `GENERIC_GAP` note is unindexed and fails the acceptance.

Usage: coverage_index_doc.py LEDGER_JSON TITLE DIR_PREFIX [DIR_PREFIX...]
       > bw00-05-0X-*.md
"""
import json
import sys

GENERIC_GAP = "尚无对应实现与证据（未映射任务）"
STATUS_CN = {"covered": "已覆盖", "partial": "部分", "gap": "缺口"}


def main():
    if len(sys.argv) < 4:
        print(__doc__)
        return 1
    ledger = json.load(open(sys.argv[1], encoding="utf-8"))
    title, prefixes = sys.argv[2], sys.argv[3:]

    rows = [r for r in ledger["make"] if r["dir"].startswith(tuple(prefixes))]
    unindexed = [r for r in rows if r["note"] == GENERIC_GAP]
    scope_txt = " 与 ".join("`%s`" % p.rstrip("/") for p in prefixes)

    print("# " + title)
    print()
    print("> 证据面（映射验收：构建清册中的每项都有索引）。")
    print()
    print(f"行基取 BW-00-01-01 的 Make 组件清册中 {scope_txt} 下的全部目标"
          f"（共 {len(rows)} 行）。索引条目二选一：")
    print()
    print("- **已映射**——`任务号` + `夹具`（练习该面的证据文档）；")
    print("- **缺口**——`缺口：` 开头的具体来源（目标是什么、新版缺什么、"
          "阻塞原因），任务号记 `未映射`。")
    print()
    print("## 汇总")
    print()
    print("| 侧 | 行数 | 已映射 | 缺口（带来源） | 未索引 |")
    print("| --- | --- | --- | --- | --- |")
    mapped = sum(1 for r in rows if r["task"] != "未映射")
    print(f"| {scope_txt} | {len(rows)} | "
          f"{mapped} | {len(rows) - mapped - len(unindexed)} | "
          f"{len(unindexed)} |")
    print()
    print(f"未索引 = {len(unindexed)}"
          + ("，验收不满足（仍有通用缺口说明）。" if unindexed
             else "，范围内每项均有索引条目。"))
    print()

    print("## 索引明细")
    print()
    print("| 原版目标 | 目录 | 类型 | 构建 | 启用条件 | 任务号 | 夹具 | "
          "状态 | 索引条目 |")
    print("| --- | --- | --- | --- | --- | --- | --- | --- | --- |")
    for r in rows:
        entry = r["note"]
        if r["status"] == "covered":
            entry = "已映射：" + entry
        elif r["status"] == "partial":
            entry = "部分：" + entry
        print(f"| `{r['target']}` | `{r['dir']}` | {r['type']} | "
              f"{r['in_build']} | `{r['enable']}` | {r['task']} | "
              f"{r['fixture']} | {STATUS_CN[r['status']]} | {entry} |")
    print()

    print("## 可追溯性")
    print()
    print("- 台账：`reference/bw00_05_01_coverage_ledger.json`。")
    print("- 行基：`reference/bw00_01_make_components.tsv`。")
    print()
    print("生成：`coverage_ledger_scan.py` + `coverage_index_doc.py`。")
    return 0


if __name__ == "__main__":
    sys.exit(main())
