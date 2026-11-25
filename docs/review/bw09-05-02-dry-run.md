# BW-09-05-02 dry-run — 复核证据

- **任务编号**: BW-09-05-02（todo.md 行 1390）
- **验收**: 报告计划且数据库前后相同
- **Boundless 模块**: `crates/db/src/tools/dry_run.rs`（实现）、
  `crates/db/src/tools/dry_run/tests.rs`（17 条单测）、`tools/mod.rs` 一行声明
- **平台**: Linux x86_64

## 原版形状

| 原版位置 | 形状 |
|---|---|
| mysql_dry_run_wrapper.hpp:10-28 | `MySqlDryRunWrapper : public MySql`，**只覆写四个重载**：`execute(statement, BinaryOStream*)`、`execute(statement, MYSQL_RES*&)`、`execute(statement, ResultSet*)`、`query(statement)`；其余继承 |
| mysql_dry_run_wrapper.cpp:10-42 `isReadOnlyStatement` | `find_first_not_of(" \t")`；`npos`→`ERROR_MSG("MySqlDryRunWrapper::isReadOnlyStatement: Invalid SQL statement: '%s'\n")` 并返回 false；从该位置起 `std::transform(..., ::toupper)`；`find("SHOW")==0`→true；`find("SELECT")==0`→**仅当**不含 `"FOR UPDATE"` **且**不含 `"LOCK IN SHARE MODE"`；否则 false |
| mysql_dry_run_wrapper.cpp:51-61 / 73-83 / 95-105 | 三个 `execute` 重载同一门：非只读→`INFO_MSG("DRYRUN:\t%s;\n")` 后 `return;`（不发 SQL），只读→`MySql::execute` |
| mysql_dry_run_wrapper.cpp:111-120 `query` | 非例外版本：非只读→`DRYRUN:\t%s;\n` + `return 0;`，只读→`MySql::query(statement)` |
| mysql_dry_run_connection.cpp:10-19 | `MySqlDryRunConnection : MySqlLockedConnection`，`createMysqlWrapper()` 返回 `new MySqlDryRunWrapper(connectionInfo_)`——即「dry-run 连接 = 真实连接 + 语句过滤」 |

## 端口语义

- `is_read_only_statement(statement, log)` 复刻上表：跳过集**只认空格与 Tab**
  （换行不跳过，故 `\nSELECT 1` 判为非只读）；空/纯空白语句返回 false 并把
  逐字 ERROR 行写入 `log`；大小写不敏感靠 `to_ascii_uppercase`。
- `dry_run_line(statement)` = `"DRYRUN:\t{statement};\n"`。
- `DryRunConnection<W: SyncDatabase>`：`execute` 是过滤门（非只读进 `report`，
  只读转发 `W`），**其余九个 trait 方法全部直通**——对应参照的
  「继承其余成员」。`into_inner()` 取回内层连接。
- `query_statement` 复刻 `query`：非只读报告并返回 `Ok(0)`，只读转发后同样 `Ok(0)`，
  内层 `Err` 上抛。
- `DryRunConnection::new` 即参照的 `createMysqlWrapper` 面。

## 验收对应：报告计划且数据库前后相同

`Synchronise::new(cfg, /*is_dry_run*/ true)` 穿过
`DryRunConnection<Recording>` 跑一次：`report` 收 14 条
（12 条特殊表 + `ALTER DATABASE CHARACTER SET ? COLLATE ?` +
`CREATE TABLE IF NOT EXISTS tbl_Account`），逐条以 `DRYRUN:\t` 开头；
**内层 `Recording` 记录到的语句数为 0**——数据库前后相同；日志侧是
`InnoDB is not the default MySQL storage engine…` 提示 +
`\tCreating table tbl_Account\n` 两条计划文字。

## 证据

- `cargo test -p db --lib`：**225 passed / 0 failed**
- `cargo test --workspace`：**4472 passed / 0 failed**（exit 0）
- `crates/db/src/tools/dry_run.rs` 行覆盖 **100.00%**（81 DA / 0 missed）
- 附带 `sync_db.rs` 升至 **99.00%**（1005 DA / 10 missed；直通
  `upgrade_database` 的默认实现被本面测到）
- 作用域内 rustfmt / clippy 全清（仅剩存量 asset-pipeline/blowfish
  manifest 警告，非本席）

## 诚实登记

- **seam 边界**：`table_names` / `describe_table` 在本 seam 里是 trait 方法而非
  语句，故**不被过滤**；参照里它们确实走 `execute`/`query`，真实 dry-run 会把
  它们也扣下。本项过滤面 = 参照覆写的四个 `execute`/`query` 重载。
- 参照三个 `execute` 重载共享同一门逻辑（仅 `pResData`/`MYSQL_RES*&`/
  `ResultSet*` 三形不同），本 seam 只有单形 `execute(&str)`，故一个实现即
  覆盖三重载。
- 参照的 `isReadOnlyStatement` 用 `find()` 做**子串**匹配：`FOR UPDATE` 出现在
  字符串字面量里也算（已测 `WHERE note = 'FOR UPDATE'` → 非只读）；下划线
  变体 `for_update` **不**匹配（已测 → 只读）。
- 未跑真 MySQL：`Recording` 记录型 double 代偿，与 BW-09-05-01 同一口径。
- 目录形态仍落 `crates/db/src/tools/` 命名空间，与另两席的扁平
  `crates/db/src/sync_db.rs` 并存，合并时择一。