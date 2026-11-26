# BW-09-06-10 验证账号重登 — 复核证据

- **任务编号**: BW-09-06-10（todo.md 行 1423）
- **验收**: 会话状态与 MySQL 基准相符
- **Boundless 模块**: `crates/db-postgres/tests/verify_relogon.rs`（新，纯验证项——
  全部会话面已在 BW-09-06-05 交付：`put_logon_record`/`logon_of`/`check_out`/
  `check_in`/`put_account`/`get_account`，本项零库改动）
- **平台**: Linux x86_64，PostgreSQL 18（127.0.0.1:5432，真服务）

## 原版形状

| 原版位置 | 形状 |
|---|---|
| entity_type_mapping.cpp:773-783 | `addLogOnRecord` = `INSERT ... ON DUPLICATE KEY UPDATE`——**加或改**：重登改写持有人记录而非撞键失败 |
| put_entity_task.cpp:99-116 | 写腿分离：`writeBaseMailbox_` 腿**先查存在**再 `addLogOnRecord`（注释 "Add or update the log on record"）；`removeBaseMailbox_` 腿无条件 `removeLogOnRecord`；`writeEntityData_` 腿（:69-97）**不碰会话** |
| get_entity_task.cpp:51-54 | `getLogOnRecord` → `hasBaseLocation_`——读实体报告是否有 base 持有及位置；无记录=absence 也是状态 |
| entity_type_mapping.cpp:533-535 | `removeLogOnRecord` 无条件 DELETE——重复签出不是错误 |
| tasks/set_entity_key_for_account_task.cpp | 账号映射 `bigworldLogOnMapping` 以登录名唯一键——重登不动映射；同名第二行是合同错误（DuplicateAccount）非静默搬移 |

## 端口语义（零改动，验证口径）

- **重登=会话搬移**：`put_logon_record`（=addLogOnRecord 的 PG 对应物，
  `INSERT ... ON CONFLICT DO UPDATE`，06-05 交付）把会话改写到新 base 邮箱；
  `check_out`（合同面）对任何在册记录一律答 `Elsewhere(当前持有人)`——
  **check-out 本身从不搬移会话**，搬移是 upsert 腿的职责——与参照「baseapp 级
  重登走新的 addLogOnRecord」逐点相符。
- **会话读**：`logon_of`（=getLogOnRecord）报 Some(持有人)/None——
  hasBaseLocation_ 的两面。
- **签出**：`check_in`（=removeLogOnRecord）无条件删，重复执行不报错。
- **写腿分离**：`put_entity`（=writeEntityData_ 腿）只写实体行，会话不动、
  不重复。
- **账号映射稳定**：`get_account` 重登前后原样；同名 `put_account` 第二次
  → `DuplicateAccount`，映射不搬。

## 验收对应

新集成用例 `tests/verify_relogon.rs`（4 例，真 PG18，每例私有库 +
`fixed_table_ddl`，结束 `DROP DATABASE WITH (FORCE)`）：

| 用例 | 基准点 |
|---|---|
| `a_relogon_moves_the_session_to_the_new_base_mailbox` | **验收正面**：check_out(A)→Granted+`logon_of`=A；重登 `put_logon_record(B)`→`logon_of`=B（ON DUPLICATE KEY UPDATE 搬移）；第三方 check_out→**`Elsewhere(B)`** 非 A；被拒后 `logon_of` 仍=B（check-out 不搬会话） |
| `a_relogon_leaves_the_mapping_and_the_rows_at_the_baseline` | 重登只动会话：`get_account("hero")` 前后同一行；同名第二账号→`DuplicateAccount` 且映射不动；`all_entities()` 恰 1 行、恰 1 条会话 |
| `the_session_read_reports_the_base_or_its_absence` | `logon_of`：None→check_out Granted→Some(持有人)→check_in→None→**重复 check_in 仍 Ok**（无条件删） |
| `a_row_write_leaves_the_session_alone_and_a_fresh_logon_grants` | 写腿（put_entity 更新）后会话原样（Some(A) 不动不重）；check_in 后新 check_out→**Granted**（签出后的重登授予新会话） |

## 证据

- `cargo test -p db-postgres`（`BOUNDLESS_PG_URL` 指真 PG18）：
  **lib 46 / accounts 4 / connection_recovery 4 / dbid_allocation 3 / dry_run 3 /
  round_trip 6 / schema_init 2 / storage_contract 17 / transaction 4 /
  transaction_conflict 4 / transaction_rollback 4 / upgrade 3 / verify_relogon 4，
  共 104 例全绿 0 failed**；verify_relogon 连跑 3 轮 exit 0。
- `cargo test --workspace`：**4705 passed / 0 failed（exit 0）**。
- llvm-cov 行覆盖：**1586/1588 = 99.87%**（store.rs 674/676；未达两行 =
  06-07/06-09 已登记的 ROLLBACK 服务端错误臂——纯验证项零库改动，覆盖不变）。
- fmt 仅作用叶子文件（tests/verify_relogon.rs）；
  `cargo clippy -p db-postgres --lib --tests` 无新告警。

## 诚实登记

- **本项零库改动**：全部会话面为 BW-09-06-05 交付形状，本项只落基准验证用例
  （「验证」类任务的本意）。
- **同持有人再 check_out 也答 Elsewhere**：合同面 check_out 只要记录在册一律
  Elsewhere（无同持有人特判）——参照 addLogOnRecord 无此分支，搬移一律走
  upsert 腿；端口同形。
- **测试勘误**：reader 连接需逐连接 `register_counter`（每连接注册表，
  06-05 已登记的形状）后 `all_entities()` 才见行；`BaseMailbox` 非 Copy
  （Clone），按值传入 check_out 后再断言处用 `.clone()`。
- **未验证面**：`bigworldLogOns` 的 `shouldAutoLoad` 列、启动清场
  （mysql_database.cpp:674/703 的 `DELETE ... WHERE NOT shouldAutoLoad` 与
  `UPDATE ... SET ip=0, port=0`）及 SQLite 归档对接归 BW-09-06-11。
