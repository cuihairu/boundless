# BW-10-06-14 迁移活动寻路控制器（移动中穿越 Cell 边界后，目标位置和回调次数与原版一致）

**验收**：移动中穿越 Cell 边界后，目标位置和回调次数与原版一致。

## 参照面链（offload 序）

真正的跨界序列——**`stopRealControllers` 不在 offload 路径上**：

1. **离开侧**：`RealEntity::writeOffloadData`
   （real_entity.cpp:501-566）→ `pControllers_->writeRealsToStream`：
   控制器**仍在注册表里**被流化（`lastAllocatedID_`、real 域计数、逐个
   id +（real-only 时）type+userArg + `writeRealToStream` 载荷）。
   `MoveController::writeRealToStream`（move_controller.cpp:161-166）=
   `metresPerTick_, faceMovement_, moveVertically_`；
   `MoveToPointController` 追加 `destination_, destinationChunk_,
   destinationWaypoint_`（:239-243）——**目标位置随流跨界**。
2. **到达侧**：`readRealControllersFromStream`（entity.cpp:1907）→
   `Controllers::readRealsFromStream`（controllers.cpp:82-120）：real-only
   控制器**重建**（type+userArg+读态），ghost 半按 id 复用；
   **`startReal` 被注释掉**（:118-121，"called later with a call to
   startRealControllers"）。
3. **实体转 Real 后**：`Entity::startRealControllers()`
   （entity.cpp:4867，"now that pReal_ has been set"）→
   `startReals()` → 每个real 域控制器 `startReal(false)`（非初始）→
   `registerForUpdate`，走步继续。

`entity_.stopRealControllers()`（real_entity.cpp:250）在
**`RealEntity::~RealEntity`** 里（isDestroyed()→true 的 final stop），
与 offload 无关——**交接本身对控制器零动作**（无 stop、无回调、记录
保留在离开侧）。`MoveController::stopReal/startReal` 只
deregister/register update（move_controller.cpp:178-194）。

## 怪癖（钉死）

1. **交接不停控制器**：offload 只 write→read→start；离开侧记录保留
   （测试断言 `leaving.controller(1).is_some()`）。
2. **跨界零回调**：onMove 只在 update 内触发；start/stop 只是
   register/deregister。无迁移与有迁移的回调总数同为 1。
3. **实体位置不是控制器状态**：控制器流里没有 position——轨迹连续性
   由实体位置流（writeOffloadData 的实体段）保证，控制器只带
   metresPerTick/两旗标/destination。
4. **到达 tick 不变**：port 实证 10 拍跨界后续跑，第 34 拍到达 = 无
   迁移对照的第 34 拍。

## 验收（实证）

- `Controllers::migrate_real_controllers`（write→read→start 序）
  + 单测 `migrating_the_active_walk_keeps_its_destination_and_callbacks`：
  moveToPoint (10,0,0) v3/hz10 走 10 拍 → 记录 real_state =
  `write_real_to_stream` 字节 → `migrate_real_controllers` 到新
  Controllers → 离开侧记录仍在、到达侧重建 real-only（userArg 9、
  DOMAIN_REAL）、`StartReal{initial:false}` 收尾；从流读回
  `destination == (10,0,0)` 精确、metresPerTick 0.3、两旗标保留；
  实体位置跨界零位移；续跑第 34 拍到达 = 对照、终位同、回调同为
  单条 `onMove{id:1,user_arg:9}`。

## 未盖行（controllers.rs 6 行，99.40%）

996/1002。未盖 693/767/1626-1629 与 06-13 前完全同源（ghost 域
`insert_if_absent` 臂、`update_ghost` 回填、extras 重触闭包体），
行号随插入平移；06-14 新增代码全覆盖。entity_navigate.rs
**99.63%**（2679/2689），未盖恰是 10 行防御集（319/603/717/936/
1042/1310/1311/1323/1644/1645）。

## 门禁

- `cargo test -p server-runtime`：**1167 过 + 1 忽略**（含 1 条新迁移
  验收）。
- `cargo test --workspace`：**4467 过 / 0 失败**。
- `cargo fmt -p server-runtime -- --check`、
  `cargo clippy -p server-runtime --all-targets -- -D warnings` 全绿。
- 覆盖（llvm-cov 0.9.1 `--lcov`，DA max 合并，`cargo clean -p` 后测）：
  `entity_navigate.rs` **99.63%**、`controllers.rs` **99.40%**。

证据测试：`crates/server-runtime/src/controllers.rs`
（`Controllers::migrate_real_controllers`）；  
`crates/server-runtime/src/entity_navigate.rs`（迁移验收测试）。