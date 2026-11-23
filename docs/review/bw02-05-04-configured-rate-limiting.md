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