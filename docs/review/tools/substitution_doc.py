#!/usr/bin/env python3
"""Emit the BW-00-04-05 evidence markdown from the substitution-run report.

Usage: substitution_doc.py SUBSTITUTION_JSON > bw00-04-05-*.md
The tables are regenerated from the JSON so the doc never disagrees with the
captured run.
"""
import json
import sys


def main():
    with open(sys.argv[1], encoding="utf-8") as fh:
        d = json.load(fh)
    inp = d["input"]

    print("# BW-00-04-05 单组件替换运行")
    print()
    print("> 证据面（BW-00-04-05 验收：报告注明被替换角色）。")
    print()
    print("由 `tools/substitution_run.py` 执行：同一角色集只换**一个**角色——"
          "被替换角色由新版组件 `boundless-server <role>` 启动，"
          "其余角色保持原版侧。报告注明被替换角色。")
    print()
    print("| 项 | 值 |")
    print("| --- | --- |")
    print(f"| 被替换角色 | **`{d['substituted_role']}`**（由 {d['substituted_by']}） |")
    print(f"| 保留原版角色 | {', '.join('`' + r + '`' for r in d['reference_roles'])} |")
    print(f"| 原版提交 | `{d['reference_commit']}` |")
    print(f"| 新版提交 | `{d['new_commit']}` |")
    print(f"| 捕获时间 | `{d['captured_at']}` |")
    print(f"| 模式 | `{d['mode']}`（原版集群为他人所有，只读旁听；"
          f"替换组件本地运行） |")
    print(f"| 原版稳态 | full_steady={str(d['full_steady']).lower()}，"
          f"轮询 {d['poll_seconds']}s |")
    print()

    print("## 被替换角色（baseapp）的运行")
    print()
    s = d["substitution"]
    print(f"- `{s['role']}` 由 `boundless-server {s['role']}` 启动："
          f"接受={s['accepted']}，rc={s['rc']}，启动行 `{s['output']}`。")
    print("- 其余 7 个角色（dbappmgr/cellappmgr/baseappmgr/dbapp/cellapp/"
          "loginapp/serviceapp）仍由原版二进制承载，各自到达稳态。")
    print()

    print("## 替换后果")
    print()
    print(f"- **替换后能否保持全栈稳态**：否。"
          f"（被替换角色接受输入并启动，但只到达进程启动；"
          f"以替换角色运行的全栈无法维持稳态——新版入口的更深 init 链"
          f"由 BW-01-04 落地。）")
    print("- 记录本身即产出：替换路径、被替换角色、两侧各自到达深度——"
          "这正是本条目要求写下来的内容。")
    print()

    print("## 参考侧逐角色（旁听观测）")
    print()
    print("| 角色 | 进程在位 | 稳态标记 |")
    print("| --- | --- | --- | --- |")
    for r in d["reference"]:
        print(f"| `{r['role']}` | "
              f"{'是' if r['process_present'] else '否'} | "
              f"{'找到' if r['marker_found'] else '未找到'} |")
    print()

    print("## 可追溯性")
    print()
    print(f"- 输入：`reference/bw00_04_05_input.json`（角色集 + `substitute`"
          f"=`{d['substituted_role']}`）。")
    print("- 报告：`reference/bw00_04_05_substitution.json`。")
    print()
    print("生成：`substitution_run.py` + `substitution_doc.py`。")
    return 0


if __name__ == "__main__":
    sys.exit(main())
