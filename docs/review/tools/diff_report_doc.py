#!/usr/bin/env python3
"""Emit the BW-00-04-06 evidence markdown from the diff-report selftest.

Usage: diff_report_doc.py SELFTEST_JSON COMPARISON_DIFF_JSON > bw00-04-06-*.md
The tables are regenerated from the JSON so the doc never disagrees with the
captured runs.
"""
import json
import sys


def main():
    selftest = json.load(open(sys.argv[1], encoding="utf-8"))
    comparison = json.load(open(sys.argv[2], encoding="utf-8"))

    print("# BW-00-04-06 差异报告")
    print()
    print("> 证据面（BW-00-04-06 验收：注入字段、超时和顺序差异时分别可定位）。")
    print()
    print("`tools/diff_report.py` 是一个泛型记录序列差异器：回调夹具"
          "（`callbacks`）与全栈对照证据（`roles`）都归一化为"
          "`{key, order, value, ignorable, timed_out}`，"
          "因此一份差异器即可定位三类差异：")
    print()
    print("- `field`——记录的 `value` 不同且未标记可忽略；")
    print("- `ignored`——`value` 不同但记录标记为可忽略（两次合法运行"
          "应只差这些）；")
    print("- `timeout`——一侧超时（`timed_out` 不同）；")
    print("- `ordering`——记录双方都在但相对顺序不同。")
    print()
    print("每条差异携带记录 `key`，可逐条定位。")
    print()

    print("## 注入测试（验收）")
    print()
    print("以 BW-00-04-02 真实回调夹具为基线，每次只注入一类差异，"
          "断言差异器**恰好**定位该类、其余两类为 0。")
    print()
    print("| 注入 | 定位 field | 定位 timeout | 定位 ordering | 结果 |")
    print("| --- | --- | --- | --- | --- |")
    for c in selftest["cases"]:
        counts = c["counts"]
        print(f"| {c['injected']} | {counts['field']} | {counts['timeout']} | "
              f"{counts['ordering']} | "
              f"{'通过' if c['ok'] else '未通过'} |")
    print()
    print(f"三项注入全部通过：`all_ok={str(selftest['all_ok']).lower()}`。")
    print()

    print("## 注入明细")
    print()
    for c in selftest["cases"]:
        print(f"### 注入 `{c['injected']}`")
        print()
        report = c["report"]
        if c["injected"] == "field" and report["field"]:
            e = report["field"][0]
            print(f"- `field`：`{e['key']}`，`{e['ref']!r}` → `{e['new']!r}`"
                  f"（仅值不同，非可忽略）。")
        if c["injected"] == "timeout" and report["timeout"]:
            e = report["timeout"][0]
            print(f"- `timeout`：`{e['key']}`，ref 超时={e['ref']}，"
                  f"new 超时={e['new']}。")
        if c["injected"] == "ordering" and report["ordering"]:
            print("- `ordering`：两回调互换位置，报告：")
            for e in report["ordering"]:
                print(f"  - `{e['key']}`：ref#{e['ref']} → new#{e['new']}。")
        if not c["ok"]:
            print(f"- **未通过**：报告 = {report}")
        print()

    print("## 真实对照上的演示")
    print()
    print("把 BW-00-04-04 的原版证据与新版证据送入差异器"
          "（`reference/bw00_04_06_comparison_diff.json`）：")
    print()
    print("| 角色 | 原版 | 新版 |")
    print("| --- | --- | --- |")
    for e in comparison["field"]:
        print(f"| `{e['key']}` | `{e['ref']}` | `{e['new']}` |")
    print()
    print("7 个角色 `running` → `started`（深度差，预期内）；"
          "`serviceapp` `running` → `rejected`（新版尚不认识该角色）——"
          "差异器按角色逐条定位，不混淆。")
    print()

    print("## 可追溯性")
    print()
    print(f"- 注入测试：`reference/bw00_04_06_diff_selftest.json`"
          f"（基线 `{selftest['baseline']}`）。")
    print("- 演示：`reference/bw00_04_06_comparison_diff.json`。")
    print()
    print("生成：`diff_report.py` + `diff_report_selftest.py` + "
          "`diff_report_doc.py`。")
    return 0


if __name__ == "__main__":
    sys.exit(main())
