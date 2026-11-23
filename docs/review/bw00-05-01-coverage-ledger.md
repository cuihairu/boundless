# BW-00-05-01 覆盖台账字段

> 证据面（BW-00-05-01 验收：每行含原版目标、启用条件、任务号、夹具及状态）。

台账以 BW-00-01-01 的 Make 组件清册（79 项）与 BW-00-01-02 的 CMake 目标组（11 组）为行基，每行六字段：

- **原版目标**——`target`（组件名 / 目标组）与 `dir`（参考树相对目录）；
- **启用条件**——Make 特性开关（`useXxx=`）与 `ifeq` 守卫，或 CMake 组的 defines / flags；
- **任务号**——映射表（`coverage_ledger_scan.py` 的 `COVERAGE`）给出的 BW 任务；无映射记 `未映射`；
- **夹具**——练习该面的证据文档（`docs/review/bw*.md`）；
- **状态**——`已覆盖`（有证据练习）/ `部分`（新版组件存在但只到进程启动）/ `缺口`（无任务或无证据，缺口来源见说明列）。

映射原则：只有证据文档实际练习过该面才记 `已覆盖`；单测目录一律映射 BW-00-02-02（原版单测基线复跑），与父库状态无关。

## 汇总

| 侧 | 行数 | 已覆盖 | 部分 | 缺口 |
| --- | --- | --- | --- | --- |
| Make 组件（in_build 77） | 79 | 35 | 9 | 35 |
| CMake 目标组 | 11 | 0 | 0 | 11 |

## Make 组件 · 已覆盖（35）

| 原版目标 | 目录 | 类型 | 构建 | 启用条件 | 任务号 | 夹具 | 说明 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `simple_python_client` | `examples/client_integration/python/simple` | binary | 1 | `useOpenSSL=1 usePython=1` | BW-00-02-04 | bw00-02-04-client-login | 登录驱动与实体脚本 |
| `connection` | `lib/connection` | library | 1 | `useOpenSSL=1` | BW-00-02-04 | bw00-02-04-client-login | 客户端登录连接 |
| `connection_model` | `lib/connection_model` | library | 1 | `useOpenSSL=1` | BW-00-02-04 | bw00-02-04-client-login | 连接模型 |
| `cstdmf` | `lib/cstdmf/unit_test` | unit_test | 1 | `—` | BW-00-02-02 | bw00-02-02-unit-tests | 原版单测基线已复跑（21 项，16 通过） |
| `db` | `lib/db` | library | 1 | `usePython=1` | BW-00-04-03 | bw00-04-03-entity-db-fixtures | 数据库表/行与实体持久化 |
| `db` | `lib/db/unit_test` | unit_test | 1 | `—` | BW-00-02-02 | bw00-02-02-unit-tests | 原版单测基线已复跑（21 项，16 通过） |
| `db_storage` | `lib/db_storage` | library | 1 | `usePython=1` | BW-00-04-03 | bw00-04-03-entity-db-fixtures | DB 存储抽象 |
| `entitydef` | `lib/entitydef` | library | 1 | `usePython=1` | BW-04-01-01 | bw04-01-01-entitydef-parse | 实体定义解析 |
| `entitydef` | `lib/entitydef/unit_test` | unit_test | 1 | `useOpenSSL=1 usePython=1` | BW-00-02-02 | bw00-02-02-unit-tests | 原版单测基线已复跑（21 项，16 通过） |
| `math` | `lib/math/unit_test` | unit_test | 1 | `—` | BW-00-02-02 | bw00-02-02-unit-tests | 原版单测基线已复跑（21 项，16 通过） |
| `network` | `lib/network` | library | 1 | `useOpenSSL=1` | BW-00-04-01 | bw00-04-01-protocol-fixtures | machined 线路协议（MGMPacket/machine_guard） |
| `network` | `lib/network/unit_test` | unit_test | 1 | `useOpenSSL=1` | BW-00-02-02 | bw00-02-02-unit-tests | 原版单测基线已复跑（21 项，16 通过） |
| `physics2` | `lib/physics2/unit_test` | unit_test | 1 | `—` | BW-00-02-02 | bw00-02-02-unit-tests | 原版单测基线已复跑（21 项，16 通过） |
| `pyscript` | `lib/pyscript` | library | 1 | `usePython=1` | BW-00-03-03 | bw00-04-02-python-callback-fixtures | Python 主机与回调序列 |
| `pyscript` | `lib/pyscript/unit_test` | unit_test | 1 | `usePython=1` | BW-00-02-02 | bw00-02-02-unit-tests | 原版单测基线已复跑（21 项，16 通过） |
| `resmgr` | `lib/resmgr` | library | 1 | `—` | BW-00-02-01 | bw00-02-01-runtime-env | 资源路径解析与 per-env 配置 |
| `resmgr` | `lib/resmgr/unit_test` | unit_test | 1 | `—` | BW-00-02-02 | bw00-02-02-unit-tests | 原版单测基线已复跑（21 项，16 通过） |
| `scene` | `lib/scene/unit_test` | unit_test | 1 | `—` | BW-00-02-02 | bw00-02-02-unit-tests | 原版单测基线已复跑（21 项，16 通过） |
| `script` | `lib/script` | library | 1 | `usePython=1` | BW-00-03-03 | bw00-04-02-python-callback-fixtures | 脚本绑定 |
| `script` | `lib/script/unit_test` | unit_test | 1 | `usePython=1` | BW-00-02-02 | bw00-02-02-unit-tests | 原版单测基线已复跑（21 项，16 通过） |
| `server` | `lib/server/unit_test` | unit_test | 1 | `—` | BW-00-02-02 | bw00-02-02-unit-tests | 原版单测基线已复跑（21 项，16 通过） |
| `terrain` | `lib/terrain/unit_test` | unit_test | 1 | `usePNG=1` | BW-00-02-02 | bw00-02-02-unit-tests | 原版单测基线已复跑（21 项，16 通过） |
| `unit_test_lib` | `lib/unit_test_lib/unit_test` | unit_test | 1 | `—` | BW-00-02-02 | bw00-02-02-unit-tests | 原版单测基线已复跑（21 项，16 通过） |
| `baseapp` | `server/baseapp/unit_test` | unit_test | 1 | `useOpenSSL=1 useSQLite=1` | BW-00-02-02 | bw00-02-02-unit-tests | 原版单测基线已复跑（21 项，16 通过） |
| `baseappmgr` | `server/baseappmgr/unit_test` | unit_test | 1 | `—` | BW-00-02-02 | bw00-02-02-unit-tests | 原版单测基线已复跑（21 项，16 通过） |
| `cellapp` | `server/cellapp/unit_test` | unit_test | 1 | `usePython=1` | BW-00-02-02 | bw00-02-02-unit-tests | 原版单测基线已复跑（21 项，16 通过） |
| `cellappmgr` | `server/cellappmgr/unit_test` | unit_test | 1 | `—` | BW-00-02-02 | bw00-02-02-unit-tests | 原版单测基线已复跑（21 项，16 通过） |
| `dbapp` | `server/dbapp/unit_test` | unit_test | 1 | `—` | BW-00-02-02 | bw00-02-02-unit-tests | 原版单测基线已复跑（21 项，16 通过） |
| `bwengine_mysql` | `server/dbapp_extensions/bwengine_mysql` | shared_library | 1 | `useMySQL=1 usePython=1 ; guard: ifneq ($(SYSTEM_HAS_MYSQL),0)` | BW-00-04-03 | bw00-04-03-entity-db-fixtures | MySQL 存储扩展（DB 证据为 MySQL 路径） |
| `dbappmgr` | `server/dbappmgr/unit_test` | unit_test | 1 | `—` | BW-00-02-02 | bw00-02-02-unit-tests | 原版单测基线已复跑（21 项，16 通过） |
| `loginapp` | `server/loginapp/unit_test` | unit_test | 1 | `—` | BW-00-02-02 | bw00-02-02-unit-tests | 原版单测基线已复跑（21 项，16 通过） |
| `reviver` | `server/reviver/unit_test` | unit_test | 1 | `—` | BW-00-02-02 | bw00-02-02-unit-tests | 原版单测基线已复跑（21 项，16 通过） |
| `bwmachined2` | `server/tools/bwmachined` | binary | 1 | `—` | BW-00-04-01 | bw00-04-01-protocol-fixtures | machined 发现协议 |
| `CppUnitLite2` | `third_party/CppUnitLite2/src` | library | 1 | `—` | BW-00-02-02 | bw00-02-02-unit-tests | 单测框架 |
| `process_defs` | `tools/process_defs` | binary | 1 | `useOpenSSL=1 usePython=1` | BW-00-02-01 | bw00-02-01-runtime-env | 运行时环境与进程定义 |

## Make 组件 · 部分（9）

| 原版目标 | 目录 | 类型 | 构建 | 启用条件 | 任务号 | 夹具 | 说明 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `server` | `lib/server` | library | 1 | `usePython=1` | BW-01-04 | bw00-04-04-stack-comparison | ServerApp/ServerAppConfig 已建；全量 init 链 BW-01-04 |
| `baseapp` | `server/baseapp` | binary | 1 | `useCurl=1 useOpenSSL=1 usePyURLRequest=1 usePython=1 useSQLite=1` | BW-00-03-05 | bw00-04-04-stack-comparison | 角色入口已实现（进程启动）；深度 init 链 BW-01-04 |
| `baseappmgr` | `server/baseappmgr` | binary | 1 | `—` | BW-00-03-05 | bw00-04-04-stack-comparison | 角色入口已实现（进程启动）；深度 init 链 BW-01-04 |
| `cellapp` | `server/cellapp` | binary | 1 | `useCurl=1 useOpenSSL=1 usePyURLRequest=1 usePython=1` | BW-00-03-05 | bw00-04-04-stack-comparison | 角色入口已实现（进程启动）；深度 init 链 BW-01-04 |
| `cellappmgr` | `server/cellappmgr` | binary | 1 | `—` | BW-00-03-05 | bw00-04-04-stack-comparison | 角色入口已实现（进程启动）；深度 init 链 BW-01-04 |
| `dbapp` | `server/dbapp` | binary | 1 | `useCustomBillingSystem=0 usePyURLRequest=1` | BW-00-03-05 | bw00-04-04-stack-comparison | 角色入口已实现（进程启动）；深度 init 链 BW-01-04 |
| `dbappmgr` | `server/dbappmgr` | binary | 1 | `—` | BW-00-03-05 | bw00-04-04-stack-comparison | 角色入口已实现（进程启动）；深度 init 链 BW-01-04 |
| `loginapp` | `server/loginapp` | binary | 1 | `useOpenSSL=1` | BW-00-03-05 | bw00-04-04-stack-comparison | 角色入口已实现（进程启动）；深度 init 链 BW-01-04 |
| `reviver` | `server/reviver` | binary | 1 | `—` | BW-00-03-05 | bw00-04-04-stack-comparison | 角色入口已实现（进程启动）；深度 init 链 BW-01-04 |

## Make 组件 · 缺口（35）

缺口来源逐行给出；`未映射` 行是尚未指派任务的显式缺口（BW-00-05-02..05 逐组补映射）。

| 原版目标 | 目录 | 类型 | 构建 | 启用条件 | 任务号 | 夹具 | 说明 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `cellapp_example` | `examples/cellapp_extension` | shared_library | 1 | `usePython=1` | BW-00-01-01 | bw00-01-build-targets | 参考侧 cellapp_example.so 已盘点；新版尚无 cellapp 扩展加载面，无运行时练习证据 |
| `build` | `lib/build` | library | 1 | `usePython=1` | 未映射 | — | 缺口：构建期辅助库，无独立运行时验收面 |
| `chunk` | `lib/chunk` | library | 1 | `usePython=1` | 未映射 | — | 缺口：空间块（chunk）加载；新版尚无对应 crate，空间数据面未启动 |
| `chunk_loading` | `lib/chunk_loading` | library | 1 | `—` | 未映射 | — | 缺口：空间块流式加载；新版尚无对应 crate，空间数据面未启动 |
| `cstdmf` | `lib/cstdmf` | library | 1 | `—` | 未映射 | — | 基础部件；新版由 crates/* 分散替代，尚无逐项映射证据 |
| `db_storage_mysql` | `lib/db_storage_mysql` | library | 1 | `useMySQL=1 usePython=1` | 未映射 | — | 缺口：MySQL 存储后端实现基座；运行时路径由 dbapp_extensions/bwengine_mysql 承载（已映射） |
| `db_storage_xml` | `lib/db_storage_xml` | library | 1 | `usePython=1` | 未映射 | — | 缺口：XML 存储后端；无运行时证据（DB 证据为 MySQL 路径） |
| `delegate_interface` | `lib/delegate_interface` | library | 1 | `—` | 未映射 | — | 缺口：内部接口基座，无独立验收面 |
| `entitydef_script` | `lib/entitydef_script` | library | 1 | `usePython=1` | 未映射 | — | 缺口：脚本侧实体定义绑定；脚本绑定面未建 |
| `math` | `lib/math` | library | 1 | `—` | 未映射 | — | 缺口：数学基元；无对应 Rust crate |
| `memhook` | `lib/memhook` | library | 1 | `—` | 未映射 | — | 缺口：内存挂钩；仅客户端/嵌入面 |
| `moo` | `lib/moo` | library | 1 | `usePNG=1` | 未映射 | — | 缺口：渲染管线；客户端渲染面未启动 |
| `physics2` | `lib/physics2` | library | 1 | `—` | 未映射 | — | 缺口：2D 物理；无对应 crate |
| `scene` | `lib/scene` | library | 1 | `—` | 未映射 | — | 缺口：场景管理；客户端/场景面未启动 |
| `speedtree` | `lib/speedtree` | library | 1 | `—` | 未映射 | — | 缺口：植被渲染；第三方渲染面未启动 |
| `sqlite` | `lib/sqlite` | library | 1 | `useSQLite=1` | 未映射 | — | 缺口：SQLite 封装；存储面未建 |
| `terrain` | `lib/terrain` | library | 1 | `—` | 未映射 | — | 缺口：地形；客户端/空间面未启动 |
| `unit_test_lib` | `lib/unit_test_lib` | library | 1 | `—` | 未映射 | — | 缺口：单测基座库；随 BW-00-02-02 单测基线（框架侧已覆盖） |
| `urlrequest` | `lib/urlrequest` | library | 1 | `useCurl=1` | 未映射 | — | 缺口：URL 请求（curl）；无对应 crate |
| `waypoint` | `lib/waypoint` | library | 1 | `usePython=1` | 未映射 | — | 缺口：寻路；无对应 crate |
| `serviceapp` | `server/baseapp` | copy_of_binary | 1 | `—` | BW-00-04-05 | bw00-04-05-substitution-run | 新版 boundless-server 尚未接受该角色（BW-00-04-05 替换运行为 rejected） |
| `bwengine_xml` | `server/dbapp_extensions/bwengine_xml` | shared_library | 1 | `usePython=1` | BW-00-01-01 | bw00-01-build-targets | XML 存储扩展；参考侧已盘点，无运行时证据（DB 证据为 MySQL 路径） |
| `bots` | `server/tools/bots` | binary | 1 | `useOpenSSL=1 usePython=1` | BW-00-01-01 | bw00-01-build-targets | bots 负载客户端，尚无对应实现与证据 |
| `_bw_profile` | `server/tools/bw_profile` | shared_library | 1 | `usePython=1 useSystemPython=0` | 未映射 | — | 缺口：性能剖析（_bw_profile.so）；无对应实现 |
| `clear_auto_load` | `server/tools/clear_auto_load` | binary | 1 | `useMySQL=1 useOpenSSL=1 usePython=1 ; guard: ifneq ($(SYSTEM_HAS_MYSQL),0)` | 未映射 | — | 缺口：DB 自动加载清理；DB 管理面未建 |
| `consolidate_dbs` | `server/tools/consolidate_dbs` | binary | 1 | `useMySQL=1 useOpenSSL=1 usePython=1 useSQLite=1 ; guard: ifneq ($(SYSTEM_HAS_MYSQL),0)` | 未映射 | — | 缺口：DB 合并；DB 管理面未建 |
| `message_logger` | `server/tools/message_logger` | binary | 1 | `useJSON=1 useMongoDB=1` | 未映射 | — | 缺口：日志服务器（message_logger + _bwlog）；无对应实现 |
| `_bwlog` | `server/tools/message_logger` | shared_library | 1 | `useMongoDB=1 usePython=1 useSystemPython=0` | 未映射 | — | 缺口：日志服务器（message_logger + _bwlog）；无对应实现 |
| `remove_db` | `server/tools/remove_db` | script_copy | 1 | `—` | 未映射 | — | 缺口：删除 DB 脚本；DB 管理面未建 |
| `snapshot_helper` | `server/tools/snapshot_helper` | binary | 1 | `useMySQL=1 ; guard: ifneq ($(SYSTEM_HAS_MYSQL),0)` | 未映射 | — | 缺口：DB 快照；DB 管理面未建 |
| `sync_db` | `server/tools/sync_db` | binary | 1 | `useMySQL=1 useOpenSSL=1 usePython=1 ; guard: ifneq ($(SYSTEM_HAS_MYSQL),0)` | 未映射 | — | 缺口：DB 同步；DB 管理面未建 |
| `transfer_db` | `server/tools/transfer_db` | binary | 1 | `useSQLite=1` | 未映射 | — | 缺口：DB 迁移；DB 管理面未建 |
| `png` | `third_party/png` | library | 0 | `—` | BW-00-01-01 | bw00-01-build-targets | vcpkg 迁移后残留空壳，不再由 Make 构建 |
| `zip` | `third_party/zip` | library | 0 | `—` | BW-00-01-01 | bw00-01-build-targets | vcpkg 迁移后残留空壳，不再由 Make 构建 |
| `res_packer` | `tools/res_packer` | binary | 1 | `usePython=1` | BW-00-01-01 | bw00-01-build-targets | 资源打包工具，尚无对应实现与证据 |

## CMake 目标组 · 缺口（11）

全部 11 组为 Windows 构建面：本 Linux 主机无产物与 MSVC 工具链（BW-00-02-05 复测），由 windows-latest CI 承载。`server` 组的 Linux 路径由 Make 承载，见上方 make 行。

| 目标组 | 工程数 | 启用条件 | 缺口来源 |
| --- | --- | --- | --- |
| `assetprocessor` | 26 | `defines: BWCLIENT_AS_PYTHON_MODULE=1, PYMODULE ; flags: —` | Windows 构建面；本 Linux 主机无产物与 MSVC 工具链，由 GitHub Actions windows-latest 承载（BW-00-02-05 复测） |
| `bwentity` | 17 | `defines: — ; flags: —` | Windows 构建面；本 Linux 主机无产物与 MSVC 工具链，由 GitHub Actions windows-latest 承载（BW-00-02-05 复测） |
| `client` | 58 | `defines: BW_CLIENT, USE_MEMHOOK, ALLOW_STACK_CONTAINER ; flags: BW_SCALEFORM_SUPPORT=OFF, USE_MEMHOOK=ON` | Windows 构建面；本 Linux 主机无产物与 MSVC 工具链，由 GitHub Actions windows-latest 承载（BW-00-02-05 复测） |
| `client_blob` | 0 | `defines: — ; flags: —` | Windows 构建面；本 Linux 主机无产物与 MSVC 工具链，由 GitHub Actions windows-latest 承载（BW-00-02-05 复测） |
| `exporters` | 33 | `defines: BW_EXPORTER, _SECURE_SCL=1 ; flags: BW_FMOD_SUPPORT=OFF, BW_SPEEDTREE_SUPPORT=OFF, BW_UMBRA_SUPPORT=OFF` | Windows 构建面；本 Linux 主机无产物与 MSVC 工具链，由 GitHub Actions windows-latest 承载（BW-00-02-05 复测） |
| `exporters_blob` | 0 | `defines: — ; flags: —` | Windows 构建面；本 Linux 主机无产物与 MSVC 工具链，由 GitHub Actions windows-latest 承载（BW-00-02-05 复测） |
| `extras` | 31 | `defines: USE_MEMHOOK ; flags: USE_MEMHOOK=ON` | Windows 构建面；本 Linux 主机无产物与 MSVC 工具链，由 GitHub Actions windows-latest 承载（BW-00-02-05 复测） |
| `navgen` | 30 | `defines: _NAVGEN ; flags: —` | Windows 构建面；本 Linux 主机无产物与 MSVC 工具链，由 GitHub Actions windows-latest 承载（BW-00-02-05 复测） |
| `server` | 77 | `defines: MF_SERVER, MF_CONFIG="Hybrid" ; flags: BW_FMOD_SUPPORT=OFF, BW_PCH_SUPPORT=OFF, BW_SCALEFORM_SUPPORT=OFF, BW_UMBRA_SUPPORT=OFF` | server 组 Linux 路径由 Make 承载（见 make 行）；Windows 构建面；本 Linux 主机无产物与 MSVC 工具链，由 GitHub Actions windows-latest 承载（BW-00-02-05 复测） |
| `tools` | 70 | `defines: EDITOR_ENABLED, ALLOW_STACK_CONTAINER, USE_MEMHOOK ; flags: USE_MEMHOOK=OFF` | Windows 构建面；本 Linux 主机无产物与 MSVC 工具链，由 GitHub Actions windows-latest 承载（BW-00-02-05 复测） |
| `tools_blob` | 0 | `defines: — ; flags: —` | Windows 构建面；本 Linux 主机无产物与 MSVC 工具链，由 GitHub Actions windows-latest 承载（BW-00-02-05 复测） |

## 可追溯性

- 行基：`reference/bw00_01_make_components.tsv`（Make 组件）、`reference/bw00_01_cmake_targets.json`（CMake 组）。
- 台账：`reference/bw00_05_01_coverage_ledger.json`。

生成：`make_component_scan.py` + `coverage_ledger_scan.py` + `coverage_ledger_doc.py`。
