# BW-17-02-08 长稳场景 — 资源使用保存证据

- **任务**: BW-17-02-08（lane BW-17 负载场景，todo.md 行 2051）
- **验收**: 保存压测进程资源使用。
- **场景**: `tests/load/scenarios/long-stability.xml`（固定种子 `173205`，500 并发/25 ramp/30s 预热/7200s 窗口，`LongBot` 账号池 1000）+ 可执行包装 `long-stability.sh`（`ci/load.sh` 契约）。
- **运行面**: Linux x86-64 本机 `apps/bots`（离线拓扑 `linux-offline-harness`），`LOAD_DURATION`/`LOAD_CLIENTS`/`LOAD_TARGET`/`LOAD_REPORT` 契约。

## 1. 实现面

`crates/load/src/long_stability.rs`（`pub mod` 已挂进 `crates/load/src/lib.rs`）：
`run_long_stability` 把 fleet 带入 Online 后整个窗口 circle-walk 不滴断（soak），
按确定性 tick 计划驱动真实 `crate::resource::ResourceTracker` 读自身 procfs
（`/proc/self/stat` CPU ticks + `/proc/self/status` VmRSS）：每窗口 8 个样本，
stride 取窗口的八分之一，窗口长度不整除时第 8 个样本落在末 tick。
`LongStabilityLedger::resource_report_entries()` 产出 BW-17-03-04 报告契约的条目：
采样周期、指标定义（`CPU_METRIC_DEF`/`RSS_METRIC_DEF`）、CPU 两序列、RSS 序列与峰值。

**可复现契约的分界**：负载面（在线曲线、tick 数、采样计划）是模拟时间，双跑
逐位相等；资源序列是 wall-clock 本体——同一固定种子的两遍采样的是同一个计划
（周期与条数相同）而非同一组 procfs 数字。所以 smoke 的 stdout 指标只携带
确定面（`bots_online`/`ticks`/`sampling_period_secs`/`resource_sample_count`/
`min_online_ratio`），`ledger` 的 `PartialEq` 也只覆盖计划与长度；墙钟数字
经 `apps/bots` 落进 `LOAD_REPORT` 的 `ScenarioReport`（非字节可复现，按设计）。

## 2. PR 短负载门禁

`CI_LOAD_DURATION=15 bash ci/load.sh`（4 clients）：

```
load-scenarios PASS  tests/load/scenarios carries 6 executable scenario(s)
load-driver   PASS  apps/bots crate present
load-run      PASS  6/6 scenario(s) produced results: load/*.tsv
[ci] no failed lane
```

`ci-out/load/long-stability.tsv`（150 tick = 15s 窗口，stride 18 → 8 个样本、
周期 1.8s）：

```
bots_online	4
ticks	150
sampling_period_secs	1.800000072
resource_sample_count	8
min_online_ratio	1
```

## 3. 完整窗口样例输出与报告

`LOAD_DURATION=60 LOAD_CLIENTS=4`（600 tick，stride 75 → 8 个样本、周期 7.5s）：

`docs/review/fixtures/load/long-stability.metrics.tsv`（stdout 确定面）：

```
bots_online	4
ticks	600
sampling_period_secs	7.5
resource_sample_count	8
min_online_ratio	1
```

`docs/review/fixtures/load/long-stability.report.json`（`LOAD_REPORT` 落盘，
schema 1；资源半即验收要保存的压测进程资源使用）：

```json
{
  "schema": 1,
  "scenario": "long-stability",
  "generator": "load 0.1.0",
  "topology": "linux-offline-harness",
  "seed": 173205,
  "values": {
    "bots_online": 4, "ticks": 600, "sampling_period_secs": 7.5,
    "resource_sample_count": 8, "min_online_ratio": 1,
    "cpu_user_secs_series":   [0.1, ..., 0.1],
    "cpu_system_secs_series": [0.05, ..., 0.05],
    "rss_kib_series":         [3436, ..., 3512],
    "peak_rss_kib": 3512,
    "cpu_metric_def": "utime+stime from /proc/self/stat, clock ticks scaled by _SC_CLK_TCK",
    "rss_metric_def": "VmRSS from /proc/self/status, KiB"
  }
}
```

（系列各 8 点，完整文件入库。stdout 指标两跑 md5 全等——确定面；
资源数值不逐位可复现，这是墙钟的本性，报告条目携带其定义可溯源。）

## 4. 单元验证

`cargo test -p load`：168 通过 / 0 失败（含 6 条 long-stability 单测 +
saved-kind 复现测含全部 6 个已交付 kind + 指标测）。钉住面：8 样本/窗口的
计划与周期、soak 保持 1.0 持有率、空窗口（<1 tick）不落任何系列且峰值/CPU
停在地板、FD 预算收缩。`an_empty_window_saves_no_series` 把
`Duration::from_millis(1)` 压成 0 tick。

## 5. 边界

本行只保存离线负载面的压测进程资源使用（与 BW-15-02-02 等同一 harness）：
真实多小时 soak 的服务端资源属 BW-14/17 跨进程卡点；BW-03-01 的离线 soak
已用同一 tracker 契约。报告里资源序列是生成器进程自身的 procfs，不声称
服务端内存/CPU 已验证。
