#!/usr/bin/env python3
"""Emit the BW-00-04-02 evidence markdown from the callback-fixture JSON.

Usage: python_callback_fixture_doc.py CALLBACK_FIXTURES_JSON > bw00-04-02-*.md
The callback table is regenerated from the JSON so the doc never disagrees with
the captured run.
"""
import json
import sys

# Site -> human label (order the capture recorded, client then server apps).
SITES = {"client": "客户端", "baseapp": "BaseApp", "cellapp": "CellApp"}
IGNORABLE_MEANING = {
    "entity_id": "每次运行分配的实体 ID 不同（本夹具为 proxy_id 4805）",
    "source_port": "客户端 UDP 源端口随运行变化（本夹具为 55283）",
    "address": "回复中命名的跳转地址（BaseApp@<addr>:<port>）随监听端口变化",
}


def main():
    with open(sys.argv[1], encoding="utf-8") as fh:
        d = json.load(fh)

    print("# BW-00-04-02 Python 回调夹具")
    print()
    print("> 证据面（BW-00-04-02 验收：保留顺序、参数与可忽略字段）。")
    print()
    print("由 `tools/python_callback_fixture.py` 从真实登录捕获"
          "（BW-00-02-04，`reference/bw00_02_04_client_login.json`）派生，"
          "不再重复拉起集群。源数据 `reference/bw00_04_02_callback_fixtures.json`。")
    print()
    print("| 项 | 值 |")
    print("| --- | --- |")
    print(f"| 参考仓提交 | `{d['source_commit']}` |")
    print(f"| 捕获时间 | `{d['captured_at']}` |")
    print(f"| 登录命令 | `{d['login_command']}` |")
    print(f"| 实体 | proxy_id `{d['proxy_id']}`，"
          f"客户端源 `{d['client_source_port']}` |")
    print(f"| 回调数 | {len(d['callbacks'])} |")
    print(f"| 可忽略字段 | {', '.join('`' + f + '`' for f in d['ignorable_fields'])} |")
    print()

    print("## 回调序列（顺序保留）")
    print()
    print("`order` 是合并客户端与两个服务端日志后的全局序号，跨站点不重排："
          "客户端请求/应答在前，随后 BaseApp 回调，最后 CellApp 回调——"
          "与捕获记录的先后一致（登录 → 使能实体 → 客户端断开 → 实体销毁）。")
    print()
    print("| 序 | 站点 | 回调 | 参数 | 可忽略 |")
    print("| --- | --- | --- | --- | --- |")
    for e in d["callbacks"]:
        ign = ", ".join(e["ignorable"]) if e["ignorable"] else "—"
        args = (e["args"] or "—").replace("|", "\\|")
        print(f"| {e['order']} | {SITES.get(e['site'], e['site'])} | "
              f"`{e['name']}` | `{args}` | {ign} |")
    print()

    print("## 参数拆分")
    print()
    print("每条回调按名称（脚本方法或引擎入口点）与参数（余下文本）拆分，"
          "使差异工具可逐字段比较；原始行保留在 `raw`。两类拆分规则：")
    print()
    print("- `name(4805)` 形式：圆括号内是名称自身的实参表，"
          "参数取 `):` 之后的意义文本（如 `Proxy::enableEntities(4805)` → "
          "参数 `4805`）。")
    print("- `name: detail` 形式：冒号后为参数（如 "
          "`ClientAvatar.onEntitiesEnabled: 4805` → 参数 `4805`）。")
    print()

    print("## 可忽略字段")
    print()
    print("两次运行同一登录流程，合法地只差三类字段；夹具按实体记录哪些字段"
          "应被忽略，使两次运行的差异恰好忽略这些、不再多忽略。")
    print()
    for f, meaning in IGNORABLE_MEANING.items():
        print(f"- `{f}`：{meaning}。")
    print()

    print("## 可追溯性")
    print()
    print("- 源捕获：`reference/bw00_02_04_client_login.json`"
          "（BW-00-02-04，同一 `proxy_id` 与客户端源端口）。")
    print("- 夹具：`reference/bw00_04_02_callback_fixtures.json`。")
    print()
    print("生成：`python_callback_fixture.py` + `python_callback_fixture_doc.py`。")
    return 0


if __name__ == "__main__":
    sys.exit(main())
