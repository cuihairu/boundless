# OPT-07-01-02 优化备份序列化和归档

Acceptance: **按同负载比较队列与恢复**。本行对备份/归档的序列化与恢复路径做三处行为保持的结构性优化，交错钉核 A/B 同负载对照。零行为改动（全部 1702 测不动）。

## 优化面（三处）

1. **恢复单遍化** — `BackupReceiver::restore` 与 `archiver::restore_bytes` 原本先在一个流上 peek 类型名、再从**另一份字节拷贝**的整条流重读头+体（每实体两次流构造、每字节两次读）。新 `restore_from_stream` 从单条流一次读头→查类型→读体，字节只过一遍。旧 `peek_type_name`/`type_name_of` 删除（`read_backup_data` 公共行为不变，拆成 `read_backup_header`+`read_backup_body` 复用）。
2. **`encode` 移出拷贝** — 归档编码 `stream.as_bytes().to_vec()`（最后一段全载荷复制）改 `stream.into_bytes()`（新 `BinaryStream::into_bytes` 直接移动内部缓冲）。`with_capacity` 同时补进 `BinaryStream`（一次性分配不生长）。
3. **`restore_base` 去双重重建** — `data.base_props.clone().into_iter().collect()` 是 clone 一份 HashMap 再重哈希进第二份，改直接 `clone()`（同结果、半工作量）。

## A/B 对照（2026-11-26 本机，taskset -c 6 钉核，交错 6 轮，warm-up 1s/measure 2s/sample 50，二进制身份 `cmp` 验字节不同）

| 面 | baseline 中位 | candidate 中位 | 比 | 候选更差轮次 |
|---|---:|---:|---:|---|
| `backup_restore_512`（接管恢复） | 2205.6 µs | 1819.9 µs | **0.825** | **0/6** |
| `archive_restore_512`（重启恢复） | 2815.1 µs | 1505.0 µs | **0.535** | **0/6** |
| `archive_tick_512`（队列面整周期排干） | 1537.5 µs | 1201.0 µs | **0.781** | **0/6** |
| `backup_write_512`（写面，代码未动=对照面） | 491.1 µs | 435.5 µs | 0.887 | 3/6（轮间交叉） |
| `backup_restore_64`（小面） | 252.1 µs | 245.2 µs | 0.973 | 1/6 |
| `backup_write_64`（小面，代码未动=对照面） | 50.5 µs | 62.3 µs | 1.235 | 5/6 |

## 诚实判读

- **恢复面达标**：`archive_restore_512` 0.535（6/6 轮候选胜）、`backup_restore_512` 0.825（6/6 胜）——单遍恢复+去双重重建的效应远超噪声；轮内每对候选均低于同轮基线。
- **队列面** `archive_tick_512` 0.781（6/6 轮方向一致）：`encode` 移出拷贝（每 tick 切片 64 实体的载荷复制）贡献，方向全胜支持其真实。
- **写面是未改对照**：`backup_write` 代码两侧完全相同，`backup_write_512` 中位 0.887 但轮间交叉、`backup_write_64` 反而 1.235——证明这台机器测量窗内噪声尺度约 ±20%，小面（write 64 / restore 64）读数不判优；恢复面与队列面的 1.9×/1.28× 收益远在噪声尺度之上。
- 每轮候选值随窗漂移（如 archive_tick 候选 767–1531µs），故判读用跨轮中位数+逐轮方向，不用单点。

## 门禁

- `cargo test -p server-runtime --lib`：**1702 passed / 0 failed**（行为测全不动；`read_backup_data`/`BackupReceiver::restore`/`restore_archived` 的既有测全绿=字节等价）。
- `cargo test -p foundation --lib` 流测全绿（新 `with_capacity`/`into_bytes` 测）。
- `cargo clippy -p foundation --lib -- -D warnings`、`cargo clippy -p server-runtime --lib --benches -- -D warnings`：0 error。
- `cargo fmt -- --check`：净。

## 坑

- **criterion 直接跑二进制不带 `--bench` 参数=测试模式**（只打 `Testing <name> / Success`、不做测量、不写 estimates.json）；要测量模式须传任意 `--bench <filter>`。
- **CRITERION_HOME 须在运行时显式隔离两腿**：criterion 把报告目录烤进 build 期的 `option_env!("CARGO_TARGET_DIR")`，直接跑二进制时相对 cwd 解析 `target/criterion`——两腿会互写对方报告（旧 run 的 stale estimates.json 被误读）；用 `CRITERION_HOME=/tmp/crit-base|/tmp/crit-cand` 每腿隔离，路径是 `$CRITERION_HOME/backup_archive/<name>/new/estimates.json`（CRITERION_HOME 即 criterion 根，不再加 `criterion` 层）。
- worktree 共享 target 时 cargo 判 fresh 不重编（旧 mtime 陷阱）——`touch` 差异文件强制重编；两腿产物同路径 hash（元数据派生）判代只能 `cmp` 字节。
- 两腿并发跑同一 script 会交叉污染 tsv/CRITERION_HOME——单跑串行。
