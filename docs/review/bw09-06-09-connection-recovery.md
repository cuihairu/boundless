# BW-09-06-09 实现断链恢复 — 复核证据

- **任务编号**: BW-09-06-09（todo.md 行 1421）
- **验收**: 数据库恢复后新请求成功
- **Boundless 模块**: `crates/db-postgres/src/store.rs`（`map_db_error` 连接级分类 +
  `reconnect` + `run_with_reconnect` 语句缝）、
  `crates/db-postgres/tests/connection_recovery.rs`（新）、
  `crates/db-postgres/tests/transaction_conflict.rs`（并发测时序修复）、
  `crates/server-runtime/tests/db_backend_isolation.rs`（分类期望更新）
- **平台**: Linux x86_64，PostgreSQL 18（127.0.0.1:5432，真服务）

## 原版形状

| 原版位置 | 形状 |
|---|---|
| wrapper.cpp:306-332 | **`MySql::realQuery` 重连腿**：语句失败且 `!inTransaction_ && (errNum == CR_SERVER_LOST \|\| errNum == CR_SERVER_GONE_ERROR)` → INFO "Connection lost. Attempting to reconnect." → `reconnect()` 成功则**同一条语句再跑一次**（重试的结果替换原结果）；重连失败 WARNING "Reconnect failed…" 且**原错误照回**；事务内永不重连——被弃工作无法在新连接上重发 |
| wrapper.cpp:294-299 | `MySql::throwError`：`isLostConnection()` 即置 `hasLostConnection(true)`——**每一次连接级失败都打标**，与后续动作无关 |
| wrapper.cpp:74-91 + wrapper.hpp:64-67 | `MySql::reconnectTo`：`close()` → `connect(connectInfo_)`（catch 任何异常返回 false）→ `return ping()`——用**存下的连接串**重建，报告新连接是否应答 |
| wrapper.cpp:94-97 | `MySql::connect` 开头 `hasLostConnection_ = false`——**新连接清标志** |
| database_exception.cpp:43-53 | `isLostConnection()` 四 `CR_*` 码：**连接期失败（`CR_CONNECTION_ERROR`/`CR_CONN_HOST_ERROR`）也在族内** |
| `crates/db/src/recovery.rs:35-44` | 仓内既有原语 `query_with_reconnect`（`Err(LostConnection) if reconnect() => op()`，BW-08 期交付）——本项把 PG 驱动接到这一语义上 |

## 端口语义

- **系统性分类**：`map_db_error` 的 fallback 之前加连接级臂——
  `error.is_closed() || error.as_db_error().is_none()`（closed = PG `Kind::Closed`；
  无 SQLSTATE = I/O 级，含**连接期失败**——对齐参照 `CR_CONN_HOST_ERROR` 在族内）
  → **`DbError::LostConnection`**。此前连接级失败落 `Storage`（06-07 的 ROLLBACK
  判定已用同一谓词，口径一致）。
- **连接串存档**：`PostgresStorage` 新增 `conninfo: String`（= 参照
  `connectInfo_`，wrapper.hpp:111），`reconnect` 用它重跑。
- **`reconnect(&self) -> bool`**（`MySql::reconnectTo`）：`Client::connect` 全握手
  （即参照 `ping()` 的应答检查）→ 成功换入新 client、`connection_lost` 清零
  （wrapper.cpp:94-97）、`in_transaction` 清零（旧事务随旧后端死了，新连接从净态
  起步——移植选择，参照由 `MySqlTransaction` 管理该标志）；失败返回 false。
  `&self` 签名靠既有 `RefCell<Client>` + 两状态标志改 `Cell<bool>`——参照
  `MySql` 上这些状态本就同挂一个共享对象（wrapper.hpp:89-111），`&self` 读路径
  （`get_entity`/`logon_of`/`all_entities`）因此同样能重连。
- **语句缝 `run_with_reconnect`**（`MySql::realQuery` 的 PG 对应物）：全部 16 处
  语句位（START TRANSACTION、COMMIT、bump_identity、put_logon_record、
  create_entity_table、固有 put/get/put_new、trait put 显式与存在性查询、
  del×2、logon_of、check_out 存在性、check_in、all_entities）统一走它：
  ① 执行语句；② 失败且连接级即打 `connection_lost` 标（`throwError`
  wrapper.cpp:294-299，**无论后续走哪条**）；③ 连接级且 `!in_transaction` →
  `reconnect()` 成功则**同一语句重跑一次**（重试结果替换原结果），失败则
  **原错误照回**（wrapper.cpp:327-330）；④ 事务内连接级错误照报
  （`MySqlTransaction` 析构跳过 ROLLBACK 的同一理由）；⑤ `COMMIT` 因标志在
  事务内天然不重连，`START TRANSACTION` 因标志尚未置位可重连——与参照逐点对齐。
  `rollback_transaction` 保持 06-07 自有面（吞掉+打标，不重连）。
- **验收语义链**：断链（真 `pg_terminate_backend` 或库被删）→ 语句报
  `LostConnection`（事务内）或静默重连重跑（事务外）→ 数据库恢复（新连接可建）
  → 新请求成功，行上无重复。

## 验收对应

新集成用例 `tests/connection_recovery.rs`（4 例，真 PG18，每例私有库 +
`fixed_table_ddl`，结束 `DROP DATABASE WITH (FORCE)`）：

| 用例 | 契约点 |
|---|---|
| `a_new_request_succeeds_after_the_database_recovers` | **验收正面**：事务外 `pg_terminate_backend` 杀本连接 → 下一条 put **静默重连重跑成功** → 独立连接读回 77、恰 1 行无重复、`connection_lost()` 已被新连接清零 |
| `a_lost_connection_inside_a_transaction_is_terminal_until_recovered` | **事务内终局腿**：事务内断链 → put 报 **`DbError::LostConnection`**（不重连）→ `connection_lost()` 真 → ROLLBACK 照常安静收场（06-07 面）→ `reconnect()` 真 → 新请求落库 300——**数据库恢复后新请求成功** |
| `a_read_that_hits_the_dropped_link_is_re_run_on_the_new_connection` | **读路径腿**：断链后 `get_entity` 与 `logon_of`（`&self` 面）同样重连重跑，读回原值与登记记录 |
| `a_reconnect_that_cannot_re_establish_reports_lost_until_the_database_returns` | **重连失败腿（wrapper.cpp:327-330）**：`DROP DATABASE WITH (FORCE)` 真删库（后端随之终止）→ put 报 `LostConnection`（重连失败、原错误照回、标志保持）→ **重建库** → `reconnect()` 真 → 显式写落库 55、独立连接读回、恰 1 行——完整验收弧「库没了→恢复→新请求成功」 |

## 证据

- `cargo test -p db-postgres`（`BOUNDLESS_PG_URL` 指真 PG18）：
  **lib 46 / accounts 4 / connection_recovery 4 / dbid_allocation 3 / dry_run 3 /
  round_trip 6 / schema_init 2 / storage_contract 17 / transaction 4 /
  transaction_conflict 4 / transaction_rollback 4 / upgrade 3，共 100 例全绿
  0 failed**。
- `cargo test --workspace`：**4701 passed / 0 failed（exit 0）**。
- llvm-cov 行覆盖：**1586/1588 = 99.87%**（store.rs 674/676 = 99.70%，未达的两行
  541-542 见诚实登记，即 06-07 已登记的 ROLLBACK 服务端错误臂——重连失败臂经真删
  库用例全覆盖）。
- 稳定性：connection_recovery 连跑 5 轮 exit 0；transaction_conflict 修复后连跑
  5 轮 exit 0。
- fmt 仅作用叶子文件（store.rs、两个 db-postgres 测试文件、db_backend_isolation.rs）；
  `cargo clippy -p db-postgres --lib --tests` 无新告警（仅既有 workspace manifest
  未用依赖警告）。

## 诚实登记

- **重连腿的触发族**：参照 `realQuery` 只认查询期两码（`CR_SERVER_LOST`/
  `CR_SERVER_GONE_ERROR`），连接期两码（`CR_CONNECTION_ERROR`/`CR_CONN_HOST_ERROR`）
  不在重连面——MySQL 按错误发生时机分族。PostgreSQL 无此区分：`execute` 上出现的
  连接级错误只能是查询期死亡（连接期失败发生在 `Client::connect`，无重连缝），
  故端口的重连腿对连接级全族（`is_closed()` 或无 SQLSTATE）触发，`map_db_error`
  的分类对齐 `isLostConnection` 全族。
- **`db_backend_isolation` 断言更新**：server-runtime 的
  `the_postgres_driver_stands_alone`（BW-16-02-03 交付面）原断言连接失败 =
  `DbError::Storage(_)`；系统性分类后连接期失败落 `LostConnection`（正是参照
  `CR_CONN_HOST_ERROR` 族），断言随之更新为 `assert_eq!(err,
  DbError::LostConnection)`——测试意图（自有失败、不碰其他后端）不变。
- **06-08 并发测时序修复（随本项交付）**：`transaction_conflict.rs` 的锁等待例
  在 workspace 全量负载下闪失败两次（一次在 06-08 交付后）——根因是 side 的
  `SET lock_timeout='200ms'` 为**会话级**，第一次尝试超时后重试继承同一预算，
  在 holder 提交（T+500ms）前 ~95ms 再吃 55P03。修复：改 `SET LOCAL`（事务内
  生效、rollback 即还原），重试腿不再带过期预算，等待时长与参照一致（无测试
  人为上限）。修复后连跑 5 轮稳定。业务代码零改动。
- **WARNING/INFO 日志未复刻**：参照重连路径有 INFO/WARNING 文本（"Connection
  lost. Attempting to reconnect." 等）；db-postgres 无日志面，动作等价可观察
  （错误分类 + 重连成功后的落库），文本不逐字。
- **store.rs 541-542 两行未覆盖**：`ROLLBACK` 自身被服务端报 SQL 错误的分支
  （06-07 已登记的不可达臂；全 crate 99.87%、store.rs 99.70% 过 ≥98% 门）。
- **`MySql::close()` 显式关连接与 `MYSQL_OPT_CONNECT_TIMEOUT`/`s_connect_timeout_`
  未复刻**：端口重连直接以新 `Client::connect` 替换（旧 client 随
  `RefCell::replace` 丢弃即关闭），libpq 连接超时走其默认值。
- **`crates/db/src/recovery.rs` 的通用原语未在 PG 侧调用**：本项在驱动内实现
  同语义的语句缝（需访问 `in_transaction`/client 替换，泛型闭包形态接不进
  `&self` 借用结构），与 `lock_recovery.rs`/`recovery.rs` 的参照行号口径一致；
  两面不合并，留给后续需要跨后端统一循环的使用者。
