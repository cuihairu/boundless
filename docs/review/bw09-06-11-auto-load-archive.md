# BW-09-06-11 对接 auto-load 与 SQLite 归档 — 复核证据

- **任务编号**: BW-09-06-11（todo.md 行 1426）
- **验收**: 重启恢复对应实体
- **Boundless 模块**: `crates/db-postgres/src/store.rs`（`add_entity_type`/
  `set_auto_load`/`auto_load_entities` 三面 + 五条语句缝）、
  `crates/db-postgres/tests/auto_load_restore.rs`（新，5 测）、
  `crates/db-postgres/tests/{dry_run,schema_init,storage_contract,transaction,
  transaction_conflict,upgrade}.rs`（fmt 债清理，style 提交）
- **平台**: Linux x86_64，PostgreSQL 18（127.0.0.1:5432，真服务）

## 原版形状

| 原版位置 | 形状 |
|---|---|
| bw_meta_data.cpp:13-21 | `getEntityTypeIDQuery`/`setEntityTypeIDQuery`/`addEntityTypeQuery` 三查询——`bigworldEntityTypes` 行（typeID 内部自增 / bigworldID 运行期 id / name 唯一键）的维护面 |
| table_inspector.cpp:250-272 | `TypesCollector::addType`：同名查得 `INVALID` 则 `addEntityType`、查得不同 id 则 `setEntityTypeID`、相同则无事——sync 过的三腿 |
| mysql_database.cpp:669-715 | **`autoLoadEntities` 重启面**：`DELETE FROM bigworldLogOns WHERE NOT shouldAutoLoad`（:674-675）→ `SELECT logOn.databaseID, entityType.bigworldID FROM bigworldLogOns logOn, bigworldEntityTypes entityType WHERE logOn.typeID = entityType.typeID AND logOn.shouldAutoLoad`（:677-683，注释：typeID=库内部 id，bigworldID=运行期 entities.xml 位置）→ `numResults > 0` 才 `reserve`+逐行 `addEntity(bwTypeID, dbID)`+`UPDATE bigworldLogOns SET ip = 0, port = 0`（:693-703，objectID/salt 不动）→ `autoLoader.start()`（:704）；catch → ERROR + `autoLoader.abort()`（:706-713） |
| entity_type_mapping.cpp:792-800 | `EntityTypeMapping::updateAutoLoad` = `updateLogOnAutoLoadQuery`：`UPDATE bigworldLogOns SET shouldAutoLoad = ? WHERE databaseID = ? AND typeID = ?`——无记录匹配零行、静默 |
| entity_type_mapping.cpp:749-758 | `addLogOnQuery` 的 upsert 只动 objectID/ip/port/salt——**shouldAutoLoad 挂在记录上，重登后幸存** |
| put_entity_task.cpp:117-121 | `updateAutoLoad_` 腿（RETAIN/TRUE/FALSE）——旗写腿，且记录先落（`writeBaseMailbox_` :99-116 先 `addLogOnRecord`） |
| put_entity_task.cpp:89-97 | `insertExplicit` 平 INSERT、重 id 失败原行幸存——归档合并「缺行以自带 id 建」的写腿（consolidate 驱动 `putEntity`） |
| dbapp.cpp:1221-1225 | `pDatabase_->autoLoadEntities(*pAutoLoader)`——DBApp 启动调用点 |

## 端口语义

- **`add_entity_type(type_id, name)`**（`TypesCollector::addType` 三腿）：
  按名查 `bigworldID`——缺行 `INSERT (typeID, bigworldID, name) VALUES ($1, $1, $2)`、
  存量不同 id 则 `UPDATE SET typeID = $1, bigworldID = $1 WHERE name = $2`、
  相同 id 无事。**参照双 id 空间折叠**：参照 `typeID`（库内部自增，稳定）与
  `bigworldID`（entities.xml 位置）分立，故 `setEntityTypeID` 只动 bigworldID、
  旧记录按内部 id 照常 join；端口一律以 [`EntityTypeId`] 一元空间写库（06-05
  以来的既定口径），join 键与运行期 id 同源，改点必须**两列同移** join 才续。
- **`set_auto_load(key, bool)`**：`updateLogOnAutoLoadQuery` 逐字对应
  （`UPDATE "bigworldLogOns" SET "shouldAutoLoad" = $1 WHERE "databaseID" = $2
  AND "typeID" = $3`）；无记录匹配零行静默（MySQL UPDATE 语义）——
  验收用例实证「先旗后记录」旗即丢（与参照一致：旗挂记录上）。
- **`auto_load_entities(&self)`**：三腿按序——`DELETE … WHERE NOT
  "shouldAutoLoad"`（陈旧会话先清）→ 列表 SELECT（返回 `(bigworldID,
  databaseID)` 即 `addEntity(bwTypeID, dbID)` 序）→ **列表非空才**发
  `UPDATE "bigworldLogOns" SET "ip" = 0, "port" = 0`（:701-703 的
  `numResults > 0` 门）。`reserve`/`addEntity`/`start` 归调用者驱动（接
  auto-load recovery 缝）；三腿全走 06-09 的 `run_with_reconnect` 缝。
- **SQLite 归档对接**：归档写落库 = 合同 `put_entity` 两腿——行缺走
  `put_explicit_id = true`（`insertExplicit`，以归档行自带 id 建、重 id
  失败原行幸存），行在走 `false`（先存在性查询、更新）——与参照
  `ConsolidateEntityTask` 驱动 `putEntity` 的合并对生同形。

## 验收对应

新集成用例 `tests/auto_load_restore.rs`（5 例，真 PG18，每例私有库 +
`fixed_table_ddl`，结束 `DROP DATABASE WITH (FORCE)`；连跑 4 轮全绿）：

| 用例 | 基准点 |
|---|---|
| `a_restart_loads_exactly_the_flagged_entities` | **验收正面**：三实体 A/B/C、旗 A/C → 重启（新连接）`auto_load_entities()` = **[(4,1),(4,3)]** 恰旗者；列表喂装载缝逐个 `get_entity` 成功（**重启恢复对应实体**）；A 持有人零化（ip=0/port=0，**objectID=7/salt=7 幸存**）；B 行原在（值 22 不动）但会话已被清（logon_of=None） |
| `the_cleanup_clears_unflagged_sessions_and_zeroing_rides_the_list` | 清腿先行：空列表轮 B 会话已没（`DELETE … NOT shouldAutoLoad` 先于列表）；旗 + 重记后会旗者上榜且**新持有人被零**（`numResults > 0` 门口径） |
| `an_archived_write_restores_the_row_with_its_own_id` | **归档对接**：崩溃前只到归档的行（主库缺）重启后合并落库——缺行 `put_explicit_id=true` 以自带 id 建读回 99、在行 `false` 原地更 77——**恢复对应实体**（归档半面） |
| `the_flag_survives_a_relogon_and_stays_silent_without_a_record` | 旗挂记录：重登 `put_logon_record`（upsert 只动 objectID/ip/port/salt）后第二轮 `auto_load_entities()` 仍 [(4,1)] 且**新持有人被零**；无记录实体 `set_auto_load` 零行静默 Ok |
| `a_repointed_type_updates_its_meta_row_and_the_join_follows` | `addType` 改点腿：同名换 id → meta 行两列同移；同 id 复过=无事；join 从新 id 解析（[(9,1)]） |

## 证据

- `cargo test -p db-postgres`（`BOUNDLESS_PG_URL` 指真 PG18）：
  **lib 46 / accounts 4 / auto_load_restore 5 / connection_recovery 4 /
  dbid_allocation 3 / dry_run 3 / round_trip 6 / schema_init 2 /
  storage_contract 17 / transaction 4 / transaction_conflict 4 /
  transaction_rollback 4 / upgrade 3 / verify_relogon 4，共 109 例全绿
  0 failed**；auto_load_restore 连跑 4 轮 exit 0。
- `cargo test --workspace`：**4710 passed / 0 failed（exit 0）**。
- llvm-cov 行覆盖：**1663/1665 = 99.88%**（store.rs 751/753 = 99.73%；
  未达两行 = 06-07/06-09 已登记的 ROLLBACK 服务端错误臂——本轮移位至
  601-602，新面语句全数覆盖）。
- fmt：`cargo fmt -p db-postgres --check` **全包零 diff**（含随本项清掉的
  六测试文件既有债）；`cargo clippy -p db-postgres --lib --tests` 无新告警
  （仅既有 workspace manifest 两警告）。

## 诚实登记

- **SQLite 读面与装载驱动在本分支为孤儿模块**：`db::SecondaryDatabase`
  （BW-09-03-02/03，62f8bf5/1475730）与 `db::auto_load_recovery` 的
  `EntityAutoLoader`（BW-09-03-04，b2d1c3a）代码在 HEAD 的 git 祖先里，
  但 `lib.rs` 声明随旧布局合并丢失（chair 旗「db 九孤儿模块」；复活提交
  d54b9fd 在 lane/bw08-ledger-tick）。本项的归档行以「归档合并将落到主库
  的已解码值」替身、装载缝以闭包替身——两模块复活后，`auto_load_entities`
  的返回列表 `(EntityTypeId, DatabaseId)` 即 `reserve`+`addEntity` 的实参
  序，直接对接、零改面。
- **参照双 id 空间折叠的行为差**：重指（同名换 id）后，旧 id 下的旧会话行
  不再 join——旗在故不被 DELETE 腿清、也不再零持有人（参照下旧记录按稳定
  内部 id 照常 join 零化）；新登录按新 id 写即续。已在 `set_entity_type_id_sql`
  注释与用例中登记。
- **`numResults > 0` 门无可观察状态差**：DELETE 腿先清非旗会话，空列表时
  余行本来就空、UPDATE 发与不发终态相同——门在代码（mysql_database.cpp:701-703
  照抄位置），用例以「空轮不列表 / 旗后上榜且持有人被零」覆盖门口径。
- **catch → ERROR + `abort()`（mysql_database.cpp:706-713）未复刻为驱动内
  catch**：端口面返回 `Result` 向上传播；`abort` 语义归 EntityAutoLoader
  驱动（孤儿模块，复活后对接）。
- **`getAutoLoadSpacesFromEntities`（mysql_database.cpp:543-600）空间腿
  未复刻**：空间属 cell 车道（BW-10），PG 后端无空间面。
- **`deleteUnwantedTypes`（table_inspector.cpp:275+）未复刻**：清不再在
  defs 的行——归 sync_db 全量同步面（BW-09-07 工具簇），本项只交
  `addType` 三腿。
- **db-postgres tests 六文件 fmt 债随本项清（独立 style 提交）**：rustfmt
  1.11.0-stable + edition 2024 下 HEAD 态过不了 `cargo fmt -p db-postgres
  --check`（import 序与 `assert!` 折行；本轮实测先前交付文件带此债）——
  与 foundation fmt 债（cdf62ec）同例，一并清掉使全包 fmt 门为真绿。
- **WARNING/ERROR 日志文本未复刻**（"Restore entities failed (%s)" 等）：
  db-postgres 无日志面，动作等价可观察。
