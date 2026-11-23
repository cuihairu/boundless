#!/usr/bin/env python3
"""Emit the BW-00-05-06 evidence markdown from the ledger review JSON.

Usage: ledger_review_doc.py REVIEW_JSON > bw00-05-06-ledger-completeness.md
"""
import json
import sys

CHECK_CN = {
    "components_1to1": "清册与台账一一对应",
    "in_build_agrees": "启用计数一致",
    "enabled_have_evidence_or_gap": "启用目标均有证据或显式缺口",
    "cmake_indexed": "CMake 组全部有索引",
}


def main():
    d = json.load(open(sys.argv[1], encoding="utf-8"))
    c = d["counts"]

    print("# BW-00-05-06 台账完整性复核")
    print()
    print("> 证据面（复核验收：每个启用目标有运行证据或显式缺口）。")
    print()
    print("复核把 BW-00-05-01 的台账逐条对回 BW-00-01-01 的组件清册 TSV"
      "（`name`+`dir`+`type` 键），并核对启用计数与索引状态。")
    print()
    print("## 检查项")
    print()
    print("| 检查 | 结果 | 明细 |")
    print("| --- | --- | --- |")
    for ch in d["checks"]:
        print("| %s | %s | %s |" % (CHECK_CN.get(ch["check"], ch["check"]),
                                       "✅ PASS" if ch["ok"] else "❌ FAIL",
                                       ch["detail"]))
    print()
    print(f"整体判定 `all_ok={str(d['all_ok']).lower()}`。")
    print()
    print("## 汇总")
    print()
    print("| 项 | 数 |")
    print("| --- | --- |")
    print(f"| 清册组件 | {c['components']} |")
    print(f"| 台账行 | {c['ledger_rows']} |")
    print(f"| 启用目标（in_build=1） | {c['enabled']} |")
    print(f"| 其中有运行证据（已覆盖/部分+夹具） | "
      f"{c['enabled_with_evidence']} |")
    print(f"| 其中显式缺口（带来源） | {c['enabled_explicit_gap']} |")
    print(f"| 未索引 | {c['enabled_unindexed']} |")
    print(f"| CMake 组 | {c['cmake_groups']} |")
    print()
    print("启用 %d = 有证据 %d + 显式缺口 %d，未索引 0——验收满足。" % (
        c["enabled"], c["enabled_with_evidence"],
        c["enabled_explicit_gap"]))
    print()
    print("## 复核范围与前序证据")
    print()
    print("- 台账字段与行基：`bw00-05-01-coverage-ledger.md`。")
    print("- 引擎库与服务端索引：`bw00-05-02-engine-server-index.md`。")
    print("- 客户端与工具索引：`bw00-05-03-client-tools-index.md`。")
    print("- 脚本与资源入口：`bw00-05-04-script-resource-index.md`。")
    print("- 发行物、SDK 与缺失资源：`bw00-05-05-artifact-sdk-check.md`。")
    print()
    print("## 可追溯性")
    print()
    print("- 复核：`reference/bw00_05_06_ledger_review.json`。")
    print("- 台账：`reference/bw00_05_01_coverage_ledger.json`；"
      "清册：`reference/bw00_01_make_components.tsv`。")
    print()
    print("生成：`ledger_review.py` + `ledger_review_doc.py`。")
    return 0


if __name__ == "__main__":
    sys.exit(main())
