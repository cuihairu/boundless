# BW-00-01-01 Linux Make 构建目标清册

**任务**: BW-00-01-01 展开 Linux Make 构建目标（todo.md 行 87）。**验收**: 清册包含二进制、库及所属规则。
**原版基准**: BigWorld 基线 `4657a17`，本机 el7 树（`game/bin/server/el7` 为其已参编快照）。

## 采集方法

1. **动态权威清单**：make 自身累加器在 recipe 期展开导出（`--eval` 先于 Makefile 读入求值，须用 recipe 而非 `$(info)`；顶层被注释掉的 `listrules` 目标因同一陷阱实际不可用）：
   ```
   cd BigWorld/programming && VCPKG_ROOT=~/vcpkg make --eval \
     'print: ; @echo "LIBS=$(BW_LIBS)"; echo "BINS=$(BW_BINS)"; echo "TESTS=$(BW_UNIT_TESTS)"; echo "MISC=$(BW_MISC)"; echo "TP=$(BW_THIRD_PARTY)"' print
   ```
2. **静态交叉核对**：全树 `Makefile*` 的 `name/type/bindir` 声明 + 各级 `Makefile.rules` 的 `subdirs` 扫描链（`programming/Makefile.rules` → `bigworld/Makefile.rules` 扫 `lib server third_party tools`（+条件 `examples`）→ 组件目录）。同名组件（如 `baseapp` 与其单测）按 type 分辨。
3. **磁盘参编核对**：`game/bin/server/el7/{server,tools,server/commands,unit_tests,examples}` 与 `build/el7/lib`。

## 顶层目标与条件开关

- `all` = `bw-libraries bw-binaries third-party-libraries bw-miscellaneous` +（`user_shouldBuildUnitTests != 0`）`bw-unit-tests` +（`user_shouldRunUnitTests = 1`）`bw-run-unit-tests`；`clean`/`veryclean`（加清三方）/`cleanall`（=veryclean 兼容别名）。
- 平台/配置：`platform_info.py` 判定（本机 el7）；`BW_CONFIG` hybrid（默认，目录名无后缀）/debug（`_debug` 后缀）；`MF_CONFIG` 可越权覆盖。
- 宏开关：`ENABLE_MEMTRACKER=1` → `-DENABLE_MEMTRACKER -DUSE_MEMHOOK`；`ENABLE_PROTECTED_ALLOCATOR=1` 同理；`shouldDefineMFServer`（客户端示例置 0 去掉 `-DMF_SERVER`）。
- 单测运行器：`bw-run-all-unit-tests`（`--root $(BW_ROOT)`、1800s 超时、不停失败）与 `bw-run-unit-tests`（首败即停，`UNIT_TEST_STOP_AT_FAILURE=1`）。

## 二进制（BW_BINS，21）

| 目标 | type | 所属规则 | bindir | 磁盘 |
|---|---|---|---|---|
| `baseapp` | binary | `bigworld/server/baseapp/Makefile.rules` | server/ | 已参编 |
| `serviceapp` | copy_of_binary | `bigworld/server/baseapp/Makefile.serviceapp` | server/ | 已参编 |
| `baseappmgr` | binary | `bigworld/server/baseappmgr/Makefile.rules` | server/ | 已参编 |
| `cellappmgr` | binary | `bigworld/server/cellappmgr/Makefile.rules` | server/ | 已参编 |
| `cellapp` | binary | `bigworld/server/cellapp/Makefile.rules` | server/ | 已参编 |
| `dbapp` | binary | `bigworld/server/dbapp/Makefile.rules` | server/ | 已参编 |
| `dbappmgr` | binary | `bigworld/server/dbappmgr/Makefile.rules` | server/ | 已参编 |
| `loginapp` | binary | `bigworld/server/loginapp/Makefile.rules` | server/ | 已参编 |
| `reviver` | binary | `bigworld/server/reviver/Makefile.rules` | server/ | 已参编 |
| `bwmachined2` | binary | `bigworld/server/tools/bwmachined/Makefile.rules` | tools/ | 已参编 |
| `bots` | binary | `bigworld/server/tools/bots/Makefile.rules` | server/ | 已参编 |
| `clear_auto_load` | binary | `bigworld/server/tools/clear_auto_load/Makefile.rules` | server/commands/ | 已参编 |
| `consolidate_dbs` | binary | `bigworld/server/tools/consolidate_dbs/Makefile.rules` | server/commands/ | 已参编 |
| `message_logger` | binary | `bigworld/server/tools/message_logger/Makefile.message_logger` | tools/ | 已参编 |
| `remove_db` | once-off copy | `bigworld/server/tools/remove_db/Makefile.rules` | server/commands/ | 已参编 |
| `snapshot_helper` | binary | `bigworld/server/tools/snapshot_helper/Makefile.rules` | server/commands/_helpers/ | 已参编 |
| `sync_db` | binary | `bigworld/server/tools/sync_db/Makefile.rules` | server/commands/ | 已参编 |
| `transfer_db` | binary | `bigworld/server/tools/transfer_db/Makefile.rules` | server/commands/ | 已参编 |
| `process_defs` | binary | `bigworld/tools/process_defs/Makefile.rules` | tools/（host 平台） | 已参编 |
| `res_packer` | binary | `bigworld/tools/res_packer/Makefile.rules` | tools/ | 已参编 |
| `simple_python_client` | binary | `bigworld/examples/client_integration/python/simple/Makefile.rules` | examples/ | 已参编 |

- `serviceapp` 是 `copy_of_binary`（`Makefile.serviceapp`，复用 baseapp 目标改宏），与 `baseapp` 经 `baseapp_and_serviceapp` 聚合目标并建。
- `remove_db` 为「一次性」规则：不走组件 name/type 流程，直接 `BW_BINS +=` 把**脚本本体拷贝**进 `server/commands/`（盘上 891 字节即其 Python 源拷贝）。
- `process_defs` 恒按 **host 平台**构建（`bwBuildPlatform = $(BW_HOST_PLATFORM)`、bindir 用 HOST 变体）——跨平台工具面的通用形态。

## 库（BW_LIBS，35：静态 .a 30 + 共享 .so 5）

| 库名 | type | 所属规则 | 产物位置 | 磁盘 |
|---|---|---|---|---|
| `libbuild` | library | `bigworld/lib/build/Makefile.rules` | build/el7/lib/libbuild.a | 已参编 |
| `libcstdmf` | library | `bigworld/lib/cstdmf/Makefile.rules` | build/el7/lib/libcstdmf.a | 已参编 |
| `libconnection` | library | `bigworld/lib/connection/Makefile.rules` | build/el7/lib/libconnection.a | 已参编 |
| `libconnection_model` | library | `bigworld/lib/connection_model/Makefile.rules` | build/el7/lib/libconnection_model.a | 已参编 |
| `libchunk` | library | `bigworld/lib/chunk/Makefile.rules` | build/el7/lib/libchunk.a | 已参编 |
| `libchunk_loading` | library | `bigworld/lib/chunk_loading/Makefile.rules` | build/el7/lib/libchunk_loading.a | 已参编 |
| `libdb` | library | `bigworld/lib/db/Makefile.rules` | build/el7/lib/libdb.a | 已参编 |
| `libdb_storage` | library | `bigworld/lib/db_storage/Makefile.rules` | build/el7/lib/libdb_storage.a | 已参编 |
| `libdb_storage_mysql` | library | `bigworld/lib/db_storage_mysql/Makefile.rules` | build/el7/lib/libdb_storage_mysql.a | 已参编 |
| `libdb_storage_xml` | library | `bigworld/lib/db_storage_xml/Makefile.rules` | build/el7/lib/libdb_storage_xml.a | 已参编 |
| `libdelegate_interface` | library | `bigworld/lib/delegate_interface/Makefile.rules` | build/el7/lib/libdelegate_interface.a | 已参编 |
| `libentitydef` | library | `bigworld/lib/entitydef/Makefile.rules` | build/el7/lib/libentitydef.a | 已参编 |
| `libentitydef_script` | library | `bigworld/lib/entitydef_script/Makefile.rules` | build/el7/lib/libentitydef_script.a | 已参编 |
| `libmath` | library | `bigworld/lib/math/Makefile.rules` | build/el7/lib/libmath.a | 已参编 |
| `libmemhook` | library | `bigworld/lib/memhook/Makefile.rules` | build/el7/lib/libmemhook.a | 已参编 |
| `libmoo` | library | `bigworld/lib/moo/Makefile.rules` | build/el7/lib/libmoo.a | 已参编 |
| `libnetwork` | library | `bigworld/lib/network/Makefile.rules` | build/el7/lib/libnetwork.a | 已参编 |
| `libphysics2` | library | `bigworld/lib/physics2/Makefile.rules` | build/el7/lib/libphysics2.a | 已参编 |
| `libpyscript` | library | `bigworld/lib/pyscript/Makefile.rules` | build/el7/lib/libpyscript.a | 已参编 |
| `libresmgr` | library | `bigworld/lib/resmgr/Makefile.rules` | build/el7/lib/libresmgr.a | 已参编 |
| `libscene` | library | `bigworld/lib/scene/Makefile.rules` | build/el7/lib/libscene.a | 已参编 |
| `libserver` | library | `bigworld/lib/server/Makefile.rules` | build/el7/lib/libserver.a | 已参编 |
| `libscript` | library | `bigworld/lib/script/Makefile.rules` | build/el7/lib/libscript.a | 已参编 |
| `libspeedtree` | library | `bigworld/lib/speedtree/Makefile.rules` | build/el7/lib/libspeedtree.a | 已参编 |
| `libsqlite` | library | `bigworld/lib/sqlite/Makefile.rules` | build/el7/lib/libsqlite.a | 已参编 |
| `libterrain` | library | `bigworld/lib/terrain/Makefile.rules` | build/el7/lib/libterrain.a | 已参编 |
| `libunit_test_lib` | library | `bigworld/lib/unit_test_lib/Makefile.rules` | build/el7/lib/libunit_test_lib.a | 已参编 |
| `liburlrequest` | library | `bigworld/lib/urlrequest/Makefile.rules` | build/el7/lib/liburlrequest.a | 已参编 |
| `libwaypoint` | library | `bigworld/lib/waypoint/Makefile.rules` | build/el7/lib/libwaypoint.a | 已参编 |
| `bwengine_mysql` | shared_library | `bigworld/server/dbapp_extensions/bwengine_mysql/Makefile.rules` | server/dbapp-extensions/bwengine_mysql.so | 已参编 |
| `bwengine_xml` | shared_library | `bigworld/server/dbapp_extensions/bwengine_xml/Makefile.rules` | server/dbapp-extensions/bwengine_xml.so | 已参编 |
| `_bwlog` | shared_library | `bigworld/server/tools/message_logger/Makefile.message_reader` | tools/_bwlog.so | 已参编 |
| `_bw_profile` | shared_library | `bigworld/server/tools/bw_profile/Makefile.rules` | tools/_bw_profile.so | 缺产物 |
| `libCppUnitLite2` | library | `bigworld/third_party/CppUnitLite2/src/Makefile.rules` | build/el7/lib/libCppUnitLite2.a | 已参编 |
| `cellapp_example` | shared_library | `bigworld/examples/cellapp_extension/Makefile.rules` | server/cellapp-extensions/cellapp_example.so | 已参编 |

- `.a` 归档仅进中间目录 `build/el7/lib/`（链接用），安装面只有 5 个 `.so`（bwlog/bw_profile 两个 Python 模块 + dbapp 双引擎 + cellapp 扩展示例）。
- `BW_DEPENDS_ON := network resmgr math cstdmf zip build` 是全部二进制的公共链接集（顶层注释自注「应改为按库/进程的链」）。

## 单元测试（BW_UNIT_TESTS，21）

| 目标 | 产物 bin | 源目录 | 磁盘 |
|---|---|---|---|
| `unit_test_cstdmf` | `unit_tests/cstdmf_test` | `bigworld/lib/cstdmf/unit_test` | 已参编 |
| `unit_test_db` | `unit_tests/db_test` | `bigworld/lib/db/unit_test` | 已参编 |
| `unit_test_entitydef` | `unit_tests/entitydef_test` | `bigworld/lib/entitydef/unit_test` | 已参编 |
| `unit_test_math` | `unit_tests/math_test` | `bigworld/lib/math/unit_test` | 已参编 |
| `unit_test_network` | `unit_tests/network_test` | `bigworld/lib/network/unit_test` | 已参编 |
| `unit_test_physics2` | `unit_tests/physics2_test` | `bigworld/lib/physics2/unit_test` | 已参编 |
| `unit_test_pyscript` | `unit_tests/pyscript_test` | `bigworld/lib/pyscript/unit_test` | 已参编 |
| `unit_test_resmgr` | `unit_tests/resmgr_test` | `bigworld/lib/resmgr/unit_test` | 已参编 |
| `unit_test_scene` | `unit_tests/scene_test` | `bigworld/lib/scene/unit_test` | 已参编 |
| `unit_test_server` | `unit_tests/server_test` | `bigworld/lib/server/unit_test` | 已参编 |
| `unit_test_script` | `unit_tests/script_test` | `bigworld/lib/script/unit_test` | 已参编 |
| `unit_test_terrain` | `unit_tests/terrain_test` | `bigworld/lib/terrain/unit_test` | 已参编 |
| `unit_test_unit_test_lib` | `unit_tests/unit_test_lib_test` | `bigworld/lib/unit_test_lib/unit_test` | 已参编 |
| `unit_test_baseapp` | `unit_tests/baseapp_test` | `bigworld/server/baseapp/unit_test` | 已参编 |
| `unit_test_baseappmgr` | `unit_tests/baseappmgr_test` | `bigworld/server/baseappmgr/unit_test` | 已参编 |
| `unit_test_cellappmgr` | `unit_tests/cellappmgr_test` | `bigworld/server/cellappmgr/unit_test` | 已参编 |
| `unit_test_cellapp` | `unit_tests/cellapp_test` | `bigworld/server/cellapp/unit_test` | 已参编 |
| `unit_test_dbapp` | `unit_tests/dbapp_test` | `bigworld/server/dbapp/unit_test` | 已参编 |
| `unit_test_dbappmgr` | `unit_tests/dbappmgr_test` | `bigworld/server/dbappmgr/unit_test` | 已参编 |
| `unit_test_loginapp` | `unit_tests/loginapp_test` | `bigworld/server/loginapp/unit_test` | 已参编 |
| `unit_test_reviver` | `unit_tests/reviver_test` | `bigworld/server/reviver/unit_test` | 已参编 |

## 杂项（BW_MISC，16，Python 运行面）

- `python_library` 等 17 项：`third_party_python.mak`/`third_party_python_modules.mak` 装出的 CPython 3.13 库与站点模块（setuptools、typing_extensions、attrs、constantly、incremental、automat、idna、hyperlink、pympler、pika、msgpack、sqlalchemy、zope.interface、twisted）。`BW_SYSTEM_PYTHON_COMPONENTS = bwlog network pyscript resmgr server math zip cstdmf`（8 组件的 Python 侧符号核对）。
- 3.13 迁移注记（`third_party_python_modules.mak` BIGWORLD 注释）：模块规则安装捆绑的 Py3 兼容树，导入由 `scripts/python-modules-smoke-import.py` 证明。

## 第三方（BW_THIRD_PARTY，8）

| 库名 | 声明 | 源 | 磁盘(.a) |
|---|---|---|---|
| `libbwssl` | `third_party_openssl.mak` | vcpkg openssl | 已参编 |
| `libbwcrypto` | `third_party_openssl.mak` | vcpkg openssl | 已参编 |
| `libbwcurl` | `third_party_curl.mak` | vcpkg curl | 已参编 |
| `libjsoncpp` | `third_party_json.mak` | vcpkg jsoncpp | 已参编 |
| `libzip` | `third_party_vcpkg.mak` | vcpkg zlib | 已参编 |
| `libpng` | `悬空（见缺口 1）` | vcpkg libpng | 已参编 |
| `libsqlite3` | `third_party_vcpkg.mak` | vcpkg sqlite3 | 已参编 |
| `libbwpython3.13` | `third_party_python.mak` | third_party/python（CPython 3.13） | 已参编 |

## 条件启用与缺口登记

1. **png/zip/jsoncpp 树内组件构建已移除**：`third_party/Makefile.rules` 仅剩 `CppUnitLite2/src`（BIGWORLD 3.13 迁移注释：三库改由 vcpkg 提供）；`third_party/{png,zip}/Makefile.rules` 为**悬空声明**（不再被扫描链包含，`name/type` 仍在）。`build/el7/lib` 里的 `libpng.a/libzip.a/libjsoncpp.a` 由三方 .mak 从 vcpkg 安装树装出，非组件规则产物。
2. **examples 条件启用**：`user_shouldBuildExamples = 0` 时跳过 `examples/` 扫描（make 打印 not building examples）。本机树 `simple_python_client` 已参编。
3. **单测条件启用**：`user_shouldBuildUnitTests = 0` 可从常规构建剥离；`bw-run-unit-tests` 另需 `user_shouldRunUnitTests=1`。
4. **`_bw_profile.so` 登记但盘上缺产物**：规则注册（`server/tools/bw_profile/Makefile.rules`，shared_library），本快照 `tools/` 无该 .so（中间目录仅 .d 无 .o）；`make -n all` 视其为待建。登记为「规则在、快照缺」。
5. **commands 命令族**：`clear_auto_load/consolidate_dbs/sync_db/transfer_db`（`server/commands/`）+ `snapshot_helper`（`_helpers/`）+ `remove_db` 全部已参编（注意实际路径是 `el7/server/commands/`——`BW_SERVER_BIN_DIR/commands`，非 `el7/commands/`）。
6. **host/target 平台分裂**：工具（process_defs、bw_profile 等）按 host 平台装 `tools/`，服务器按 target；本机两者同为 el7 无感。
7. **静态面与动态面一致性**：进入累加器的 21+34+21 个目标全部找到所属规则；未进入累加器的声明仅 png/zip（悬空）——静态/动态交叉核对闭合。

## 结论

清册闭合：**21 二进制 + 35 库（+21 单测 + 16 杂项 + 8 三方）逐项有所属规则文件与安装目录**，磁盘已参编状态逐项核对（唯一缺口 `_bw_profile.so` 已登记）。验收达成。
