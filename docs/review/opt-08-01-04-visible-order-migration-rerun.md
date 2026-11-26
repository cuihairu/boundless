# OPT-08-01-04 复跑可见集、事件顺序和迁移

Acceptance: **状态正确**。本行是复跑行（零代码改动）：把可见集、事件顺序、迁移三族在 **OPT-08-01-02 调度改动之后** 的代码上全量复跑——重点是新排序+游标队列与缓存最小值实现下，出队序、消息序与迁移状态仍与记录的参考契约逐条一致。

## 可见集（2026-11-26 本机）

- `witness.rs` **66/66**：AoI enter/leave 生命周期、alias 池分配/归还/守恒、withheld、vehicle 同步、LoD 细节级切换——全部与交付时记录的参考契约一致（含新钉测 `the_dense_area_broadcast_volume_is_pinned`：enter 128 消息/1024B、稳态 tick 64 消息/1024B、迁移波 192 消息/1536B，池守恒零 warning）。
- `vision.rs` **14/14**：proximity/vision 派发、trap_members 台账、可见集两道门（entity_type_faces + witness capacity）。
- `noise.rs` **11/11**：entities_in_x_range 双向 x 链、严格 < 送达、零级无人听。
- `viewer.rs` 查询面 **19/19**：网格 chunk 后端、短读、空输入断连。
- `range_list/proximity` 族 **47/47**：RangeList shuffle 与 face 门。

## 事件顺序

出队序契约在**新实现**上逐条复验：

- `first_observation_and_reentry_produce_identical_messages`：首观察与再入消息逐字节相同（EnterAoI alias 复得）。
- `updates_go_out_in_priority_order_within_the_delta_window`：窗口内严格按优先级序出队。
- `prioritised_vehicles_go_first_and_keep_their_debt`：vehicle 先行且保债务。
- `a_sent_entity_is_clamped_to_the_earliest_queued_priority`：已发实体不落到队尾优先级之past。
- `an_emptied_queue_resets_priorities_to_zero`：排干后全员归零。

排序键 `(priority.to_bits(), id)` 与原 `queue_front` 决胜键逐字一致 → 事件顺序零变化，上述测全绿即证。

## 迁移

- `migrat/offload/teleport` 族 **75/75**（mailbox 更新/目标失联 zombie/可见集迁移面/控制器恢复/乘客与 Space 传送）。
- load crate **156/156**（dense-aoi 场景期望 5、smoke 三通道、cross-cell 存档解析面）。
- 基准迁移面读数（OPT-08-01-02 后）：64 实体迁移波 78.7µs（1.2µs/实体）、offload 决策 ~6.6ns/real——迁移时序与本簇记录一致。

## 状态正确判读

三族全部复跑与记录契约逐条一致；**零代码改动、零回归**。本复跑同时是 OPT-08-01-02 两处调度候选（排序+游标、缓存最小值）的行为验收：1704 lib 全套（含全部 witness 行为测与广播量钉测）在新实现上原值通过。

## 门禁

- `cargo test -p server-runtime --lib`：**1704 passed / 0 failed / 1 ignored**。
- 过滤族：`-- witness` 66/66、`-- vision` 14/14、`-- noise` 11/11、`-- viewer` 19/19、`-- migrat offload teleport` 75/75、`-- range_list proximity` 47/47。
- `cargo test -p load --lib`：**156 passed / 0 failed**（`-- dense` 5/5）。
