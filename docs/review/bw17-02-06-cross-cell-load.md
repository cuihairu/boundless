# BW-17-02-06 跨 Cell 场景 — 穿越成功率运行证据

- **任务**: BW-17-02-06（lane BW-17 负载场景，todo.md 行 1269）
- **验收**: 实现跨 Cell 场景，报告穿越成功率。
- **场景**: `tests/load/scenarios/cross-cell.xml`（固定种子 `161803`，800 并发/80 ramp/10s 预热/540s 窗口，`CrossBot` 账号池）+ 可执行包装 `cross-cell.sh`（`ci/load.sh` 契约）。
- **运行面**: Linux x86-64 本机 `apps/bots`（离线拓扑 `linux-offline-harness`），`LOAD_DURATION`/`LOAD_CLIENTS`/`LOAD_TARGET`/`LOAD_REPORT` 契约。

## 1. 实现面

`crates/load/src/cross_cell.rs`（`pub mod` 已挂进 `crates/load/src/lib.rs`）：
`run_cross_cell` 用真实 `load::Bot` 的移动面把每个 bot 从西侧偏移 `x=-300` 移到东侧
`x=300`；cell 边界在 `x=0`，逐 tick 记录每个 bot 的 `crossed_at_tick`，
`CrossingLedger::crossing_success_rate()` = 完成穿越的 bot 数 / 出发 bot 数。
固定种子固定每个 bot 的速度抽样，因此账本可逐位复现。

`smoke.rs` 新增 `SmokeRun::CrossCell` 分派与指标臂，发出三个指标：
`bots_set_out`、`crossings_completed`、`crossing_success_rate`。
`ScenarioReport` JSON schema 1 含 schema/scenario/generator/topology/seed/values 六字段。

## 2. PR 短负载门禁

`CI_LOAD_DURATION=15 bash ci/load.sh`（4 clients）：

```
load-scenarios PASS  tests/load/scenarios carries 4 executable scenario(s)
load-driver   PASS  apps/bots crate present
load-run      PASS  4/4 scenario(s) produced results: load/*.tsv
[ci] no failed lane
```

15 秒预算下 `ci-out/load/cross-cell.tsv` 记录 `bots_set_out=4`、
`crossings_completed=0`、`crossing_success_rate=0` —— 预算不足以走完 600 单位
跨度，正是指标要报告的失败情形；单元测 `the_cross_cell_metrics_report_the_success_rate`
钉住该短窗口行为。

## 3. 完整窗口样例输出与报告

`LOAD_DURATION=60 LOAD_CLIENTS=4`（同一场景、固定种子，两次独立运行，
stdout TSV 与 `ScenarioReport` JSON 的 md5 全等：

`docs/review/fixtures/load/cross-cell.metrics.tsv`：

```
bots_set_out	4
crossings_completed	4
crossing_success_rate	1
```

`docs/review/fixtures/load/cross-cell.report.json`：

```json
{
  "schema": 1,
  "scenario": "cross-cell",
  "generator": "load 0.1.0",
  "topology": "linux-offline-harness",
  "seed": 161803,
  "values": {
    "bots_set_out": 4,
    "crossings_completed": 4,
    "crossing_success_rate": 1
  }
}
```

双跑 md5：`b0dd39b2f27884d3c611ffd773cf6310`（TSV）、
`c6e279e0ac7cdc463de5251dd29b5b11`（JSON），逐字节相同。

## 4. 单元验证

`cargo test -p load`：155 通过 / 0 失败（含新增 5 条 cross-cell 单测 +
saved-kind 复现测 + 指标测）。`cargo clippy -p load --all-targets` 无新增告警
（3 条 pre-existing `expect(&format!)` 位于既有 `saved()`/`find()` 测试辅助函数）。

## 5. 边界

本行只覆盖离线负载面（与 BW-15-02-02 同一 harness）——真实多 cell 服务端
AoI 移交链路属 BW-10-07-05/BW-14 卡点；本场景复现的是穿越成功率指标，
不声称服务端范围集已验证。
