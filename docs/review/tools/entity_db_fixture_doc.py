#!/usr/bin/env python3
"""Emit the BW-00-04-03 evidence markdown from the entity/DB fixture JSON.

Usage: entity_db_fixture_doc.py ENTITY_DB_FIXTURES_JSON > bw00-04-03-*.md
Every table is regenerated from the JSON so the doc never disagrees with the
captured run.
"""
import json
import re
import sys

ROLE = {
    "db-seed": "数据库种子（dbapp 装载输入）",
    "registration": "实体注册表",
    "alias": "数据类型别名",
    "entity-def": "实体定义（.def）",
    "entity-script": "实体脚本",
    "config": "运行配置",
}


def columns(create_sql):
    """Column names out of a SHOW CREATE TABLE body."""
    m = re.search(r"\((.*)\)\s*ENGINE", create_sql, re.S)
    if not m:
        return []
    return re.findall(r"^\s*`([A-Za-z0-9_]+)`", m.group(1), re.M)


def main():
    with open(sys.argv[1], encoding="utf-8") as fh:
        d = json.load(fh)

    print("# BW-00-04-03 资源、实体和数据库夹具")
    print()
    print("> 证据面（BW-00-04-03 验收：每份可追溯原版版本与输入）。")
    print()
    print("由 `tools/entity_db_fixture_scan.py` 采集（只读，不驱动集群）："
          "资源夹具取参考树文件，实体夹具取 `.def` 解析 + 数据库类型 ID + "
          "一次真实登录的运行时实体，数据库夹具取 dbapp 落下的表结构与行。"
          "源数据 `reference/bw00_04_03_entity_db_fixtures.json`。")
    print()
    print("| 项 | 值 |")
    print("| --- | --- |")
    print(f"| 参考仓提交 | `{d['source_commit']}` |")
    print(f"| 捕获时间 | `{d['captured_at']}` |")
    print(f"| 模式 | `{d['mode']}`（集群为他人所有，只旁听、不驱动） |")
    print(f"| 数据库 | `{d['inputs']['database']}`，`{d['inputs']['db_server']}` |")
    print(f"| 资源路径 | `{d['inputs']['res_path']}` |")
    print(f"| 运行时实体 | `{d['runtime_entity']['class']}` "
          f"proxy_id `{d['runtime_entity']['proxy_id']}`，"
          f"客户端 `{d['runtime_entity']['client_source_port']}` |")
    print(f"| 夹具数 | 资源 {len(d['resources'])}、实体 {len(d['entities'])}、"
          f"表 {len(d['database']['tables'])} |")
    print()

    print("## 资源夹具")
    print()
    print("每个文件记录路径、角色、SHA-256 与字节数；`source_commit` 即其版本。"
          "`.def`/`.py`/`.xml` 均取自客户端集成示例资产树，"
          "`bw_deep.xml` 是 deep-chain 实际运行的配置覆盖层。")
    print()
    print("| 角色 | 路径 | 字节 | SHA-256 |")
    print("| --- | --- | --- | --- |")
    for r in d["resources"]:
        print(f"| {ROLE.get(r['role'], r['role'])} | `{r['path']}` | "
              f"{r['bytes']} | `{r['sha256']}` |")
    print()

    print("## 实体夹具")
    print()
    print("每个实体类型由 `.def` 解析（属性、可见性、持久化、默认值与方法），"
          "并接到数据库认识它的类型 ID（`bigworldEntityTypes`）。"
          "`ClientOnly` 无数据库类型 ID——客户端专属实体不参与服务端/数据库，"
          "这是预期。运行时实体取自 BW-00-02-04 真实登录：`ClientAvatar`"
          "（proxy_id `%s`）。" % d["runtime_entity"]["proxy_id"])
    print()
    for e in d["entities"]:
        print(f"### `{e['type']}`")
        print()
        print(f"- 定义：`{e['path']}`（SHA-256 `{e['sha256']}`，{e['bytes']} B）")
        tid = e['db_type_id'] if e['db_type_id'] is not None else '—'
        bid = e['db_bigworld_id'] if e['db_bigworld_id'] is not None else '—'
        note = '（无——客户端专属实体）' if e['db_type_id'] is None else ''
        print(f"- 数据库类型 ID：`{tid}`{note}；BigWorld ID `{bid}`。")
        print(f"- Volatile：{', '.join(e['volatile']) or '—'}。")
        print(f"- 方法：客户端 {len(e['client_methods'])}、"
              f"cell {len(e['cell_methods'])}、base {len(e['base_methods'])}。")
        if e["properties"]:
            print()
            print("| 属性 | 类型 | 可见性 | 持久 | 默认 | 标识 |")
            print("| --- | --- | --- | --- | --- | --- |")
            for p in e["properties"]:
                flags = ", ".join(p["flags"]) or "—"
                print(f"| `{p['name']}` | `{p['type']}`"
                      f"{'<'+p['implemented_by']+'>' if p['implemented_by'] else ''}"
                      f" | {flags} | {p['persistent'] or '—'} | "
                      f"`{p['default'] or '—'}` | {p['identifier'] or '—'} |")
            print()
        else:
            print()
    print("## 数据库夹具")
    print()
    print("`SHOW CREATE TABLE` 是 dbapp 从 `.def` 派生的列类型权威记录，"
          "`bigworldEntityDefsChecksum` 把表结构钉回实体定义夹具；"
          f"本次捕获稳定={d['database']['stable']}"
          f"（{d['database']['attempts']} 次连续读取一致）。"
          "实体行在登出后清空（实体表 rows 0），属正常已登出状态；"
          "书签表仍记录世界状态。")
    print()
    print("| 表 | 行 | 列 |")
    print("| --- | --- | --- |")
    for t in d["database"]["tables"]:
        cols = ", ".join(columns(t["create_sql"]))
        if len(cols) > 90:
            cols = cols[:90] + "…"
        print(f"| `{t['name']}` | {t['rows']} | `{cols}` |")
    print()
    print("书签值：")
    print()
    for key, v in sorted(d["database"]["bookkeeping"].items()):
        print(f"- `{key}` = `{v['value'] or '—'}`（{v['note']}）")
    if d["database"]["row_counts"]:
        print()
        print("其余表行数：" + "、".join(
            f"`{k}`={v}" for k, v in sorted(d["database"]["row_counts"].items())))
    print()

    print("## 可追溯性")
    print()
    print("- 每份夹具携带 `source_commit`（参考仓提交）与 `inputs`"
          "（资源路径、登录命令、数据库实例），差异工具可对每个字节指出"
          "它来自哪个版本、哪个输入。")
    print("- 资源夹具 `db.xml`（db-seed）是数据库夹具的种子输入；"
          "实体夹具 `.def` 与数据库夹具表结构通过 "
          "`bigworldEntityDefsChecksum` 互相钉住。")
    print("- 完整数据：`reference/bw00_04_03_entity_db_fixtures.json`。")
    print()
    print("生成：`entity_db_fixture_scan.py` + `entity_db_fixture_doc.py`。")
    return 0


if __name__ == "__main__":
    sys.exit(main())
