# OPT-07-01-01 记录备份和归档基线

Acceptance: **保存带宽、队列及恢复耗时**。本行交付 BaseApp 容错数据路径（备份 + 归档）的基线记录：criterion 基准 + 结构性地板钉测。零行为改动。

## 交付面

- `crates/server-runtime/benches/backup_archive.rs` + `[[bench]] backup_archive`（`Cargo.toml`）— 四个基准面：
  - `backup_write_{64,512}` — **带宽面**：一个 primary app 的整轮备份周期（每实体一次 `writeBackupData`，即 `backupBaseEntity` 消息携带的字节），`Throughput::Bytes` 标注。
  - `backup_restore_{64,512}` — **接管恢复面**：失联 app 存下的全部备份重建为实体（`BackedUpEntities::restore`）。
  - `archive_tick_512` — **归档队列面**：一个完整 `Archiver` 周期把持久化集合按 period=8 tick 排干。
  - `archive_restore_512` — **重启恢复面**：`ArchiveStore` 里全部持久化条目读回重建（`restore_archived`）。
- `crates/server-runtime/src/base_backup.rs` 新测 `the_standard_entity_backup_has_a_pinned_wire_size` — **结构性带宽地板**：标准五属性实体的备份是精确 181 wire 字节（header id/type_id/type_name/db_id + cell addr/两 pending/space + base+cell 两组五属性），且字节可回读重建同一实体（id/db_id/base_props 相等）。墙钟中位数是本机读数，不入断言。

## 基线数字（2026-11-26 本机，热机单轮读数，机器相关不入带）

| 面 | 中位 | 每实体摊 | 判读 |
|---|---:|---:|---|
| `backup_write_64`（2.7 KiB/轮） | 49.96 µs | ≈0.78 µs | 带宽地板=181 B/实体/轮（钉测） |
| `backup_write_512`（90.5 KiB/轮） | 451.7 µs | ≈0.88 µs | 序列化近线性 |
| `backup_restore_64` | 266.7 µs | ≈4.2 µs | 恢复比写入贵 ~5×（parse+重建） |
| `backup_restore_512` | 2.52 ms | ≈4.9 µs | 线性 |
| `archive_tick_512`（8 tick 整周期） | 905.9 µs | ≈113 µs/tick | 队列切片=size/period=64 实体/tick（既有周期钉测） |
| `archive_restore_512` | 1.90 ms | ≈3.7 µs | thrpt ≈269 K 实体/s |

## 诚实判读

- **带宽**：结构性地板 181 B/实体/轮由钉测钉死（跨机器可复现）；序列化耗时本机读数 ~0.8–0.9 µs/实体。
- **队列**：归档队列形状是确定性的——`bases_to_archive` 按 `size*index/period` 切片，每 tick 排 `size/period` 条（`a_cycle_spreads_entities_over_the_period_ticks` 已钉）；本行只记录整周期排干耗时。
- **恢复耗时**：备份接管恢复 ≈4–5 µs/实体、归档重启恢复 ≈3.7 µs/实体（本机读数）；两者都是 O(n) 全量重建，基线留给 OPT-07-01-02 的优化对照。

## 门禁

- `cargo test -p server-runtime --lib`：**1702 passed / 0 failed**（33 backup 族含新钉测）。
- `cargo clippy -p server-runtime --lib --benches -- -D warnings`：0 error。
- `cargo fmt -p server-runtime -- --check`：净。

## 坑

- `cell_data` 里没有 `Addr`——`Addr=(u32,u16)` 元组别名在 `baseappmgr`/`dbappmgr`/`cellappmgr` 各有一份（结构等价），备份消息面用的是元组不是 `BaseAddr`（那是 BaseEntity 的 cell_entity_addr 字段类型）。
- criterion 对 <5 s 的样本集会告警「Unable to complete 100 samples」并自动延窗（archive_restore_512 延到 9.9 s）——告警不是失败。
