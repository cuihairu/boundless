# BW-00-02-02 原版单测复跑

> 证据面（BW-00-02-02 验收：保存参编列表、执行命令及结果）。

复跑参考仓 `cuihairu/BigWorld` 的 21 个单测二进制（`game/bin/server/el7/unit_tests/`），命令对齐 `build/make/Makefile:382-407` 的 `bw-run-all-unit-tests`：`cd <组件源目录>; timeout 1800 <bin> --root $(BW_ROOT)`（本复跑把 1800s 换成 180s 有界超时，只影响挂起项）。由 `tools/unit_test_run.py` 执行；源数据 `reference/bw00_02_02_unit_tests.json`。

**结果：16/21 通过，5 失败/挂起**。

## 参编列表与结果

| 模块 | 源目录 | 状态 | 退出码 | 耗时 |
| --- | --- | --- | --- | --- |
| `baseapp_test` | `server/baseapp/unit_test` | ✅ pass | 0 | 9.967s |
| `baseappmgr_test` | `server/baseappmgr/unit_test` | ✅ pass | 0 | 0.018s |
| `cellapp_test` | `server/cellapp/unit_test` | ✅ pass | 0 | 0.197s |
| `cellappmgr_test` | `server/cellappmgr/unit_test` | ✅ pass | 0 | 0.020s |
| `cstdmf_test` | `lib/cstdmf/unit_test` | ✅ pass | 0 | 18.936s |
| `db_test` | `lib/db/unit_test` | ✅ pass | 0 | 0.274s |
| `dbapp_test` | `server/dbapp/unit_test` | ✅ pass | 0 | 0.019s |
| `dbappmgr_test` | `server/dbappmgr/unit_test` | ✅ pass | 0 | 0.027s |
| `entitydef_test` | `lib/entitydef/unit_test` | ❌ fail | -6 | 3.141s |
| `loginapp_test` | `server/loginapp/unit_test` | ✅ pass | 0 | 0.031s |
| `math_test` | `lib/math/unit_test` | ✅ pass | 0 | 0.069s |
| `network_test` | `lib/network/unit_test` | ⏱ timeout | — | 180.110s |
| `physics2_test` | `lib/physics2/unit_test` | ✅ pass | 0 | 0.027s |
| `pyscript_test` | `lib/pyscript/unit_test` | ❌ fail | -11 | 0.091s |
| `resmgr_test` | `lib/resmgr/unit_test` | ❌ fail | -11 | 0.096s |
| `reviver_test` | `server/reviver/unit_test` | ✅ pass | 0 | 0.020s |
| `scene_test` | `lib/scene/unit_test` | ✅ pass | 0 | 0.021s |
| `script_test` | `lib/script/unit_test` | ❌ fail | -11 | 0.156s |
| `server_test` | `lib/server/unit_test` | ✅ pass | 0 | 0.066s |
| `terrain_test` | `lib/terrain/unit_test` | ✅ pass | 0 | 1.221s |
| `unit_test_lib_test` | `lib/unit_test_lib/unit_test` | ✅ pass | 0 | 15.034s |

`--root $(BW_ROOT)` 解析到 `programming`（Makefile:16）；参编列表即 BW-00-01-01 的 21 个 `unit_test` 组件，与安装树 `unit_tests/` 下 21 个 `*_test` 二进制一一对应。

## 失败项特征

- **entitydef**（rc=-6，3.141s）：最先失败的断言是 `DataType_const_iterator_FIXED_DICT_*` / `DataType_persistent_only_*`（`test_datatype_const_iterator.cpp:280/283`、`test_datatype_persistent_only_const_iterator.cpp:280/283`）——固定字典 / 持久类型包装器注册表为空；随后 `Script_ScriptDataSink_UDO_REFTest`、`Script_ScriptDataSink_PyFixedDict_as_*`（`test_script_data_sink.cpp:514/519/521/1118/1168`）断言失败，末了 SIGABRT（rc=-6）。
  - 命令：`/home/cui/workspaces/BigWorld/game/bin/server/el7/unit_tests/entitydef_test --root /home/cui/workspaces/BigWorld/programming`（cwd `/home/cui/workspaces/BigWorld/programming/bigworld/lib/entitydef/unit_test`）
- **network**（rc=N/A，180.110s）：无任何输出即忙转（`/proc/<pid>/state = R`，100% CPU，两个 UDP 套接字，`net.core.rmem_max` 已按 CI 提到 16 MiB 仍复现）——本机基线挂起（180s 有界超时截断）。
  - 命令：`/home/cui/workspaces/BigWorld/game/bin/server/el7/unit_tests/network_test --root /home/cui/workspaces/BigWorld/programming`（cwd `/home/cui/workspaces/BigWorld/programming/bigworld/lib/network/unit_test`）
- **pyscript**（rc=-11，0.091s）：4 个导入断言：`Python_cPickleTest`（`test_extensions.cpp:15`）、`Python_zlibTest`（:55）、`Python_ImportHashlibModuleTest`（:72）、`PassSTLVectorToPythonTest`（`test_python_and_stl.cpp:13`）。导入的是测试脚本 `test_cpickle.py`/`test_zlib.py`/`test_hashlib.py`/`test_stl.py`（均在 `lib/pyscript/unit_test/res/`），它们再导入标准库 `pickle`/`zlib`/`hashlib`——嵌入解释器经 sys.path 找不到标准库（`scripts/common/Lib` + `scripts/server_common/lib-dynload-el7`，stage 在 `game/res/`），断言 `module` 为空后 SIGSEGV（rc=-11）。
  - 命令：`/home/cui/workspaces/BigWorld/game/bin/server/el7/unit_tests/pyscript_test --root /home/cui/workspaces/BigWorld/programming`（cwd `/home/cui/workspaces/BigWorld/programming/bigworld/lib/pyscript/unit_test`）
- **resmgr**（rc=-11，0.096s）：`PackedSection_*`（`test_packed_section.cpp`，`fixture_ != NULL`）与 `DataSection_splitSaveAsFileNameViaSaveTest`（`test_datasection.cpp:594`）断言失败后 SIGSEGV（rc=-11）。复跑验证：`--root` 在列则失败；去掉 `--root` 并设 `BW_RES_PATH=<unit_test>/res` 则 rc=0——属 `--root` 搜索路径分支的差异，非代码缺陷。
  - 命令：`/home/cui/workspaces/BigWorld/game/bin/server/el7/unit_tests/resmgr_test --root /home/cui/workspaces/BigWorld/programming`（cwd `/home/cui/workspaces/BigWorld/programming/bigworld/lib/resmgr/unit_test`）
- **script**（rc=-11，0.156s）：`ScriptModule_ReloadTest`（`test_scriptmodule.cpp:64`）断言 `module` 导入失败后 SIGSEGV（rc=-11）——与 pyscript 同因：`test_script.py` 导入标准库失败。
  - 命令：`/home/cui/workspaces/BigWorld/game/bin/server/el7/unit_tests/script_test --root /home/cui/workspaces/BigWorld/programming`（cwd `/home/cui/workspaces/BigWorld/programming/bigworld/lib/script/unit_test`）

## 与本机既有痕迹的印证

安装树 `unit_tests/` 下已有 `assert.network_test.coding.*.log` 与 `assert.pyscript_test.coding.*.log` 断言转储——这两项在本机复跑前就曾产出过崩溃转储，说明上述失败非本次运行新引入。

## 失败项溯源

- **resmgr（可归因）**：`base_resmgr_unit_test_harness.cpp:39-100` 在无 `--res` 时自行构造搜索路径——`--root` 分支拼 `<root>/programming/bigworld/lib/<test>/unit_test/res` 与 `<root>/game/res/bigworld`；实测 `--root` 在列即 SIGSEGV，去掉 `--root`、改设 `BW_RES_PATH=<unit_test>/res` 则全部通过（2.6s）。夹具（`*.zip`/`*.cdata`）都在 `unit_test/res/`，故为搜索路径差异，非代码缺陷。
- **pyscript / script（可归因）**：测试导入的 `test_*.py` 与标准库 stage 位置（`game/res/bigworld/scripts/common/Lib`、`scripts/server_common/lib-dynload-el7`，共 59 个 dynload 模块）都存在，且源目录留有 `__pycache__`（此前导入成功过）；但 `--root` 场景下 sys.path 未解析到它们（追加 `BW_RES_PATH` 复跑无变化）。
- **entitydef**：固定字典 / 持久类型包装器注册表为空起的断言失败——脚本数据槽注册依赖 Python 运行时初始化成功，与 pyscript 同源的可能性高。
- **network（本机挂起）**：CI 在测试前 `sysctl -w net.core.rmem_max=16777216`（`lib/network/unit_test/test_config.cpp:20` 断言 16 MiB `SO_RCVBUF`）；本机 `rmem_max` 已为 16 MiB 仍忙转，故挂起另有原因（见失败项特征）。

生成：`unit_test_run.py` + `unit_test_doc.py`。
