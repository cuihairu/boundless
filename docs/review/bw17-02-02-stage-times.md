# BW-17-02-02 实现爬升、预热和持续时间

- **任务编号**: BW-17-02-02（lane BW-17 Bots 压测，todo.md 行 1921）
- **验收**: 执行器输出各阶段时间。
- **结论**: 满足并勾行。`crates/load` 新增执行器阶段计时模型（`Executor` + `StageTimeline`）：
  给定上一项已交付的 `Scenario`（并发量/爬升速率/预热/持续时间），`Executor::stages()` 输出
  各阶段时间——预热（无负载）、爬升（至峰值的斜坡）、稳态（峰值倒计）、总时长；并附爬升曲线
  `active_bots_at` 供执行器逐秒调度。阶段时间全部为纯函数计算，逐值断言通过。

**参考锚点**: 原版 Bots（`server/tools/bots/`）是固定 bot 集合，无「爬升/预热」概念；本项的阶段
计时是**压测新设计**（任务书 BW-17 头：场景含「爬升速率、预热及持续时间」）。消费的
`Scenario` 字段在 BW-17-02-01（`crates/load/src/scenario.rs`）已交付，本项只做计时。

## 1. 阶段计时模型

`crates/load/src/executor.rs`：

| 输出 | 语义 | 公式 |
|---|---|---|
| `warmup` | 预热期，无负载 | `Scenario.warmup` |
| `ramp` | 爬升期，bot 增至峰值 | `ceil(concurrency / rampRate)` 秒（`rampRate=0` 或空峰值 → 0） |
| `steady` | 稳态期，峰值倒计 | `duration - ramp`（`saturating_sub`，爬升长于持续 → 0） |
| `total` | 全程 | `warmup + duration` |
| `active_bots_at(t)` | t 秒时活跃 bot 数 | `t<=warmup → 0`；`t≥warmup+ramp → concurrency`；其间 `min((t-warmup)·rampRate, concurrency)` |

`Executor::new(scenario)` 持有场景；`Executor::stages()` 返回 `StageTimeline`。执行器据此逐秒
调度：先预热、再按 `rampRate`/秒 加到 `concurrency`、稳态保持至 `duration` 结束。

## 2. 验收映射

「执行器输出各阶段时间」＝ `Executor::stages()` 返回各阶段 `Duration`（warmup/ramp/steady/total）。
不变量：`ramp + steady == total - warmup`（即爬升与稳态覆盖整个负载窗口）；非整除爬升向上取整，
保证斜坡末端恰达峰值；`rampRate=0` 为瞬时爬升（`ramp=0`，负载窗整段为稳态）。

## 3. 实测

```
$ cargo test -p load
test result: ok. 24 passed; 0 failed   (lib: scenario 16 + executor 8)
test result: ok. 1 passed; 0 failed    (tests/scenarios.rs::every_saved_scenario_loads)
```

- 新增 8 个单元测试（executor）：阶段逐值断言（100 并发/10 爬升/5 预热/30 持续 →
  预热 5s/爬升 10s/稳态 20s/总 35s）；全程覆盖不变量；瞬时爬升；爬升曲线过界
  （预热末 0、爬升中段 30、峰值 100）；爬升长于持续→稳态饱和 0；非整除爬升向上取整
  （10 bot/3 每秒 → 4s）；空峰值无需爬升；`Executor::stages()` 与其场景一致。
- 行覆盖 99.01%（TOTAL 405/409 行，函数 58/58=100%，门禁 ≥98%）；executor.rs 自身 100%。
- `cargo fmt --check` 净；`cargo clippy -p load --all-targets` 本 crate 0 警告
  （`resources` 依赖既有 23 警告属上游，非本 crate）。
- workspace 全绿（`cargo test --workspace` exit 0）；`test-support` 层门禁
  `workspace_rules_hold` 绿（`load` 已登记 LAYER_DOMAIN，本项未新增依赖）。

## 4. 边界

本项交付**阶段计时模型**（验收即「输出各阶段时间」）。把真实 bot 驱动过这些阶段
（连接、登录、移动、断线）属 BW-17-01 Bot 运行时，需客户端连接层 + 活服务端，不在本项；
结果聚合（BW-17-03）亦不在本项。本项不触碰服务端/客户端/mercury，不生成任何压测结果。
