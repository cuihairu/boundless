# OPT-08-01-02 评估 AOI 索引与 Witness 调度

Acceptance: **报告候选收益**。基线（OPT-08-01-01）已定位热点：tick/migration 每实体成本超线性（345→112 Kelem/s），根因是 witness 调度面的两处 O(n²) 形——update pass 每次出队做全队列线性找前缀（`queue_front`+`position`），`add_to_seen` 每次入队折叠加全队列求最小。本行交付两处候选实现 + 交错钉核 A/B 实测收益。

## 候选（`30ffc54`，行为保持）

1. **update pass 单次排序+游标**：pass 内队列只缩不增（所有优先级突变都落在已出队的 id 上），一次按 `(priority.to_bits(), id)` 排序即完全复现逐次线性找前缀产生的 min-first 出队序（全序：同优先级按 id 决胜，确定性不变）；出队推进游标，pass 尾 `drain(..cursor)`。每 pass O(n²)→O(n log n)。
2. **add_to_seen 缓存队列最小值**：`queue_min: Option<f64>`，push 只能压低最小值故 fold（O(1)）；任何出队/移除/队内优先级上升处失效为 None（drain 尾、flush swap_remove、CREATE_PENDING 的 update_priority），下次入队扫描重算一次。入队批 O(n²)→O(n)。

行为网：`queue_front` 的决胜键与排序键逐字一致；40+ witness 行为测（含 1704 lib 全套）与 OPT-08-01-01 广播量钉测全绿——出队序、消息序、优先级语义零变化。

## A/B 实测（taskset -c 6 交错 6 轮，双独立编译二进制 cmp 判代，2026-11-26）

| 面 | base 中位 | cand 中位 | 收益 | 轮胜 | 轮间比值 |
|---|---:|---:|---:|---:|---|
| `tick_64` | 126.3 µs | 31.0 µs | **4.10×** | 6/6 | 2.81–4.21 |
| `tick_512` | 4.76 ms | 0.29 ms | **16.33×** | 6/6 | 15.38–16.66 |
| `migration_64` | 164.0 µs | 78.7 µs | **2.08×** | 6/6 | 1.54–2.41 |
| `migration_512` | 6.37 ms | 0.71 ms | **8.97×** | 6/6 | 8.21–10.34 |
| `offload_decision_64`（未改，对照） | 693 ns | 668 ns | 1.04 | 2/6 | 0.89–1.59 |
| `offload_decision_512`（未改，对照） | 3.46 µs | 4.16 µs | 0.83 | 2/6 | 0.59–1.25 |

判读：四个被改面全部 6/6 胜且最小轮收益 1.54×（migration_64）；两个**未改面**（offload 决策扫）方向混杂、包络 ±60%——本轮共享钉核上的噪声尺度对照。tick/migration 的收益全部落在噪声包络之外，判真。

## 报告结论（候选收益）

- **AOI 索引**：`collect_offloads` 的 BSP 决策扫本身便宜（~6.6ns/real @512，基线读数），索引面无可报告收益；密集区的索引热点不在空间索引而在 witness 调度。
- **Witness 调度**：两处候选已实测落地（上表），密集最坏面（512 全员在 AoI）tick 16.3×、迁移波 9.0×。残余成本在每实体两次 EntitySnapshot 克隆（`send_queue_element`+update 循环，借引用拆分所致）——量级次之，留作后续候选，本行不动。
- 广播量面为协议地板（OPT-08-01-01 钉测），本行不碰消息形状，改后钉测原值通过。

## 门禁

- `cargo test -p server-runtime --lib`：**1704 passed / 0 failed / 1 ignored**。
- `cargo clippy -p server-runtime --lib --benches -- -D warnings`：0 error。
- `cargo fmt --check`：净。

## 坑

- `cargo build --release --bench` 的产物不在 `target/release/deps/`——工作区 build.rs 把 bench 二进制复制进 `target/release/build/server-runtime/<hash>/out/`，且 hash 两树相同（元数据派生非代际指纹），判代只能 `cmp` 两份 out/ 二进制。
- 对照面（未改代码）在共享钉核上也会读出 0.59–1.59 的轮间比值——判收益必须 6/6 同向且越出对照包络，不能只看中位比值。
