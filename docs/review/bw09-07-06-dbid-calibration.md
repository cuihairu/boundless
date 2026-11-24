# BW-09-07-06 校准导入后的 DBID 分配 — 复核证据

- **任务编号**: BW-09-07-06（todo.md 行 1441）
- **验收**: 新建实体不冲突
- **Boundless 模块**: `crates/db-postgres/tests/import_mysql.rs`
  （追加 2 测，**纯验证项零库改动**——校准面为 BW-09-06-04/05 既有
  `bump_identity` 腿：`setval(GREATEST($2, nextval(seq)), true)`，07-05 的
  `import_entities` 显式写腿逐行经过它）
- **平台**: Linux x86_64，PostgreSQL 18（127.0.0.1:5432，真服务）

## 原版形状

| 原版位置 | 形状 |
|---|---|
| entity_type_mapping.cpp（实体表 DDL） | MySQL `id BIGINT UNSIGNED AUTO_INCREMENT`——表自增序列发 DBID；导入既有行时新分配的 id 必须**越过**全部已导入 id（AUTO_INCREMENT 迁移口径：`ALTER TABLE ... AUTO_INCREMENT = <max+1>` 的语义） |
| put_entity_task.cpp:89-97 | `insertExplicit`——显式 id 写腿（导入的落地腿） |
| put_entity_task.cpp:85-88 + buffered_entity_tasks.cpp:316-320 | 新建腿：`PENDING_DATABASE_ID` → 0 → 表自增发 id（表分配口径） |
| store.rs `bump_identity_sql`（BW-09-06-04 交付） | `SELECT setval(pg_get_serial_sequence($1,'id')::regclass, GREATEST($2, nextval(...)), true)`——显式写后把序列重定座到 max(导入 id, 序列现值)；**GREATEST 只升不降** |

## 端口语义（零改动，验证口径）

- **校准挂在导入写腿上**：`import_entities` 的每个缺席 DBID 走
  `put_entity(explicit = true)`，该腿先 `bump_identity`——导入 id 900 →
  序列重定座 900 → 下一个分配 901。导入集整体之上的分配天然不冲突。
- **重跑不动座位**：rerun 的行全部 `skipped`（07-05 面），不经过
  `bump_identity`，也不需要——座位在首次落行时已越过。
- **低位导入不回拉**：`GREATEST(2, nextval)` 保证座位只升——低位 id 落行后
  分配仍越过全部已导入/已分配 id（`nextval` 探测消耗一个号属既有
  `bump_identity` 形状，id 可跳号、不冲突即验收）。

## 验收对应

`tests/import_mysql.rs` 追加 2 例（真 PG18，连跑 3 轮全绿）：

| 用例 | 基准点 |
|---|---|
| `allocations_reseat_past_the_imported_ids` | **验收正面**：导入 id 5/900 → 表分配 `PENDING_DATABASE_ID` 新建 = **901、902**（越过整个导入集）；rerun 全 `skipped` → 再分配 = **903**（座位未动）；逐 id 读回 5 行（2 导入 + 3 分配）各自原值、零冲突 |
| `a_low_import_does_not_pull_the_allocation_seat_back_down` | 导入 id 3 → 分配 = **4**；低位 id 2 落行 → 分配 = **6**（GREATEST 只升；跳号 = `nextval` 探测既有形状）；id 2/3 各自在位原值——**新建实体不冲突** |

## 证据

- `cargo test -p db-postgres`（`BOUNDLESS_PG_URL` 指真 PG18）：
  **lib 46 / accounts 4 / auto_load_restore 5 / connection_recovery 4 /
  dbid_allocation 3 / dry_run 3 / import_mysql 4 / round_trip 6 /
  schema_init 2 / storage_contract 17 / transaction 4 /
  transaction_conflict 4 / transaction_rollback 4 / upgrade 3 /
  verify_relogon 4，共 113 例全绿 0 failed**；import_mysql 连跑 3 轮 exit 0。
- `cargo test --workspace`：**4714 passed / 0 failed（exit 0）**。
- llvm-cov 行覆盖：**1700/1702 = 99.88%（不变）**——纯验证项零库改动
  （store.rs 751/753，未达两行 = 已登记 ROLLBACK 臂）。
- fmt：`cargo fmt -p db-postgres --check` 全包零 diff；
  `cargo clippy -p db-postgres --lib --tests` 无新告警。

## 诚实登记

- **零库改动**：校准能力是 BW-09-06-04/05 的既有 `bump_identity` 面
  （07-05 的导入写腿逐行经过），本项只落验证用例（「校准」类任务的本意）。
- **`GREATEST` 内嵌 `nextval` 探测消耗一个号**：`setval(GREATEST($2,
  nextval(seq)), true)` 每次显式写都会消耗一个序列号，id 可跳号——参照
  AUTO_INCREMENT 亦不保证连续，验收口径为不冲突、不承诺连续。
- **MySQL `AUTO_INCREMENT = <max+1>` 迁移语句未复刻为独立语句**：校准由
  逐行写腿等价给出（每行 setval GREATEST），终态一致（座位 ≥ max 导入 id）。
