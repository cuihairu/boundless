# OPT-15-01-01 采集各平台 I/O 及定时精度

Acceptance: **统一负载下产出报告**。行保持 `- [ ]`（验收是三端矩阵，本席只交 Linux 腿实测——同 BW-15-01 先例：Linux 实测+其余腿命名卡点，不虚报三端）。

## 交付面

- `crates/platform/tests/io_timer_baseline.rs` — 单测试四面，**统一负载**（512 ops、64 KiB payload、热身 64 ops 排除冷缓存）：
  1. 顺序写/读（std fs）
  2. `file::atomic_replace`（write+fsync+rename——配置链的持久化路径）
  3. mmap 读（页缓存热=服务器稳态）
  4. `clock::monotonic_now_micros` 分辨率 + 1 ms sleep 节拍保真（±10% 断言，粗时钟在此现形）
  5. `wakeup::Wakeup` trigger→consume 往返（tick 循环骑的 1 字节戳）
- 断言全是数据面（往返字节、映射内容、分辨率>0、节拍在界内）；墙钟读数只入报告不入断言（机器相关）。
- `PLATFORM_IO_OUT=<path>` 把 `metric<TAB>value` 落 TSV；夹具 `docs/review/fixtures/opt-15-01-01/io-timer-linux.tsv`。

## Linux x86_64 基线（2026-11-26 本机，512 ops @ 64 KiB）

| 面 | 每操作 | 判读 |
|---|---:|---|
| 顺序写 | 30.33 µs | 页缓存热写 |
| 顺序读 | 9.04 µs | 页缓存热读 |
| atomic_replace | 55.81 µs | 比裸写贵 ~1.8×：fsync 是大头（持久化代价） |
| mmap 读一遍 | 506.11 µs | 512 遍求和防优化消除；单遍 ~1 µs 量级 |
| 时钟分辨率 | 1 µs | `CLOCK_MONOTONIC` 微秒粒度 |
| 1 ms sleep 均值 | 1.0864 ms | +8.6%，调度粒度内（断言界 ±10%…1.6 宽容） |
| wakeup 往返 | 0.95 µs | 每次唤醒约 1 µs，千次/tick 无感 |

## 三端状态

- **Linux x86_64**：上表，实测。
- **Windows / macOS**：同一测试三端可跑（platform 各模块已有 cfg(windows)/cfg(darwin) 臂），但两腿无 runner——`windows-gnu` 全量编译卡 mercury 无 Windows 臂（BW-15-03 留白），macOS 卡 darwin SDK。待 BW-15-05 三端 runner 矩阵点亮后各腿跑同测试写各自 TSV。

## 复跑

```sh
PLATFORM_IO_OUT=/tmp/io.tsv cargo test -p platform --test io_timer_baseline -- --nocapture
```

## 门禁

- `cargo test -p platform --test io_timer_baseline` 1 passed / 0 failed。
- `cargo fmt --all -- --check` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo test --workspace` 全绿后推（见交付 commit）。

## 坑

- mmap 视图内容是**最后一次** atomic_replace 的字节（交替 payload 下最后 i=511 是 0x5A），求和断言按最终内容算，不是各轮混合——首跑 expected 按交替积算错即被断言纠正。
- `foundation::time::Stamp` 无 `Sub` 实现，做差用 `.micros()` 提 u64。
- 测试里 `Record::add` 先 println 后 push，避免 String move 后再借。
