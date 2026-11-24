# BW-09-05-01 sync_db — 复核证据

- **任务编号**: BW-09-05-01（todo.md 行 1389）
- **验收**: 原版 schema 上的 DDL 差异可复核
- **Boundless 模块**: `crates/db/src/tools/sync_db.rs`（实现）、
  `crates/db/src/tools/sync_db/tests.rs`（208 条 db crate 单测中的
  同步器面）、`crates/db/src/tools/mod.rs`、`lib.rs` 一行声明
- **平台**: Linux x86_64

## 原版形状

| 原版位置 | 形状 |
|---|---|
| column_type.cpp:17-271 `getAsString` | 按 family 渲染：数值族带 `UNSIGNED`（**只有数值族**，FLOAT/DOUBLE 不带）；`YEAR(n)`/`VARCHAR(n)`/`BIT(n)` 带长度；字符串族 `NOT NULL` 尾缀 + **主键的 NOT NULL 在中段**；blob 族永不带默认值；`ON UPDATE` 尾缀 |
| column_type.cpp:272-330 `operator==` | 数值忽略长度；VARCHAR 只比长度；STRING 比长度+binary；`""`↔`"0"` 数值别名；STRING 的 `""`↔按列长填 `' '`（binary 填 `'\0'`）的 null-string；TIMESTAMP 默认值**恒等**；`onUpdateCmd` 不参与比较；blob 默认值不参与 |
| table_inspector.cpp:20-72 `visitTable` | 已访问→`MF_ASSERT`（重名）；表不存在→`onNeedNewTable`；否则 diff 四桶非空才 `onNeedUpdateTable`；`isSynced &= handler 返回值` |
| table_inspector.cpp:79-104 `deleteUnvisitedTables` | `getEntityTables`(`tbl_%`) 与 `visitedTables_` 取差集；`onNeedDeleteTables` 返回 false→`isSynced_ = false` |
| table_inspector.cpp:122-166 `classifyColumns` | 旧有新无→obsolete；新有旧无→new；两者都有→updated/indexed（索引名差异另记）；差集迭代中**双端同时推进** |
| table_inspector.cpp:169-239 `TableValidator` | 只报告不执行：`"\tRequire table %s\n"`、`"\tNeed to add/delete/update column %s into/from/in table %s\n"`、`"\tNeed to %s index to column %s in table %s\n"`（add/delete 双用词）、`"Need to remove table %s\n"`（**无制表符**）；恒返 false |
| table_inspector.cpp:362-374 `isEntityTablesInSync` | `types.visit(defs, validator)`；**无 try/catch**，异常上抛调用者 |
| mysql_table_initialiser.cpp:177-243 `generateCreateIndexStatement` | 索引名 `%sIndex`；非 VAR_STRING 列长度截到 255 并逐字告警；`VARCHAR(n)`/blob 族/`VAR STRING`/`STRING` 带长度后缀 |
| mysql_table_initialiser.cpp:244-352 `createIndex`/`removeIndex` | None/Primary 是**空操作**；Unique/NonUnique 失败→逐字 ERROR 后**重新抛出**；`parentIDIndex` 走 `CREATE INDEX`；External→`CRITICAL`（本实现 panic 逐字 `createEntityTableIndex: Unknown index type 5\n`）；`removeIndex` **吞掉异常**（let-chains 式继续） |
| mysql_table_initialiser.cpp:357-407 `onNeedNewTable` | `"\tWitholding table creation: %s\n"`（**typo 逐字保留**）；`CREATE TABLE ... ENGINE=InnoDB`；id 列在**建表串里先出现**、desired 里的 id 随后被 erase、live defs 无 id 才 drop 占位列；仅当合并 ALTER 有内容（>0）才执行 |
| mysql_table_initialiser.cpp:409-434 `onExistingTable` | `CONVERT TO CHARACTER SET '<escaped>'` [COLATE '<escaped>'] |
| mysql_table_initialiser.cpp:436-495 `onNeedUpdateTable` | 顺序：add → drop（**先 removeIndex 再 ALTER DROP COLUMN**）→ modify → 单条合并 `ALTER TABLE ... ,` → 失败逐字 ERROR；随后才 `createIndex`/backfill 默认值 |
| mysql_synchronise.cpp:88-216 `doSynchronise` | 门序：引擎支持态→`getTableNames("bigworld%")`→`checkTableEngines`→未合并二级库拒→版本/口令哈希读取（**仅 dry-run 且无 bigworldInfo 时跳过**）→`version > DBAPP_CURRENT_VERSION(9)` 拒→字符集/排序规则 NOTICE→`ALTER DATABASE CHARACTER SET ?`/`? COLLATE ?`（**占位符原样**）→升级钩子（版本≠9）→口令哈希迁移→dry-run 且无 bigworldEntityTypes 早退→实体表同步+未访问表删除 |
| mysql_synchronise.cpp:273-349 特殊表 | 12 条语句：`bigworldInfo`/`EntityTypes`/`LogOns`/`NewID`/`UsedIDs`/`GameTime`/`Spaces`/`SpaceData`(+MODIFY `data %s`)/`SecondaryDatabases`/`EntityDefsChecksum` 等，逐字含 `IF NOT EXISTS` 与 `maxSpaceDataSize` 重映射（默认 2048→BLOB） |
| mysql_synchronise.cpp:352-385 `updatePasswordHash` | 原哈希→去哈希**拒**；去→加则两条 UPDATE + commit |
| mysql_synchronise.cpp:387-415 `synchroniseEntityDefinitions` | try/catch `std::exception`→逐字 `"MySqlSynchronise::synchroniseEntityDefinitions: %s\n"` 并置 false（`visit` 内的 `deleteUnvisitedTables` 也在同一 try 内，property_mappings_per_type.cpp:213） |
| mysql_synchronise.cpp:216 | `syncToDatabase` 恒以 `allowNew = true` 调用 |
| main.cpp | `ConnectionInfo` + `CommandLine`：选项**存在才覆盖** config（secure-auth 仅在真给了选项时生效）；退出码语义与三条消息（Initialisation failed / sync failed / sync succeeded） |

## 端口语义

- 逐字节保留参照词表：`MYSQL_ENGINE_TYPE = "InnoDB"`、`TABLE_NAME_PREFIX = "tbl"`、
  `ID_COLUMN_NAME = "id"`、`PARENTID_COLUMN_NAME = "parentID"`、
  `PARENTID_INDEX_NAME = "parentIDIndex"`、`utf8`/`utf8_bin` 缺省、
  `DBAPP_CURRENT_VERSION = 9`、`DBAPP_OLDEST_SUPPORTED_VERSION = 1`、
  `MAX_SECONDARY_DB_LOCATION_LENGTH = 4096`，以及 MySQL 标志位
  `PRI_KEY=2 / UNIQUE_KEY=16 / UNSIGNED=32 / MULTIPLE_KEY=256 / AUTO_INCREMENT=512`。
- `FieldType` 用 MySQL 判别值（`Decimal=0`…`String=254`），`from_field` 按
  `charsetnr` 查宽度并**整除**（utf8 的 `VARCHAR(40)` 长度 40/3=13），
  再按字节大小把 STRING/VAR_STRING 重映射进 blob 阶梯
  （<256 TINYBLOB / <65536 BLOB / <1<<24 MEDIUMBLOB / 其余 LONGBLOB）。
- 差异判定：`ColumnType::partial_eq` 复刻上面整张等价矩阵；
  `derive_index_type` 七分支 + 索引名缺失逐字 panic（`MF_ASSERT` 同形）。
- 驱动：`Inspector::visit_table` / `delete_unvisited_tables`，
  处理器 `TableHandler` 返回 `Result<bool, String>`——`Err` 就是
  `DatabaseException` 文本，沿参照的异常展开路径最终落到
  `synchroniseEntityDefinitions` 的逐字 catch；`TableValidator` 只报告、
  恒 `Ok(false)`。
- 门序：`Synchronise::do_synchronise` 按上表顺序逐门判定，
  `SyncFailure` 变体各自持有参照的逐字错误文本。
- 主界面：`resolve_connection` 逐字实现「选项存在才覆盖」，
  三条退出消息与退出码。

## 验收对应：DDL 差异可复核

MySQL 线缆层留在 `SyncDatabase` trait 背后（真库对接是另一面），
可复核的 DDL 制品是**语句日志**：测试用一个记录型 double 跑通全新库与
已有 schema 两条路径，并断言逐字语句——`CREATE TABLE`（含
`id BIGINT AUTO_INCREMENT, PRIMARY KEY idKey (id)` 的 id 占位怪癖）、
合并 `ALTER TABLE ... ADD COLUMN name VARCHAR(64) NOT NULL, DROP COLUMN id`、
`CREATE UNIQUE INDEX nameIndex ON tbl_Account (name(64))`、
`DROP TABLE tbl_Old`、12 条特殊表语句、以及
`CONVERT TO CHARACTER SET 'utf8'` / `ALTER DATABASE CHARACTER SET ? COLLATE ?`。

## 证据

- `cargo test -p db --lib`：**208 passed / 0 failed**
- `cargo test --workspace`：**4455 passed / 0 failed**
- `crates/db/src/tools/sync_db.rs` 行覆盖 **98.61%**（1005 DA / 14 missed；
  未覆盖者全是 `Default` 桩、panic 分支、闭合括号行与 trait 默认
  `upgrade_database` no-op——double 覆写了它）
- 作用域内 rustfmt / clippy 全清（`lib.rs` 未跑 rustfmt：递归进既有 fmt 债）

## 诚实登记

- 目录形态与本席其它车道不同：本实现落在 `crates/db/src/tools/`
  命名空间下（另两席用扁平 `crates/db/src/sync_db.rs`），
  **合并时必须择一**；本路径刻意避开同一路径 add/add 冲突。
- 期望表清单（`&[DesiredTable]`）是**入参**：property-mapping 面
  （`PropertyMappingsPerType::visit` 的递归子表、`INFO_MSG` 空属性提示、
  未用类型回收）不在本项范围。
- 升级阶梯内容（`mysql_upgrade_database.cpp`）留作 `upgrade_database`
  trait 钩子，默认 no-op；真阶梯属另一项。
- dry-run 的语句级过滤在连接层，归 BW-09-05-02；本项的 double 只验证
  驱动侧的语句发射。
- 无真 MySQL 实测：验收的「原版 schema」以 `SHOW COLUMNS`/
  `SHOW KEYS` 记录型 double 的形状代偿。