# BW-17-03-06 容量与长稳作业 — 基线阈值证据

- **任务**: BW-17-03-06（lane BW-17 负载场景，todo.md 行 2084）
- **验收**: 阈值取自已记录基线。
- **作业面**: 容量与长稳作业本体是 `.github/workflows/load-schedule.yml`（BW-22-03-02：
  `schedule` 每日 02:17 UTC 长稳形 + `workflow_dispatch` 32 clients/600s 容量形，
  都跑 `ci/load.sh`，工作流注释明言「substrate 落地后零改动产出可比数字」）。
  本行交付的是作业的阈值面——此前卡点「容量/长稳阈值需已记录基线——依赖
  BW-17-02-08 长稳实测」随 BW-17-02-08 落地解除。

## 1. 阈值表（取自已记录基线）

`tests/load/baselines.tsv`：每行一条规则 `scenario<TAB>metric<TAB>op<TAB>value<TAB>baseline`，
`baseline` 列引用产生该阈值的已入库夹具（`docs/review/fixtures/load/*.report.json`，
各场景证据文档记录其产生运行）。规则集（7 条）：

| 场景 | 指标 | 带 | 基线 |
|---|---|---|---|
| login-hurricane | login_success_rate | min 1.0 | `login-hurricane.report.json` |
| steady-move | bots_online | min 1 | `steady-move.report.json` |
| dense-aoi | bots | min 1 | `dense-aoi.report.json` |
| cross-cell | bots_set_out | min 1 | `cross-cell.report.json` |
| reconnect | mean_recovery_ticks | min 2.0（离线地板） | `reconnect.report.json` |
| long-stability | resource_sample_count | min 1 | `long-stability.report.json` |
| long-stability | min_online_ratio | min 1.0（soak 不滴断） | `long-stability.report.json` |

只收**包络无关**的不变量（比率与计数在任意 harness 旋钮下成立），同一张带
既卡 PR 短门禁也卡 nightly 大旋钮。**资源序列故意不设带**：驻留大小随机器而变，
一个 runner 上记录的数字不是另一个 runner 的带——该半面由 BW-17-02-08 的报告
条目携带自身指标定义落盘，不做阈值声明。

## 2. 检查通道

`ci/load.sh` 新增 `load-thresholds` 通道（`run_all` 在 `load-run` 之后）：纯函数
`check_one` 用一个 awk 把每份 `ci-out/load/<name>.tsv` 对着基线表核对——
min/max 出带点名 `metric=value` 与带值；规则有而 TSV 无的指标点名
`(metric missing)`；缺基线表时通道 SKIP 点名本行（不虚绿）。缺失表=SKIP、
出带=FAIL、全部在带=PASS，判定文案逐条可读。工作流零改动——`ci/load.sh`
多一条通道，`load-schedule.yml` 的下一次 schedule/dispatch 自动带上。

## 3. 运行证据

PR 形（15s/4 clients）：

```
load-run        PASS  6/6 scenario(s) produced results: load/*.tsv
load-thresholds PASS  6/6 scenario(s) inside the recorded baseline bands
[ci] no failed lane
```

调度形（本机等价 `CI_LOAD_CLIENTS=32 CI_LOAD_DURATION=600`，容量旋钮）：

```
load-thresholds PASS  6/6 scenario(s) inside the recorded baseline bands
```

两个形全在带——「同一张带卡两种作业」的验收声明有本机实测背书。

`bash ci/load.sh self-test` 新增 4 条：在带记录通过、低出带点名
（`login_success_rate=0.5 below the recorded min 1.0`）、高出带点名
（`peak_rss_kib=99999 above the recorded max 2048`，人为违反来验检查器本身）、
缺指标点名（`peak_rss_kib (metric missing)`）。既有 5 条自测不受影响，
`shellcheck -S warning` 与 `actionlint` 双绿。

## 4. 边界

本行交付阈值面与检查通道：基线全部取自已入库夹具（BW-17-02-02..08 的证据运行），
不虚称来自真实多小时 soak——时长形基线（如容量上限曲线、服务端资源带）仍待
跨进程长跑（BW-14/17 卡点）后才有可记录的实测；彼时按本表格式增行即可，
通道无需改动。
