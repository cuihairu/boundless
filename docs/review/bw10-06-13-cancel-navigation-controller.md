# BW-10-06-13 取消活动寻路控制器（取消后下一 tick 停止推进，回调序列与原版一致）

**验收**：取消后下一 tick 停止推进，回调序列与原版一致。

## 参照面链

`Entity.cancel` 的脚本实现 `Controllers::py_cancel`
（controllers.cpp:529-584），参数两分支：

1. **整数** ControllerID → `deleteByID = true`。
2. **字符串** exclusive 类别名 →
   `Controller::getExclusiveID(categoryName, /*createIfNecessary:*/ false)`
   （controller.cpp:498-521）；查不到（0）→ TypeError
   **`"invalid exclusive controller category '%s'"`**。
3. 其他类型 → TypeError
   **`"argument must be a ControllerID or a string"`**。

之后 `container_.find(controllerID)`：命中 → `Controller::cancel()`
（controller.cpp:107-117）→ `pEntity_->delController(controllerID_)`；
未命中且 `deleteByID` → **WARNING_MSG
`"Entity.cancel(%u): Cancelling an unknown controller ID %d\n"`**；
未命中且按类别 → 静默。返回 `Py_RETURN_NONE`。

**exclusive 类别就是控制器 id**：`IMPLEMENT_EXCLUSIVE_CONTROLLER_TYPE(
MoveToPointController, DOMAIN_REAL, "Movement")` 展开成静态
`TypeRegisterer`，构造时 `getExclusiveID("Movement", true)`
（controller.hpp:146-153）——类别 id 分配在 `EXCLUSIVE_CONTROLLER_ID_MASK
(0x7fff)` 之上，永不与 `nextControllerID` 冲突；
`Controllers::addController`（controllers.cpp:307-311）对 exclusive
控制器直接取 `pController->exclusiveID()` 当 controllerID，并先 del 旧的同
类别控制器（一实体一运动）。故 **`Entity.cancel("Movement")` 就是
取消活动寻路的正途**。

`delController`（controllers.cpp:371-428）**先 erase 再 stopReal(true)**：
容器不再持 id，stop 回调里再删也诚实。`MoveController::stopReal`
（move_controller.cpp:190-194）只 `deregisterForUpdate`。

## 怪癖（钉死）

1. **取消不产生任何回调**（moveToPoint docstring："The notification
   methods are not called when movement is cancelled"）——onMove/
   onMoveFailure 只在 `update` 内触发，注销后即永不进入。
2. **未知 id 只告警不报错**：`cancel(99)` 返回 None（`Ok(cancelled:
   false)`），文本逐字含 `\n`。
3. **按类别取消的未命中静默**：类别已注册但控制器已走（如已到达自取消）
   → `deleteByID == false`，无 WARNING 也无 TypeError。
4. **类别解析不创建**：`createIfNecessary = false`，未注册的类别名是
   TypeError 而非「顺手建一个」。
5. 单参数 arity 的 TypeError
   （`"cancel takes exactly 1 argument (%zd given)"`）经此类型化端口
   不可达（Rust 调用恒为单参），仅留档。

## 验收（实证）

- **引擎循环受注册门控**：moveToPoint 走 (10,0,0)、速度 3/hertz 10 =
  0.3/tick；tick 5 内脚本发 `cancel("Movement")` → 下一拍门检查发现注册
  表已空 → `now == 5` 结束、位置停在 1.5（五拍 0.3，未跑第 6 拍）、
  **log 恒空**（无回调）。后续十拍门仍闭，位置与 log 不动。
- **按 id 取消**：注册 id 取消后 `controller(id)` 为 None、位置冻结、
  log 空；随后 `cancel(99)` 答逐字告警
  `Entity.cancel(7): Cancelling an unknown controller ID 99\n`，
  `missed_deletes` 不动（未走 del 的告警臂）。
- **拒绝面**：未注册类别 → `UnknownCategory`，message 逐字
  `invalid exclusive controller category 'Trap'`；其他类型 →
  `BadArgument`，message 逐字 `argument must be a ControllerID or a
  string`。

## 未盖行（controllers.rs 6 行，99.40%）

988/994。未盖 674/748（`insert_if_absent` 的 ghost 域臂与
`update_ghost` 的 ghost_state 回填）、1607-1610（既有
extras「重触返回同一实例」测试的闭包体，永不新建故不落地）——均为
**06-13 之前**的既有行；06-13 新增代码全覆盖。entity_navigate.rs
**99.61%**（2587/2597），未盖恰是 06-02..06-12 的 10 行防御集
（319/603/717/936/1042/1310/1311/1323/1643/1644，行号随插入平移）。

## 门禁

- `cargo test -p server-runtime`：**1166 过 + 1 忽略**（含 4 条新测试：
  2 条 py_cancel 单测 + 2 条验收）。
- `cargo test --workspace`：**4466 过 / 0 失败**。
- `cargo fmt -p server-runtime -- --check`、
  `cargo clippy -p server-runtime --all-targets -- -D warnings` 全绿。
- 覆盖（llvm-cov 0.9.1 `--lcov`，DA max 合并，`cargo clean -p` 后测）：
  `entity_navigate.rs` **99.61%**、`controllers.rs` **99.40%**。

证据测试：`crates/server-runtime/src/controllers.rs`（`CancelArg` /
`CancelFailure` / `CancelOutcome` + `Controllers::py_cancel` + 2 单测）；
`crates/server-runtime/src/entity_navigate.rs`（2 条验收测试）。