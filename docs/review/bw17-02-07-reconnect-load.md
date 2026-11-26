# BW-17-02-07 断线重连场景 — 恢复时长运行证据

- **任务**: BW-17-02-07（lane BW-17 负载场景，todo.md 行 2048）
- **验收**: 统计恢复时长。
- **场景**: `tests/load/scenarios/reconnect.xml`（固定种子 `141421`，200 并发/20 ramp/10s 预热/300s 窗口，显式账号池 `ReconnectBot1..4`）+ 可执行包装 `reconnect.sh`（`ci/load.sh` 契约）。
- **运行面**: Linux x86-64 本机 `apps/bots`（离线拓扑 `linux-offline-harness`），`LOAD_DURATION`/`LOAD_CLIENTS`/`LOAD_TARGET`/`LOAD_REPORT` 契约。

## 1. 实现面

`crates/load/src/reconnect.rs`（`pub mod` 已挂进 `crates/load/src/lib.rs`）：
`run_reconnect` 先把 `concurrency` 个 bot 带入 Online（每个分配 entity/space），
然后在窗口前半段按种子排定给每个 bot 投递一次 `feed_logged_off(Some(true))`
——人格脚本的「留待重连」决定（`client_app.cpp:1193-1235`）。每个 bot 在
断线事件被 tick 应用后立即 `log_on()` 重新发起，服务器一个 mini-tick 后回
`feed_log_on_result`，`RecoveryTrack` 记录 `dropped_at_tick`、`relogin_at_tick`
与 `back_online_at_tick`。`ReconnectLedger::mean_recovery_ticks()` 是恢复时长
统计量（完成恢复的 bot 的「回线 tick − 掉线 tick」均值）；窗口太短时恢复
未落地，该均值为 0.0 且 `recoveries_completed < bots_dropped`。

`smoke.rs` 新增 `SmokeRun::Reconnect` 分派与指标臂，发出四个指标：
`bots_dropped`、`recoveries_completed`、`mean_recovery_ticks`、`sample_period`。

## 2. PR 短负载门禁

`CI_LOAD_DURATION=15 bash ci/load.sh`（4 clients）：

```
load-scenarios PASS  tests/load/scenarios carries 5 executable scenario(s)
load-driver   PASS  apps/bots crate present
load-run      PASS  5/5 scenario(s) produced results: load/*.tsv
[ci] no failed lane
```

15 秒预算下 `ci-out/load/reconnect.tsv` 记录 `bots_dropped=4`、
`recoveries_completed=4`、`mean_recovery_ticks=2`、`sample_period=0.1` ——
掉线→重发→服务器回复各占一个 mini-tick，这是离线面的最短恢复时长，
即本场景要统计的数。

## 3. 完整窗口样例输出与报告

`LOAD_DURATION=60 LOAD_CLIENTS=4`（同一场景、固定种子，两次独立运行，
stdout TSV 与 `ScenarioReport` JSON 的 md5 全等）：

`docs/review/fixtures/load/reconnect.metrics.tsv`：

```
bots_dropped	4
recoveries_completed	4
mean_recovery_ticks	2
sample_period	0.1
```

`docs/review/fixtures/load/reconnect.report.json`：

```json
{
  "schema": 1,
  "scenario": "reconnect",
  "generator": "load 0.1.0",
  "topology": "linux-offline-harness",
  "seed": 141421,
  "values": {
    "bots_dropped": 4,
    "recoveries_completed": 4,
    "mean_recovery_ticks": 2,
    "sample_period": 0.10000000149011612
  }
}
```

双跑 md5：`1928199a0cefdb9a58d61d399b7b64ad`（TSV）、
`2bfd8bbc567949ef1070ad7ee32c60d6`（JSON），逐字节相同。

## 4. 单元验证

`cargo test -p load`：161 通过 / 0 失败（含新增 5 条 reconnect 单测 +
saved-kind 复现测 + 指标测）。`a_short_window_strands_the_recovery` 钉住
「200ms 窗口容不下一个回复 → 恢复未落地」的失败臂；
`a_refused_admission_shrinks_the_fleet` 钉住 FD 预算收缩行为。

## 5. 边界

本行只统计离线负载面的恢复时长（与 BW-15-02-02/BW-17-02-06 同一 harness）：
真实服务端断线检测与会话迁移属 BW-14/17 跨进程卡点；本场景把
`ClientApp` 的掉线事件→重发→回线精确建模，不声称真实网络故障下的
恢复时长已验证。
