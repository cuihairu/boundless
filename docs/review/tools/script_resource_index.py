#!/usr/bin/env python3
"""Index the common-code, example and FantasyDemo script/resource entrypoints
(BW-00-05-04).

The entrypoints are the staged Python runtime (`game/res/bigworld/scripts/`),
the FantasyDemo game scripts/resources (`programming/fantasydemo/res/`), its
project file, and the integration examples. Each entrypoint is mapped to the
task + fixture that exercises it (the BW-00-02-04 login drive, the BW-00-04-03
entity/db resource fixture, the BW-04-01-01 def parse) or to a concrete gap.

Every row counts the real files under its path so the index is grounded in the
tree, not asserted.

Usage: script_resource_index.py BIGWORLD_ROOT OUTPUT_JSON
"""
import json
import os
import sys

# (path-rel-to-BWROOT, kind, task, fixture, status, note)
ENTRIES = [
    ("game/res/bigworld/scripts/common/bw_site.py", "公共代码", "BW-00-03-03",
     "bw00-04-02-python-callback-fixtures", "covered",
     "嵌入 Python 宿主 site 钩子（解释器启动面）"),
    ("game/res/bigworld/scripts/common/Lib", "公共代码", "BW-00-03-03",
     "bw00-04-02-python-callback-fixtures", "covered",
     "Python 标准库 stage（嵌入 sys.path）"),
    ("game/res/bigworld/scripts/common/site-packages", "公共代码", "BW-00-03-03",
     "bw00-04-02-python-callback-fixtures", "covered", "site-packages stage"),
    ("game/res/bigworld/scripts/server_common/lib-dynload-el7", "公共代码",
     "BW-00-03-03", "bw00-04-02-python-callback-fixtures", "covered",
     "动态扩展模块 stage"),
    ("game/res/bigworld/scripts/server_common/site-packages", "公共代码",
     "BW-00-03-03", "bw00-04-02-python-callback-fixtures", "covered",
     "site-packages stage"),
    ("programming/fantasydemo/res/scripts/base", "FantasyDemo 实体脚本",
     None, None, "gap", "独立游戏资产；本机证据均取自 simple_python_client "
     "示例（BW-00-02-04/BW-00-04-03），FantasyDemo 未被任何证据练习"),
    ("programming/fantasydemo/res/scripts/cell", "FantasyDemo 实体脚本",
     None, None, "gap", "独立游戏资产；本机证据均取自 simple_python_client "
     "示例（BW-00-02-04/BW-00-04-03），FantasyDemo 未被任何证据练习"),
    ("programming/fantasydemo/res/scripts/client", "FantasyDemo 实体脚本",
     None, None, "gap", "独立游戏资产；本机证据均取自 simple_python_client "
     "示例（BW-00-02-04/BW-00-04-03），FantasyDemo 未被任何证据练习"),
    ("programming/fantasydemo/res/scripts/entity_defs", "FantasyDemo 实体定义",
     None, None, "gap", "独立游戏资产（Avatar.def 等）；未被任何证据练习"),
    ("programming/fantasydemo/res/scripts/db.xml", "FantasyDemo 配置",
     None, None, "gap", "独立游戏资产；未被任何证据练习"),
    ("programming/fantasydemo/res/scripts/entities.xml", "FantasyDemo 配置",
     None, None, "gap", "独立游戏资产；未被任何证据练习"),
    ("programming/fantasydemo/res/server", "FantasyDemo 配置", None, None,
     "gap", "独立游戏资产；未被任何证据练习"),
    ("programming/fantasydemo/fantasydemo.wpr", "FantasyDemo 项目", None, None,
     "gap", "工作区项目文件；无对应编辑器（Windows 面）"),
    ("programming/bigworld/examples/client_integration/python/simple", "示例",
     "BW-00-02-04", "bw00-02-04-client-login", "covered",
     "登录驱动与实体脚本"),
    ("programming/bigworld/examples/client_integration/c_plus_plus/android",
     "示例", None, None, "gap", "客户端 C++ 示例（Android）；无对应实现"),
    ("programming/bigworld/examples/client_integration/c_plus_plus/ios",
     "示例", None, None, "gap", "客户端 C++ 示例（iOS）；无对应实现"),
    ("programming/bigworld/examples/cellapp_extension", "示例", "BW-00-01-01",
     "bw00-01-build-targets", "gap",
     "cellapp 扩展示例；参考侧已盘点，无运行时练习证据"),
]


def count_files(root, path):
    full = os.path.join(root, path)
    if os.path.isfile(full):
        return 1
    if not os.path.isdir(full):
        return 0
    n = 0
    for dirpath, dirnames, filenames in os.walk(full):
        dirnames[:] = sorted(d for d in dirnames if d != "__pycache__")
        n += len(filenames)
    return n


def main():
    if len(sys.argv) < 3:
        print(__doc__)
        return 1
    root, out = sys.argv[1:3]

    rows = []
    for path, kind, task, fixture, status, note in ENTRIES:
        rows.append({
            "entry": os.path.basename(path.rstrip("/")),
            "dir": path, "kind": kind, "files": count_files(root, path),
            "task": task or "未映射", "fixture": fixture or "—",
            "status": status, "note": note,
        })

    counts = {"rows": len(rows), "covered": sum(1 for r in rows
                                                if r["status"] == "covered"),
              "gap": sum(1 for r in rows if r["status"] == "gap"),
              "files": sum(r["files"] for r in rows), "kinds": {}}
    for r in rows:
        counts["kinds"][r["kind"]] = counts["kinds"].get(r["kind"], 0) + 1

    data = {"counts": counts, "rows": rows}
    with open(out, "w", encoding="utf-8") as fh:
        json.dump(data, fh, indent=1, sort_keys=True)

    print("rows %d  covered %d  gap %d  files %d" % (
        counts["rows"], counts["covered"], counts["gap"], counts["files"]))
    for r in rows:
        print("  %-12s %-52s %5d %-7s %s" % (
            r["kind"], r["dir"], r["files"], r["status"], r["task"]))
    return 0


if __name__ == "__main__":
    sys.exit(main())
