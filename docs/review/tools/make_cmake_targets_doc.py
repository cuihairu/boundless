#!/usr/bin/env python3
"""Emit the BW-00-01-02 evidence markdown from the scanned CMake JSON.

Usage: make_cmake_targets_doc.py CMAKE_TARGETS_JSON > bw00-01-cmake-targets.md
The per-target tables are regenerated from the JSON so the doc never disagrees
with the scan output.
"""
import json
import sys
from collections import defaultdict

BLOB_TARGETS = ("client_blob", "exporters_blob", "tools_blob")

GUARD_NOTE = {
    "BW_SPEEDTREE_SUPPORT": "`speedtree` 库（默认 OFF）",
    "BW_FMOD_SUPPORT": "`fmodsound` 库（默认 OFF）",
    "BW_SCALEFORM_SUPPORT": "`scaleform` 库（默认 OFF，仅 client）",
    "USE_MEMHOOK": "`memhook` 库（默认 OFF；tools 里 `SET( USE_MEMHOOK OFF )` 后 "
    "`IF( USE_MEMHOOK )` 才加 `-DUSE_MEMHOOK`）",
    "NOT MSVC": "`cellapp_extension` 只在 clang-cl 构建下编（非 MSVC 真身）",
    "NOT PNG_FOUND": "`libpng` 只在 `find_package(PNG)` 未命中时编",
    "NOT ZLIB_FOUND": "`zip` 只在 `find_package(ZLIB)` 未命中时编",
}


def esc(s):
    return s.replace("|", "\\|")


def target_row(t):
    libs = [p for p in t["projects"] if p[0] == "BW_LIBRARY_PROJECTS"]
    bins = [p for p in t["projects"] if p[0] == "BW_BINARY_PROJECTS"]
    flags = " ".join(f"{k}={v}" for k, v in sorted(t["flags"].items()))
    defines = ", ".join(t["defines"]) or "—"
    cond = "是" if t["consumer_release"] else "否"
    return f"| `{t['name']}` | {len(libs)} | {len(bins)} | {cond} | `{esc(defines)}` | {flags or '—'} |"


def main():
    path = sys.argv[1]
    with open(path, encoding="utf-8") as fh:
        targets = json.load(fh)

    print("# BW-00-01-02 CMake 与 Windows 目标清册")
    print()
    print("> 证据面（BW-00-01-02 验收：清册记录平台及启用宏）。")
    print()
    print("扫描自参考 `programming/bigworld` 的 CMake 构建（根 `CMakeLists.txt` + "
          "`build/cmake/BWConfiguration_*.cmake`），由 `tools/cmake_target_scan.py` 提取。")
    print()

    print("## 平台与生成方式")
    print()
    print("- **入口**：根 `CMakeLists.txt` 要求 `-DBW_CMAKE_TARGET=<name>`；从 "
          "`build/cmake/BWConfiguration_*.cmake` 文件名收集 `BW_AVAILABLE_TARGETS`，"
          "未命中即 `FATAL_ERROR`。")
    print("- **平台门**：`BW_IS_REMOTE_ONLY` 时为 Linux 远程构建"
          "（`BW_PLATFORM=${BW_REMOTE_PLATFORM}`，由 SSH 执行远端 "
          "`build/make/platform_info.py` 得到，恒为 64 位，工程名带 `_remote` 后缀）；"
          "`WIN32` 时按 `CMAKE_SIZEOF_VOID_P` 取 `win32` / `win64`；其余平台 "
          "`FATAL_ERROR \"Only Windows builds are currently supported.\"`。"
          "即 **CMake 面只在 Windows 上生成**，Linux 只走 Make 面。")
    print("- **编译器 token**：`MSVC9..MSVC14` 映射 `vc9..vc14`；"
          "`MSVC AND CMAKE_C_COMPILER_ID MATCHES Clang` 时置 `CLANG_CL`。")
    print("- **工程名**：`${BW_CMAKE_TARGET}_${BW_PLATFORM}`（远程加 `_remote`），"
          "输出目录 `game/bin`、`game/res/bigworld`。")
    print("- **配置类型**：`CMAKE_CONFIGURATION_TYPES Debug;Hybrid`（`Hybrid` = /MD 无 "
          "_DEBUG 的 release 风味）；`CMAKE_MAP_IMPORTED_CONFIG_HYBRID \"Release;\"`"
          " 把 `Hybrid` 的导入库映射到 Release。`client` 与 `bwentity` 额外调 "
          "`BW_ADD_CONSUMER_RELEASE_CONFIG`，追加 `Consumer_Release`。")
    print("- **BW_IS_&lt;TARGET&gt;**：每个可用目标先置 `OFF`，选中的那个置 `ON`；"
          "剥 `_BLOB` 后缀后再比较，故 `client_blob` 与 `client` 共用 `BW_IS_CLIENT`。")
    print()

    print("## 目标一览（11 个 BWConfiguration_*.cmake）")
    print()
    print("| 目标 | 库 | 可执行 | Consumer_Release | 编译宏 | 特性开关 |")
    print("| --- | --- | --- | --- | --- | --- |")
    for name in sorted(targets):
        t = targets[name]
        t["name"] = name
        print(target_row(t))
    print()
    print("库 / 可执行数为该目标 `BW_LIBRARY_PROJECTS` / `BW_BINARY_PROJECTS`"
          " 中登记的项目数（未去重，条件块内也会计入）。")
    print()

    print("## 库 / Blob 变体")
    print()
    print("| 目标 | 委托到 | 效果 |")
    print("| --- | --- | --- |")
    for name in BLOB_TARGETS:
        t = targets[name]
        print(f"| `{name}` | `{t['includes'][0]}` | `SET( BW_BLOB_CONFIG ON )` 后 "
              "`INCLUDE` 基座配置，本身不登记项目 |")
    print()
    print("`BW_BLOB_CONFIG` 让基座里的资源打包步骤产出 blob 而非散文件，"
          "三个变体与各自的基座共享 `BW_IS_<TARGET>` 宏。")
    print()

    print("## 条件项（IF 守卫下的项目登记）")
    print()
    seen = defaultdict(list)
    for name in sorted(targets):
        for p in targets[name]["projects"]:
            if p[3]:
                seen[p[3]].append(f"{name}:{p[1]}")
    print("| 守卫 | 受控项 |")
    print("| --- | --- |")
    for g in sorted(seen):
        note = GUARD_NOTE.get(g, "")
        print(f"| `{g}` | {esc(note)}（{'、'.join(sorted(seen[g]))}） |")
    print()
    print("根 `CMakeLists.txt` 的缺省特性开关：`BW_PYTHON_AS_SOURCE=ON`、"
          "`BW_PCH_SUPPORT=ON`；`USE_MEMHOOK`、`BW_FMOD_SUPPORT`、`BW_SCALEFORM_SUPPORT`、"
          "`BW_SPEEDTREE_SUPPORT`、`BW_UMBRA_SUPPORT`、`BW_AWESOMIUM_SUPPORT` 均为 `OFF`，"
          "各目标可自行覆盖（如 `client` 把 `USE_MEMHOOK` 置 `ON` 并加 `-DUSE_MEMHOOK`）。")
    print()
    print("另外：`BW_UNIT_TESTS_ENABLED`（缺省 `ON`）门控各目标的 `*_unit_test` 项目；"
          "server 目标额外带 `-DMF_SERVER -DMF_CONFIG=\"Hybrid\"`。")
    print()

    print("## 计数")
    print()
    print("| 目标 | 库 | 可执行 |")
    print("| --- | --- | --- |")
    for name in sorted(targets):
        t = targets[name]
        libs = sum(1 for p in t["projects"] if p[0] == "BW_LIBRARY_PROJECTS")
        bins = sum(1 for p in t["projects"] if p[0] == "BW_BINARY_PROJECTS")
        print(f"| `{name}` | {libs} | {bins} |")
    print()
    print("生成：`cmake_target_scan.py` + `make_cmake_targets_doc.py`。")
    return 0


if __name__ == "__main__":
    sys.exit(main())