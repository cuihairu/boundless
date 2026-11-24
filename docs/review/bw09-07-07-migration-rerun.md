# BW-09-07-07 验证迁移失败与重跑 — 复核证据

- **任务编号**: BW-09-07-07（todo.md 行 1442）
- **验收**: 下次执行不重复导入
- **Boundless 模块**: `crates/db-postgres/tests/import_mysql.rs`
  （追加 2 测，**纯验证项零库改动**——失败/重跑面为 BW-09-07-05
  `import_entities`/`import_accounts` 既有三腿驱动：缺席写 / 在位等值
  skipped / 冲突 Err 即中止）
- **平台**: Linux x86_64，PostgreSQL 18（127.0.0.1:5432，真服务）

## 原版形状

| 原版位置 | 形状 |
|---|---|
| put_entity_task.cpp:89-97 | `insertExplicit`——平 INSERT、显式 id；中途撞键即该次写失败，**已落行幸存**（迁移中途失败不回滚已写实体） |
| mysql_database.cpp 迁移流程（consolidate/transfer 簇） | 逐实体/逐账号顺序落地，一步失败即停；修复后**重跑整批**是恢复路径——已落行靠幂等跳过 |
| account.cpp `addAccount`（06-03 已复刻面） | `logOnName` 唯一键——重跑同名映射即撞唯一键，参照侧以此天然防重 |
| entity_type_mapping.cpp `createTable` | MySQL `id` 列为主键——重跑同 id 即撞主键，参照侧以此天然防重 |

## 端口语义（零改动，验证口径）

- **驱动三腿即幂等合同**（07-05 交付）：缺席 DBID → 显式写落地
  （顺带 `bump_identity` 校准座位）；DBID 在位且值相等 → `skipped`
  （内容重比对）；在位且值不同 → `Err(Storage)` 报两形状、**原行原地**。
  「下次执行不重复导入」由第二腿给出，不需要新面。
- **批量中途失败不回滚**：逐行顺序落地，一行 Err 即返回——之前已写的行
  留在库里（参照 insertExplicit 同形状）。恢复路径就是修复坏行后重跑
  整批：已落行走 skipped，余下行走 written，总量对账。
- **accounts 侧同合同**：映射冲突走 `DuplicateAccount` 中止，已落的新名
  留在库里；重跑整批时全部 skipped，映射永不搬移、永不重复。
- **确定性失败注入**：坏行取 arity 错形状（1 值 vs schema 3 列），在
  `put_entity` 的 `validate_put_values` 处稳定触发——不依赖网络/锁等
  环境抖动，中途失败点可精确放置（好行→坏行→尾行）。

## 验收对应

`tests/import_mysql.rs` 追加 2 例（真 PG18，连跑 3 轮全绿）：

| 用例 | 基准点 |
|---|---|
| `a_failing_batch_leaves_the_landed_rows_intact_and_the_rerun_never_duplicates` | 批 `[好(7), 坏(8, arity 1≠3), 尾(9)]` → `Err(Storage)` 中止；状态对账：**行 7 原值在位**，8/9 缺席；修复后重跑全批 `[7, 8fixed, 9]` → `{written: 2, skipped: 1}`；终态 3 行，行 7 **原值未被覆盖、未被重复**，8/9 各自落地 |
| `the_account_rerun_lands_the_rest_without_moving_the_mapping` | 预置映射 hero→7；批 `[新scout→8, 冲突hero→99]` → `Err(DuplicateAccount)` 中止；对账：scout 已落地，**hero 映射仍指 7**；重跑全批 `[scout, hero]` → `{written: 0, skipped: 2}`；hero 仍 7（未搬、未重）、scout 恰一份 |

## 证据

- `cargo test -p db-postgres`（`BOUNDLESS_PG_URL` 指真 PG18）：
  **lib 46 / accounts 4 / auto_load_restore 5 / connection_recovery 4 /
  dbid_allocation 3 / dry_run 3 / import_mysql 6 / round_trip 6 /
  schema_init 2 / storage_contract 17 / transaction 4 /
  transaction_conflict 4 / transaction_rollback 4 / upgrade 3 /
  verify_relogon 4，共 115 例全绿 0 failed**；import_mysql 连跑 3 轮 exit 0。
- `cargo test --workspace`：**4716 passed / 0 failed（exit 0）**。
- llvm-cov 行覆盖：**1700/1702 = 99.88%（不变）**——纯验证项零库改动
  （import_mysql.rs **37/37 = 100%**；store.rs 601-602 未达 =
  已登记 ROLLBACK 臂）。
- fmt：`cargo fmt -p db-postgres --check` 全包零 diff；
  `cargo clippy -p db-postgres --lib --tests` 无新告警。

## 诚实登记

- **零库改动**：失败/重跑语义是 07-05 导入驱动的既有三腿面，本项只落
  验证用例（「验证」类任务的本意，沿 06-10/07-06 先例）。
- **「中途失败不回滚」是参照形状而非缺陷**：参照 insertExplicit 撞键
  亦不回滚已落行，恢复路径即修复重跑——验收「下次执行不重复导入」
  正是这条恢复路径的合同；测试按此口径断言行 7 留存而非全批回滚。
- **坏行注入用 arity 错**：真实迁移的失败形态多样（撞键/连接断/约束
  违例），此处选 schema 校验臂因其确定性（无环境依赖）；「已落行幸存」
  的断言对任意中途 Err 臂同构成立——Err 都从同一循环返回，行已在库。
