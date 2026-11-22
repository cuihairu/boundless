# BW-00-05-02 引擎库和服务端目标索引

> 证据面（映射验收：构建清册中的每项都有索引）。

行基取 BW-00-01-01 的 Make 组件清册中 `lib` 与 `server` 下的全部目标（共 72 行）。索引条目二选一：

- **已映射**——`任务号` + `夹具`（练习该面的证据文档）；
- **缺口**——`缺口：` 开头的具体来源（目标是什么、新版缺什么、阻塞原因），任务号记 `未映射`。

## 汇总

| 侧 | 行数 | 已映射 | 缺口（带来源） | 未索引 |
| --- | --- | --- | --- | --- |
| `lib` 与 `server` | 72 | 44 | 28 | 0 |

未索引 = 0，范围内每项均有索引条目。

## 索引明细

| 原版目标 | 目录 | 类型 | 构建 | 启用条件 | 任务号 | 夹具 | 状态 | 索引条目 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `build` | `lib/build` | library | 1 | `usePython=1` | 未映射 | — | 缺口 | 缺口：构建期辅助库，无独立运行时验收面 |
| `chunk` | `lib/chunk` | library | 1 | `usePython=1` | 未映射 | — | 缺口 | 缺口：空间块（chunk）加载；新版尚无对应 crate，空间数据面未启动 |
| `chunk_loading` | `lib/chunk_loading` | library | 1 | `—` | 未映射 | — | 缺口 | 缺口：空间块流式加载；新版尚无对应 crate，空间数据面未启动 |
| `connection` | `lib/connection` | library | 1 | `useOpenSSL=1` | BW-00-02-04 | bw00-02-04-client-login | 已覆盖 | 已映射：客户端登录连接 |
| `connection_model` | `lib/connection_model` | library | 1 | `useOpenSSL=1` | BW-00-02-04 | bw00-02-04-client-login | 已覆盖 | 已映射：连接模型 |
| `cstdmf` | `lib/cstdmf` | library | 1 | `—` | 未映射 | — | 缺口 | 基础部件；新版由 crates/* 分散替代，尚无逐项映射证据 |
| `cstdmf` | `lib/cstdmf/unit_test` | unit_test | 1 | `—` | BW-00-02-02 | bw00-02-02-unit-tests | 已覆盖 | 已映射：原版单测基线已复跑（21 项，16 通过） |
| `db` | `lib/db` | library | 1 | `usePython=1` | BW-00-04-03 | bw00-04-03-entity-db-fixtures | 已覆盖 | 已映射：数据库表/行与实体持久化 |
| `db` | `lib/db/unit_test` | unit_test | 1 | `—` | BW-00-02-02 | bw00-02-02-unit-tests | 已覆盖 | 已映射：原版单测基线已复跑（21 项，16 通过） |
| `db_storage` | `lib/db_storage` | library | 1 | `usePython=1` | BW-00-04-03 | bw00-04-03-entity-db-fixtures | 已覆盖 | 已映射：DB 存储抽象 |
| `db_storage_mysql` | `lib/db_storage_mysql` | library | 1 | `useMySQL=1 usePython=1` | 未映射 | — | 缺口 | 缺口：MySQL 存储后端实现基座；运行时路径由 dbapp_extensions/bwengine_mysql 承载（已映射） |
| `db_storage_xml` | `lib/db_storage_xml` | library | 1 | `usePython=1` | 未映射 | — | 缺口 | 缺口：XML 存储后端；无运行时证据（DB 证据为 MySQL 路径） |
| `delegate_interface` | `lib/delegate_interface` | library | 1 | `—` | 未映射 | — | 缺口 | 缺口：内部接口基座，无独立验收面 |
| `entitydef` | `lib/entitydef` | library | 1 | `usePython=1` | BW-04-01-01 | bw04-01-01-entitydef-parse | 已覆盖 | 已映射：实体定义解析 |
| `entitydef` | `lib/entitydef/unit_test` | unit_test | 1 | `useOpenSSL=1 usePython=1` | BW-00-02-02 | bw00-02-02-unit-tests | 已覆盖 | 已映射：原版单测基线已复跑（21 项，16 通过） |
| `entitydef_script` | `lib/entitydef_script` | library | 1 | `usePython=1` | 未映射 | — | 缺口 | 缺口：脚本侧实体定义绑定；脚本绑定面未建 |
| `math` | `lib/math` | library | 1 | `—` | 未映射 | — | 缺口 | 缺口：数学基元；无对应 Rust crate |
| `math` | `lib/math/unit_test` | unit_test | 1 | `—` | BW-00-02-02 | bw00-02-02-unit-tests | 已覆盖 | 已映射：原版单测基线已复跑（21 项，16 通过） |
| `memhook` | `lib/memhook` | library | 1 | `—` | 未映射 | — | 缺口 | 缺口：内存挂钩；仅客户端/嵌入面 |
| `moo` | `lib/moo` | library | 1 | `usePNG=1` | 未映射 | — | 缺口 | 缺口：渲染管线；客户端渲染面未启动 |
| `network` | `lib/network` | library | 1 | `useOpenSSL=1` | BW-00-04-01 | bw00-04-01-protocol-fixtures | 已覆盖 | 已映射：machined 线路协议（MGMPacket/machine_guard） |
| `network` | `lib/network/unit_test` | unit_test | 1 | `useOpenSSL=1` | BW-00-02-02 | bw00-02-02-unit-tests | 已覆盖 | 已映射：原版单测基线已复跑（21 项，16 通过） |
| `physics2` | `lib/physics2` | library | 1 | `—` | 未映射 | — | 缺口 | 缺口：2D 物理；无对应 crate |
| `physics2` | `lib/physics2/unit_test` | unit_test | 1 | `—` | BW-00-02-02 | bw00-02-02-unit-tests | 已覆盖 | 已映射：原版单测基线已复跑（21 项，16 通过） |
| `pyscript` | `lib/pyscript` | library | 1 | `usePython=1` | BW-00-03-03 | bw00-04-02-python-callback-fixtures | 已覆盖 | 已映射：Python 主机与回调序列 |
| `pyscript` | `lib/pyscript/unit_test` | unit_test | 1 | `usePython=1` | BW-00-02-02 | bw00-02-02-unit-tests | 已覆盖 | 已映射：原版单测基线已复跑（21 项，16 通过） |
| `resmgr` | `lib/resmgr` | library | 1 | `—` | BW-00-02-01 | bw00-02-01-runtime-env | 已覆盖 | 已映射：资源路径解析与 per-env 配置 |
| `resmgr` | `lib/resmgr/unit_test` | unit_test | 1 | `—` | BW-00-02-02 | bw00-02-02-unit-tests | 已覆盖 | 已映射：原版单测基线已复跑（21 项，16 通过） |
| `scene` | `lib/scene` | library | 1 | `—` | 未映射 | — | 缺口 | 缺口：场景管理；客户端/场景面未启动 |
| `scene` | `lib/scene/unit_test` | unit_test | 1 | `—` | BW-00-02-02 | bw00-02-02-unit-tests | 已覆盖 | 已映射：原版单测基线已复跑（21 项，16 通过） |
| `script` | `lib/script` | library | 1 | `usePython=1` | BW-00-03-03 | bw00-04-02-python-callback-fixtures | 已覆盖 | 已映射：脚本绑定 |
| `script` | `lib/script/unit_test` | unit_test | 1 | `usePython=1` | BW-00-02-02 | bw00-02-02-unit-tests | 已覆盖 | 已映射：原版单测基线已复跑（21 项，16 通过） |
| `server` | `lib/server` | library | 1 | `usePython=1` | BW-01-04 | bw00-04-04-stack-comparison | 部分 | 部分：ServerApp/ServerAppConfig 已建；全量 init 链 BW-01-04 |
| `server` | `lib/server/unit_test` | unit_test | 1 | `—` | BW-00-02-02 | bw00-02-02-unit-tests | 已覆盖 | 已映射：原版单测基线已复跑（21 项，16 通过） |
| `speedtree` | `lib/speedtree` | library | 1 | `—` | 未映射 | — | 缺口 | 缺口：植被渲染；第三方渲染面未启动 |
| `sqlite` | `lib/sqlite` | library | 1 | `useSQLite=1` | 未映射 | — | 缺口 | 缺口：SQLite 封装；存储面未建 |
| `terrain` | `lib/terrain` | library | 1 | `—` | 未映射 | — | 缺口 | 缺口：地形；客户端/空间面未启动 |
| `terrain` | `lib/terrain/unit_test` | unit_test | 1 | `usePNG=1` | BW-00-02-02 | bw00-02-02-unit-tests | 已覆盖 | 已映射：原版单测基线已复跑（21 项，16 通过） |
| `unit_test_lib` | `lib/unit_test_lib` | library | 1 | `—` | 未映射 | — | 缺口 | 缺口：单测基座库；随 BW-00-02-02 单测基线（框架侧已覆盖） |
| `unit_test_lib` | `lib/unit_test_lib/unit_test` | unit_test | 1 | `—` | BW-00-02-02 | bw00-02-02-unit-tests | 已覆盖 | 已映射：原版单测基线已复跑（21 项，16 通过） |
| `urlrequest` | `lib/urlrequest` | library | 1 | `useCurl=1` | 未映射 | — | 缺口 | 缺口：URL 请求（curl）；无对应 crate |
| `waypoint` | `lib/waypoint` | library | 1 | `usePython=1` | 未映射 | — | 缺口 | 缺口：寻路；无对应 crate |
| `baseapp` | `server/baseapp` | binary | 1 | `useCurl=1 useOpenSSL=1 usePyURLRequest=1 usePython=1 useSQLite=1` | BW-00-03-05 | bw00-04-04-stack-comparison | 部分 | 部分：角色入口已实现（进程启动）；深度 init 链 BW-01-04 |
| `serviceapp` | `server/baseapp` | copy_of_binary | 1 | `—` | BW-00-04-05 | bw00-04-05-substitution-run | 缺口 | 新版 boundless-server 尚未接受该角色（BW-00-04-05 替换运行为 rejected） |
| `baseapp` | `server/baseapp/unit_test` | unit_test | 1 | `useOpenSSL=1 useSQLite=1` | BW-00-02-02 | bw00-02-02-unit-tests | 已覆盖 | 已映射：原版单测基线已复跑（21 项，16 通过） |
| `baseappmgr` | `server/baseappmgr` | binary | 1 | `—` | BW-00-03-05 | bw00-04-04-stack-comparison | 部分 | 部分：角色入口已实现（进程启动）；深度 init 链 BW-01-04 |
| `baseappmgr` | `server/baseappmgr/unit_test` | unit_test | 1 | `—` | BW-00-02-02 | bw00-02-02-unit-tests | 已覆盖 | 已映射：原版单测基线已复跑（21 项，16 通过） |
| `cellapp` | `server/cellapp` | binary | 1 | `useCurl=1 useOpenSSL=1 usePyURLRequest=1 usePython=1` | BW-00-03-05 | bw00-04-04-stack-comparison | 部分 | 部分：角色入口已实现（进程启动）；深度 init 链 BW-01-04 |
| `cellapp` | `server/cellapp/unit_test` | unit_test | 1 | `usePython=1` | BW-00-02-02 | bw00-02-02-unit-tests | 已覆盖 | 已映射：原版单测基线已复跑（21 项，16 通过） |
| `cellappmgr` | `server/cellappmgr` | binary | 1 | `—` | BW-00-03-05 | bw00-04-04-stack-comparison | 部分 | 部分：角色入口已实现（进程启动）；深度 init 链 BW-01-04 |
| `cellappmgr` | `server/cellappmgr/unit_test` | unit_test | 1 | `—` | BW-00-02-02 | bw00-02-02-unit-tests | 已覆盖 | 已映射：原版单测基线已复跑（21 项，16 通过） |
| `dbapp` | `server/dbapp` | binary | 1 | `useCustomBillingSystem=0 usePyURLRequest=1` | BW-00-03-05 | bw00-04-04-stack-comparison | 部分 | 部分：角色入口已实现（进程启动）；深度 init 链 BW-01-04 |
| `dbapp` | `server/dbapp/unit_test` | unit_test | 1 | `—` | BW-00-02-02 | bw00-02-02-unit-tests | 已覆盖 | 已映射：原版单测基线已复跑（21 项，16 通过） |
| `bwengine_mysql` | `server/dbapp_extensions/bwengine_mysql` | shared_library | 1 | `useMySQL=1 usePython=1 ; guard: ifneq ($(SYSTEM_HAS_MYSQL),0)` | BW-00-04-03 | bw00-04-03-entity-db-fixtures | 已覆盖 | 已映射：MySQL 存储扩展（DB 证据为 MySQL 路径） |
| `bwengine_xml` | `server/dbapp_extensions/bwengine_xml` | shared_library | 1 | `usePython=1` | BW-00-01-01 | bw00-01-build-targets | 缺口 | XML 存储扩展；参考侧已盘点，无运行时证据（DB 证据为 MySQL 路径） |
| `dbappmgr` | `server/dbappmgr` | binary | 1 | `—` | BW-00-03-05 | bw00-04-04-stack-comparison | 部分 | 部分：角色入口已实现（进程启动）；深度 init 链 BW-01-04 |
| `dbappmgr` | `server/dbappmgr/unit_test` | unit_test | 1 | `—` | BW-00-02-02 | bw00-02-02-unit-tests | 已覆盖 | 已映射：原版单测基线已复跑（21 项，16 通过） |
| `loginapp` | `server/loginapp` | binary | 1 | `useOpenSSL=1` | BW-00-03-05 | bw00-04-04-stack-comparison | 部分 | 部分：角色入口已实现（进程启动）；深度 init 链 BW-01-04 |
| `loginapp` | `server/loginapp/unit_test` | unit_test | 1 | `—` | BW-00-02-02 | bw00-02-02-unit-tests | 已覆盖 | 已映射：原版单测基线已复跑（21 项，16 通过） |
| `reviver` | `server/reviver` | binary | 1 | `—` | BW-00-03-05 | bw00-04-04-stack-comparison | 部分 | 部分：角色入口已实现（进程启动）；深度 init 链 BW-01-04 |
| `reviver` | `server/reviver/unit_test` | unit_test | 1 | `—` | BW-00-02-02 | bw00-02-02-unit-tests | 已覆盖 | 已映射：原版单测基线已复跑（21 项，16 通过） |
| `bots` | `server/tools/bots` | binary | 1 | `useOpenSSL=1 usePython=1` | BW-00-01-01 | bw00-01-build-targets | 缺口 | bots 负载客户端，尚无对应实现与证据 |
| `_bw_profile` | `server/tools/bw_profile` | shared_library | 1 | `usePython=1 useSystemPython=0` | 未映射 | — | 缺口 | 缺口：性能剖析（_bw_profile.so）；无对应实现 |
| `bwmachined2` | `server/tools/bwmachined` | binary | 1 | `—` | BW-00-04-01 | bw00-04-01-protocol-fixtures | 已覆盖 | 已映射：machined 发现协议 |
| `clear_auto_load` | `server/tools/clear_auto_load` | binary | 1 | `useMySQL=1 useOpenSSL=1 usePython=1 ; guard: ifneq ($(SYSTEM_HAS_MYSQL),0)` | 未映射 | — | 缺口 | 缺口：DB 自动加载清理；DB 管理面未建 |
| `consolidate_dbs` | `server/tools/consolidate_dbs` | binary | 1 | `useMySQL=1 useOpenSSL=1 usePython=1 useSQLite=1 ; guard: ifneq ($(SYSTEM_HAS_MYSQL),0)` | 未映射 | — | 缺口 | 缺口：DB 合并；DB 管理面未建 |
| `message_logger` | `server/tools/message_logger` | binary | 1 | `useJSON=1 useMongoDB=1` | 未映射 | — | 缺口 | 缺口：日志服务器（message_logger + _bwlog）；无对应实现 |
| `_bwlog` | `server/tools/message_logger` | shared_library | 1 | `useMongoDB=1 usePython=1 useSystemPython=0` | 未映射 | — | 缺口 | 缺口：日志服务器（message_logger + _bwlog）；无对应实现 |
| `remove_db` | `server/tools/remove_db` | script_copy | 1 | `—` | 未映射 | — | 缺口 | 缺口：删除 DB 脚本；DB 管理面未建 |
| `snapshot_helper` | `server/tools/snapshot_helper` | binary | 1 | `useMySQL=1 ; guard: ifneq ($(SYSTEM_HAS_MYSQL),0)` | 未映射 | — | 缺口 | 缺口：DB 快照；DB 管理面未建 |
| `sync_db` | `server/tools/sync_db` | binary | 1 | `useMySQL=1 useOpenSSL=1 usePython=1 ; guard: ifneq ($(SYSTEM_HAS_MYSQL),0)` | 未映射 | — | 缺口 | 缺口：DB 同步；DB 管理面未建 |
| `transfer_db` | `server/tools/transfer_db` | binary | 1 | `useSQLite=1` | 未映射 | — | 缺口 | 缺口：DB 迁移；DB 管理面未建 |

## 可追溯性

- 台账：`reference/bw00_05_01_coverage_ledger.json`。
- 行基：`reference/bw00_01_make_components.tsv`。

生成：`coverage_ledger_scan.py` + `coverage_index_doc.py`。
