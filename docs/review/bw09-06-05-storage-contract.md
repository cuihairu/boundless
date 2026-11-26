# BW-09-06-05 实体读写删查 — 复核证据

- **任务编号**: BW-09-06-05（todo.md 行 1414）
- **验收**: 共用存储契约用例通过
- **Boundless 模块**: `crates/db-postgres/src/store.rs`、
  `crates/db-postgres/tests/storage_contract.rs`（新）、
  `crates/db-postgres/src/schema.rs`、`crates/db-postgres/src/schema_init.rs`
- **平台**: Linux x86_64，PostgreSQL 18（127.0.0.1:5432，真服务）

## 原版形状

| 原版位置 | 形状 |
|---|---|
| `crates/db/src/storage.rs` | **契约本身**：`db::EntityStorage` 的读写删查签名（get/put/del/logon_of/check_out/check_in/all_entities）；内存后端（MemoryStorage）的单测即「共用存储契约用例」，验收要求同一组用例在 PostgreSQL 上通过 |
| entity_type_mapping.cpp:147-148 | 构造期语句：`deleteIDQuery_`（`DELETE FROM tbl WHERE id=?`）等——每类型一组预编译语句，本端口为每 schema 现拼 SQL |
| entity_type_mapping.cpp:527-529 | `removeLogOnQuery_ = "DELETE FROM bigworldLogOns WHERE databaseID=? AND typeID=?"` |
| entity_type_mapping.cpp:533-535 | `getLogOnQuery_ = "SELECT objectID, ip, port, salt FROM bigworldLogOns WHERE databaseID = ? and typeID = ?"` |
| entity_type_mapping.cpp:749-758 | `addLogOnQuery_`：`INSERT INTO bigworldLogOns (databaseID, typeID, objectID, ip, port, salt) VALUES (?,?,?,?,?,?) ON DUPLICATE KEY UPDATE …`——**重登覆盖旧行** |
| entity_type_mapping.cpp:764-766 / 773-783 / 796 / 804-816 | `checkOut`/`getLogOnRecord`/`writeBaseMailbox`：登记记录 ip/port 以 `htonl`/`htons` 网络字节序落库，读回 `ntohl`/`ntohs`；salt 按列宽整存 |
| del_entity_task.cpp:69-76 | **删除顺序**：`deleteWithID` 失败 → `setFailure` 而**不碰** LogOn 记录；成功 → 无条件 `removeLogOnRecord` |
| put_entity_task.cpp:85-97 / 103-108 | put 三分支：update / insertNew（待分配）/ insertExplicit（重复 id 失败，`AlreadyExists`）；update 缺行 → 参照 XML 后端 `isOK=false` |
| xml_database.cpp:617-620 | XML 后端 update 缺行即失败——「缺行更新 = NotFound」的合同出处 |

## 端口语义

- **每连接类型注册表**：`register_entity_type(type_id, &schema)` +
  `schema_of(type_id)`，对应参照的 `entityTypeMappings_`（参照为进程级启动注册；
  PG 端口每连接各自注册一次，本文件测试里读写双方都注册）。未注册类型的各操作
  结局照契约：get → `Ok(None)`，put → `Storage("entity type {} is invalid")`
  （XML 后端文案），del/check_out → `NotFound`；logon_of/check_in 不设类型门
  （固定表上按 type_id 查询，未注册类型自然无行）。
- **put 三腿**（trait `put_entity(key, values, put_explicit_id)`）：
  - `db_id == PENDING_DATABASE_ID` → `put_new_entity`（06-04 的 RETURNING 分配腿）；
  - `put_explicit_id` → 先 `bump_identity`（`setval(GREATEST($id, nextval))`，
    MySQL `AUTO_INCREMENT` 越过显式 id 的等效物，nextval 先取保证只升不降）再裸
    `INSERT … OVERRIDING SYSTEM VALUE`；重复 id 经 `map_entity_db_error` →
    `AlreadyExists`（实体表唯一键只有主键 id，账户表才用 `DuplicateAccount`）；
  - 其余 → update 腿：`row_exists` 先查（upsert 会把缺行写成新行，故先挡），
    缺行 `NotFound`，否则 `ON CONFLICT ("id") DO UPDATE`。
- **del**：`DELETE … WHERE id=$1` 受影响 0 行 → `NotFound` **且不碰** LogOn 记录
  （del_entity_task.cpp:69-74）；成功则无条件 `remove_logon_sql`。
- **LogOn 面**（固定表 `bigworldLogOns`，DDL 已在 `schema_init::fixed_table_ddl`）：
  - `put_logon_record(key, &mailbox)`（公有裸面，= 参照 `addLogOnRecord`）：
    `INSERT … ON CONFLICT ("typeID","databaseID") DO UPDATE`——重登覆盖；
  - `logon_of`：`SELECT "objectID","ip","port","salt" …`，ip/port 落库为网络
    字节序（`stored_ip`/`stored_port` 即 `htonl`/`htons`，读回 `read_ip`/`read_port`
    即 `ntohl`/`ntohs`，两端同为 `swap_bytes`）；salt 为 `mercury::endpoint::Address`
    的 u16，整宽存 INTEGER；
  - `check_out`：行存在且已被占用 → `CheckOut::Elsewhere`，否则写入登记记录返回
    `Granted`；实体缺行 → `NotFound`；
  - `check_in`：无条件 `DELETE`（= 参照 removeLogOnRecord，不查存在性）。
- **all_entities**：按注册类型序逐表 `SELECT`，行序再按 (type_id, db_id) 全序排定
  （契约要求确定序）。
- **RefCell<Client>**：`postgres::Client` 的方法都要 `&mut self`，而契约的读方法
  是 `&self`——`client: RefCell<Client>` + `client_mut()` 提供内部可变性。

## 验收对应

新集成用例 `tests/storage_contract.rs`（17 例，真 PG18，每例私有库
`boundless_pg_test_<pid>_<n>` + 固定表，结束 `DROP DATABASE WITH (FORCE)`）：

| 用例 | 契约点 |
|---|---|
| a_create_allocates_a_fresh_id_visible_from_an_independent_connection | 待分配腿 + 独立连接可见 |
| an_update_overwrites_the_row / an_update_of_a_missing_row_is_not_found | update 腿 + xml_database.cpp:617-620 |
| an_explicit_id_inserts_under_that_id_and_rejects_duplicates | insertExplicit 腿 + AlreadyExists |
| created_ids_clear_the_seeded_rows | 造 id 与显式 id 共存（seed 5 → 下一造 6） |
| a_write_and_a_delete_through_one_connection_are_visible_from_the_others | 跨连接读写删 |
| a_deleted_row_stops_matching_and_a_repeat_delete_matches_that / a_delete_only_takes_its_own_row | del 缺行 NotFound + 类型域隔离 |
| concurrent_logons_never_double_occupy / checkin_releases_and_a_later_checkout_grants_again / a_successful_delete_releases_its_checkout | check_out Granted/Elsewhere + check_in 释放 |
| all_entities_hands_over_every_record_in_a_deterministic_order | 全序契约（(3,1),(4,2),(4,9)） |
| the_logon_record_stores_the_address_in_network_byte_order / a_relogon_overwrites_the_log_on_record | htonl/htons + 重登覆盖 |
| an_unregistered_type_answers_the_contract_outcomes | 四操作未注册类型结局 |
| a_registry_entry_that_drifted_from_its_table_is_a_storage_fault / a_statement_on_a_missing_table_is_a_storage_fault | `map_entity_db_error` 非重复臂 + 缺表故障面 |

内存后端的契约用例逐条覆盖：读（None/Some）、写（造/改/显式/重复）、删
（命中/缺行/隔离）、查（确定全序）、登记（Granted/Elsewhere/释放/字节序/覆盖）。

## 证据

- `cargo test -p db-postgres`（`BOUNDLESS_PG_URL` 指真 PG18）：
  **lib 46 / accounts 4 / dbid_allocation 3 / dry_run 3 / round_trip 6 /
  schema_init 2 / storage_contract 17 / upgrade 3，84 例全绿 0 failed**。
- `cargo test --workspace`：**4685 passed / 0 failed（exit 0）**。
- llvm-cov 行覆盖：**1523/1523 = 100.00%**
  （store.rs 611/611、schema.rs 166/166、json.rs 300/300、schema_init.rs 181/181、
  accounts.rs 74/74、dry_run.rs 98/98、upgrade.rs 93/93）。
- fmt 仅作用叶子文件；`cargo clippy -p db-postgres --lib --tests` 无告警。

## 诚实登记

- **UFCS**：`PostgresStorage` 固有方法 `put_entity(schema, record)`/`get_entity`
  与 trait 方法同名不同元数，方法解析直落固有版（E0061 不回退），测试调用点一律
  `EntityStorage::put_entity(&mut storage, …)` 显式走契约。
- **登记记录的事务面未交付**：put/delete 仍逐语句自动提交，显式事务与回滚是
  BW-09-06-06/07 的活；`put_logon_record` 以公有裸面先交（06-10 账号重登复用）。
- **bump 先于 INSERT**：nextval 先取保证只升不降（被拒的显式写也烧掉一个 id，
  等效 MySQL 拒绝插入仍爬 AUTO_INCREMENT），代价是 id 序列里留一个不可见空洞；
  `pg_get_serial_sequence` 的表名参数必须**带引号**传入，否则混合大小写标识符
  折叠成小写找不到序列。
- **salt 整宽**：`mercury::endpoint::Address.salt` 是 u16，整段存 INTEGER 列；
  参照 MySQL 列为 INTEGER，语义一致。
- **注册表与表可能漂移**：注册是连接内存态、表是库态；漂移（如注册列多于实表列）
  落通用 `Storage` 故障面，`a_registry_entry_that_drifted…` 一例钉死该臂。
- 集成用例在无服务器环境按仓内惯例跳过并提示；覆盖率数字取自真跑。
