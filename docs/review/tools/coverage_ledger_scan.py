#!/usr/bin/env python3
"""Build the functional-coverage ledger (BW-00-05-01 fields).

The ledger is one row per build target in the reference inventory (the Make
components from BW-00-01-01, and the CMake target groups from BW-00-01-02),
each row carrying the five ledger fields:

  * target  - the original target (name / group) and its directory;
  * enable  - the enable condition (Make feature flags + ifeq guard, or the
    CMake group's defines / feature switches);
  * task    - the BW task number whose evidence covers this target (the
    mapping below is explicit and traceable to the evidence doc that exists);
  * fixture - the verification fixture / evidence doc that exercises it;
  * status  - covered / partial / gap, with the gap source when not covered.

The coverage mapping is deliberate: a target is `covered` only where a
captured evidence doc actually exercises its surface (the machined protocol,
the entity def parse, the login drive, the stack comparison), `partial` where
the new component exists but only reaches part of the reference behaviour
(process startup for the server roles), and `gap` where no evidence or task
exists yet. Nothing is claimed covered without an evidence file behind it.

Usage: coverage_ledger_scan.py BIGWORLD_ROOT CMAKE_TARGETS_JSON OUTPUT_JSON
"""
import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import make_component_scan as make_scan

# (dir prefix, task, fixture, status, note) - longest prefix wins.
COVERAGE = [
    ("server/baseapp", "BW-00-03-05", "bw00-04-04-stack-comparison", "partial",
     "角色入口已实现（进程启动）；深度 init 链 BW-01-04"),
    ("server/baseappmgr", "BW-00-03-05", "bw00-04-04-stack-comparison", "partial",
     "角色入口已实现（进程启动）；深度 init 链 BW-01-04"),
    ("server/cellapp", "BW-00-03-05", "bw00-04-04-stack-comparison", "partial",
     "角色入口已实现（进程启动）；深度 init 链 BW-01-04"),
    ("server/cellappmgr", "BW-00-03-05", "bw00-04-04-stack-comparison", "partial",
     "角色入口已实现（进程启动）；深度 init 链 BW-01-04"),
    ("server/dbapp", "BW-00-03-05", "bw00-04-04-stack-comparison", "partial",
     "角色入口已实现（进程启动）；深度 init 链 BW-01-04"),
    ("server/dbappmgr", "BW-00-03-05", "bw00-04-04-stack-comparison", "partial",
     "角色入口已实现（进程启动）；深度 init 链 BW-01-04"),
    ("server/loginapp", "BW-00-03-05", "bw00-04-04-stack-comparison", "partial",
     "角色入口已实现（进程启动）；深度 init 链 BW-01-04"),
    ("server/reviver", "BW-00-03-05", "bw00-04-04-stack-comparison", "partial",
     "角色入口已实现（进程启动）；深度 init 链 BW-01-04"),
    ("server/tools/bwmachined", "BW-00-04-01", "bw00-04-01-protocol-fixtures",
     "covered", "machined 发现协议"),
    ("server/tools/bots", "BW-00-01-01", "bw00-01-build-targets", "gap",
     "bots 负载客户端，尚无对应实现与证据"),
    ("server/dbapp_extensions/bwengine_mysql", "BW-00-04-03",
     "bw00-04-03-entity-db-fixtures", "covered",
     "MySQL 存储扩展（DB 证据为 MySQL 路径）"),
    ("server/dbapp_extensions/bwengine_xml", "BW-00-01-01",
     "bw00-01-build-targets", "gap",
     "XML 存储扩展；参考侧已盘点，无运行时证据（DB 证据为 MySQL 路径）"),
    ("lib/network", "BW-00-04-01", "bw00-04-01-protocol-fixtures", "covered",
     "machined 线路协议（MGMPacket/machine_guard）"),
    ("lib/entitydef", "BW-04-01-01", "bw04-01-01-entitydef-parse", "covered",
     "实体定义解析"),
    ("lib/db", "BW-00-04-03", "bw00-04-03-entity-db-fixtures", "covered",
     "数据库表/行与实体持久化"),
    ("lib/pyscript", "BW-00-03-03", "bw00-04-02-python-callback-fixtures",
     "covered", "Python 主机与回调序列"),
    ("lib/resmgr", "BW-00-02-01", "bw00-02-01-runtime-env", "covered",
     "资源路径解析与 per-env 配置"),
    ("lib/connection", "BW-00-02-04", "bw00-02-04-client-login", "covered",
     "客户端登录连接"),
    ("lib/connection_model", "BW-00-02-04", "bw00-02-04-client-login", "covered",
     "连接模型"),
    ("lib/server", "BW-01-04", "bw00-04-04-stack-comparison", "partial",
     "ServerApp/ServerAppConfig 已建；全量 init 链 BW-01-04"),
    ("lib/cstdmf", None, None, "gap",
     "基础部件；新版由 crates/* 分散替代，尚无逐项映射证据"),
    ("lib/script", "BW-00-03-03", "bw00-04-02-python-callback-fixtures",
     "covered", "脚本绑定"),
    ("lib/db_storage", "BW-00-04-03", "bw00-04-03-entity-db-fixtures", "covered",
     "DB 存储抽象"),
    ("tools/process_defs", "BW-00-02-01", "bw00-02-01-runtime-env", "covered",
     "运行时环境与进程定义"),
    ("tools/res_packer", "BW-00-01-01", "bw00-01-build-targets", "gap",
     "资源打包工具，尚无对应实现与证据"),
    ("examples/client_integration/python/simple", "BW-00-02-04",
     "bw00-02-04-client-login", "covered", "登录驱动与实体脚本"),
    ("examples/cellapp_extension", "BW-00-01-01", "bw00-01-build-targets", "gap",
     "参考侧 cellapp_example.so 已盘点；新版尚无 cellapp 扩展加载面，"
     "无运行时练习证据"),
    ("third_party/CppUnitLite2", "BW-00-02-02", "bw00-02-02-unit-tests",
     "covered", "单测框架"),
    ("third_party/png", "BW-00-01-01", "bw00-01-build-targets", "gap",
     "vcpkg 迁移后残留空壳，不再由 Make 构建"),
    ("third_party/zip", "BW-00-01-01", "bw00-01-build-targets", "gap",
     "vcpkg 迁移后残留空壳，不再由 Make 构建"),
]
UNIT_TEST_SUFFIX = "/unit_test"

# Target-name overrides: a name carries evidence of its own even when it shares
# a directory with another component (the serviceapp copy_of_binary is built in
# server/baseapp, but the new stack rejects that role - BW-00-04-05).
TARGET_OVERRIDE = {
    "serviceapp": ("BW-00-04-05", "bw00-04-05-substitution-run", "gap",
                   "新版 boundless-server 尚未接受该角色（BW-00-04-05 替换运行"
                   "为 rejected）"),
}

# Concrete gap source (来源 + 阻塞) for engine libs and server targets that
# have no task yet. BW-00-05-02 requires every such row to carry a real index
# entry, not the generic "unmapped" note. Keyed by dir.
GAP_SOURCES = {
    "lib/build": "构建期辅助库，无独立运行时验收面",
    "lib/chunk": "空间块（chunk）加载；新版尚无对应 crate，空间数据面未启动",
    "lib/chunk_loading": "空间块流式加载；新版尚无对应 crate，空间数据面未启动",
    "lib/db_storage_mysql": "MySQL 存储后端实现基座；运行时路径由 "
                            "dbapp_extensions/bwengine_mysql 承载（已映射）",
    "lib/db_storage_xml": "XML 存储后端；无运行时证据（DB 证据为 MySQL 路径）",
    "lib/delegate_interface": "内部接口基座，无独立验收面",
    "lib/entitydef_script": "脚本侧实体定义绑定；脚本绑定面未建",
    "lib/math": "数学基元；无对应 Rust crate",
    "lib/memhook": "内存挂钩；仅客户端/嵌入面",
    "lib/moo": "渲染管线；客户端渲染面未启动",
    "lib/physics2": "2D 物理；无对应 crate",
    "lib/scene": "场景管理；客户端/场景面未启动",
    "lib/speedtree": "植被渲染；第三方渲染面未启动",
    "lib/sqlite": "SQLite 封装；存储面未建",
    "lib/terrain": "地形；客户端/空间面未启动",
    "lib/unit_test_lib": "单测基座库；随 BW-00-02-02 单测基线（框架侧已覆盖）",
    "lib/urlrequest": "URL 请求（curl）；无对应 crate",
    "lib/waypoint": "寻路；无对应 crate",
    "server/tools/bots": "bots 负载客户端；无对应实现",
    "server/tools/bw_profile": "性能剖析（_bw_profile.so）；无对应实现",
    "server/tools/clear_auto_load": "DB 自动加载清理；DB 管理面未建",
    "server/tools/consolidate_dbs": "DB 合并；DB 管理面未建",
    "server/tools/message_logger": "日志服务器（message_logger + _bwlog）；"
                                  "无对应实现",
    "server/tools/remove_db": "删除 DB 脚本；DB 管理面未建",
    "server/tools/snapshot_helper": "DB 快照；DB 管理面未建",
    "server/tools/sync_db": "DB 同步；DB 管理面未建",
    "server/tools/transfer_db": "DB 迁移；DB 管理面未建",
}
GENERIC_GAP = "尚无对应实现与证据（未映射任务）"


def coverage_for(dir, target):
    if target in TARGET_OVERRIDE:
        return TARGET_OVERRIDE[target]
    # A unit_test dir always maps to the reference unit-test baseline
    # (BW-00-02-02), even when its parent library maps elsewhere.
    if dir.endswith(UNIT_TEST_SUFFIX):
        return "BW-00-02-02", "bw00-02-02-unit-tests", "covered", \
            "原版单测基线已复跑（21 项，16 通过）"
    for prefix, task, fixture, status, note in COVERAGE:
        if dir == prefix or dir.startswith(prefix + "/"):
            return task, fixture, status, note
    if dir in GAP_SOURCES:
        return None, None, "gap", "缺口：" + GAP_SOURCES[dir]
    return None, None, "gap", GENERIC_GAP


def main():
    if len(sys.argv) < 4:
        print(__doc__)
        return 1
    root, cmake_json, out_json = sys.argv[1:4]

    make_rows = []
    for r in make_scan.scan(root):
        task, fixture, status, note = coverage_for(r["dir"], r["name"])
        make_rows.append({
            "target": r["name"], "dir": r["dir"], "type": r["type"],
            "in_build": r["in_build"], "sources": r["sources"],
            "enable": (r.get("flags") or "—") + (
                (" ; guard: " + r["guard"]) if r.get("guard") else ""),
            "task": task or "未映射", "fixture": fixture or "—",
            "status": status, "note": note,
        })

    with open(cmake_json, encoding="utf-8") as fh:
        cmake = json.load(fh)
    cmake_rows = []
    for group, g in sorted(cmake.items()):
        defines = ", ".join(g.get("defines") or [])
        flags = ", ".join("%s=%s" % kv for kv in (g.get("flags") or {}).items())
        note = "Windows 构建面；本 Linux 主机无产物与 MSVC 工具链，由 GitHub " \
            "Actions windows-latest 承载（BW-00-02-05 复测）"
        if group == "server":
            note = "server 组 Linux 路径由 Make 承载（见 make 行）；" + note
        cmake_rows.append({
            "target": group,
            "dir": "build/cmake/BWConfiguration_%s.cmake" % group,
            "type": "cmake-target",
            "in_build": "1" if g.get("projects") else "0",
            "sources": len(g.get("projects") or []),
            "enable": "defines: %s ; flags: %s" % (defines or "—",
                                                   flags or "—"),
            "task": "BW-00-02-05", "fixture": "bw00-02-05-windows-baseline",
            "status": "gap",
            "note": note,
        })

    # BW-00-05-02 scope: every engine lib (lib/*) and server target
    # (server/*, server/tools/*) must carry a concrete index entry.
    scope = [r for r in make_rows
             if r["dir"].startswith("lib/") or r["dir"].startswith("server/")]
    generic = [r for r in scope if r["note"] == GENERIC_GAP]
    if generic:
        print("WARN engine/server generic gap: "
              + ", ".join(r["dir"] for r in generic))

    data = {
        "fields": ["target", "dir", "enable", "task", "fixture", "status"],
        "counts": {
            "make": len(make_rows),
            "make_in_build": sum(1 for r in make_rows if r["in_build"] == "1"),
            "cmake": len(cmake_rows),
            "covered": sum(1 for r in make_rows if r["status"] == "covered"),
            "partial": sum(1 for r in make_rows if r["status"] == "partial"),
            "gap": sum(1 for r in make_rows if r["status"] == "gap"),
            "engine_server": len(scope),
            "engine_server_indexed": len(scope) - len(generic),
        },
        "make": make_rows,
        "cmake": cmake_rows,
    }
    with open(out_json, "w", encoding="utf-8") as fh:
        json.dump(data, fh, indent=1, sort_keys=True)

    print("make %d (in_build %d)  cmake %d" % (
        data["counts"]["make"], data["counts"]["make_in_build"],
        data["counts"]["cmake"]))
    print("make status: covered %d  partial %d  gap %d" % (
        data["counts"]["covered"], data["counts"]["partial"],
        data["counts"]["gap"]))
    for r in make_rows:
        print("  %-24s %-9s %-7s %-10s %s" % (
            r["dir"], r["type"], r["status"], r["task"], r["fixture"]))
    return 0


if __name__ == "__main__":
    sys.exit(main())
