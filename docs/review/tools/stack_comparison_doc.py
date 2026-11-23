#!/usr/bin/env python3
"""Emit the BW-00-04-04 evidence markdown from the comparison-run outputs.

Usage: stack_comparison_doc.py INPUT_JSON REFERENCE_JSON NEW_JSON > bw00-04-04-*.md
The tables are regenerated from the JSON so the doc never disagrees with the
captured runs.
"""
import json
import sys


def main():
    inp, ref, new = (json.load(open(p, encoding="utf-8"))
                     for p in sys.argv[1:4])

    print("# BW-00-04-04 原版与新版各自全栈的对照运行")
    print()
    print("> 证据面（BW-00-04-04 验收：同一输入产生两份证据）。")
    print()
    print("由 `tools/stack_comparison_run.py` 执行：一份输入"
          "（`reference/bw00_04_04_input.json`：角色集 + 资源路径 + 配置）"
          "同时驱动两栈，各自产出证据文件——原版侧"
          " `reference/bw00_04_04_reference_evidence.json`，"
          "新版侧 `reference/bw00_04_04_new_evidence.json`。")
    print()
    print("| 项 | 原版 | 新版 |")
    print("| --- | --- | --- |")
    print(f"| 提交 | `{ref['source_commit']}` | `{new['source_commit']}` |")
    print(f"| 捕获时间 | `{ref['captured_at']}` | `{new['captured_at']}` |")
    print(f"| 驱动 | 全栈二进制 `{inp['reference_bin_dir']}`"
          f"（模式 `{ref['mode']}`）"
          f" | `{new['binary']}` |")
    print(f"| 输入 | {len(inp['roles'])} 角色，"
          f"`{inp['config']}` | 同左（同一文件） |")
    print(f"| 角色证据 | {len(ref['roles'])} 份 | {len(new['roles'])} 份 |")
    print()
    print("输入（两栈共用，逐字节相同）：")
    print()
    print("```json")
    print(json.dumps(inp, indent=1, ensure_ascii=False))
    print("```")
    print()

    print("## 原版全栈证据")
    print()
    if ref["mode"] == "live":
        print("集群为他人所有（mode=live）：只旁听、不驱动，只读轮询"
              "（1s 一次）直至全角色在位且各自日志出现"
              "`---- <App> is running ----` 稳态标记"
              f"（本次 full_steady={str(ref['full_steady']).lower()}，"
              f"轮询 {ref['poll_seconds']}s）。")
    else:
        print("deep-chain.sh 拉起全栈（rc=%s，%ss），断言各角色到达稳态后采集。"
              % (ref["bringup_rc"], ref["bringup_seconds"]))
    print()
    print("| 角色 | 进程在位 | 稳态标记 | 日志 |")
    print("| --- | --- | --- | --- |")
    for r in ref["roles"]:
        print(f"| `{r['role']}` | {'是' if r['process_present'] else '否'} | "
              f"{'找到' if r['marker_found'] else '未找到'} | "
              f"`{r['log_file']}` |")
    print()

    print("## 新版全栈证据")
    print()
    print("`boundless-server <role>` 以同一输入逐角色运行"
          "（`BW_RES_PATH` 与原版侧相同），记录接受性、退出码与启动行。")
    print()
    print("| 角色 | 接受 | rc | 启动行 |")
    print("| --- | --- | --- | --- |")
    for r in new["roles"]:
        line = r["output"].replace("|", "\\|")
        print(f"| `{r['role']}` | {'是' if r['accepted'] else '否'} | "
              f"{r['rc']} | `{line}` |")
    print()

    print("## 对照结论")
    print()
    both = [r for r in new["roles"] if r["accepted"]]
    rejected = [r for r in new["roles"] if not r["accepted"]]
    print(f"- 双栈均有证据的角色：{len(both)}/{len(inp['roles'])}"
          f"（{', '.join(r['role'] for r in both)}）。")
    if rejected:
        print(f"- 新版尚不认识的角色：{', '.join(r['role'] for r in rejected)}"
              f"——`KNOWN_ROLES`（`crates/server-runtime/src/app.rs`）无此项，"
              f"拒绝并 rc=1（证据本身记录了这个缺口）。")
    print("- 深度差（预期内）：原版到达完整稳态（machined/管理器/dbapp/"
          "cellapp/baseapp/loginapp 各自打印运行标记），新版入口当前只到达"
          "进程启动（校验角色 + 启动行 + rc）；更深的 init 链"
          "（bw.xml 全量、网络接口、信号）由 BW-01-04 落地。对照运行如实"
          "记录两侧各自到达的深度。")
    print()

    print("## 可追溯性")
    print()
    print("- 输入：`reference/bw00_04_04_input.json`（两份证据均内嵌同一输入）。")
    print("- 原版证据：`reference/bw00_04_04_reference_evidence.json`"
          f"（提交 `{ref['source_commit']}`）。")
    print("- 新版证据：`reference/bw00_04_04_new_evidence.json`"
          f"（提交 `{new['source_commit']}`）。")
    print()
    print("生成：`stack_comparison_run.py` + `stack_comparison_doc.py`。")
    return 0


if __name__ == "__main__":
    sys.exit(main())
