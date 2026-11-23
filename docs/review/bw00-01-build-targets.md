# BW-00-01-01 Linux Make 构建目标清册

> 证据面（BW-00-01-01 验收：清册包含二进制、库及所属规则）。

扫描自参考 `programming/bigworld` 的 Make 构建（`build/make/Makefile` 入口），
由 `tools/make_component_scan.py` 提取：遍历 `Makefile*`，按 `name :=` / `type :=`
登记每个组件，用 `subdirs =` 递归图做可达性判断（`in_build`）。

共 79 个声明组件，其中 77 个在当前构建图内；png / zip 是 vcpkg 迁移后残留的空壳（不再由 Make 构建）。

聚合规则（`build/make/Makefile` 的 `depsForRuleAll`）：`bw-libraries`、`bw-binaries`、`third-party-libraries`、`bw-miscellaneous`，可选 `bw-unit-tests`（`user_shouldBuildUnitTests`）与 `bw-run-unit-tests`（`user_shouldRunUnitTests`）。

安装布局（`BW_*_BIN_DIR`，`build/make/Makefile:238-247`）：`bin/server/<platform>_<config>/server`（角色二进制）、`.../server/commands`（`remove_db` 等一次性命令）、`.../server/commands/_helpers`（`bw-miscellaneous` 的 Python 模块 16 个：twisted、sqlalchemy、msgpack、pika、setuptools 等 `python_*` 目标）、`.../server/tools`（工具与 `.so`）。

与实盘二进制差分（`game/bin/server/el7/`）：`server/` 下 baseapp、baseappmgr、bots、cellapp、cellappmgr、dbapp、dbappmgr、loginapp、reviver、serviceapp 与清册一致；`server/commands/` 下 clear_auto_load、consolidate_dbs、remove_db、sync_db、transfer_db 与清册一致（`snapshot_helper` 已声明但该检出未装）；`server/cellapp-extensions/` 与 `server/dbapp-extensions/` 的三个 `.so` （cellapp_example、bwengine_mysql、bwengine_xml）与清册一致；`tools/` 下 bwmachined2、message_logger、process_defs、res_packer、`_bw_profile.so`、`_bwlog.so` 与清册一致。

## Binaries (Make `type := binary`)（19）

| 目标 | 目录 | 构建内 | 源数 | 特性 | 所属规则 |
| --- | --- | --- | --- | --- | --- |
| `simple_python_client` | `examples/client_integration/python/simple` | 是 | 7 | useOpenSSL=1 usePython=1 | `examples/client_integration/python/simple/Makefile.rules` |
| `baseapp` | `server/baseapp` | 是 | 57 | useCurl=1 useOpenSSL=1 usePyURLRequest=1 usePython=1 useSQLite=1 | `server/baseapp/Makefile.rules` |
| `baseappmgr` | `server/baseappmgr` | 是 | 11 | — | `server/baseappmgr/Makefile.rules` |
| `cellapp` | `server/cellapp` | 是 | 85 | useCurl=1 useOpenSSL=1 usePyURLRequest=1 usePython=1 | `server/cellapp/Makefile.rules` |
| `cellappmgr` | `server/cellappmgr` | 是 | 24 | — | `server/cellappmgr/Makefile.rules` |
| `dbapp` | `server/dbapp` | 是 | 1 | useCustomBillingSystem=0 usePyURLRequest=1 | `server/dbapp/Makefile.rules` |
| `dbappmgr` | `server/dbappmgr` | 是 | 7 | — | `server/dbappmgr/Makefile.rules` |
| `loginapp` | `server/loginapp` | 是 | 10 | useOpenSSL=1 | `server/loginapp/Makefile.rules` |
| `reviver` | `server/reviver` | 是 | 6 | — | `server/reviver/Makefile.rules` |
| `bots` | `server/tools/bots` | 是 | 25 | useOpenSSL=1 usePython=1 | `server/tools/bots/Makefile.rules` |
| `bwmachined2` | `server/tools/bwmachined` | 是 | 9 | — | `server/tools/bwmachined/Makefile.rules` |
| `clear_auto_load` | `server/tools/clear_auto_load` | 是 | 2 | useMySQL=1 useOpenSSL=1 usePython=1 | `server/tools/clear_auto_load/Makefile.rules` |
| `consolidate_dbs` | `server/tools/consolidate_dbs` | 是 | 15 | useMySQL=1 useOpenSSL=1 usePython=1 useSQLite=1 | `server/tools/consolidate_dbs/Makefile.rules` |
| `message_logger` | `server/tools/message_logger` | 是 | 35 | useJSON=1 useMongoDB=1 | `server/tools/message_logger/Makefile.message_logger` |
| `snapshot_helper` | `server/tools/snapshot_helper` | 是 | 1 | useMySQL=1 | `server/tools/snapshot_helper/Makefile.rules` |
| `sync_db` | `server/tools/sync_db` | 是 | 6 | useMySQL=1 useOpenSSL=1 usePython=1 | `server/tools/sync_db/Makefile.rules` |
| `transfer_db` | `server/tools/transfer_db` | 是 | 4 | useSQLite=1 | `server/tools/transfer_db/Makefile.rules` |
| `process_defs` | `tools/process_defs` | 是 | 2 | useOpenSSL=1 usePython=1 | `tools/process_defs/Makefile.rules` |
| `res_packer` | `tools/res_packer` | 是 | 10 | usePython=1 | `tools/res_packer/Makefile.rules` |

## Copied binaries (Make `type := copy_of_binary`)（1）

| 目标 | 目录 | 构建内 | 源数 | 特性 | 所属规则 |
| --- | --- | --- | --- | --- | --- |
| `serviceapp` | `server/baseapp` | 是 | 0 | — | `server/baseapp/Makefile.serviceapp` |

## Libraries (Make `type := library`)（32）

| 目标 | 目录 | 构建内 | 源数 | 特性 | 所属规则 |
| --- | --- | --- | --- | --- | --- |
| `build` | `lib/build` | 是 | 1 | usePython=1 | `lib/build/Makefile.rules` |
| `chunk` | `lib/chunk` | 是 | 34 | usePython=1 | `lib/chunk/Makefile.rules` |
| `chunk_loading` | `lib/chunk_loading` | 是 | 6 | — | `lib/chunk_loading/Makefile.rules` |
| `connection` | `lib/connection` | 是 | 26 | useOpenSSL=1 | `lib/connection/Makefile.rules` |
| `connection_model` | `lib/connection_model` | 是 | 23 | useOpenSSL=1 | `lib/connection_model/Makefile.rules` |
| `cstdmf` | `lib/cstdmf` | 是 | 57 | — | `lib/cstdmf/Makefile.rules` |
| `db` | `lib/db` | 是 | 7 | usePython=1 | `lib/db/Makefile.rules` |
| `db_storage` | `lib/db_storage` | 是 | 5 | usePython=1 | `lib/db_storage/Makefile.rules` |
| `db_storage_mysql` | `lib/db_storage_mysql` | 是 | 65 | useMySQL=1 usePython=1 | `lib/db_storage_mysql/Makefile.rules` |
| `db_storage_xml` | `lib/db_storage_xml` | 是 | 2 | usePython=1 | `lib/db_storage_xml/Makefile.rules` |
| `delegate_interface` | `lib/delegate_interface` | 是 | 1 | — | `lib/delegate_interface/Makefile.rules` |
| `entitydef` | `lib/entitydef` | 是 | 55 | usePython=1 | `lib/entitydef/Makefile.rules` |
| `entitydef_script` | `lib/entitydef_script` | 是 | 2 | usePython=1 | `lib/entitydef_script/Makefile.rules` |
| `math` | `lib/math` | 是 | 12 | — | `lib/math/Makefile.rules` |
| `memhook` | `lib/memhook` | 是 | 1 | — | `lib/memhook/Makefile.rules` |
| `moo` | `lib/moo` | 是 | 2 | usePNG=1 | `lib/moo/Makefile.rules` |
| `network` | `lib/network` | 是 | 78 | useOpenSSL=1 | `lib/network/Makefile.rules` |
| `physics2` | `lib/physics2` | 是 | 7 | — | `lib/physics2/Makefile.rules` |
| `pyscript` | `lib/pyscript` | 是 | 18 | usePython=1 | `lib/pyscript/Makefile.rules` |
| `resmgr` | `lib/resmgr` | 是 | 25 | — | `lib/resmgr/Makefile.rules` |
| `scene` | `lib/scene` | 是 | 14 | — | `lib/scene/Makefile.rules` |
| `script` | `lib/script` | 是 | 4 | usePython=1 | `lib/script/Makefile.rules` |
| `server` | `lib/server` | 是 | 45 | usePython=1 | `lib/server/Makefile.rules` |
| `speedtree` | `lib/speedtree` | 是 | 1 | — | `lib/speedtree/Makefile.rules` |
| `sqlite` | `lib/sqlite` | 是 | 4 | useSQLite=1 | `lib/sqlite/Makefile.rules` |
| `terrain` | `lib/terrain` | 是 | 18 | — | `lib/terrain/Makefile.rules` |
| `unit_test_lib` | `lib/unit_test_lib` | 是 | 4 | — | `lib/unit_test_lib/Makefile.rules` |
| `urlrequest` | `lib/urlrequest` | 是 | 4 | useCurl=1 | `lib/urlrequest/Makefile.rules` |
| `waypoint` | `lib/waypoint` | 是 | 14 | usePython=1 | `lib/waypoint/Makefile.rules` |
| `CppUnitLite2` | `third_party/CppUnitLite2/src` | 是 | 7 | — | `third_party/CppUnitLite2/src/Makefile.rules` |
| `png` | `third_party/png` | 否 | 15 | — | `third_party/png/Makefile.rules` |
| `zip` | `third_party/zip` | 否 | 14 | — | `third_party/zip/Makefile.rules` |

## Shared libraries (Make `type := shared_library`)（5）

| 目标 | 目录 | 构建内 | 源数 | 特性 | 所属规则 |
| --- | --- | --- | --- | --- | --- |
| `cellapp_example` | `examples/cellapp_extension` | 是 | 7 | usePython=1 | `examples/cellapp_extension/Makefile.rules` |
| `bwengine_mysql` | `server/dbapp_extensions/bwengine_mysql` | 是 | 1 | useMySQL=1 usePython=1 | `server/dbapp_extensions/bwengine_mysql/Makefile.rules` |
| `bwengine_xml` | `server/dbapp_extensions/bwengine_xml` | 是 | 1 | usePython=1 | `server/dbapp_extensions/bwengine_xml/Makefile.rules` |
| `_bw_profile` | `server/tools/bw_profile` | 是 | 1 | usePython=1 useSystemPython=0 | `server/tools/bw_profile/Makefile.rules` |
| `_bwlog` | `server/tools/message_logger` | 是 | 37 | useMongoDB=1 usePython=1 useSystemPython=0 | `server/tools/message_logger/Makefile.message_reader` |

## Script copies registered against `BW_BINS` directly（1）

| 目标 | 目录 | 构建内 | 源数 | 特性 | 所属规则 |
| --- | --- | --- | --- | --- | --- |
| `remove_db` | `server/tools/remove_db` | 是 | 0 | — | `server/tools/remove_db/Makefile.rules` |

## Unit tests (Make `type := unit_test`)（21）

Each unit test is the `unit_test_<name>` target of its component; the `includeUnitTestDirectory` hook pulls the `unit_test/` rules in.

| 目标 | 目录 | 构建内 | 源数 | 特性 | 所属规则 |
| --- | --- | --- | --- | --- | --- |
| `cstdmf` | `lib/cstdmf/unit_test` | 是 | 56 | — | `lib/cstdmf/unit_test/Makefile.rules` |
| `db` | `lib/db/unit_test` | 是 | 4 | — | `lib/db/unit_test/Makefile.rules` |
| `entitydef` | `lib/entitydef/unit_test` | 是 | 18 | useOpenSSL=1 usePython=1 | `lib/entitydef/unit_test/Makefile.rules` |
| `math` | `lib/math/unit_test` | 是 | 8 | — | `lib/math/unit_test/Makefile.rules` |
| `network` | `lib/network/unit_test` | 是 | 54 | useOpenSSL=1 | `lib/network/unit_test/Makefile.rules` |
| `physics2` | `lib/physics2/unit_test` | 是 | 5 | — | `lib/physics2/unit_test/Makefile.rules` |
| `pyscript` | `lib/pyscript/unit_test` | 是 | 20 | usePython=1 | `lib/pyscript/unit_test/Makefile.rules` |
| `resmgr` | `lib/resmgr/unit_test` | 是 | 12 | — | `lib/resmgr/unit_test/Makefile.rules` |
| `scene` | `lib/scene/unit_test` | 是 | 9 | — | `lib/scene/unit_test/Makefile.rules` |
| `script` | `lib/script/unit_test` | 是 | 9 | usePython=1 | `lib/script/unit_test/Makefile.rules` |
| `server` | `lib/server/unit_test` | 是 | 4 | — | `lib/server/unit_test/Makefile.rules` |
| `terrain` | `lib/terrain/unit_test` | 是 | 3 | usePNG=1 | `lib/terrain/unit_test/Makefile.rules` |
| `unit_test_lib` | `lib/unit_test_lib/unit_test` | 是 | 2 | — | `lib/unit_test_lib/unit_test/Makefile.rules` |
| `baseapp` | `server/baseapp/unit_test` | 是 | 6 | useOpenSSL=1 useSQLite=1 | `server/baseapp/unit_test/Makefile.rules` |
| `baseappmgr` | `server/baseappmgr/unit_test` | 是 | 1 | — | `server/baseappmgr/unit_test/Makefile.rules` |
| `cellapp` | `server/cellapp/unit_test` | 是 | 13 | usePython=1 | `server/cellapp/unit_test/Makefile.rules` |
| `cellappmgr` | `server/cellappmgr/unit_test` | 是 | 1 | — | `server/cellappmgr/unit_test/Makefile.rules` |
| `dbapp` | `server/dbapp/unit_test` | 是 | 1 | — | `server/dbapp/unit_test/Makefile.rules` |
| `dbappmgr` | `server/dbappmgr/unit_test` | 是 | 1 | — | `server/dbappmgr/unit_test/Makefile.rules` |
| `loginapp` | `server/loginapp/unit_test` | 是 | 1 | — | `server/loginapp/unit_test/Makefile.rules` |
| `reviver` | `server/reviver/unit_test` | 是 | 1 | — | `server/reviver/unit_test/Makefile.rules` |

## 条件与特别情况

- **cellappmgr**：`server/Makefile.rules` 里 `LICENSE_SERVER` 门——`cellappmgr` 只在 `server/cellappmgr/Makefile` **缺失**时被 `filter-out`（此时 `LICENSE_SERVER=1`）；文件在则照常构建。本检出库该文件在，故 cellappmgr 在构建内。
- **examples**：`programming/bigworld/Makefile.rules` 以 `ifneq ($(user_shouldBuildExamples),0)` 门控（缺省=构建）；`examples/` 目标因此按默认计入 `in_build`。
- **remove_db**：`server/tools/remove_db` 是一次性 Makefile，无 `type`，直接 `BW_BINS += $(name)` 拷贝脚本，`destFile` 走 `$(BW_COMMAND_BIN_DIR)`。
- **message_logger**：`Makefile.rules` 是元 makefile，实际组件在 `Makefile.message_logger`（binary `message_logger`）与 `Makefile.message_reader`（shared_library `_bwlog`）。
- **third-party**：仅 `CppUnitLite2/src` 在树内构建；curl / mysql / mongodb / openssl / python / sdl / json / vcpkg 由 `build/make/third_party_*.mak` 引入，jsoncpp / png / zip(zlib) 走 `vcpkg.json`。属 BW-00-01-04。
- **serviceapp**：`server/baseapp/Makefile.serviceapp` 声明 `type := copy_of_binary`，与 `baseapp` 同目录、不同目标名。

## 计数

| 类型 | 声明 | 构建内 |
| --- | --- | --- |
| binary | 19 | 19 |
| copy_of_binary | 1 | 1 |
| library | 32 | 30 |
| shared_library | 5 | 5 |
| script_copy | 1 | 1 |
| unit_test | 21 | 21 |

生成：`make_component_scan.py` + `make_components_doc.py`。
