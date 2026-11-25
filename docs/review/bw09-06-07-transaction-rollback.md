# BW-09-06-07 回滚 PostgreSQL 事务 — 复核证据

- **任务编号**: BW-09-06-07（todo.md 行 1417）
- **验收**: 写入中途返回错误时，实体和关联记录从独立连接读出旧值
- **Boundless 模块**: `crates/db-postgres/src/store.rs`、
  `crates/db-postgres/tests/transaction_rollback.rs`（新）
- **平台**: Linux x86_64，PostgreSQL 18（127.0.0.1:5432，真服务）

## 原版形状

| 原版位置 | 形状 |
|---|---|
| transaction.cpp:29-51 | **析构即回滚**：`if (!committed_ && !sql_.hasLostConnection())` 才执行 `WARNING_MSG( "MySqlTransaction::~MySqlTransaction: Rolling back\n" )` + `execute( "ROLLBACK" )`；`ROLLBACK` 抛异常时 `catch (DatabaseException & e) { if (e.isLostConnection()) sql_.hasLostConnection( true ); }`（**断链吞掉并置标志**，其余异常继续逃出析构）；块外 **`sql_.inTransaction( false )` 无条件执行**（回滚自身失败也清标志） |
| transaction.cpp:41-47 | 失败分类的关键行：连接级失败（`CR_*` 族）→ 置 `hasLostConnection`；SQL 级失败 → 逃出析构（`terminate()` 兜底） |
| database_exception.cpp:47-52 | `isLostConnection()` 只认四个 `CR_` 客户端连接码——**连接级** vs **SQL 级**的分界 |
| `crates/db/src/retry.rs:99-101` | 仓内既有分类：`CR_CONNECTION_ERROR/CR_CONN_HOST_ERROR/CR_SERVER_GONE_ERROR/CR_SERVER_LOST` → `LostConnection` |

结论：参照回滚面 = 三件事——**断链不回滚**（`!hasLostConnection()` 才送
ROLLBACK）、**回滚自身失败按连接级/SQL 级分类**、**`inTransaction(false)` 殿后
无条件执行**。本端口已交付 begin/commit（06-06），本项补齐 ROLLBACK 腿。

## 端口语义

- `PostgresStorage` 新增 `connection_lost: bool` 字段（参照连接上的
  `hasLostConnection` 标志，connect 初始 false）与查询面 `connection_lost()`。
- `rollback_transaction(&mut self)`：
  1. 无打开事务 → `Storage("there is no transaction to roll back")`（参照析构
     只回滚 `!committed_` 的在事务，不会无端发送 ROLLBACK）；
  2. `ROLLBACK` 成功 → 清 `in_transaction`；
  3. `ROLLBACK` 失败且**连接级**（`error.is_closed()` ——PG 对应物 `Kind::Closed`；
     或无 SQLSTATE 的错误——`ROLLBACK` 不带参数，故无 SQLSTATE 即连接级/I-O 级，
     即参照 `CR_*` 族）→ **吞掉失败、置 `connection_lost`**（transaction.cpp:41-47）；
  4. `ROLLBACK` 失败且**服务端 SQL 错误**（有 SQLSTATE）→ 清标志后照报
     （参照让该异常逃出析构，`terminate()` 兜底；端口显式返回 `Err`）；
  5. 无论走哪条，`in_transaction` 都清掉（`inTransaction( false )` 殿后）。
- 验收场景的语义链：PG 里一条语句失败（如重复主键 `insertExplicit`）即把事务置为
  aborted，其余语句不再执行；显式 `ROLLBACK` 撤掉整组，独立连接读到的实体、关联
  实体与 `bigworldLogOns` 登记记录都是旧值——与参照「写入中途返回错误 → 析构回滚
  → 无部分可见」逐点对应。

## 验收对应

新集成用例 `tests/transaction_rollback.rs`（4 例，真 PG18，每例私有库 +
`fixed_table_ddl`，结束 `DROP DATABASE WITH (FORCE)`）：

| 用例 | 契约点 |
|---|---|
| `a_failed_write_leaves_the_entity_and_its_records_at_their_old_values` | **验收正面**：事务内先更新 counter(id=1)→77，再对同 id 显式插入撞 `AlreadyExists` → `ROLLBACK` → 独立连接读回 counter 旧值 1、关联 thing 旧值 "a"、`bigworldLogOns` 登记记录原样（`logon_of` 相等） |
| `a_rollback_discards_everything_staged_before_it` | 显式回滚撤掉组内造行：提交前独立连接 `all_entities()` 空、回滚后仍空、标志已清 |
| `rolling_back_without_a_transaction_is_refused` | 无在事务的回滚拒（合同面） |
| `a_lost_connection_during_the_rollback_flags_the_connection` | **断链腿**：另一连接 `pg_terminate_backend` 真杀本连接后台 → `ROLLBACK` 不报错、`connection_lost()` 为真、`in_transaction()` 已清（transaction.cpp:41-47/50 逐条对应） |

## 证据

- `cargo test -p db-postgres`（`BOUNDLESS_PG_URL` 指真 PG18）：
  **lib 46 / accounts 4 / dbid_allocation 3 / dry_run 3 / round_trip 6 /
  schema_init 2 / storage_contract 17 / transaction 4 / transaction_rollback 4 /
  upgrade 3，共 92 例全绿 0 failed**。
- `cargo test --workspace`：**4693 passed / 0 failed（exit 0）**。
- llvm-cov 行覆盖：**1568/1570 = 99.87%**（store.rs 656/658 = 99.70%，未达的两行
  见诚实登记）。
- fmt 仅作用叶子文件（store.rs、tests/transaction_rollback.rs）；
  `cargo clippy -p db-postgres --lib --tests` 无告警。

## 诚实登记

- **两行未覆盖（store.rs 519-520）**：`ROLLBACK` 自身被服务端报 SQL 错误的分支。
  PostgreSQL 对裸 `ROLLBACK` 没有服务端错误面——aborted 事务里的 `ROLLBACK` 照常
  成功，连接断开走的是连接级分支——故该臂在本后端实际不可达；保留它是为对齐参照
  「SQL 级失败逃出析构」的合同（`postgres::Error` 无公开构造器，无法单测合成）。
- **WARNING 未复刻**：参照析构先 `WARNING_MSG("… Rolling back\n")`；db-postgres
  无日志面，回滚动作本身等价可观察（行组不可见），文本不逐字。
- **断链判定口径**：参照 `isLostConnection` 认四个 `CR_` 码；端口在 ROLLBACK 处
  判「客户端已关（`is_closed()`）或错误无 SQLSTATE」（`ROLLBACK` 无参数，故无
  SQLSTATE 即连接级/I-O 级）。`DbError::LostConnection` 的系统性分类（对全部语句）
  归 BW-09-06-09 断链恢复。
- **`shouldRetry`/死锁重试未交付**：transaction.cpp:57-60 的 `ER_LOCK_DEADLOCK`
  面归 BW-09-06-08 并发写冲突。
- **`db::EntityTransaction`（阶段化+镜像恢复）仍未在 PG 后端重做**：PG 的原子性由
  服务端事务给出，06-06 已登记的合并决定继续留给后续需要者。
