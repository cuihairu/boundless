# BW-09-07-04 导出并恢复备份 — 复核证据

- **任务编号**: BW-09-07-04（todo.md 行 1439）
- **验收**: 空目标库恢复相同记录
- **Boundless 模块**: `crates/db-postgres/src/backup.rs`（新模块，
  `export_backup` / `restore_backup` + `DbBackup` / `EntityTypeBackup`
  快照结构）、`crates/db-postgres/src/lib.rs`（注册+re-export）、
  `crates/db-postgres/tests/backup.rs`（3 例验收）
- **平台**: Linux x86_64，PostgreSQL 18（127.0.0.1:5432，真服务）

## 原版形状

| 原版位置 | 形状 |
|---|---|
| 参照树无内置备份工具（grep backup/mysqldump 仅第三方噪音） | 参照的备份是**库外** `mysqldump`：整库导出 schema+行+`AUTO_INCREMENT=N` 座位（CREATE TABLE 尾携带），恢复即整库重放——端口在此定义接缝：快照结构即端口侧的 dump 文件 |
| docs/operations/database.md:84-92 | 恢复副本对账口径=07 导入同一组值：实体 id、账号链、二进制字段、默认值、**下一个可分配 DBID**——「座位随备份走」是验收硬条件，丢了座位即 BW-09-06-04 不在备份里 |
| entity_type_mapping.cpp `entityTypeMappings_` | 类型面=映射表（`bigworldEntityTypes`）逐行：坍缩 id+表名词干——导出按 `typeID` 序逐行读，恢复先落映射行（addType 三腿） |
| put_entity_task.cpp:89-97 `insertExplicit` | 行落位=显式 id 平 INSERT（07-05 三腿驱动的缺席腿），撞键=该行失败 |
| account.cpp `addAccount`/`getAccount` | 账号面=`bigworldLogOnMapping` 逐行，恢复走导入三腿（搬移映射 → `DuplicateAccount`） |
| mysql_database.cpp `nextID`/AUTO_INCREMENT 语义（06-04 已复刻） | 座位=序列的「下一个发放值」：`last_value+1`；从未发放（处女序列）取 `start_value`（=1） |

## 端口语义

- **导出=数据库自身真相**：`bigworldEntityTypes` 映射行（`typeID` 序）
  → 每类型解码行（走注册面 `all_entities`，(type,id) 全序）→
  `bigworldLogOnMapping` 逐行（`logOnName` 序）→ **每类型座位**
  （`pg_sequences` 读 `last_value+1` / 处女取 `start_value`）。
  快照按此序冻结，重导出逐字段可比。
- **座位读键原始目录名**：建表路径（`"id" BIGSERIAL`）恒名序列
  `<table>_id_seq`，`pg_sequences` 存裸名——裸名等值绕开了
  regclass 文本比较的引号错配（`pg_get_serial_sequence` 打印
  `public."tbl_Hero_id_seq"`，目录合成 `public.tbl_Hero_id_seq`）。
  这同时把 `pg_get_serial_sequence` 整个请出座位读：PG18 对不存在
  关系它**抛错而非返 NULL**，且计划器会把非关联标量子查询从 CASE
  里提升成急切求值的 init-plan——CASE 惰性护不住它（两形均已实测）。
- **座位写 `setval(seq, seat, true)`**：`is_called` 保持 true——
  恢复库再导出算回同一个 next（导出/恢复往返无痕）。写法**不带
  GREATEST**：备份座位是权威值，既升也降（restore 先被显式写
  `bump_identity` 顶到 max(id)，备份座位随后覆盖——正是「序列属于
  备份」的兑现）。座位 0（处女表 next=1）不写：新表天然在位。
- **恢复=三腿驱动全租**：映射行先落（addType 三腿），行与账号走
  07-05 导入三腿（缺席 verbatim 显式写 / 等值 skipped / 冲突具名
  失败）——07-07 的重跑合同顺路骑过：对已恢复目标重跑恢复，
  全部 skipped，绝不二次落地。座位**最后**落（行全落定后覆盖）。
- **具名失败两臂**：恢复遇目标未注册的类型 → `entity type {} is not
  registered on the target`（映射行已落、行拒绝盲移）；导出遇映射行
  指向未建表 → `table {} has no identity sequence`（映射无表状态
  拒绝静默冻结）。
- **座位判据的边界诚实**：写路径只能升（GREATEST），座位低于 max(id)
  的库经公共面不可达——判据取「max(id) 之上的余量」（已分配后删除
  的 id）：恢复丢座位者发 9001，守座位者发 9004。

## 验收对应

`tests/backup.rs` 3 例（真 PG18，连跑 3 轮全绿）：

| 用例 | 验收点 |
|---|---|
| `a_backup_restores_into_an_empty_target_with_identical_records` | **验收主形**：源 Hero 行 7（`玩家·β`、Int 3、blob 全 261 恶意字节）+行 9000（全默认）+三次分配后删除（座位 9004>max 9000）+Item 表分配行 1（空 Vector 默认形）+2 账号（密码含 `\x00\xFF\x7F`）；快照形冻结 `[(4,"Hero",2,9004),(5,"Ghost",0,1),(9,"Item",1,2)]`；空目标（表+注册、零映射行）恢复 `{written:5, skipped:0}`；双侧独立读 `all_entities` 全等；**目标重导出与原快照逐字段相等**；三类型双侧 `put_new_entity` 发号一致（Hero 双侧 9004——丢座位者给 9001；Item 2；Ghost 1） |
| `a_rerun_of_a_restore_lands_nothing_and_leaves_the_state_alone` | 重跑恢复：首 `{written:2, skipped:0}`，再跑 `{written:0, skipped:2}`（07-07 合同骑过），终态重导出仍与快照相等 |
| `restore_names_an_unregistered_type_and_export_names_a_missing_table` | 恢复遇未注册类型 4 → 具名 `entity type 4 is not registered`；导出遇映射行无表（Phantom）→ 具名 `has no identity sequence` |

## 证据

- `cargo test -p db-postgres`（`BOUNDLESS_PG_URL` 指真 PG18）：
  **全 19 目标共 123 例全绿 0 failed**（lib 48、backup 3、其余不变）；
  backup 连跑 3 轮 3/3 全绿。
- `cargo test --workspace`：**4721 passed / 0 failed（exit 0）**。
- llvm-cov 行覆盖（`cargo clean -p db-postgres` 后）：**1812/1814 =
  99.89%**——`backup.rs` **112/112 = 100%**；store.rs 2 行未达 =
  已登记 ROLLBACK 臂（601-602，与 07-05/07-07 同臂）。
- fmt：叶子文件全路径 fmt + `cargo fmt -p db-postgres -- --check`
  全包零 diff；`cargo clippy -p db-postgres --lib --tests`
  无新告警。

## 诚实登记

- **座位读走过的三版弯路**（全实测后定稿）：① `pg_sequence` 目录
  无 `is_called`/`last_value` 列（E: column s.is_called does not
  exist）→ 改 `pg_sequences` 视图；② `pg_sequences` 裸名合成与
  `pg_get_serial_sequence` 引号打印错配（regclass 文本比较
  `to_regclass('public.tbl_Hero_id_seq')` 返 NULL——无引号名折小写）
  → 改 `quote_ident` 复刻打印形；③ PG18 对不存在关系
  `pg_get_serial_sequence` **抛错**，且 CASE 惰性护不住（计划器把
  非关联标量子查询提升为 init-plan 急切求值，psql 两形实测均抛
  `relation "tbl_Phantom" does not exist`）→ 最终裸名等值，彻底
  不做关系解析。以上三坑均非猜测，现形皆在 `NEXT_IDENTITY_SQL`
  文档注释内。
- **`query_one` 在零行时抛 RowNotFound**（先于我们的具名错误）——
  座位读必须 `query_opt` + `ok_or_else` 具名，第一版踩过后回改。
- **导出对「映射行无表」用数据库自身抛错不行**——报错文本进不了
  我们的具名形状，改判据后（裸名不命中）自然落进具名臂，100% 覆盖。
- **恢复假定目标已到空 schema 态**（表已建+类型已注册，07-01 的
  面）——`restore_backup` 不建表；未注册类型走具名失败臂而非静默。
- **座位为 0 不写 setval**：处女表天然 next=1，写 `setval(seq,0)`
  反是错误（PG 拒 0 以下），判据 `next_dbid > 1` 才写。
- **跨席登记**：BW-09-07-04 与 bw19-docs 席（e4d24e5）、
  bw08-ledger-tick 席（f33e7cf）的平行交付处于未合并车道——
  合并时由主席择一，本席交付以本 doc+测试为准。
