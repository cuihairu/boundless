# OPT-08-01-01 记录密集 AOI 基线

Acceptance: **保存 Tick、广播量和迁移耗时**。本行交付密集 AOI 的结构化广播量钉测 + 三面基准（tick / 迁移 / 跨边界 offload 决策），读数为 2026-11-26 本机热机值（负载 9-14，不入带）。

## 广播量（结构性，`the_dense_area_broadcast_volume_is_pinned` 钉测）

64 实体、间距 5 密集网格、预算 2 Mbit/s@10Hz（`max_packet_size`=24972 字节/pass，整个队列每 pass 排干=密集最坏面）：

| 面 | 每实体消息 | 每实体 wire 字节 | 构成 |
|---|---:|---:|---|
| 首次观察（enter cycle） | 2 | **16** | EnterAoI(8) + CreateEntity(8)；首个实体拿 alias 254 |
| 稳态 tick（全员移动） | 1 | **16** | EntityUpdate = alias(1) + PackedXZ 相对坐标(3) + YawPitchRoll(12) |
| 迁移波（全体出界再入） | 3 | **24** | LeaveAoI(8) + 再 EnterAoI(8) + CreateEntity(8) |

64 实体合计：enter 128 消息/1024 字节、稳态 tick 64 消息/1024 字节、迁移波 192 消息/1536 字节。钉测另锁：迁移波后 alias 池守恒（持 64 个互异 alias、池恰余 191、无一耗尽）、全程零 warning。任何改动的带宽代价从此有对照。

## Tick / 迁移 / 决策耗时（`dense_aoi` bench，2026-11-26 本机）

| 面 | 64 | 512 | 说明 |
|---|---:|---:|---|
| `tick` | 185.5 µs（345 Kelem/s） | 4.57 ms（112 Kelem/s） | 全员移动 + 全队排干的 update pass；含 harness 喂移动（快照克隆+upsert）成本 |
| `migration` | 163.6 µs（391 Kelem/s） | 6.18 ms（82.8 Kelem/s） | 整波 leave→re-enter→create 三 flush |
| `offload_decision` | 619 ns（103 Melem/s） | 3.43 µs（149 Melem/s） | `collect_offloads` 跨 x=100 边界决策扫，恰好半数迁移 |

诚实判读：
- **tick 与 migration 每实体成本超线性增长**（345→112 Kelem/s，~3×），与实现的 O(queue) 全队列扫形一致——`queue_front`/`queue_pop` 是线性找前缀（`Vec` 无堆不变式），这正是 OPT-08-01-02（AOI 索引与 Witness 调度评估）的候选收益所在。
- **offload 决策扫本身便宜**（~6.6ns/real @512，BSP 两叶），迁移的时延大头在 witness 侧的 leave/re-enter 波（每实体 3 消息 + 三遍队列扫），不在决策面。
- 广播量面是协议地板（参考路由语义：每移动实体恰一条 16 字节更新、每迁移恰 24 字节），由钉测锁死；减量必丢更新或丢迁移事件。
- 本机读数、负载 9-14 下采集，只作相对量级；跨机不可比（钉测的结构数字才是跨机契约）。

## 门禁

- `cargo test -p server-runtime --lib`：**1704 passed / 0 failed / 1 ignored**（新钉测含内）。
- `cargo clippy -p server-runtime --lib --benches -- -D warnings`：0 error。
- `cargo fmt --check`：净。
- `cargo test -p load --lib`：156 passed / 0 failed（场景面不动）。

## 坑

- 密集首次观察/迁移的 re-enter alias **不是** 254：64 实体批量进出后 alias 池的归还序跟队列堆向量游走序走（swap_remove 形），单实体才复得原 alias——钉测只锁池守恒与互异，不锁个体 alias 值。
- 密集网格 tick 的实体位置漂移会被 PackedXZ ±500 打包限幅翻成 Detailed 投影（12 字节），基准里用往返位移（±0.25 ping-pong）把投影钉在 Relative 面。
- witness 的 AoI 成员是 harness 决定的（add_to_aoi 由调用方给），witness 本体不按半径裁剪——密集基准的「全员在 AoI」是显式构造的最坏面。
