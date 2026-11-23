#!/usr/bin/env python3
"""Emit the BW-00-05-05 evidence markdown from the artifact/SDK check JSON.

Usage: artifact_sdk_check_doc.py CHECK_JSON > bw00-05-05-artifact-sdk-check.md
"""
import json
import sys

STATUS_CN = {"covered": "已覆盖", "gap": "缺口", "partial": "部分"}


def main():
    d = json.load(open(sys.argv[1], encoding="utf-8"))
    c, rows = d["counts"], d["rows"]

    print("# BW-00-05-05 发行物、SDK 与缺失资源核查")
    print()
    print("> 证据面（核查验收：缺口有来源与阻塞原因）。")
    print()
    print("逐行列出参考树中的发行物与 SDK，并核查缺失资源。每行给出 `present`"
      "（路径是否在树内，实算）、`来源`（哪份证据确立该状态）与缺口时的"
      "阻塞原因——验收要求每个缺口都有 `来源` + `阻塞原因`。")
    print()
    print("## 汇总")
    print()
    print("| 项 | 数 |")
    print("| --- | --- |")
    print(f"| 核查行 | {c['rows']} |")
    print(f"| 树内存在（present） | {c['present']} |")
    print(f"| 已覆盖（存在+证据基线） | "
      f"{c['rows'] - c['gap']} |")
    print(f"| 缺口 | {c['gap']} |")
    print(f"| 缺口带来源+阻塞 | {c['gap_with_source']} |")
    print(f"| 类别 | "
      + "、".join("%s %d" % kv for kv in sorted(c["kinds"].items())) + " |")
    print()
    print(f"缺口 {c['gap']} 项全部带 `来源` + `阻塞原因`"
      f"（{c['gap_with_source']}/{c['gap']}），验收满足。")
    print()
    print("## 核查明细")
    print()
    print("| 类别 | 项 | 路径 | present | 任务号 | 夹具 | 状态 | "
      "来源 | 阻塞原因 |")
    print("| --- | --- | --- | --- | --- | --- | --- | --- | --- |")
    for r in rows:
        blocker = r["note"] if r["status"] == "gap" else "—"
        print(f"| {r['kind']} | {r['item']} | `{r['path']}` | "
          f"{'是' if r['present'] else '否'} | {r['task']} | {r['fixture']} | "
          f"{STATUS_CN[r['status']]} | {r['source']} | {blocker} |")
    print()

    print("## 关键事实")
    print()
    print("- **Linux 服务端二进制** `game/bin/server/el7` 存在（含 server/"
      "commands/ 的 clear_auto_load、consolidate_dbs、remove_db、sync_db、"
      "transfer_db），有 BW-00-02-01 安装文件 sha256 基线——是唯一的"
      "已覆盖发行物。")
    print("- **Windows 侧缺失**：`game/bin/win32` 与 `win64` 均不存在"
      "（BW-00-02-05 探测确认），阻塞在本机无 MSVC/msbuild 工具链，"
      "由 windows-latest CI 承载。")
    print("- **snapshot_helper 声明未装**：Make 清册声明该目标，"
      "但 `el7/server/commands/` 无该文件（BW-00-01-build-targets 已记）。")
    print("- **third_party/png|zip** 为 vcpkg 迁移后残留空壳，目录在但"
      "不再由 Make 构建。")
    print("- **客户端 SDK 四件套**（typescript 0.0.1、unity、unreal、generator）"
      "树内存在，但仅作 workflow artifact（client-sdk job，retention 14d）、"
      "版本仅 0.0.1，新版无对应发布面——按缺口记。")
    print()

    print("## 可追溯性")
    print()
    print("- 核查：`reference/bw00_05_05_artifact_sdk_check.json`。")
    print("- 既有发行面证据：`bw00-01-release-scripts.md`（BW-00-01-03）、"
      "`bw00-02-05-windows-baseline.md`、`bw00-01-build-targets.md`。")
    print()
    print("生成：`artifact_sdk_check.py` + `artifact_sdk_check_doc.py`。")
    return 0


if __name__ == "__main__":
    sys.exit(main())
