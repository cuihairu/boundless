#!/usr/bin/env python3
"""Emit the BW-00-05-01 evidence markdown from the coverage ledger JSON.

Usage: coverage_ledger_doc.py LEDGER_JSON > bw00-05-01-coverage-ledger.md
The tables are regenerated from the JSON so the doc never disagrees with the
captured scan.
"""
import json
import sys

STATUS_CN = {"covered": "已覆盖", "partial": "部分", "gap": "缺口"}


def main():
    d = json.load(open(sys.argv[1], encoding="utf-8"))
    c = d["counts"]

    print("# BW-00-05-01 覆盖台账字段")
    print()
    print("> 证据面（BW-00-05-01 验收：每行含原版目标、启用条件、任务号、"
          "夹具及状态）。")
    print()
    print("台账以 BW-00-01-01 的 Make 组件清册（79 项）与 BW-00-01-02 的 "
          "CMake 目标组（11 组）为行基，每行六字段：")
    print()
    print("- **原版目标**——`target`（组件名 / 目标组）与 `dir`（参考树相对"
          "目录）；")
    print("- **启用条件**——Make 特性开关（`useXxx=`）与 `ifeq` 守卫，"
          "或 CMake 组的 defines / flags；")
    print("- **任务号**——映射表（`coverage_ledger_scan.py` 的 `COVERAGE`）"
          "给出的 BW 任务；无映射记 `未映射`；")
    print("- **夹具**——练习该面的证据文档（`docs/review/bw*.md`）；")
    print("- **状态**——`已覆盖`（有证据练习）/ `部分`（新版组件存在但只到"
          "进程启动）/ `缺口`（无任务或无证据，缺口来源见说明列）。")
    print()
    print("映射原则：只有证据文档实际练习过该面才记 `已覆盖`；"
          "单测目录一律映射 BW-00-02-02（原版单测基线复跑），"
          "与父库状态无关。")
    print()

    print("## 汇总")
    print()
    print("| 侧 | 行数 | 已覆盖 | 部分 | 缺口 |")
    print("| --- | --- | --- | --- | --- |")
    print(f"| Make 组件（in_build {c['make_in_build']}） | {c['make']} | "
          f"{c['covered']} | {c['partial']} | {c['gap']} |")
    print(f"| CMake 目标组 | {c['cmake']} | 0 | 0 | {c['cmake']} |")
    print()

    rows = d["make"]

    def table(rs):
        print("| 原版目标 | 目录 | 类型 | 构建 | 启用条件 | 任务号 | 夹具 |"
              " 说明 |")
        print("| --- | --- | --- | --- | --- | --- | --- | --- |")
        for r in rs:
            print(f"| `{r['target']}` | `{r['dir']}` | {r['type']} | "
                  f"{r['in_build']} | `{r['enable']}` | {r['task']} | "
                  f"{r['fixture']} | {r['note']} |")
        print()

    for status in ("covered", "partial", "gap"):
        rs = [r for r in rows if r["status"] == status]
        print(f"## Make 组件 · {STATUS_CN[status]}（{len(rs)}）")
        print()
        if status == "gap":
            print("缺口来源逐行给出；`未映射` 行是尚未指派任务的显式缺口"
                  "（BW-00-05-02..05 逐组补映射）。")
            print()
        table(rs)

    print("## CMake 目标组 · 缺口（11）")
    print()
    print("全部 11 组为 Windows 构建面：本 Linux 主机无产物与 MSVC 工具链"
          "（BW-00-02-05 复测），由 windows-latest CI 承载。"
          "`server` 组的 Linux 路径由 Make 承载，见上方 make 行。")
    print()
    print("| 目标组 | 工程数 | 启用条件 | 缺口来源 |")
    print("| --- | --- | --- | --- |")
    for r in d["cmake"]:
        print(f"| `{r['target']}` | {r['sources']} | `{r['enable']}` | "
              f"{r['note']} |")
    print()

    print("## 可追溯性")
    print()
    print(f"- 行基：`reference/bw00_01_make_components.tsv`（Make 组件）、"
          f"`reference/bw00_01_cmake_targets.json`（CMake 组）。")
    print("- 台账：`reference/bw00_05_01_coverage_ledger.json`。")
    print()
    print("生成：`make_component_scan.py` + `coverage_ledger_scan.py` + "
          "`coverage_ledger_doc.py`。")
    return 0


if __name__ == "__main__":
    sys.exit(main())
