# BW-09-06/07 PostgreSQL 勾账（合并后主基核对）

- **任务编号**: BW-09-06-01..11 / BW-09-07-01..07（todo.md BW-09-06/07 节）
- **动机**: 五条 lane 并入 main（`f1c9f06`）后，`crates/db-postgres` 曾被并账解到
  早代形状：BW-09-06/07 共 18 行全部未勾、`store.rs` 无 `impl EntityStorage`。
  本席对照 main 现树逐行核验，只勾代码可证者，并把未勾原因登记在册。

## ⚠️ 终局（2026-11-25 同日续记）

本文的「重交清单」**已被 main 自身取代**：主席（boundless2 merge 工作树）把
整代 EntityStorage 落回 main——`b3afdea`（feat: complete the postgres storage
backend，store.rs 1158 行改动 + storage_contract/transaction 三件/
connection_recovery/verify_relogon/auto_load_restore/import_mysql/dbid_allocation
全套集成）+ `5f63798`（docs: tick bw-09-06 and bw-09-07 rows，18 行全勾）。
本席同窗口的重交尝试（`f5fd218`/`9344601`，仅 06-05 一行、弃 bump 腿的
另一形状）成为**严格子集的重复交付，已撤档**至 `lane/bw16-18-archive2`；
本 lane 重排为新 main 基 + 本席 BW-10-01-05 系列重放（`c8dc49e`..`ddf78c1`）。
下文核对过程留档，裁定表按 `f1c9f06` 时点有效。

## 背景：db-postgres 的并账取形

- `git log origin/main -- crates/db-postgres/` 显示并账后该目录由八个交付提交
  推进：`4e8ba87`（实体 schema 映射）→ `e68158f`（账号表）→ `e71ccdc`（建库）→
  `022bee4`（dry-run）→ `9aacde0`（schema 更新）→ `a289d8d`（表序列 DBID）→
  `e4d24e5`（备份导出恢复）→ `a5225bb`（并入 lane/bw19-docs）。
- 本席 lane 的后续交付虽在 main 祖先链中（`git merge-base --is-ancestor` 均命中：
  `fe41751` 存储契约、`e240260` 事务提交、`23108c6`/`bb1dc2c` 回滚与冲突、
  `348c360` 断链恢复、`e7df81e` 重登验证、`4de1257` auto-load、`fc61d0d` 导入、
  `0cbc334` 校准、`3f218d8` 重跑），但其 `crates/db-postgres` 内容被上述并账覆盖：
  时点 main 的 `store.rs` 无 `impl EntityStorage for PostgresStorage`，无事务/
  重连面，无 `import_mysql.rs`。属 f2ef112 同类的并账静默失码。
- `git grep "impl EntityStorage for"` 当时只有 `crates/db` 侧
  （`StorageConnection`/`MemoryStorage`/`MemoryConnection`/`XmlDatabase`）与测试替身。
- `docs/review/bw09-06-05..11`、`bw09-07-05..07` 各文档在树留存（文档不参与该冲突
  取舍），故现树「有文档、无对应代码」。

## 核验方法与门禁

- 环境：本地 PG18（`127.0.0.1:5432`），`BOUNDLESS_PG_URL="host=127.0.0.1 user=cui
  password=boundless-dev dbname=postgres"`；集成测试以该变量为服务端门，缺则跳过，
  本次真跑未跳过。凭据仅留在会话环境，不入树。
- `cargo test -p db-postgres`：**79/79 绿**（lib 50 + 集成 29）。
- 未提交任何代码改动；本核对为台账勾账，不改 crate。

## 逐行裁定（f1c9f06 时点）

| 行 | 裁定 | main 面 | 验收证据 |
| --- | --- | --- | --- |
| 06-01 主存储选择 | **未勾** | 无选择/报告面；`apps/server` 仍 36 行无 dbapp 入口 | 验收要求「DBApp 启动时报告所用后端」，启动面缺位 |
| 06-02 schema 与字段映射 | **已勾** | `schema.rs` + `json.rs`（4e8ba87） | `round_trip.rs` 6 例 |
| 06-03 账号表与唯一约束 | **已勾** | `accounts.rs`（e68158f） | `accounts.rs` 集成 4 例 |
| 06-04 DBID 分配 | **已勾** | `create_entity` 表序列（a289d8d） | `dbid.rs` 5 例（并发无重复/重启连续/显式不推序列/各表独立） |
| 06-05 实体读写删查 | **未勾** | 无 `EntityStorage` 实现、无删除腿 | 本席 `fe41751` 被覆没 |
| 06-06 事务提交 | **未勾** | 无 `BEGIN`/`COMMIT` 面 | 本席 `e240260` 被覆没 |
| 06-07 事务回滚 | **未勾** | 同上 | 本席 `23108c6` 被覆没 |
| 06-08 并发写冲突 | **未勾** | 仅 `map_write_error`，无重试回路 | 本席 `bb1dc2c` 被覆没 |
| 06-09 断链恢复 | **未勾** | 无 `run_with_reconnect` 面 | 本席 `348c360` 被覆没 |
| 06-10 账号重登验证 | **未勾** | 无会话记录面 | 本席 `e7df81e` 被覆没 |
| 06-11 auto-load 与 SQLite 归档 | **未勾** | `crates/db/src/auto_load*.rs` 在，无 PG 对接 | 本席 `4de1257` 被覆没 |
| 07-01 初始化新库 | **已勾** | `schema_init.rs`（e71ccdc） | `schema_init.rs` 集成 2 例 |
| 07-02 schema dry-run | **已勾** | `dry_run.rs`（022bee4） | `dry_run.rs` 集成 3 例 |
| 07-03 应用 schema 更新 | **已勾** | `upgrade.rs`（9aacde0） | `upgrade.rs` 集成 3 例 |
| 07-04 备份导出与恢复 | **已勾** | `backup.rs` export/restore（e4d24e5） | `backup.rs` 集成 6 例 |
| 07-05 导入 MySQL | **未勾** | 无 `import_mysql.rs` | 本席 `fc61d0d` 被覆没 |
| 07-06 校准导入后 DBID | **未勾** | 校准机制仅存于恢复腿（backup.rs setval） | 本席 `0cbc334` 被覆没 |
| 07-07 迁移失败与重跑 | **未勾** | 无重跑/幂等面 | 本席 `3f218d8` 被覆没 |

## 教训留档

- 并账失码类（代码是祖先、内容不在树）用路径日志 + `merge-base --is-ancestor`
  + `git grep <符号>` 三件套核验，文档在树而代码不在是典型指纹。
- 同窗口多席竞速：勾账/重交前先 `git ls-remote origin refs/heads/main` 取实时
  tip，勿用本树陈旧 `origin/main` 引用下结论——本席裁定表做出时 b3afdea 已在
  推送途中。
- 重交前先 diff 现树形状再动手：本席弃 bump 腿改裸 INSERT 的裁定与 main 终选
  形状（保 bump_identity + OVERRIDING，BY DEFAULT 下语义照常）相左，撤档即免
  三方仲裁。
