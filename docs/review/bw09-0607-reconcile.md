# BW-09-06/07 PostgreSQL 勾账（合并后主基核对）

- **任务编号**: BW-09-06-01..11 / BW-09-07-01..07（todo.md 行 1355-1381）
- **动机**: 五条 lane 并入 main（`f1c9f06`）后，`todo.md` 的 BW-09-01..05 各行为已勾
  状态、`crates/db-postgres` 代码在树，而 BW-09-06/07 共 18 行全部未勾。本席把这两节
  逐行对照 main 现树重新核验，只勾代码可证者，并把未勾原因登记在册。

## 背景：db-postgres 的并账取形

- `git log origin/main -- crates/db-postgres/` 显示 main 上该目录仅由八个交付提交推进：
  `4e8ba87`（实体 schema 映射）→ `e68158f`（账号表）→ `e71ccdc`（建库）→
  `022bee4`（dry-run）→ `9aacde0`（schema 更新）→ `a289d8d`（表序列 DBID）→
  `e4d24e5`（备份导出恢复）→ `a5225bb`（并入 lane/bw19-docs）。
- 本席 `lane/bw16-18` 的后续交付虽在 main 祖先链中（`git merge-base --is-ancestor`
  均命中：`fe41751` 存储契约、`e240260` 事务提交、`23108c6`/`bb1dc2c` 回滚与冲突、
  `348c360` 断链恢复、`e7df81e` 重登验证、`4de1257` auto-load、`fc61d0d` 导入、
  `0cbc334` 校准、`3f218d8` 重跑），但**其 `crates/db-postgres` 内容被上述并账覆盖**：
  main 现树的 `store.rs` 无 `impl EntityStorage for PostgresStorage`，无事务/重连面，
  无 `import_mysql.rs`。属 f2ef112 同类的并账静默失码。
- 对照现状：`git grep "impl EntityStorage for"` 在 main 只有 `crates/db` 侧
  （`StorageConnection`/`MemoryStorage`/`MemoryConnection`/`XmlDatabase`）与测试替身，
  无任何 PG 实现。
- `docs/review/bw09-06-05..11`、`bw09-07-05..07` 各文档在 main 树中留存（文档不参与该冲突
  取舍），故现树「有文档、无对应代码」。

## 核验方法与门禁

- 环境：本地 PG18（`127.0.0.1:5432`），`BOUNDLESS_PG_URL="host=127.0.0.1 user=cui
  password=boundless-dev dbname=postgres"`；集成测试以该变量为服务端门，缺则跳过，
  本次真跑未跳过。凭据仅留在会话环境，不入树。
- `cargo test -p db-postgres`：**79/79 绿**（lib 50 + 集成 29 = accounts 4、backup 6、
  dbid 5、dry_run 3、round_trip 6、schema_init 2、upgrade 3）。
- 未提交任何代码改动；本核对为台账勾账，不改 crate。

## 逐行裁定

| 行 | 裁定 | main 面 | 验收证据 |
| --- | --- | --- | --- |
| 06-01 主存储选择 | **未勾** | 无选择/报告面；`apps/server` 仍 36 行无 dbapp 入口 | 验收要求「DBApp 启动时报告所用后端」，启动面缺位 |
| 06-02 schema 与字段映射 | **已勾** | `schema.rs` + `json.rs`（4e8ba87） | `round_trip.rs` 6 例：`unicode_binary_and_nested_values_round_trip`、`a_missing_row_reads_as_none`、`a_row_keeps_only_the_last_write`、`backend_errors_map_to_the_contract` |
| 06-03 账号表与唯一约束 | **已勾** | `accounts.rs`（e68158f，`bigworldLogOnMapping` 主键 + `map_unique_violation`） | `accounts.rs` 集成 4 例：`a_duplicate_account_name_is_a_contract_error`（重复名=合同错误）、`the_name_key_is_bytes_not_text`、`a_missing_account_reads_as_none`、`the_entity_type_and_id_survive_the_round_trip` |
| 06-04 DBID 分配 | **已勾** | `create_entity` 表序列（a289d8d） | `dbid.rs` 5 例：`concurrent_creations_never_repeat_an_id`（并发无重复）、`allocation_continues_after_a_reconnect`（重启连续可用）、`an_explicit_id_does_not_advance_the_sequence`、`each_entity_table_allocates_its_own_ids` |
| 06-05 实体读写删查 | **未勾** | `put_entity`/`create_entity`/`get_entity` 在，但无 `EntityStorage` 实现、无删除腿 | 验收「共用存储契约用例通过」不成立；本席 `fe41751`（+537 行 store.rs + 契约用例）被覆没 |
| 06-06 事务提交 | **未勾** | 无 `BEGIN`/`COMMIT` 面 | 本席 `e240260`（store.rs +50、tests/transaction.rs 274 行）被覆没 |
| 06-07 事务回滚 | **未勾** | 同上 | 本席 `23108c6` 被覆没 |
| 06-08 并发写冲突 | **未勾** | 仅 `map_write_error`，无重试回路 | 本席 `bb1dc2c`（死锁两锁条件 Retry 映射 + `shouldRetry`）被覆没 |
| 06-09 断链恢复 | **未勾** | 无 `run_with_reconnect`/reconnect 面 | 本席 `348c360` 对应的 16 语句缝统一 + conninfo 重连被覆没 |
| 06-10 账号重登验证 | **未勾** | 无会话记录面（`server-runtime` 侧 log_on 面属 BW-14） | 本席 `e7df81e`（`bigworldLogOns` upsert + `htonl`/`htons`）被覆没 |
| 06-11 auto-load 与 SQLite 归档 | **未勾** | `crates/db/src/auto_load*.rs` 在（BW-09-03-04 侧），无 PG 对接 | 本席 `4de1257` 被覆没 |
| 07-01 初始化新库 | **已勾** | `schema_init.rs` plan/apply/read_version（e71ccdc） | `schema_init.rs` 集成 2 例：`an_empty_database_becomes_a_startable_schema`、`an_uninitialised_database_reads_no_version` |
| 07-02 schema dry-run | **已勾** | `dry_run.rs`（022bee4，只读语句门 + 缺口报告） | `dry_run.rs` 集成 3 例：`a_fresh_database_is_reported_and_left_unchanged`（前后不变）、`an_up_to_date_database_reports_nothing`、`a_partially_initialised_database_reports_its_gaps` |
| 07-03 应用 schema 更新 | **已勾** | `upgrade.rs`（9aacde0） | `upgrade.rs` 集成 3 例：`an_old_sample_upgrades_and_stays_readable`、`a_missing_table_plans_a_create`、`an_unchanged_table_plans_nothing` |
| 07-04 备份导出与恢复 | **已勾** | `backup.rs` export/restore（e4d24e5，本席 e2cf2e8 同面双实现，并账取此） | `backup.rs` 集成 6 例，含 `an_empty_target_restores_identical_records`（空目标恢复相同记录）；src 内 10 例 |
| 07-05 导入 MySQL | **未勾** | 无 `import_mysql.rs` | 本席 `fc61d0d`（import_mysql.rs 111 行 + 集成 275 行）被覆没 |
| 07-06 校准导入后 DBID | **未勾** | 校准机制仅在恢复腿：`backup.rs:509` `setval(pg_get_serial_sequence(…), …)`，无导入后校准入口 | 本席 `0cbc334`（`calibrate = setval(pg_get_serial_sequence, max+1, false)`，直传带引号表名）被覆没；导入腿缺位使验收「新建实体不冲突」无从复现 |
| 07-07 迁移失败与重跑 | **未勾** | 无重跑/幂等面 | 本席 `3f218d8` 被覆没 |

## 结论与后续

- 本次勾账只动 `todo.md`：新勾 7 行（06-02/03/04、07-01/02/03/04），余 11 行保持未勾并
  在节末登记原因。
- 重交清单（本席 BW-09-06/07 接手范围，用户令「不做重复，转 06/07」）：06-05 契约接线为
  基座（`impl EntityStorage for PostgresStorage` + 删除腿 + 共用契约用例），其上挂
  06-06/07/08/09（事务与恢复）；06-10/06-11（重登与归档）依赖 06-05；07-05/06/07（导入、
  校准、重跑）独立成簇；06-01 需 dbapp 启动面，随 BW-14 tick 循环面一并解锁。
- 参考源现成：上述提交均在 main 祖先链中，`git show <sha>:crates/db-postgres/src/store.rs`
  等可直接取回参照实现，重交时须按 main 现树形状（尤其 `create_entity` 的表序列分配，
  并账已定）改写，不得整体回填旧 store.rs。