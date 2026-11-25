# BW-09-07-05 导入 MySQL 实体及账号 — 复核证据

- **任务编号**: BW-09-07-05（todo.md 行 1440）
- **验收**: ID、关系和二进制字段对照
- **Boundless 模块**: `crates/db-postgres/src/import_mysql.rs`（新，2 面）、
  `crates/db-postgres/src/lib.rs`（模块登记）、
  `crates/db-postgres/tests/import_mysql.rs`（新，2 测）
- **平台**: Linux x86_64，PostgreSQL 18（127.0.0.1:5432，真服务）

## 原版形状

| 原版位置 | 形状 |
|---|---|
| entity_type_mapping.cpp `createTable` | MySQL 实体表布局：`tbl_<TypeName>`，id 列 `BIGINT UNSIGNED NOT NULL AUTO_INCREMENT` + 每持久属性一 `sm_*` 列（BLOB/VARBINARY 走二进制列） |
| put_entity_task.cpp:89-97 | **insertExplicit** 写腿——平 INSERT、显式 id，重 id 失败时原行幸存 |
| mysql_database.cpp `getEntity`（读腿） | 参考侧经实体定义把 MySQL 行读成属性值——导入源行以**已解码值**形态到达 |
| bigworldLogOnMapping / tbl_Account | 账号映射：`logOnName` 唯一键 → 账号的实体类型与 DBID，密码列 VARBINARY |
| account.cpp `addAccount`（06-03 已复刻面） | 映射写入的唯一键即重复契约来源 |

## 端口语义

- **`MySqlEntityRow`**：源行形态——显式 `db_id` + `sm_*` 列在 schema 访问序的
  `Vec<PropertyValue>`（`PartialEq` only：行可带浮点，非 `Eq`）。
- **`import_entities(storage, type_id, schema, rows)`**：逐行——DBID 缺席走
  `put_entity(explicit = true)`（`insertExplicit`：以自带 id 建，**顺带把
  identity 序列重定座位该 id**，07-06 的「新建不冲突」由既有
  `bump_identity` 腿给出）；DBID 在位且值相等 = `skipped`；在位且值不同 =
  `Err(Storage)` 报两形状（源值 vs 存量）——不静默搬。
- **`import_accounts(storage, rows)`**：名缺席 → `put_account`；在位且等 = `skipped`；
  在位且异 = `DuplicateAccount`（映射唯一键不搬）。
- 返回 `ImportReport { written, skipped }`——rerun 的「不重复导入」由
  `skipped` 报告给出（07-07 复用同面）。
- 无活 MySQL 服务（workspace 无 mysql 依赖）：源行是 MySQL 表布局的已解码
  形态，二进制字段经 `PropertyValue::Blob` → `BYTEA` 落地。

## 验收对应

新集成用例 `tests/import_mysql.rs`（2 例，真 PG18，每例私有库 +
`fixed_table_ddl`，结束 `DROP DATABASE WITH (FORCE)`）：

| 用例 | 基准点 |
|---|---|
| `the_import_lands_ids_relations_and_binary_fields_verbatim` | **验收正面**：三实体行（id 7/42/9000，Blob 含 `\x00`/`\xFF`/全字节对/空）+ 两账号（名字与密码含 NUL 与高位字节，映射指向 7/42）→ 导入报告 3+2/0+0 → **独立连接**逐行比对：id 原样、值逐字段相等（含 Unicode 名）、映射名与密码**逐字节相等**（关系与二进制字段对照） |
| `a_rerun_skips_the_landed_rows_and_a_conflict_fails_without_moving_them` | 重跑同批 → 全部 `skipped`（written 0，无重复）；冲突形状（id 7 值异、同名映射指向异实体）→ 分别为 `Err(Storage)` 报两形状 / `DuplicateAccount`，存量行/映射**原地未动** |

## 证据

- `cargo test -p db-postgres`（`BOUNDLESS_PG_URL` 指真 PG18）：
  **lib 46 / accounts 4 / auto_load_restore 5 / connection_recovery 4 /
  dbid_allocation 3 / dry_run 3 / import_mysql 2 / round_trip 6 /
  schema_init 2 / storage_contract 17 / transaction 4 /
  transaction_conflict 4 / transaction_rollback 4 / upgrade 3 /
  verify_relogon 4，共 111 例全绿 0 failed**；import_mysql 连跑 4 轮 exit 0。
- `cargo test --workspace`：**4712 passed / 0 failed（exit 0）**。
- llvm-cov 行覆盖：**1700/1702 = 99.88%**（import_mysql.rs **37/37 100%**；
  store.rs 751/753 未达两行 = 06-07/06-09 已登记的 ROLLBACK 臂）。
- fmt：改动叶子文件 fmt 后全包 `cargo fmt -p db-postgres --check` 零 diff；
  `cargo clippy -p db-postgres --lib --tests` 无新告警。

## 诚实登记

- **无活 MySQL 服务**：workspace 无 mysql 客户端依赖（`Cargo.toml` 零 mysql
  条目），源以 MySQL 表布局的已解码行表达（`tbl_<TypeName>` 的 id + `sm_*`
  列序、`bigworldLogOnMapping` 的 VARBINARY 名/密码），二进制字段经
  `PropertyValue::Blob` 走 `BYTEA`——对照是字节相等，不是与 MySQL 服务器的
  线上比对；线上读取面（`MySqlDatabase::getEntity` 的解码）归 MySQL 后端车道。
- **冲突实体形状报 `DbError::Storage` 字符串**（两形状内联）而非独立错误型：
  与参照「insertExplicit 撞键即失败、原行幸存」语义一致，冲突诊断走消息；
  accounts 侧的冲突走既有 `DuplicateAccount` 合同型。
- **DBID 校准面复用既有腿**：07-06 的序列重定座位 `put_entity(explicit)`
  的 `bump_identity` 既有面（setval GREATEST），无新面——07-06 以验证交付。
