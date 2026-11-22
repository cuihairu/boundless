# BW-00-05-02 映射引擎库和服务端目标 — 记录

- **任务编号**: BW-00-05-02（lane BW-00-05 功能覆盖台账，todo.md 行 145）
- **验收**: 构建清册中的每项都有索引。

## 结论

`docs/review/data/bw00-05-coverage-ledger.tsv` 的 79 个 Make 清册行**全部有
任务号索引**（`task=pending` 清零）。加上 05-01 的种子，状态分布：
covered 4、partial 8、blocked 5、excluded 3、pending 59。

## 映射方法

任务号以 todo.md 各 lane 的**源码列表**为权威：组件的 `dir` 命中哪个 lane
的源码列表，就归该 lane（如 `bigworld/lib/db_storage_mysql` 命中 BW-09 源码
列表 → BW-09-02 MySQL 持久化）。有明确小节的落到小节级，跨小节的落到
lane 级。本次填入 59 行（05-01 已种 20 行不动）：

| 面 | 行 | 任务号 |
|---|---|---|
| 基础库 | build、memhook、math/unit_test、server/unit_test | BW-01（lib/build、lib/memhook 在 BW-01 源码列表） |
| 网络 | network、connection、network/unit_test | BW-02 |
| 连接模型 | connection_model | BW-06-04（BW-06 源码列表含 lib/connection_model） |
| 脚本 | script、pyscript 及其 unit_test | BW-03 |
| EntityDef | entitydef、entitydef_script、process_defs | BW-04-01 / BW-04-04 |
| 集群 | reviver 及 unit_test、baseappmgr/cellappmgr/dbappmgr unit_test | BW-05-02 / 03 / 04 / 05 |
| 进程单测 | loginapp/baseapp/cellapp/dbapp unit_test | BW-06-01 / BW-07-01 / BW-08-01 / BW-09-01 |
| 持久化 | db、db_storage、db_storage_mysql、db_storage_xml、sqlite、bwengine_* 及 db/dbapp unit_test | BW-09-01..05 |
| 世界 | chunk、chunk_loading、delegate_interface、physics2、scene、terrain、waypoint 及 unit_test | BW-10-01..04 / BW-10 |
| 客户端 | moo、speedtree | BW-11 |
| 观测工具 | message_logger、_bwlog、_bw_profile、bots | BW-12-02 / 03 / 04 |
| 离线工具 | res_packer | BW-13-02 |
| 示例 | cellapp_example、simple_python_client | BW-14-02 |
| 测试框架 | unit_test_lib/unit_test | BW-18（内部测试框架） |
| 运维工具 | sync_db、consolidate_dbs、transfer_db、snapshot_helper、clear_auto_load、remove_db | BW-09-05（源码列表逐名点名） |

18 个原版单测行（pending）补 note「原版单测声明未构建（BW-00-02-02: 21 个
目标仅 3 个有可执行）」——rerun 面的缺口可追溯。

## 判断项（源码列表未点名，如实标注）

- `urlrequest`（lib/urlrequest）不在任何 lane 源码列表——按「外部传输」
  归 BW-02-04（HTTP 经 curl，传输面），后续 BW-02 落地时如需改判再更新行。
- `delegate_interface` 归 BW-10 lane 级（源码列表点名 lib/delegate_interface
  但跨小节）。
- `bots` 归 BW-12-04（BW-12 源码列表点名 server/tools/bots；BW-17 Bots 压测
  为后续压测面，不动本索引）。

## 顺带修正（本项内）

- 校验器 `task` 正则放宽为 `BW-xx[-yy[-zz]]`：lane 级引用（BW-02 等）是
  合法任务号（13 行 lane 级索引被旧正则误拒）；字段契约文档同步。

## 校验与门禁

- `python3 docs/review/tools/check_coverage_ledger.py`：**79 行全字段合法、
  与清单双向 1:1、task=pending 清零**，rc=0。
- `--scaffold`：空；`--schema-only`：rc=0；`py_compile` 过。
- 本项纯台账/工具，未触任何 crate；workspace 级 cargo 测试未跑（同前，
  兄弟会话在途债务与本项无关）。

## 边界声明

本项只完成 **Make 清册 79 项**的索引（引擎库+服务端目标+服务端工具+单测+第三方）；
CMake/Windows 面（origin=cmake）、common/examples/FantasyDemo（origin=resource）、
发行物/SDK/缺失资源（origin=release/gap）分别归 BW-00-05-03/04/05 登记。

## 状态

- [x] 79 项全部有任务号索引（pending 清零）
- [x] 归属按 lane 源码列表，判断项如实标注
- [x] 18 个未构建原版单测补缺口 note
- [x] 校验绿（rc=0，双向 1:1）
