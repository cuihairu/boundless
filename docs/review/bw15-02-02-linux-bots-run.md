# BW-15-02-02 跑 Linux Bots 与离线工具 — 运行证据

- **任务**: BW-15-02-02（lane BW-15 Linux/Windows/macOS 原生支持，todo.md 行 1782）
- **验收**: 样例输出和负载报告有效。
- **运行面**: Linux x86-64 本机，`apps/bots`（`LOAD_DURATION`/`LOAD_CLIENTS`/`LOAD_TARGET`/`LOAD_REPORT` 契约，见 `apps/bots/src/main.rs` 文档头）。
- **场景**: `tests/load/scenarios/` 下三例（`steady-move.xml`/`login-hurricane.xml`/`dense-aoi.xml`），`ci/load.sh` 三通道（load-scenarios/load-driver/load-run）全 PASS（budget 15s、4 clients：3/3 场景出 `metric<TAB>value` 结果，落 `ci-out/load/*.tsv`）。
- **口径**: 固定种子（steady-move 314159 / login-hurricane 20261123 / dense-aoi 271828），同一环境两次运行输出逐字节相同（md5 全等），与 `ci/load.sh` 通道跑出的 trajectory_sum（396.2326…）一致，可比性成立。

## 1. 样例输出（metric<TAB>value）

`steady-move`（固定种子轨迹，双跑 md5 全等；与 `ci/load.sh` 通道预算跑出的 396.2326… 同值）：

```
bots_online	4
sample_period	0.1
trajectory_sum	396.2326774597168
```

`login-hurricane`（登录洪峰，4 号登录）：

```
login_attempts	4
login_admitted	4
login_refused	0
login_success_rate	1
online_peak	4
online_final	4
```

`dense-aoi`（密集 AOI，密度 0.04）：

```
bots	4
density	0.04
avg_visible	3
max_visible	3
```

## 2. 负载报告（ScenarioReport JSON，schema 1）

`LOAD_REPORT=<path>` 写入机器可读报告（`ScenarioReport::record` + `write_json`）。逐份样例（完整文件入库
`docs/review/fixtures/load/<name>.report.json`）：

```json
{
  "schema": 1,
  "scenario": "login-hurricane",
  "generator": "load 0.1.0",
  "topology": "linux-offline-harness",
  "seed": 20261123,
  "values": {
    "login_attempts": 4,
    "login_admitted": 4,
    "login_refused": 0,
    "login_success_rate": 1,
    "online_peak": 4,
    "online_final": 4
  }
}
```

三份报告均含 schema/scenario/generator/topology/seed/values 六字段，metrics 与 stdout 逐值一致。

## 3. 离线工具半

「与离线工具」以 2026-11-25 已闭环的 EL7 工具互通为证（见 `bw15-02-05-el7-baseline.md` §3.5-3.7）：
`process_defs` 完整 defs dump 已复现（digest `18D00DC69AC756433D091F537DA7030E`，5 实体），
本仓输出与 EL7 dump 深比较逐结构相等；`res_packer` 产物 magic 逐字节同、`PackedSection` 回读与源 XML
逐树相等（永久测试 `an_el7_res_packer_file_reads_back_identical_to_its_xml_source`）。

## 4. 边界

本行只覆盖 **Linux 单机**运行面与离线工具证据；三端（BW-15-03/04）与跨端组合（BW-15-05-04/05）
不在本行。bots 跑在离线面（`topology: offline`）：真实多机拓扑与跨端登录链路属 BW-14-02-01/BW-15-05-04 的卡点。
