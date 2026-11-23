#!/usr/bin/env python3
"""Emit the BW-00-01-01 evidence markdown from the scanned TSV.

Usage: make_components_doc.py COMPONENTS_TSV > bw00-01-build-targets.md
The inventory table is regenerated from the TSV so the doc never disagrees
with the scan output.
"""
import csv
import sys

TYPE_LABEL = {
    "binary": "Binaries (Make `type := binary`)",
    "copy_of_binary": "Copied binaries (Make `type := copy_of_binary`)",
    "library": "Libraries (Make `type := library`)",
    "shared_library": "Shared libraries (Make `type := shared_library`)",
    "script_copy": "Script copies registered against `BW_BINS` directly",
    "unit_test": "Unit tests (Make `type := unit_test`)",
}

SECTION_NOTE = {
    "unit_test": "Each unit test is the `unit_test_<name>` target of its component; the "
    "`includeUnitTestDirectory` hook pulls the `unit_test/` rules in.",
}


def esc(s):
    return s.replace("|", "\\|")


def main():
    tsv = sys.argv[1]
    rows = list(csv.DictReader(open(tsv, encoding="utf-8"), delimiter="\t"))
    order = [
        "binary", "copy_of_binary", "library", "shared_library",
        "script_copy", "unit_test",
    ]
    total_built = sum(1 for r in rows if r["in_build"] == "1")
    print("# BW-00-01-01 Linux Make 构建目标清册")
    print()
    print("> 证据面（BW-00-01-01 验收：清册包含二进制、库及所属规则）。")
    print()
    print("扫描自参考 `programming/bigworld` 的 Make 构建（`build/make/Makefile` 入口），")
    print("由 `tools/make_component_scan.py` 提取：遍历 `Makefile*`，按 `name :=` / `type :=`")
    print("登记每个组件，用 `subdirs =` 递归图做可达性判断（`in_build`）。")
    print()
    print(f"共 {len(rows)} 个声明组件，其中 {total_built} 个在当前构建图内；"
          "png / zip 是 vcpkg 迁移后残留的空壳（不再由 Make 构建）。")
    print()
    print("聚合规则（`build/make/Makefile` 的 `depsForRuleAll`）：`bw-libraries`、"
          "`bw-binaries`、`third-party-libraries`、`bw-miscellaneous`，可选 "
          "`bw-unit-tests`（`user_shouldBuildUnitTests`）与 `bw-run-unit-tests`"
          "（`user_shouldRunUnitTests`）。")
    print()
    print("安装布局（`BW_*_BIN_DIR`，`build/make/Makefile:238-247`）："
          "`bin/server/<platform>_<config>/server`（角色二进制）、"
          "`.../server/commands`（`remove_db` 等一次性命令）、"
          "`.../server/commands/_helpers`（`bw-miscellaneous` 的 Python 模块 16 个："
          "twisted、sqlalchemy、msgpack、pika、setuptools 等 `python_*` 目标）、"
          "`.../server/tools`（工具与 `.so`）。")
    print()
    print("与实盘二进制差分（`game/bin/server/el7/`）：`server/` 下 baseapp、baseappmgr、"
          "bots、cellapp、cellappmgr、dbapp、dbappmgr、loginapp、reviver、serviceapp 与清册"
          "一致；`server/commands/` 下 clear_auto_load、consolidate_dbs、remove_db、sync_db、"
          "transfer_db 与清册一致（`snapshot_helper` 已声明但该检出未装）；"
          "`server/cellapp-extensions/` 与 `server/dbapp-extensions/` 的三个 `.so` "
          "（cellapp_example、bwengine_mysql、bwengine_xml）与清册一致；`tools/` 下 "
          "bwmachined2、message_logger、process_defs、res_packer、`_bw_profile.so`、"
          "`_bwlog.so` 与清册一致。")
    print()
    for t in order:
        group = [r for r in rows if r["type"] == t]
        if not group:
            continue
        print(f"## {TYPE_LABEL[t]}（{len(group)}）")
        print()
        if t in SECTION_NOTE:
            print(SECTION_NOTE[t])
            print()
        print("| 目标 | 目录 | 构建内 | 源数 | 特性 | 所属规则 |")
        print("| --- | --- | --- | --- | --- | --- |")
        for r in sorted(group, key=lambda x: (x["in_build"] != "1", x["dir"])):
            print(
                f"| `{esc(r['name'])}` | `{esc(r['dir'])}` | "
                f"{'是' if r['in_build'] == '1' else '否'}"
                f" | {r['sources']} | {esc(r['flags']) or '—'} "
                f"| `{esc(r['rules'])}` |"
            )
        print()
    print("## 条件与特别情况")
    print()
    print("- **cellappmgr**：`server/Makefile.rules` 里 `LICENSE_SERVER` 门——`cellappmgr` 只在 "
          "`server/cellappmgr/Makefile` **缺失**时被 `filter-out`（此时 `LICENSE_SERVER=1`）；"
          "文件在则照常构建。本检出库该文件在，故 cellappmgr 在构建内。")
    print("- **examples**：`programming/bigworld/Makefile.rules` 以 "
          "`ifneq ($(user_shouldBuildExamples),0)` 门控（缺省=构建）；`examples/` 目标因此按默认"
          "计入 `in_build`。")
    print("- **remove_db**：`server/tools/remove_db` 是一次性 Makefile，无 `type`，直接 "
          "`BW_BINS += $(name)` 拷贝脚本，`destFile` 走 `$(BW_COMMAND_BIN_DIR)`。")
    print("- **message_logger**：`Makefile.rules` 是元 makefile，实际组件在 "
          "`Makefile.message_logger`（binary `message_logger`）与 "
          "`Makefile.message_reader`（shared_library `_bwlog`）。")
    print("- **third-party**：仅 `CppUnitLite2/src` 在树内构建；curl / mysql / mongodb / "
          "openssl / python / sdl / json / vcpkg 由 `build/make/third_party_*.mak` 引入，"
          "jsoncpp / png / zip(zlib) 走 `vcpkg.json`。属 BW-00-01-04。")
    print("- **serviceapp**：`server/baseapp/Makefile.serviceapp` 声明 "
          "`type := copy_of_binary`，与 `baseapp` 同目录、不同目标名。")
    print()
    print("## 计数")
    print()
    print("| 类型 | 声明 | 构建内 |")
    print("| --- | --- | --- |")
    for t in order:
        group = [r for r in rows if r["type"] == t]
        if not group:
            continue
        print(f"| {t} | {len(group)} | {sum(1 for r in group if r['in_build']=='1')} |")
    print()
    print("生成：`make_component_scan.py` + `make_components_doc.py`。")
    return 0


if __name__ == "__main__":
    sys.exit(main())
