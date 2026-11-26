# OPT-26-01-01 固定事件回放并采集查询基线

Acceptance: **retain the import, query and memory data**（保留导入、查询及内存数据）。行勾账带本交付 hash，交付即勾（三端面不适用：本行落点是 `message_logger` 查询面，三端 runner 不适用——平台无关的纯内存查询面）。

## 交付面

- `crates/message_logger/tests/qlog_replay_baseline.rs` —— 一套固定事件回放，逐面记录导入/查询/内存基线。
- **固定事件集**（确定性，无 RNG）：20 000 条 entry，4 组件（id 1..=4，host/pid/appid/apptype/username/name 各派生自 id）、8 格式串（`i%8`，含唯一词 `flush`×2/唯一词 `delta`）、4 类别（id `(i%4)+1`）、时间 `secs=1_000_000+i/1000, msecs=i%1000`（严格递增）、priority `i%4`、source `i%2`（CPP/SCRIPT）、args 16 字节 `(i+j·7)&0xFF`、metadata 每 8 条留 1 条空。
- **导入面**：`push_entry` 全量回放，计数台账（alloc 数/字节/peak-live）。
- **保留面（数据断言）**：entry 数==N；首/中/末三条逐字段+args 字节+metadata 字段精确；每条查询的幸存数为生成函数闭式（全滤 N、单 severity N/4、pid/appid/procs N/4、script N/2、combat N/4、include `flush` N/4、exclude `delta` N−N/8、backwards N 序反转、post-interpolate 与 raw 字面一致）。
- **查询面（墙钟记录、不断言）**：11 张查询参数面（all/severity0/pid2/appid3/procs2/source_script/category_combat/include_flush/include_flush_rerun/exclude_delta/backwards/post_interpolate_flush），每面记 µs+幸存数+alloc 数+peak-live。
- **样本报表面**：`sample_report`（severity×category 计数，first/last 两个四分之一窗口），各 4 cell/总 5000 —— OPT-26-01-04 复算对照基线。
- 记录 sink：`MESSAGELOG_QLOG_OUT=<path>` 落 `metric<TAB>value` TSV（60 行）。

## Linux x86_64 基线（2026-11-26 本机，N=20000）

| 面 | µs | 幸存 | 备注 |
|---|---:|---:|---|
| 导入 | 57 161 | 20 000 | 349 891 entries/s；alloc 37 514/字节 6 261 816/peak-live 4 730 906 B |
| 全滤查询 | 219 222 | 20 000 | alloc 175 015/peak-live 10 039 898 B（结果字符串分配为主）|
| severity0 | 136 008 | 5 000 | |
| pid2 | 139 868 | 5 000 | |
| appid3 | 225 485 | 5 000 | |
| procs2 | 139 810 | 5 000 | |
| source_script | 156 420 | 10 000 | |
| category_combat | 133 070 | 5 000 | |
| include_flush ×2 | 150 199/145 943 | 5 000 | 两次独立运行同幸存同序=查询确定性 |
| exclude_delta | 339 435 | 17 500 | 单 pass 扫描全 log+正则=最贵面 |
| backwards | 同 all | 20 000 | 首行=末 entry 时间 |
| post_interpolate_flush | 同 include | 5 000 | 字面格式下 raw/插值同数 |

## 复跑

```sh
MESSAGELOG_QLOG_OUT="$PWD/docs/review/fixtures/opt-26-01-01/qlog-replay-baseline.tsv" \
    cargo test -p message_logger --test qlog_replay_baseline -- --nocapture
```

## 门禁

- `cargo test -p message_logger --test qlog_replay_baseline` 1 passed/0 failed。
- `cargo fmt --all -- --check` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo test --workspace` 全绿后推（见交付 commit）。

## 坑

- 计数分配器三连坑（沿用 `mercury/tests/buffer_alloc.rs` 形状 + 扩展）：①台账 map 自身存储经本分配器增长会重入活跃 borrow → map 变动必须在 `LEDGERING` 门内（汞同）；②`try_with` 在进程 teardown 期 TLS 销毁中失败 → 全部记账走 `try_with`+`unwrap_or` 兜底，销毁期透明转 System，否则 dealloc 阶段 AccessError abort；③`live==0` 作「TLS 已毁」哨兵要排除零长分配，否则 size-0 的 System 伪 alloc 会吞掉后续真实记账。
- `window` 的 base 含建 log 已有活结构（format/component/category 表），footprint 是 base 之上的 peak，故不要求窗口间台账排空。
- exclude 面期望数按「唯一词 `delta` 只在一个格式串」算 N−N/8，不是 N−N/4（初跑断言错被测试纠正）。
- `cargo test` 把测试二进制的 CWD 设为 **package 根**（`crates/message_logger`），不是 workspace 根——仓库相对的输出路径会 NotFound；复跑命令用 `$PWD` 绝对化，`flush()` 侧补 `create_dir_all(parent)` 兜底。
