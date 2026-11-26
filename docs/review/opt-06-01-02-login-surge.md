# OPT-06-01-02 优化登录排队及会话接续

Acceptance: **在登录洪峰场景比较尾延迟**。本行交付登录洪峰基准面 + retry 队列排水优化。

## 交付面

- `crates/load/src/fleet.rs` — `botsForLogOnRetry_` 的排水从「每条到期 bot 单独一次 `Vec::remove`」改为**单遍分区**：一次 `std::mem::take` 遍历把到期 bot（`log_on_retry_time() < now`）直接 `log_on()` 后推回 `bots`，未到期的收进 waiting 向量一次归还。旧形状在洪峰（多条 retry 同刻到期）是 O(n²)，新形状 O(n)；幸存顺序与重登顺序都跟随队列原序，固定 seed 下派遣序不变。
- `crates/load/src/fleet.rs` tests — 新测 `a_surge_drains_due_retries_in_queue_order_and_keeps_the_rest`：5 条 staggered retry 同刻到期，钉住「到期 4 条按队序重登 + 未到期 1 条留队」。
- `crates/load/benches/login_surge.rs` + `[[bench]] login_surge` — criterion 基准：`parked(64/512)` 条 bot 全部同刻到期，单 tick 排水；`Throughput::Elements` 标注队列长度。测的是排水机制本身，不冒充墙钟登录 RTT（离线面是模拟时间）。
- `crates/load/src/smoke.rs` — 顺手清三处预存 `expect(&format!)` clippy `expect_fun_call`（--benches 开启才浮出的被遮 lint）。

## A/B 对照（2026-11-26 本机，taskset -c 6 钉核，交错 6 轮）

二进制身份：baseline=HEAD 提交树（旧 remove-in-loop）独立编译，candidate=工作树（分区排水）独立编译，`cmp` 验字节不同；同 bench 源两处编译。

| 面 | baseline 中位 | candidate 中位 | 比 |
|---|---:|---:|---:|
| `drain_64` | 13.93 µs | 7.39 µs | **0.530**（1.9×） |
| `drain_512` | 776.47 µs | 80.43 µs | **0.104**（9.65×） |

每轮 candidate 全部优于同轮 baseline（drain_512 基线 718–807µs、候选 70–104µs 无一交叉），效应远超轮内噪声——512 条同刻到期的洪峰正是旧形状的最坏情形，量级与 O(n²)→O(n) 预测相符。

## 门禁

- `cargo test -p load`：156 lib + 1 scenarios + 1 空 target 全绿（新测含 surge 分区序）。
- `cargo clippy -p load --lib --benches --tests -- -D warnings`：0 error（含顺手清的三处被遮 lint）。
- `cargo fmt -p load -- --check`：净。
- 固定 seed 场景面：`login-hurricane` 等已保存场景的 ledger `PartialEq` 双跑一致不受影响（fleet tick 语义不变，只换排水实现）。

## 坑

- **测试里 `feed_log_on_failure(Some(0.0))` 是「不排程」**：retry time 0.0 表示 bot 直接丢弃（`a_zero_retry_failure_drops_the_bot` 的契约），surge 测试的 staggered 时间必须全部 > 0。
- criterion 输出按 `[low median high unit]` 解析时单位分隔在 median 之后（`µs` 单独 token），python 解析器第一次按 bracket[2] 取到的是数字、bracket[3] 才是单位。
- `--benches` 开启才编译 bench target 的 dev-deps 路径，被遮 lint（expect_fun_call）在 `--lib` 单独跑时不可见——clippy 门禁要带 `--tests`。
