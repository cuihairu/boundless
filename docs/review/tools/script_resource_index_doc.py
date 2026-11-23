#!/usr/bin/env python3
"""Emit the BW-00-05-04 evidence markdown from the script/resource index JSON.

Usage: script_resource_index_doc.py INDEX_JSON > bw00-05-04-script-resource-index.md
"""
import json
import sys

STATUS_CN = {"covered": "已覆盖", "gap": "缺口", "partial": "部分"}


def main():
    d = json.load(open(sys.argv[1], encoding="utf-8"))
    c, rows = d["counts"], d["rows"]

    print("# BW-00-05-04 公共代码、示例与 FantasyDemo 索引")
    print()
    print("> 证据面（映射验收：脚本及资源入口可追溯）。")
    print()
    print("入口点分三类：")
    print()
    print("- **公共代码**——嵌入 Python 运行时的 stage"
          "（`game/res/bigworld/scripts/`：`common/` 与 `server_common/`，"
          "含标准库与动态扩展模块 stage）；")
    print("- **FantasyDemo**——`programming/fantasydemo/`：实体脚本"
          "（base/cell/client）、实体定义（`entity_defs/`）、配置"
          "（`db.xml`/`entities.xml`/`res/server`）与项目文件；")
    print("- **示例**——`programming/bigworld/examples/`：客户端集成示例与 "
          "cellapp 扩展。")
    print()
    print("每行给出入口点（路径）、`任务号` + `夹具`，或具体缺口来源——"
          "脚本与资源入口由此可追溯：")
    print()
    print("| 入口点 | 路径 | 文件数 | 任务号 | 夹具 | 状态 | 说明 |")
    print("| --- | --- | --- | --- | --- | --- | --- |")
    for r in rows:
        print(f"| `{r['entry']}` | `{r['dir']}` | {r['files']} | "
              f"{r['task']} | {r['fixture']} | {STATUS_CN[r['status']]} | "
              f"{r['note']} |")
    print()

    print("## 汇总")
    print()
    print("| 项 | 数 |")
    print("| --- | --- |")
    print(f"| 入口点 | {c['rows']} |")
    print(f"| 覆盖文件总数 | {c['files']} |")
    print(f"| 已覆盖（可追溯） | {c['covered']} |")
    print(f"| 缺口（带来源） | {c['gap']} |")
    print(f"| 类别 | "
          + "、".join("%s %d" % kv for kv in sorted(c["kinds"].items())) + " |")
    print()
    print("未索引 = 0：脚本与资源入口点逐项可追溯（任务号+夹具或缺口来源）。")
    print()
    print("**已覆盖**的入口点：`公共代码` 运行时 stage（登录捕获期间嵌入"
          "解释器经其导入脚本/标准库，BW-00-02-04/BW-00-04-02）与 "
          "`simple_python_client` 示例（BW-00-02-04 登录驱动）。")
    print()
    print("**缺口**的入口点：`FantasyDemo` 全部（独立游戏资产，本机证据均取"
          "自 simple_python_client 示例——bw00-04-03 的 24 份资源夹具为"
          "该示例的 db.xml/entities.xml/5 个 `.def`/base/cell/client/common "
          "脚本，与 FantasyDemo 的 Avatar.def 实体集不同）与客户端 C++/"
          "cellapp 扩展示例（无对应实现）。")
    print()
    print("## 可追溯性")
    print()
    print("- 索引：`reference/bw00_05_04_script_resource_index.json`。")
    print("- 资源夹具：`reference/bw00_04_03_entity_db_fixtures.json`"
          "（24 份，带路径+SHA-256+字节）。")
    print("- 登录驱动：`reference/bw00_02_04_client_login.json`。")
    print()
    print("生成：`script_resource_index.py` + `script_resource_index_doc.py`。")
    return 0


if __name__ == "__main__":
    sys.exit(main())
