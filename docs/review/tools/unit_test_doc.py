#!/usr/bin/env python3
"""Emit the BW-00-02-02 evidence markdown from the unit-test run JSON.

Usage: unit_test_doc.py UNIT_TESTS_JSON > bw00-02-02-unit-tests.md
The results table is regenerated from the JSON so the doc never disagrees with
the run output.
"""
import json
import sys

NOTE = {
    "entitydef": "最先失败的断言是 `DataType_const_iterator_FIXED_DICT_*` / "
    "`DataType_persistent_only_*`（`test_datatype_const_iterator.cpp:280/283`、"
    "`test_datatype_persistent_only_const_iterator.cpp:280/283`）——固定字典 / "
    "持久类型包装器注册表为空；随后 `Script_ScriptDataSink_UDO_REFTest`、"
    "`Script_ScriptDataSink_PyFixedDict_as_*`（`test_script_data_sink.cpp:514/519/521/1118/1168`）"
    "断言失败，末了 SIGABRT（rc=-6）。",
    "network": "无任何输出即忙转（`/proc/<pid>/state = R`，100% CPU，两个 UDP 套接字，"
    "`net.core.rmem_max` 已按 CI 提到 16 MiB 仍复现）——本机基线挂起（180s 有界超时截断）。",
    "pyscript": "4 个导入断言：`Python_cPickleTest`（`test_extensions.cpp:15`）、"
    "`Python_zlibTest`（:55）、`Python_ImportHashlibModuleTest`（:72）、"
    "`PassSTLVectorToPythonTest`（`test_python_and_stl.cpp:13`）。导入的是测试脚本"
    " `test_cpickle.py`/`test_zlib.py`/`test_hashlib.py`/`test_stl.py`（均在 "
    "`lib/pyscript/unit_test/res/`），它们再导入标准库 `pickle`/`zlib`/`hashlib`——"
    "嵌入解释器经 sys.path 找不到标准库（`scripts/common/Lib` + "
    "`scripts/server_common/lib-dynload-el7`，stage 在 `game/res/`），断言 `module` 为空后 "
    "SIGSEGV（rc=-11）。",
    "resmgr": "`PackedSection_*`（`test_packed_section.cpp`，`fixture_ != NULL`）与 "
    "`DataSection_splitSaveAsFileNameViaSaveTest`（`test_datasection.cpp:594`）断言失败后 "
    "SIGSEGV（rc=-11）。复跑验证：`--root` 在列则失败；去掉 `--root` 并设 "
    "`BW_RES_PATH=<unit_test>/res` 则 rc=0——属 `--root` 搜索路径分支的差异，非代码缺陷。",
    "script": "`ScriptModule_ReloadTest`（`test_scriptmodule.cpp:64`）断言 `module` 导入失败后 "
    "SIGSEGV（rc=-11）——与 pyscript 同因：`test_script.py` 导入标准库失败。",
}


def main():
    with open(sys.argv[1], encoding="utf-8") as fh:
        results = json.load(fh)
    passed = sum(1 for r in results if r["status"] == "pass")

    print("# BW-00-02-02 原版单测复跑")
    print()
    print("> 证据面（BW-00-02-02 验收：保存参编列表、执行命令及结果）。")
    print()
    print("复跑参考仓 `cuihairu/BigWorld` 的 21 个单测二进制"
          "（`game/bin/server/el7/unit_tests/`），命令对齐 `build/make/Makefile:382-407` "
          "的 `bw-run-all-unit-tests`：`cd <组件源目录>; timeout 1800 <bin> --root "
          "$(BW_ROOT)`（本复跑把 1800s 换成 180s 有界超时，只影响挂起项）。"
          "由 `tools/unit_test_run.py` 执行；源数据 `reference/bw00_02_02_unit_tests.json`。")
    print()
    print(f"**结果：{passed}/{len(results)} 通过，{len(results) - passed} 失败/挂起**。")
    print()
    print("## 参编列表与结果")
    print()
    print("| 模块 | 源目录 | 状态 | 退出码 | 耗时 |")
    print("| --- | --- | --- | --- | --- |")
    for r in results:
        print(f"| `{r['name']}_test` | `{r['src_dir']}` | "
              f"{'✅' if r['status'] == 'pass' else '❌' if r['status'] == 'fail' else '⏱'}"
              f" {r['status']} | {r['returncode'] if r['returncode'] is not None else '—'} "
              f"| {r['seconds']:.3f}s |")
    print()
    print("`--root $(BW_ROOT)` 解析到 `programming`（Makefile:16）；"
          "参编列表即 BW-00-01-01 的 21 个 `unit_test` 组件，与安装树"
          " `unit_tests/` 下 21 个 `*_test` 二进制一一对应。")
    print()
    print("## 失败项特征")
    print()
    for r in results:
        if r["status"] != "pass":
            print(f"- **{r['name']}**（rc={r['returncode'] if r['returncode'] is not None else 'N/A'}，"
                  f"{r['seconds']:.3f}s）：{NOTE[r['name']]}")
            print(f"  - 命令：`{r['command']}`（cwd `{r['cwd']}`）")
    print()
    print("## 与本机既有痕迹的印证")
    print()
    print("安装树 `unit_tests/` 下已有 `assert.network_test.coding.*.log` 与 "
          "`assert.pyscript_test.coding.*.log` 断言转储——这两项在本机复跑前就曾"
          "产出过崩溃转储，说明上述失败非本次运行新引入。")
    print()
    print("## 失败项溯源")
    print()
    print("- **resmgr（可归因）**：`base_resmgr_unit_test_harness.cpp:39-100` 在无 `--res` 时"
          "自行构造搜索路径——`--root` 分支拼 `<root>/programming/bigworld/lib/<test>/"
          "unit_test/res` 与 `<root>/game/res/bigworld`；实测 `--root` 在列即 SIGSEGV，"
          "去掉 `--root`、改设 `BW_RES_PATH=<unit_test>/res` 则全部通过（2.6s）。"
          "夹具（`*.zip`/`*.cdata`）都在 `unit_test/res/`，故为搜索路径差异，非代码缺陷。")
    print("- **pyscript / script（可归因）**：测试导入的 `test_*.py` 与标准库 stage 位置"
          "（`game/res/bigworld/scripts/common/Lib`、`scripts/server_common/lib-dynload-el7`，"
          "共 59 个 dynload 模块）都存在，且源目录留有 `__pycache__`（此前导入成功过）；"
          "但 `--root` 场景下 sys.path 未解析到它们（追加 `BW_RES_PATH` 复跑无变化）。")
    print("- **entitydef**：固定字典 / 持久类型包装器注册表为空起的断言失败——"
          "脚本数据槽注册依赖 Python 运行时初始化成功，与 pyscript 同源的可能性高。")
    print("- **network（本机挂起）**：CI 在测试前 `sysctl -w net.core.rmem_max=16777216`"
          "（`lib/network/unit_test/test_config.cpp:20` 断言 16 MiB `SO_RCVBUF`）；"
          "本机 `rmem_max` 已为 16 MiB 仍忙转，故挂起另有原因（见失败项特征）。")
    print()
    print("生成：`unit_test_run.py` + `unit_test_doc.py`。")
    return 0


if __name__ == "__main__":
    sys.exit(main())
