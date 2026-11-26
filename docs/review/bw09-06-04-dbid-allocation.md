# BW-09-06-04 DBID 分配 — 复核证据

- **任务编号**: BW-09-06-04（todo.md 行 1412）
- **验收**: 并发创建无重复且重启连续可用
- **Boundless 模块**: `crates/db-postgres/src/schema.rs`、
  `crates/db-postgres/src/store.rs`、`crates/db-postgres/tests/dbid_allocation.rs`
- **平台**: Linux x86_64，PostgreSQL 18（127.0.0.1:5432，真服务）

## 原版形状

| 原版位置 | 形状 |
|---|---|
| basictypes.hpp:191-192 | `typedef int64 DatabaseID;` + `const DatabaseID PENDING_DATABASE_ID = -1;`（移植即 `foundation/src/ids.rs` 的 `DatabaseId` 与 `PENDING_DATABASE_ID`） |
| column_type.cpp:55-58 / 78-80 | `addAutoIncrement()` → `" AUTO_INCREMENT"`；`" PRIMARY KEY"`——**DBID 列就是表的主键自增列** |
| buffered_entity_tasks.cpp:316-320 | 播放缓冲任务时 `if (pNextTask->dbID() == PENDING_DATABASE_ID) { pNextTask->dbID( 0 ); }`——**待分配在写入前变 0** |
| put_entity_task.cpp:60-95 | `MF_ASSERT( dbID_ != PENDING_DATABASE_ID )`；三分支：`dbID_ != 0 && !putExplicitID_`→`update`；`!putExplicitID_`→**`dbID_ = entityTypeMapping_.insertNew( conn, stream_ )`**（此前 `MF_ASSERT( pGameTime_ == NULL )`）；`putExplicitID_`→`insertExplicit(conn, dbID_, stream_)`；随后 **`if (dbID_ == 0)` 即失败**→`"PutEntityTask::performBackgroundTask: Failed to create new Entity '%s'\n"` |
| entity_type_mapping.cpp:147-148 / 190-191 | 构造即建**两条独立插入语句**：`insertNewQuery_`（`createInsertStatement`）与 `insertExplicitQuery_`（`createExplicitInsertStatement`）；`deleteIDQuery_ = "DELETE FROM <tbl> WHERE id=?"` |
| entity_type_mapping.cpp:710-721 `insertNew` | `UpdateFromStreamVisitor(connection, *this, /*dbID=*/0, insertNewQuery_, strm)` → `visit` → **`return visitor.execute() ? visitor.databaseID() : 0;`**（0 即失败信号） |
| stream_to_query_helper.cpp:38 / entity_type_mapping.cpp:605 | 读回的是 `connection().insertID()`（`mysql_insert_id`，**per-connection 状态**） |

结论：参照从不在应用侧造 DBID——**表自身的 `AUTO_INCREMENT` 主键列**分配，
`insertNew` 把该列留在语句外并把读回值作为返回值。

## 端口语义

- `EntitySchema::create_table_ddl` 的 DBID 列改为
  `"id" BIGINT NOT NULL GENERATED ALWAYS AS IDENTITY` + `PRIMARY KEY ("id")`
  ——PostgreSQL 的 `AUTO_INCREMENT` 拼写。`GENERATED ALWAYS` 是**严格**的：
  显式给值必须带 `OVERRIDING SYSTEM VALUE`，故既有显式写路径
  （`put_entity_sql`，即参照的 `insertExplicit`）加了该子句，两路径都不改语义。
- `PostgresStorage::put_new_entity(schema, record) -> Result<i64, DbError>`：
  语句 `put_new_entity_sql` = 列清单**不含** `"id"`（只 `quoted_property_columns`，
  占位符从 `$1` 起）+ **`RETURNING "id"`**；返回分配到的 DatabaseID。
- 校验拆分：`validate_put_values`（值数与 schema 列数、`Uint` 装得进 `BIGINT`）
  两路共享；`validate_put` 再查 DBID 已分配（显式路径的 `MF_ASSERT`）。待分配的
  `db_id = -1` 在**新实体路径合法**（正是参照把它变 0 的那一步），在显式路径
  仍报错。
- 参数构造抽成 `value_params` 化的 `put_entity_params(schema, record,
  include_db_id)`，占位符构造抽成 `nested_placeholder`（嵌套列 `CAST($n::text AS
  JSONB)` 的注释语义随之集中）。

## 验收对应：并发创建无重复且重启连续可用

`crates/db-postgres/tests/dbid_allocation.rs`（真 PostgreSQL，每例自建私有库、
结束即 `DROP DATABASE … WITH (FORCE)`；无可达服务时打印提示并跳过，与
entitydef 差分用例同门）：

- **`concurrent_creations_get_distinct_ids`**：先建表，4 个线程各开一条
  `PostgresStorage` 连接、各写 25 行（100 次分配）；断言 100 个 DatabaseID
  **两两不同**且全 > 0。identity 序列天然满足，真并发下无重复。
- **`allocation_continues_after_a_restart`**：第一条连接分配 `first` 后作用域
  结束（连接即「进程」），再用同库 conninfo **新建连接**分配 `second`，断言
  `second > first`——不重置、不复用。
- **`an_explicit_dbid_coexists_with_the_allocation`**：显式 DBID
  `1_000_000` 的 `put_entity`（`OVERRIDING SYSTEM VALUE`）照写并可 `get_entity`
  读回，随后一次 `put_new_entity` 仍严格递增——显式写不打乱 identity 分配。

验收映射：`insertNew` 的 `visitor.databaseID()` 读回 = 本移植的
`RETURNING "id"`；参照用 `0` 表失败、移植用 `Err(DbError)`。

## 证据

- `cargo test -p db-postgres`（带 `BOUNDLESS_PG_URL`，真服务）：**lib 41 /
  accounts 4 / dbid_allocation 3 / dry_run 3 / round_trip 6 / schema_init 2 /
  upgrade 3，全部 0 failed**
- `cargo test --workspace`：**4663 passed / 0 failed**（exit 0）
- `crates/db-postgres` 全 crate 行覆盖 **100.00%**（1233 DA / 0 missed）；其中
  `schema.rs` 166 DA / 0 missed、`store.rs` 321 DA / 0 missed
- 作用域内 rustfmt（仅三个叶子文件）/ clippy `-D warnings` 全清
  （仅存量 asset-pipeline/blowfish manifest 警告）

## 诚实登记

- **失败信号不同形**：参照以 `dbID_ == 0` 表「创建失败」（PutEntityTask 另行打
  `Failed to create new Entity`），移植以 `Err(DbError)` 表失败。PostgreSQL
  的 identity 值恒 > 0，`0` 不可能来自成功分配，故两者不可混同；错误文案不
  在 06-04 的验收面内。
- **读回机制**：参照的 `insertID()` 是插入后**另一次** per-connection 调用
  （`mysql_insert_id`），移植用同语句的 `RETURNING "id"` 取回——PostgreSQL 无
  「上次插入自增值」的可移植调用，`RETURNING` 是其等效物。
- **只交付分配腿**：参照 `PutEntityTask::performBackgroundTask` 还有 update 分支
  （`dbID_ != 0` → `update`）、`writeBaseMailbox_`/LogOn 腿与
  `MF_ASSERT(pGameTime_ == NULL)` 断言；读写删查属 BW-09-06-05，本项不动
  `put_entity` 的既有 upsert 语义（只加 `OVERRIDING SYSTEM VALUE`）。
- **`putExplicitID_` 标志未建模**：参照用该标志选 insertExplicit 路径；移植里
  「调用显式写方法」即选显式路径。
- **upgrade/schema_init 同步受益**：二者经 `create_table_ddl`（`upgrade.rs:50`）
  生成建表语句，故新库初始化与升级路径一并获得 identity 列。
- 测试依赖可达的 PostgreSQL 服务；无服务时按既有门禁打印提示并跳过（与
  本 crate 其余集成用例同策略）。
