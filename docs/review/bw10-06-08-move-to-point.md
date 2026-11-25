# BW-10-06-08 实现 moveToPoint（逐 tick 对照位置、终点容差和到达回调）

**验收**：逐 tick 对照位置、终点容差和到达回调。

## 参照面链

`EntityNavigate::moveToPoint`（entity_navigate.cpp:156-172）：脚本脸只有
一查一造——`!entity_.isRealToScript()` → TypeError
`"This method can only be called on a real entity."`（**无前缀**的共享
文本，与 navigate 不同）；`new MoveToPointController( destination, "",
0, velocity, faceMovement, moveVertically )` 后
`entity_.addController( pController, userArg )` 返回 controllerID
（`Script::getData( controllerID )`）。

控制器本体 `MoveToPointController`（move_controller.hpp:73-91 /
move_controller.cpp:245-303）**BW-08-02-03 已交付**（movement.rs，
构造器/update/writeRealToStream/readRealFromStream + 轨迹测试），本项
只补脚本脸。

## 怪癖（钉死）

1. **无载具检查**：navigate/navigateStep 都拒载具，moveToPoint 不查——
   docstring 契约是「destination 是实体本地空间（在载具上时即载具
   坐标）；要全局坐标先下载具」。
2. **TypeError 文本无 `Entity.moveToPoint:` 前缀**：共享的
   `"This method can only be called on a real entity."`（端口复用
   `NavigateFailure::NotReal`，其 message 本就是这条无前缀文本）。
3. C++ 构造器的 `""` / `0` 是 Controller 基类 name/userArg 占位，真实
   id/userArg 由 `addController(pController, userArg)` 赋——端口直接
   在 `MoveToPointController::new(id, user_arg, ...)` 落位（06-01
   navigate 脸先例：调用方供给 id 与 hertz 建模 addController 分配与
   注册节拍）。

## 验收（实证）

- **逐 tick 位置**（velocity 3 / hertz 10 = 0.3 per tick，
  (0,0,0)→(10,0,0)）：tick 1 x≈0.3、tick 2 x≈0.6、tick 10 x≈3.0、
  tick 33 x≈9.9；**终点容差** = 剩余距离落进一个步长内当拍吸附——
  tick 34 吸附到最后 0.1 → x≈10.0，共 34 tick；到达拍 `onMove`
  一次（controllerId 4 / userArg 9）、实体离地
  （`is_on_ground=false`，faceMovement=true 抬升既有）。
- **容差近点**：目的地 (0.2,0,0) 本就在一步长内 → 第一拍即吸附到
  0.2 且 `onMove`。
- **ghost** → `Err(NotReal)`，message 无前缀逐字。

## 未盖行（共 10 行，99.51%）

与 06-02..06-07 相同的防御集（行号随插入平移）：
`DestinationUnresolved` 返回 + message 臂 ×2 处、`canNavigateTo`
目的地非法臂、06-01 失败路线永不重入循环守卫 2 行、
`SafetyCountExceeded` 返回 + message 臂。**06-08 新增代码全覆盖**。

## 门禁

- `cargo test -p server-runtime`：**1153 过 + 1 忽略**（含 3 条新
  moveToPoint 测试，模块共 32）。
- `cargo test --workspace`：**4453 过 / 0 失败**。
- `cargo fmt -p server-runtime -- --check`、
  `cargo clippy -p server-runtime --all-targets -- -D warnings` 全绿
  （`move_to_point` 8 参再触 too_many_arguments，self 计入，按 06-01
  先例补 `#[allow]`）。
- 覆盖（llvm-cov 0.9.1 `--lcov`，DA max 合并，`cargo clean -p` 后测）：
  `entity_navigate.rs` **99.51%**（2029/2039）。

证据测试：`crates/server-runtime/src/entity_navigate.rs`
（move_to_point 脸 + 3 测试）；
`crates/server-runtime/src/movement.rs`（控制器本体既有测试）。
