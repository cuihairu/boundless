# BW-09-06-08 处理 PostgreSQL 并发写冲突 — 复核证据

- **任务编号**: BW-09-06-08（todo.md 行 1418）
- **验收**: 两个写入同一实体的请求按存储契约返回结果，重试后没有重复记录
- **Boundless 模块**: `crates/db-postgres/src/store.rs`（`map_db_error` 锁条件分类）、
  `crates/db-postgres/tests/transaction_conflict.rs`（新）
- **平台**: Linux x86_64，PostgreSQL 18（127.0.0.1:5432，真服务）

## 原版形状

| 原版位置 | 形状 |
|---|---|
| database_exception.cpp:35-40 | `DatabaseException::shouldRetry()`：`errNum_ == ER_LOCK_DEADLOCK \|\| errNum_ == ER_LOCK_WAIT_TIMEOUT` —— **可重试面恰是两个锁条件**；其余 SQL 级失败不可重试 |
| transaction.cpp:57-60 | `MySqlTransaction::shouldRetry()`：`sql_.getLastErrorNum() == ER_LOCK_DEADLOCK` —— 事务层面只在死锁时重试 |
| database_exception.cpp:43-53 | 对照面：`isLostConnection()` 只认四个 `CR_*` 连接码（重试 ≠ 断链，两类互斥） |
| `crates/db/src/retry.rs:94-101` | 仓内既有分类：`ER_LOCK_DEADLOCK\|ER_LOCK_WAIT_TIMEOUT` → `Retryable`；`CR_*` → `LostConnection`；其余 → `Fatal` |

结论：参照契约 = **锁条件（死锁 + 锁等待超时）→ 重试**（重试前整组撤回、重试成功后
无重复落库），非锁条件照报。本项把两个锁条件接到 PG 的 SQLSTATE 上。

## 端口语义

- PG 的对应物：`40P01 deadlock_detected`（`SqlState::T_R_DEADLOCK_DETECTED`，
  死锁牺牲品）与 `55P03 lock_not_available`（`SqlState::LOCK_NOT_AVAILABLE`，
  `lock_timeout` 到期）。`map_db_error` 里这两个 SQLSTATE → **`DbError::Retry`**，
  `23505 unique_violation` → `DuplicateAccount`，其余照旧 `Storage`（既有形状不变，
  布尔变量形而非 `if let Some(code)` 形——后者在 llvm-cov 下把 fallback 区映射到
  收尾括号行，见证据）。
- 重试协议本身（整组撤回再跑一遍）由调用方走既有 `rollback_transaction` +
  `begin_transaction`（06-06/06-07 交付的面）完成——与参照 `shouldRetry()` 只
  回答「要不要重试」、循环在调用侧（如 `MySqlTransaction` 使用方）一致。
- 同实体并发写在 PG 里天然被行锁串行化（后写等前写）；两写交错成环时死锁检测器
  选一牺牲品发 40P01；`SET lock_timeout` 到期发 55P03；事务内语句失败后事务
  aborted，但显式 `ROLLBACK` 仍成功（06-07 已交付），重试组干净。

## 验收对应

新集成用例 `tests/transaction_conflict.rs`（4 例，真 PG18，每例私有库 +
`fixed_table_ddl`，结束 `DROP DATABASE WITH (FORCE)`）：

| 用例 | 契约点 |
|---|---|
| `two_writers_of_one_entity_land_on_one_row` | **验收正面**：两连接同写 counter(id=1)（200/300）→ 都成功，行锁串行，终值 ∈ {200,300}，`all_entities()` 恰 1 行——**无重复记录** |
| `a_lock_wait_timeout_reports_retry_and_the_retry_lands_once` | **锁等待超时腿（55P03）**：holder 持行（mpsc 握手保证次序），side `SET lock_timeout='200ms'` 后写 → `expect_err` **`DbError::Retry`** → 回滚 → 重试 → 等 holder 提交后落库；终值 300、1 行 |
| `a_deadlock_reports_retry_and_the_retries_leave_no_duplicates` | **死锁腿（40P01）**：两侧以相反次序写两行（双向 mpsc 握手保证各持首行后再要对方行）→ 成环 → 死锁检测器选牺牲品报 **`Retry`** → 整组回滚重跑（预算 4）→ 两侧都落库，恰 2 行无重复 |
| `a_concurrent_delete_of_one_entity_reports_not_found_for_the_loser` | **删除竞争腿**：winner 持行删（握手保证次序）→ loser 的 delete 匹配 0 行 → `DbError::NotFound`（存储契约，非 Retry）→ 回滚；终态行恰删一次 |

时序确定性：并发用例一律 **mpsc 握手通道**（`recv_timeout(2s)`）锁序，不再依赖
错峰 sleep——workspace 全量负载下 sleep 型曾翻车（side 抢在 holder 持行前写完，
`expect_err(Retry)` 收到 `Ok`）；握手后连跑 3 轮稳定（~1.6-1.7s）。

## 证据

- `cargo test -p db-postgres`（`BOUNDLESS_PG_URL` 指真 PG18）：
  **lib 46 / accounts 4 / dbid_allocation 3 / dry_run 3 / round_trip 6 /
  schema_init 2 / storage_contract 17 / transaction 4 / transaction_conflict 4 /
  transaction_rollback 4 / upgrade 3，共 96 例全绿 0 failed**。
- `cargo test --workspace`：**4697 passed / 0 failed（exit 0）**（flake 修复后全绿）。
- llvm-cov 行覆盖：**1575/1577 = 99.87%**（store.rs 663/665 = 99.70%，未达的两行
  528-529 见诚实登记，与 06-07 相同的 ROLLBACK 防御臂）。
- fmt 仅作用叶子文件（store.rs、tests/transaction_conflict.rs）；
  `cargo clippy -p db-postgres --lib --tests` 无告警（仅既有 workspace manifest
  未用依赖警告）。

## 诚实登记

- **`SqlState` 常量名**：PG 的死锁 SQLSTATE 常量是 `T_R_DEADLOCK_DETECTED`（40P01）
  而非字面 `DEADLOCK_DETECTED`（后者不存在，E0599 实测）。
- **事务层 vs 异常层的重试面差异照原样保留**：参照 `MySqlTransaction::shouldRetry`
  只认死锁、`DatabaseException::shouldRetry` 认死锁+锁等待超时；端口在
  `map_db_error`（异常层对应物）按后者分类（40P01+55P03），调用方可自行收窄——
  55P03 由 `lock_timeout` 测试腿真触发，40P01 由死锁测试腿真触发，两条都过。
- **WARNING/重试日志未复刻**：参照重试路径有 `WARNING_MSG`；db-postgres 无日志面，
  重试动作本身等价可观察（Retry 错误 + 干净重试落库），文本不逐字。
- **store.rs 528-529 两行未覆盖**：`ROLLBACK` 自身被服务端报 SQL 错误的分支
  （06-07 已登记的不可达臂，PG 对裸 ROLLBACK 无服务端错误面；全 crate 99.87%、
  store.rs 99.70% 过 ≥98% 门）。
- **`DbError::LostConnection` 系统性分类仍未交付**：对全部语句的断链分类归
  BW-09-06-09 断链恢复；本项只交付锁条件 → `Retry`。
- **`db::EntityTransaction`（阶段化+镜像恢复）仍未在 PG 后端重做**：PG 的原子性
  由服务端事务给出，06-06 已登记的合并决定继续留给后续需要者。
