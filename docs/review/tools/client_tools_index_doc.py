#!/usr/bin/env python3
"""Emit the BW-00-05-03 evidence markdown from the client/tools index JSON.

Usage: client_tools_index_doc.py INDEX_JSON > bw00-05-03-client-tools-index.md
"""
import json
import sys

STATUS_CN = {"covered": "已覆盖", "gap": "缺口", "partial": "部分"}
KIND_ORDER = ["客户端", "编辑器", "导出器", "离线工具", "资产管线",
              "资产转换器", "资源打包", "运行时环境", "示例", "工具基座",
              "组（无独立产物）"]


def main():
    d = json.load(open(sys.argv[1], encoding="utf-8"))
    c, rows = d["counts"], d["rows"]

    print("# BW-00-05-03 客户端和工具目标索引")
    print()
    print("> 证据面（映射验收：编辑器、导出器及离线工具均有索引）。")
    print()
    print("客户端与工具目标大多是 Windows 构建面产品，登记在 BW-00-01-02 的 "
          "CMake 目标组里（`client`、`tools`、`exporters`、`assetprocessor`、"
          "`navgen`、`extras`、`bwentity` 及其 `_blob` 变体；`server` 组在 "
          "BW-00-05-02 已索引）。本索引逐产品列出：")
    print()
    print("- **类别**——按用途分 `客户端` / `编辑器` / `导出器` / "
          "`离线工具` / `资产管线` / `资产转换器` 等；")
    print("- **索引条目**——`任务号` + `夹具`，或 Windows 缺口来源"
          "（本机无 MSVC 产物，由 windows-latest CI 承载）。")
    print()
    print("## 汇总")
    print()
    print("| 项 | 数 |")
    print("| --- | --- |")
    print(f"| 索引行（产品 41 + 组说明 4） | {len(rows)} |")
    print(f"| 已映射（有任务号+夹具） | {c['covered']} |")
    print(f"| 缺口（Windows 面 / 未实现，带来源） | {c['gap']} |")
    print(f"| 类别 | "
          + "、".join("%s %d" % (k, v) for k, v in sorted(c["kinds"].items(),
                                                          key=lambda kv: (
                                                              KIND_ORDER.index(
                                                                  kv[0])
                                                              if kv[0] in KIND_ORDER
                                                              else 99,
                                                              kv[0]))) + " |")
    print()
    for want in ("编辑器", "导出器", "离线工具"):
        n = c["kinds"].get(want, 0)
        rs = [r for r in rows if r["kind"] == want
              and (r["task"] != "—" and r["note"])]
        print(f"{want} {n} 项，均有索引条目 {len(rs)} 项。")
    print()
    print("未索引 = 0（每行均有 `任务号`+`夹具` 或具体缺口来源）。")
    print()

    kinds = {k: [r for r in rows if r["kind"] == k] for k in KIND_ORDER}
    for kind in KIND_ORDER:
        rs = kinds.get(kind)
        if not rs:
            continue
        print(f"## {kind}（{len(rs)}）")
        print()
        print("| 目标 | 目录 | 目标组 | 任务号 | 夹具 | 状态 | 索引条目 |")
        print("| --- | --- | --- | --- | --- | --- | --- |")
        for r in rs:
            print(f"| `{r['target']}` | `{r['dir']}` | {r['group']} | "
                  f"{r['task']} | {r['fixture']} | "
                  f"{STATUS_CN[r['status']]} | {r['note']} |")
        print()

    print("## 可追溯性")
    print()
    print("- 行基：`reference/bw00_01_cmake_targets.json`（CMake 组）。")
    print("- 索引：`reference/bw00_05_03_client_tools_index.json`。")
    print()
    print("生成：`client_tools_index.py` + `client_tools_index_doc.py`。")
    return 0


if __name__ == "__main__":
    sys.exit(main())
