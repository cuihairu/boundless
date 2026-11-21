# BW-00-01-01 展开 Linux Make 构建目标 — 清册

- **任务编号**: BW-00-01-01（lane BW-00 构建目标清册，todo.md 行 87）
- **原版构建入口/源码**: `programming/Makefile` → `bigworld/build/make/Makefile`（服务端组件构建根）；组件声明分散在 `programming/bigworld/**/Makefile.rules`（及少数伴随 `Makefile.<name>`）；框架规则在 `bigworld/build/make/{common_header.mak,common_footer.mak,common_footer_config.mak,commands.mak}`。
- **Boundless 模块**: 无（本条目是**参考侧清册**，不产出 Rust 代码；供后续 BW-00-05 覆盖台账与 BW-15 平台矩阵引用）。
- **平台**: Linux x86_64（el7 参考构建；`make -f programming/Makefile`）。
- **输入**: `/home/cui/workspaces/BigWorld/programming/` 下全部 `Makefile.rules`（90 个）+ 伴随 `Makefile.*`（3 个非规则 Makefile 声明目标）+ 特殊一次性规则（`remove_db`）。
- **采集工具**: `docs/review/tools/make_inventory.py`（确定性解析全部 `Makefile*`，输出 JSON/表格；`python3 docs/review/tools/make_inventory.py --json /tmp/make_inventory.json`）。

## 结果：清册（79 个声明的目标）

按 `type` 汇总：**binary 19**、**copy_of_binary 1**、**library 32**、**shared_library 5**、**unit_test 21**，另 **remove_db** 是非组件的一次性拷贝规则（`type` 未声明）。合计 79 = 78（有 `type`）+ 1（remove_db）。

`type` 取值全集（来自 `common_header.mak`/`common_footer_config.mak`）：`binary`、`copy_of_binary`、`copy_of_shared_library`、`library`、`shared_library`、`unit_test`。

### 二进制（binary，19）

| 目标 | 目录 | 所属规则 |
|---|---|---|
| simple_python_client | bigworld/examples/client_integration/python/simple | bigworld/examples/client_integration/python/simple/Makefile.rules |
| baseapp | bigworld/server/baseapp | bigworld/server/baseapp/Makefile.rules |
| baseappmgr | bigworld/server/baseappmgr | bigworld/server/baseappmgr/Makefile.rules |
| cellapp | bigworld/server/cellapp | bigworld/server/cellapp/Makefile.rules |
| cellappmgr | bigworld/server/cellappmgr | bigworld/server/cellappmgr/Makefile.rules |
| dbapp | bigworld/server/dbapp | bigworld/server/dbapp/Makefile.rules |
| dbappmgr | bigworld/server/dbappmgr | bigworld/server/dbappmgr/Makefile.rules |
| loginapp | bigworld/server/loginapp | bigworld/server/loginapp/Makefile.rules |
| reviver | bigworld/server/reviver | bigworld/server/reviver/Makefile.rules |
| bots | bigworld/server/tools/bots | bigworld/server/tools/bots/Makefile.rules |
| bwmachined2 | bigworld/server/tools/bwmachined | bigworld/server/tools/bwmachined/Makefile.rules |
| clear_auto_load | bigworld/server/tools/clear_auto_load | bigworld/server/tools/clear_auto_load/Makefile.rules |
| consolidate_dbs | bigworld/server/tools/consolidate_dbs | bigworld/server/tools/consolidate_dbs/Makefile.rules |
| snapshot_helper | bigworld/server/tools/snapshot_helper | bigworld/server/tools/snapshot_helper/Makefile.rules |
| sync_db | bigworld/server/tools/sync_db | bigworld/server/tools/sync_db/Makefile.rules |
| transfer_db | bigworld/server/tools/transfer_db | bigworld/server/tools/transfer_db/Makefile.rules |
| process_defs | bigworld/tools/process_defs | bigworld/tools/process_defs/Makefile.rules |
| res_packer | bigworld/tools/res_packer | bigworld/tools/res_packer/Makefile.rules |
| message_logger | bigworld/server/tools/message_logger | bigworld/server/tools/message_logger/Makefile.message_logger |

### 拷贝二进制（copy_of_binary，1）

`serviceapp` 由 `baseapp` 拷贝而来（`sourceBinaryName := baseapp`），两者共享源码目录。

| 目标 | 目录 | 所属规则 |
|---|---|---|
| serviceapp | bigworld/server/baseapp | bigworld/server/baseapp/Makefile.serviceapp |

### 静态库（library，32）

| 目标 | 目录 | 所属规则 |
|---|---|---|
| build | bigworld/lib/build | bigworld/lib/build/Makefile.rules |
| chunk | bigworld/lib/chunk | bigworld/lib/chunk/Makefile.rules |
| chunk_loading | bigworld/lib/chunk_loading | bigworld/lib/chunk_loading/Makefile.rules |
| connection | bigworld/lib/connection | bigworld/lib/connection/Makefile.rules |
| connection_model | bigworld/lib/connection_model | bigworld/lib/connection_model/Makefile.rules |
| cstdmf | bigworld/lib/cstdmf | bigworld/lib/cstdmf/Makefile.rules |
| db | bigworld/lib/db | bigworld/lib/db/Makefile.rules |
| db_storage | bigworld/lib/db_storage | bigworld/lib/db_storage/Makefile.rules |
| db_storage_mysql | bigworld/lib/db_storage_mysql | bigworld/lib/db_storage_mysql/Makefile.rules |
| db_storage_xml | bigworld/lib/db_storage_xml | bigworld/lib/db_storage_xml/Makefile.rules |
| delegate_interface | bigworld/lib/delegate_interface | bigworld/lib/delegate_interface/Makefile.rules |
| entitydef | bigworld/lib/entitydef | bigworld/lib/entitydef/Makefile.rules |
| entitydef_script | bigworld/lib/entitydef_script | bigworld/lib/entitydef_script/Makefile.rules |
| math | bigworld/lib/math | bigworld/lib/math/Makefile.rules |
| memhook | bigworld/lib/memhook | bigworld/lib/memhook/Makefile.rules |
| moo | bigworld/lib/moo | bigworld/lib/moo/Makefile.rules |
| network | bigworld/lib/network | bigworld/lib/network/Makefile.rules |
| physics2 | bigworld/lib/physics2 | bigworld/lib/physics2/Makefile.rules |
| pyscript | bigworld/lib/pyscript | bigworld/lib/pyscript/Makefile.rules |
| resmgr | bigworld/lib/resmgr | bigworld/lib/resmgr/Makefile.rules |
| scene | bigworld/lib/scene | bigworld/lib/scene/Makefile.rules |
| script | bigworld/lib/script | bigworld/lib/script/Makefile.rules |
| server | bigworld/lib/server | bigworld/lib/server/Makefile.rules |
| speedtree | bigworld/lib/speedtree | bigworld/lib/speedtree/Makefile.rules |
| sqlite | bigworld/lib/sqlite | bigworld/lib/sqlite/Makefile.rules |
| terrain | bigworld/lib/terrain | bigworld/lib/terrain/Makefile.rules |
| unit_test_lib | bigworld/lib/unit_test_lib | bigworld/lib/unit_test_lib/Makefile.rules |
| urlrequest | bigworld/lib/urlrequest | bigworld/lib/urlrequest/Makefile.rules |
| waypoint | bigworld/lib/waypoint | bigworld/lib/waypoint/Makefile.rules |
| CppUnitLite2 | bigworld/third_party/CppUnitLite2/src | bigworld/third_party/CppUnitLite2/src/Makefile.rules |
| png | bigworld/third_party/png | bigworld/third_party/png/Makefile.rules |
| zip | bigworld/third_party/zip | bigworld/third_party/zip/Makefile.rules |

### 共享库（shared_library，5）

| 目标 | 目录 | 所属规则 |
|---|---|---|
| cellapp_example | bigworld/examples/cellapp_extension | bigworld/examples/cellapp_extension/Makefile.rules |
| bwengine_mysql | bigworld/server/dbapp_extensions/bwengine_mysql | bigworld/server/dbapp_extensions/bwengine_mysql/Makefile.rules |
| bwengine_xml | bigworld/server/dbapp_extensions/bwengine_xml | bigworld/server/dbapp_extensions/bwengine_xml/Makefile.rules |
| _bw_profile | bigworld/server/tools/bw_profile | bigworld/server/tools/bw_profile/Makefile.rules |
| _bwlog | bigworld/server/tools/message_logger | bigworld/server/tools/message_logger/Makefile.message_reader |

### 单元测试（unit_test，21）

| 目标 | 目录 | 所属规则 |
|---|---|---|
| cstdmf | bigworld/lib/cstdmf/unit_test | bigworld/lib/cstdmf/unit_test/Makefile.rules |
| db | bigworld/lib/db/unit_test | bigworld/lib/db/unit_test/Makefile.rules |
| entitydef | bigworld/lib/entitydef/unit_test | bigworld/lib/entitydef/unit_test/Makefile.rules |
| math | bigworld/lib/math/unit_test | bigworld/lib/math/unit_test/Makefile.rules |
| network | bigworld/lib/network/unit_test | bigworld/lib/network/unit_test/Makefile.rules |
| physics2 | bigworld/lib/physics2/unit_test | bigworld/lib/physics2/unit_test/Makefile.rules |
| pyscript | bigworld/lib/pyscript/unit_test | bigworld/lib/pyscript/unit_test/Makefile.rules |
| resmgr | bigworld/lib/resmgr/unit_test | bigworld/lib/resmgr/unit_test/Makefile.rules |
| scene | bigworld/lib/scene/unit_test | bigworld/lib/scene/unit_test/Makefile.rules |
| script | bigworld/lib/script/unit_test | bigworld/lib/script/unit_test/Makefile.rules |
| server | bigworld/lib/server/unit_test | bigworld/lib/server/unit_test/Makefile.rules |
| terrain | bigworld/lib/terrain/unit_test | bigworld/lib/terrain/unit_test/Makefile.rules |
| unit_test_lib | bigworld/lib/unit_test_lib/unit_test | bigworld/lib/unit_test_lib/unit_test/Makefile.rules |
| baseapp | bigworld/server/baseapp/unit_test | bigworld/server/baseapp/unit_test/Makefile.rules |
| baseappmgr | bigworld/server/baseappmgr/unit_test | bigworld/server/baseappmgr/unit_test/Makefile.rules |
| cellapp | bigworld/server/cellapp/unit_test | bigworld/server/cellapp/unit_test/Makefile.rules |
| cellappmgr | bigworld/server/cellappmgr/unit_test | bigworld/server/cellappmgr/unit_test/Makefile.rules |
| dbapp | bigworld/server/dbapp/unit_test | bigworld/server/dbapp/unit_test/Makefile.rules |
| dbappmgr | bigworld/server/dbappmgr/unit_test | bigworld/server/dbappmgr/unit_test/Makefile.rules |
| loginapp | bigworld/server/loginapp/unit_test | bigworld/server/loginapp/unit_test/Makefile.rules |
| reviver | bigworld/server/reviver/unit_test | bigworld/server/reviver/unit_test/Makefile.rules |

### 非组件一次性拷贝（remove_db）

`bigworld/server/tools/remove_db/Makefile.rules` 不声明 `type`，是手写的一次性规则：把源码目录里的 `remove_db` 脚本复制到 `$(BW_INSTALL_DIR)/$(BW_BIN_PREFIX)/$(BW_COMMAND_BIN_DIR)/remove_db`（`bwCommand_createCopy`），并加入 `BW_BINS`。文件头注释自陈「a once off Makefile… quick work around for the interim」。

## 所属规则（每个目标怎么被构建）

组件框架的规则链：

1. **入口**: `programming/Makefile` 仅 `include bigworld/build/make/Makefile`；`bigworld/build/make/Makefile` 是服务端组件构建根（支持 `all` / `everything` / `clean`，含 `delayed_` 变量约定）。
2. **聚合**: 每个目录的 `Makefile.rules` 用 `$(call includeMakefilesFrom,$(subdirs))` 下钻（`programming/Makefile.rules` → `subdirs = bigworld`；`bigworld/Makefile.rules` → `server`、`lib`、`tools`、`examples`、`third_party` 等）。
3. **组件声明**: 组件目录 `Makefile.rules` 里 `name := <target>`、`type := <type>`、`bindir = $(BW_*_BIN_DIR)`、`cxxSource = <源列表>`、`dependsOn = <组件列表>`、`useXxx = 1`（特性开关）、`buildTimeFile := main`，然后 `include $(BW_COMPONENT_HEADER)` … `include $(BW_COMPONENT_FOOTER)`。
4. **类型分派**: `common_footer_config.mak` 按 `type` 生成 `$(destFile)` 目标 —— binary（35 行）/ copy_of_binary（44）/ unit_test（52）/ library（65）/ shared_library（71）/ copy_of_shared_library（82）；对象集 `componentObjects = $(componentBaseFiles:=.o)`（134 行，由 `cxxSource` 派生）。
5. **创建命令**: `commands.mak` 定义实际命令 —— `actual_createBinary`（`$(CXX) $(cxxFlags) -o $@ $(componentObjects) $(ldFlags) $(ldLibs)`）、`actual_createLibrary`（`$(AR) $(ARFLAGS) $@ $(componentObjects)`）、`actual_createSharedLibrary`（`$(CXX) $(cxxFlags) -shared -o $@ …`）、`actual_createCopy`（拷贝目标）。

即：**一个目标的「所属规则」= 其组件 `Makefile.rules`（或伴随 `Makefile.<name>`）声明 + `common_footer_config.mak` 的类型分派 + `commands.mak` 的创建命令**。

## 特性开关矩阵（字面 `= 1` 的组件）

| 开关 | 声明组件（脚本 `use_flags`，含孪生 unit_test 目标） |
|---|---|
| usePython（30） | cellapp_example、simple_python_client、build、chunk、db、db_storage、db_storage_mysql、db_storage_xml、entitydef、entitydef（unit_test）、entitydef_script、pyscript、pyscript（unit_test）、script、script（unit_test）、server、waypoint、baseapp、cellapp、cellapp（unit_test）、bwengine_mysql、bwengine_xml、bots、_bw_profile、clear_auto_load、consolidate_dbs、sync_db、process_defs、res_packer、_bwlog |
| useOpenSSL（15） | simple_python_client、connection、connection_model、entitydef、network、network（unit_test）、baseapp、baseapp（unit_test）、cellapp、loginapp、bots、clear_auto_load、consolidate_dbs、sync_db、process_defs |
| useMySQL（6） | db_storage_mysql、bwengine_mysql、clear_auto_load、consolidate_dbs、snapshot_helper、sync_db |
| useSQLite（5） | sqlite、baseapp、baseapp（unit_test）、consolidate_dbs、transfer_db |
| usePyURLRequest（3） | baseapp、cellapp、dbapp |
| useCurl（3） | urlrequest、baseapp、cellapp |
| useMongoDB（2） | message_logger、_bwlog |
| usePNG（2） | moo、terrain |

非字面 `= 1` 的开关（脚本不捕获，登记在案）：
- `useBigWorld := 1` 在 `bigworld/Makefile.rules:3` —— 整个 bigworld 子树的子树级开关，所有组件继承。
- `useSystemPython := 0`（`message_reader:65`、`bw_profile:17`；原为 1，`BIGWORLD(c++23 migration)` 注释说明已关）。
- `useCustomBillingSystem = 0`（`dbapp/Makefile.rules:9`，被 `ifeq ($(useCustomBillingSystem), 1)` 门控）。

> 矩阵来自脚本的 `use_flags` 字段（仅捕获字面 `= 1`/`:= 1`/`+= 1`）；`dependsOn`（组件间依赖）与 `cxxSource` 计数见 `/tmp/make_inventory.json` 的完整记录。

## 边界与已知登记项

1. **客户端与编辑器不在 Make 范围**: `programming/bigworld/client/` 与 `programming/bigworld/tools/{worldeditor,modeleditor,particle_editor,asset_pipeline,navgen,…}` 均用 CMake（`CMakeLists.txt`），不是 Make 组件；`bigworld/tools/Makefile.rules` 只聚合 `process_defs` 与 `res_packer`。故本清册不含客户端与编辑器二进制，归 BW-00-01-02（CMake 与 Windows 目标）。
2. **`fixwork/`、`origin/` 是参考树的副本/镜像**（各含 90 个 `Makefile.rules`），非规范构建树；本清册只从 `programming/` 提取。
3. **解析局限**: 脚本按行解析赋值，不展开 `ifeq` 块——特性开关条件内的 `cxxSource +=`（如 baseapp 的 `usePyURLRequest` → `../../common/py_urlrequest`）计入 `cxxSource` 但未反映开关真值；目标清单（name/type/dir/规则）不受影响，逐项核对无误。
4. **`remove_db`** 无 `type`，是唯一非组件目标；`serviceapp`/`message_logger`/`_bwlog` 在伴随 `Makefile.*` 中声明，脚本已单独收录。

## 状态

清册完整：79 个声明的目标（19 binary + 1 copy_of_binary + 32 library + 5 shared_library + 21 unit_test + 1 非组件一次性拷贝 remove_db），每个目标均列出目录与所属规则（组件 Makefile + 框架分派 + 创建命令）。采集脚本 `docs/review/tools/make_inventory.py` 可复现。
