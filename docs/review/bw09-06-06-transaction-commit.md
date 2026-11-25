# BW-09-06-06 提交 PostgreSQL 事务 — 复核证据

- **任务编号**: BW-09-06-06（todo.md 行 1415）
- **验收**: 已提交的实体更新可从另一连接读回
- **Boundless 模块**: `crates/db-postgres/src/store.rs`、
  `crates/db-postgres/tests/transaction.rs`（新）
- **平台**: Linux x86_64，PostgreSQL 18（127.0.0.1:5432，真服务）

## 原版形状

| 原版位置 | 形状 |
|---|---|
| transaction.cpp:15-23 | 构造即 `sql_.execute( "START TRANSACTION" )`，**然后**才 `sql_.inTransaction( true )`——注释明言「Important that this is last as execute can cause an exception」（execute 抛异常时连接不算在事务里） |
| transaction.cpp:29-51 | 析构：`if (!committed_ && !sql_.hasLostConnection())` 才 `WARNING_MSG( "… Rolling back\n" )` + `execute( "ROLLBACK" )`；`ROLLBACK` 自身抛 `isLostConnection` 则置 `hasLostConnection(true)`；最后 `inTransaction( false )` 无条件执行 |
| transaction.cpp:66-71 | `commit()`：`MF_ASSERT( !committed_ )`、`execute( "COMMIT" )`、`committed_ = true`——**先送 COMMIT 再置标志** |
| transaction.cpp:57-60 | `shouldRetry()`：`getLastErrorNum() == ER_LOCK_DEADLOCK`（死锁才重试，归 BW-09-06-08） |
| `crates/db/src/transaction.rs` | 仓内已有的合同映射：`EntityTransaction` 分「阶段化写入 + 失败恢复镜像」两步语义（内存后端版），本项交付的是 **服务端真事务** 的提交腿 |

结论：参照的事务是**连接状态**（`inTransaction` 在 `MySql` 上），三条语句
`START TRANSACTION` / `COMMIT` / `ROLLBACK` 各自成对校验状态标志。

## 端口语义

- `PostgresStorage` 新增 `in_transaction: bool` 字段，`connect` 初始化为
  `false`——即参照 `MySql` 连接上的 `inTransaction` 标志。
- `begin_transaction(&mut self)`：
  1. `in_transaction` 已为真 → `DbError::Storage("the connection already has a
     transaction open")`（参照在 `MF_ASSERT` / 连接态约束下不会有第二次构造）；
  2. `START TRANSACTION` 失败 → 错误透传，**标志保持 false**（对齐
     transaction.cpp:19-22 的「execute 可能抛异常，故置标志殿后」）；
  3. 成功后置 `in_transaction = true`。
- `commit_transaction(&mut self)`：无打开事务 → `Storage("there is no transaction
  to commit")`；否则 `COMMIT`，成功后 `in_transaction = false`（对齐 commit()
  先送语句后置 `committed_`）。
- `in_transaction(&self) -> bool`：状态查询（复核用）。
- 事务期内本连接的契约操作（get/put/del/…）不改语句形状，天然落在同一事务里；
  提交后其它连接读回新值。

`ROLLBACK` 腿（析构语义、断链跳过、`shouldRetry`）属 **BW-09-06-07 回滚** 与
**BW-09-06-08 并发写冲突**，本项不抢跑。

## 验收对应

新集成用例 `tests/transaction.rs`（4 例，真 PG18，每例私有库 +
`fixed_table_ddl`，结束 `DROP DATABASE WITH (FORCE)`）：

| 用例 | 契约点 |
|---|---|
| `a_committed_entity_update_is_read_back_from_another_connection` | **验收正面**：连接 A 事务内更新实体 → `COMMIT` → 独立连接 B 读回新值；同时断言 `in_transaction()` 开合 |
| `another_connection_keeps_the_old_value_until_the_commit` | 提交前另一连接读旧值、提交后读新值（无部分可见） |
| `a_group_of_writes_becomes_visible_together_with_the_commit` | 事务内两条写（两个类型）提交前 `all_entities()` 为空、提交后成组可见 |
| `the_transaction_state_is_guarded_at_both_ends` | 重复 begin 拒、无事务 commit 拒（标志两端的合同面） |

## 证据

- `cargo test -p db-postgres`（`BOUNDLESS_PG_URL` 指真 PG18）：
  **lib 46 / accounts 4 / dbid_allocation 3 / dry_run 3 / round_trip 6 /
  schema_init 2 / storage_contract 17 / transaction 4 / upgrade 3，
  88 例全绿 0 failed**。
- `cargo test --workspace`：**4689 passed / 0 failed（exit 0）**。
- llvm-cov 行覆盖：**1551/1551 = 100.00%**（store.rs 639/639）。
- fmt 仅作用叶子文件（store.rs、tests/transaction.rs）；
  `cargo clippy -p db-postgres --lib --tests` 无告警。

## 诚实登记

- **状态标志是移植选择**：参照 `inTransaction` 由 `MySql` 维护、`MySqlTransaction`
  构造/析构成对翻转且无「重复 begin」检查（只有一个栈式用户）；端口把它显式化为
  两个方法的合同错误，避免把第二次 `START TRANSACTION` 交给 PG 的
  warning-and-no-op。
- **ROLLBACK 未交付**：本项只做提交腿；析构式回滚、断链跳过
  （`!hasLostConnection()`）、`ROLLBACK` 失败置 `hasLostConnection` 属 06-07，
  死锁重试（`shouldRetry`）属 06-08。当前未提交事务由 PG 在连接断开时自动回滚。
- **未包装契约操作**：`EntityTransaction`（`crates/db/src/transaction.rs`）的
  「阶段化写入 + 镜像恢复」内存后端语义未在 PG 后端重做——PG 侧的原子性由服务端
  事务本身给出，无需应用侧镜像；两者的合并留给 06-07 回滚项决定。
