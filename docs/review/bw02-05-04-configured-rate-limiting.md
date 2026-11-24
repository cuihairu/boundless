# BW-02-05-04 实现配置限流 — 复核证据

- **任务编号**: BW-02-05-04（todo.md 行 433）
- **验收**: 边界流量下的处置与原版吻合。
- **Boundless 模块**: `crates/mercury/src/network_interface.rs`（限流状态、配置接口、
  `increment_and_check_rate_limit` 与 `process_packet` 首臂）
- **平台**: Linux x86_64

## 原版形状

限流是 `NetworkInterface` 自身的一份周期计数，配置由外部（watcher）写入：

| 原版位置 | 形状 |
|---|---|
| network_interface.hpp:242-259 | `rateLimitPeriod()` / `perIPAddressRateLimit()` / `perIPAddressPortRateLimit()` 及三个 setter |
| network_interface.cpp:936-951 | `rateLimitPeriod( float )`：先 `rateLimitTimer_.cancel()`、再 `clearRateLimitCounts()`、赋值，最后当 `!isZero(p) && p > 0` 时 `addTimer(int64(p * 1e6), this, NULL, "NetworkInterface::rateLimitPeriod")`（**循环**计时器） |
| cpp:957-964 | `clearRateLimitCounts()`：两张计数 map 清空，两个平均器的累计值归 0 |
| cpp:970-1004 | `incrementAndCheckRateLimit( addr )`：`isZero(period)` 直接 return false；`perIPAddress > 0` 时以 **端口清零** 的地址（`Address( addr.ip, 0 )`）为键计数，超过阈值则累加 `ipAddressRateLimitAverage_` 并置 `shouldRateLimit`；`perIPAddressPort > 0` 时以完整地址为键计数，同理 |
| cpp:540-546 | `handleTimeout`：两个 `AccumulatingEMA::sample()`（`shouldReset = true`）后 `clearRateLimitCounts()` |
| packet_receiver.cpp:445-455 | 接收入口**第一**臂：`incrementAndCheckRateLimit( addr )` 为真则 verbose 打印后返回 `REASON_GENERAL_NETWORK` |

关键边界：**阈值判定是 `++count > limit`**，所以恰好 `limit` 个包通过、第 `limit+1`
个被丢；计数**每包自增**（被丢的包也计入），故超限后地址会一直被丢，直到周期
结束；两个限制**独立**计数、独立累加平均，任一超限即丢。

平均器构造参数取 `EMA::calculateBiasFromNumSamples( 5 )`（默认权重 0.95，
cpp:102/105），端口直接复用 `foundation::math::ema` 的
`Ema::calculate_bias_from_num_samples(5.0, 0.95)` 与 `AccumulatingEma<u32>`。

## 边界流量的处置

`process_packet` 首臂（packet_receiver.cpp:445-455）置于 `findChannel` **之前**，
且**不带 `isExternal` 门**——内部与外部接口都限流。因此：

- 超限地址的数据报回答 `Reason::GeneralNetwork`，**已注册通道的包也一并被丢**
  （测试 `a_channelled_address_is_limited_before_its_channel_is_found` 钉死：第二包
  返回 `GeneralNetwork` 且 sink 只收到第一包）；
- 未超限地址不受影响，其计数照旧自增；
- 周期计时器每次触发：把两个累计值采样进各自的 `AccumulatingEMA`
  （`avg = (1-bias)*accum + bias*avg`，bias≈0.4507197，`1-bias`≈0.5492803），
  再把计数清零，地址重新可以从 0 计数。

## 有意偏离

1. **周期字段用 `f64`，`isZero` 用精确 0.0 判定**。原版字段是 `float`，其
   `isZero`（stdmf_minimal.hpp:136 + `float` 的 `isEqual` 特化）是
   `FLT_EPSILON` 级判定；限流周期是人工秒数，实际只会遇到默认值 0 与正数，
   端口以 `== 0.0` 判定开关，并在 `set_rate_limit_period` 里保留原版的
   `!isZero && > 0` 双条件形式。
2. **循环计时器在析构时取消**。原版只在 `rateLimitPeriod( float )` 里 cancel，
   对象析构后循环计时器仍指向已删除的 `this`（依赖计时器先于接口销毁）。
   端口改为在 `Drop` 里一并 cancel：回调本身经 `Weak<NetworkInterface>` 升级，
   升级失败即空转，取消后也不再空转。
3. **`rateLimitedIPAddresses_` 用 `BTreeMap`**（原版 `BW::map` 同样有序）；
   键是端口清零后的地址（`Address::new( addr.ip, 0 )`），与原版一致。
4. **watcher 树（`rateLimit/*`，cpp:1207-1228）不移植**——端口无 watcher 框架；
   同等信息经 `rate_limit_period()` / `per_ip_address_rate_limit()` /
   `per_ip_address_port_rate_limit()` / `rate_limit_averages()` 暴露
   （`rate_limit_averages` 对应 `rateLimit/stats/perIPAddressAverage` 与
   `.../perIPAddressPortAverage`，前者在前）。

## 门禁（本项）

| 门禁 | 结果 |
|---|---|
| `cargo test -p mercury` | **434 passed / 0 failed**（本项 +7，上项 427） |
| `cargo clippy -p mercury --all-targets -- -D warnings` | 0 finding（仅工作区既有 `python-host` 提示） |
| `rustfmt --edition 2024`（本批 1 个文件） | 0 diff（幂等） |
| `cargo llvm-cov --package mercury --lib --summary-only` | TOTAL 行覆盖 **98.76%**（≥98% 达标） |

新增测试（7 个，全部锚定原版行号）：

1. `rate_limiting_starts_disabled` — 三项默认值（0 / 0 / 0），8 个包全部通过，
   平均器保持 0；
2. `the_per_ip_address_limit_drops_over_the_boundary` — 限 3，同 IP 四个不同
   端口：前三个 `Success`，第四个 `GeneralNetwork`，sink 恰 3；
3. `the_per_ip_address_port_limit_counts_each_port_separately` — 限 2，同一完整
   地址第三包被丢，另一端口另起一桶仍通过；
4. `the_period_timer_samples_the_averages_and_clears_the_counts` — 先丢一个包，
   推进一个周期后 `process_timers`，地址端口平均器恰为 `(1-bias)*1`
   （用 `calculate_bias_from_num_samples(5.0, 0.95)` 自算，不写魔数），计数清零后
   地址重新通过；
5. `both_limits_trip_and_accumulate_separately` — 两个限制同开，先由每地址桶
   触发，采样后只有每地址平均器有值；
6. `a_channelled_address_is_limited_before_its_channel_is_found` — 已注册通道的
   第二包被丢（首臂在通道查找之前），sink 只 1 个；
7. `setting_the_period_to_zero_disables_and_cancels` — 归零后包恢复通过，且推进
   一个周期 `process_timers()` 返回 0（循环计时器已取消）。

## 状态

已验收：限流配置（周期 + 两个阈值）经 setter 生效，`process_packet` 首臂按原版
顺序与阈值语义（`++count > limit`）处置边界流量——恰好 `limit` 个通过、第
`limit+1` 个以 `Reason::GeneralNetwork` 丢弃、已注册通道的包同样被丢、两桶独立
计数独立累加平均、周期结束时采样并清零。边界流量下的处置与原版吻合。

## 回归与恢复（2026-11-24）

上表的交付在 `f2ef112`（`merge: land lane/bw20-28`）的冲突解决里丢了。该合并把
`network_interface.rs` 解到 1541 行（取 bw20-28 侧），而合并前 main 侧（`3cb67c1`）
是 3346 行：内嵌限流字段、`increment_and_check_rate_limit` 与 `process_packet`
首臂一并消失，`lib.rs` 的 `pub mod rate_limiter;` 也随之撤掉，只剩
`ba5fc38` 留下的 `rate_limiter.rs` 孤儿文件（`RateLimiter` 类型全仓零引用、
不参与编译、10 个验收测试从未运行）。

恢复方式：不再手工移植，而是把限流**落回它自己的模块**并接回接收路径——

| 改动 | 内容 |
|---|---|
| `lib.rs` | 加回 `pub mod rate_limiter;`（1 行），孤儿模块恢复编译 |
| `network_interface.rs` | 新增字段 `rate_limiter: Rc<RateLimiter>`，构造时以 `RateLimiter::new(dispatcher.clone())` 建（周期计时器落在接口自己的 dispatcher 上）；六个转发面 `rate_limit_period` / `set_rate_limit_period` / `per_ip_address_rate_limit` / `set_per_ip_address_rate_limit` / `per_ip_address_port_rate_limit` / `set_per_ip_address_port_rate_limit` + `rate_limit_averages`；`process_packet` 首臂按原版顺序咨询限流器 |

接回后本项的验收测试是**接收路径级**的五条（锚定原版行号，pinned clock 驱动周期计时器）：

1. `rate_limiting_starts_disabled` — 三项默认 0/0/0，8 个包全部 `Success`，
   平均器 0；
2. `the_per_ip_address_limit_drops_the_packet_over_the_boundary` — 限 3，同 IP 四
   个不同端口：前三个 `Success`、第四个 `GeneralNetwork`，sink 恰 3；
3. `the_per_ip_address_port_limit_counts_each_address_separately` — 限 2，同一完整
   地址第三包被丢，另一地址另起一桶仍通过；
4. `the_period_expiry_clears_the_counts_and_samples_the_average` — 超限后推进一个
   周期 `process_timers()`：地址重新通过，per-IP 平均器恰为 `(1-bias)*2`
   （用 `calculate_bias_from_num_samples(5.0, 0.95)` 自算，不写魔数）；
5. `a_channelled_address_is_limited_before_its_channel_is_found` — 已注册通道的
   第二包被丢（首臂在通道查找之前）。

`rate_limiter.rs` 自带的 9 条单元验收测试同时恢复运行（默认关闭、零周期关闭、
边界恰为预算、IP 桶共享与地址桶独立、周期到期清零、超预算计入平均器、换周期清
账并重挂、归零停限、析构取消计时器）。

### 门禁（恢复后）

| 门禁 | 结果 |
|---|---|
| `cargo test -p mercury --lib` | **380 passed / 0 failed**（恢复前 366：+5 接口级 +9 限流器） |
| `cargo clippy -p mercury --all-targets` | 0 finding（仅工作区既有 `asset-pipeline` / `blowfish` manifest 提示） |
| `rustfmt --edition 2024 --check`（本批 2 个文件） | 0 diff |
| `cargo test --workspace` | **exit 0，3757 passed / 0 failed** |
| `network_interface.rs` 行覆盖 | **98.81%**（999/1011） |
| `rate_limiter.rs` 行覆盖 | **99.57%**（234/235） |

注意：对 `lib.rs` 跑 rustfmt 会递归格式到它声明的全部模块——本次曾把
9 个无关模块的格式债一起改掉，已 `git checkout` 回滚，只保留本批 2 个文件。

### 仍未恢复的相邻面（同一次合并所丢，不在本项范围）

`f2ef112` 同时丢了 `interface_table` / `set_interface_table` /
`fragmented_bundles` / `receive_stats` / `p_off_channel_filter` /
`set_p_off_channel_filter` / `process_filtered_packet` 七个 `pub` 面，
分别对应 BW-02-02 / BW-02-03 / BW-02-04-03 的已勾条目，待另立项恢复。
其中只有 `send_bundle` → `udp_channel.rs` 属正常落位；`is_dead` →
`recently_dead_channels.rs` 与 `set_should_use_checksums` → `packet_sender.rs`
**不是**落位——那两个文件自身就是孤儿（未声明、不参与编译），故这两面同样
丢失。真实丢失面数为 **18**，不是 8。

更大的事实：`crates/mercury/src/` 下有 **18 个孤儿 `.rs` 文件**（约 300 KB
已交付代码）存在于磁盘却在 `lib.rs` 未声明，因此从不编译、自带测试从不
运行：`encryption_stream_filter`、`exposed_message_range`、
`initial_connection_filter`、`irregular_channels`、`message_frame`、
`packet_receiver_stats`、`packet_sender`、`recently_dead_channels`、
`reliable_order`、`symmetric_block_cipher`、`tcp_bundle_dispatch`、
`tcp_channel_lifecycle`、`tcp_connection_header`、`tcp_frame`、`tcp_send`、
`tcp_send_buffer`、`tcp_transport_tests`、`udp_bundle_processor`。
即 docs-architecture 车道已登记的「mercury 19 孤儿模块」。

排查法：把 `lib.rs` 的 `pub mod X;` 清单与 `src/` 下的 `.rs` 文件求差集。
判定一个「搬家」是否真成立的唯一判据是定义它的模块**已声明**——
`grep -rn 'fn is_dead' src/` 命中不等于可用，还得 `grep -n 'mod <name>' lib.rs`
也为命中。

全部恢复是一个项目（18 个模块、每个自带测试面，文件每模块约 +2500 行），
不是单个 todo 条目；不要拆零恢复，`network_interface.rs` 会被撑到数千行且
每步都要重跑全部门禁。