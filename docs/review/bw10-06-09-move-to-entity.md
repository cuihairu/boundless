# BW-10-06-09 实现 moveToEntity（目标实体移动时，跟随结果和停止条件与原版一致）

**验收**：目标实体移动时，跟随结果和停止条件与原版一致。

## 参照面链

`EntityNavigate::moveToEntity`（entity_navigate.cpp:228-247）：脚本脸与
moveToPoint 同形——`!entity_.isRealToScript()` → TypeError
`"This method can only be called on a real entity."`（无前缀共享文本）；
`new MoveToEntityController( destEntityID, velocity, range,
faceMovement, moveVertically )` 后 `entity_.addController( pController,
userArg )` 返回 controllerID。

控制器本体 `MoveToEntityController`（move_controller.cpp:466-612）
**BW-08-02-03 已交付**（movement.rs：首拍 recalcOffset 定随机朝向整
range 偏移、None 目标 onMoveFailure 自取消、close_enough 先于迈步、
到达时 faceMovement 精确朝向目标），本项只补脚本脸。

## 怪癖（钉死）

1. **无载具检查 + 无前缀 TypeError**：同 moveToPoint（复用
   `NavigateFailure::NotReal`）。
2. **停止条件量的是 raw 目标，不是偏移接近点**：update 里
   `remaining = target - position`、`close_enough = remaining.length()
   < range` 先于 `dest_local = target + offset` 的迈步——朝
   target+offset 走的实体，一旦**自身**与目标距离落进 range 就停，
   落点可远在偏移环内测（实证：朝 (11,0,0) 走、目标 (10,0,0)、
   range 1.0，从 9 迈到 10 后下拍 remaining 0 < 1 停在目标上，
   非 range 整距）。
3. **offset 是首拍一次性随机朝向、整 range 出**（`rand() % 360` →
   端口 `rng % 360` 钉测）：rng 0 → 朝 +x 整 range。
4. **close_enough 到达拍不迈步**：实体原位，仅
   faceMovement 精确朝向目标 + onMove（实证 (0.5,0,0)/range 1.0 →
   位置零位移、yaw=π/2）。
5. **目标消失 → onMoveFailure 自取消**（不是 onMove）。

## 验收（实证）

velocity 10 / hertz 10 = 1.0 per tick，range 1.0，rng 0：

- **跟随移动目标**：目标 (5,0,0)，两拍后挪到 (10,0,0)——tick 1
  x=1.0、tick 2 x=2.0（朝 target+offset=(6,0,0)），tick 3 x=3.0
  （重定向朝 (11,0,0)），tick 10 x=10.0，tick 11 close_enough 臂停
  在目标上（remaining 0 < range），onMove 一次（id 6/userArg 7），
  共 11 拍。
- **已在 range 内**：目标 (0.5,0,0) 首拍 close_enough——位置零位移、
  yaw 精确 π/2、onMove。
- **目标消失**：`None` → onMoveFailure（id 6/userArg 3）、自取消、
  位置不动。
- **ghost** → `Err(NotReal)` 无前缀逐字。

## 未盖行（共 10 行，99.53%）

与 06-02..06-08 相同的防御集（行号随插入平移）：
`DestinationUnresolved` 返回 + message 臂 ×2 处、`canNavigateTo`
目的地非法臂、06-01 失败路线永不重入循环守卫 2 行、
`SafetyCountExceeded` 返回 + message 臂。**06-09 新增代码全覆盖**。

## 门禁

- `cargo test -p server-runtime`：**1156 过 + 1 忽略**（含 3 条新
  moveToEntity 测试，模块共 35）。
- `cargo test --workspace`：**4456 过 / 0 失败**。
- `cargo fmt -p server-runtime -- --check`、
  `cargo clippy -p server-runtime --all-targets -- -D warnings` 全绿。
- 覆盖（llvm-cov 0.9.1 `--lcov`，DA max 合并，`cargo clean -p` 后测）：
  `entity_navigate.rs` **99.53%**（2137/2147）。

证据测试：`crates/server-runtime/src/entity_navigate.rs`
（move_to_entity 脸 + 3 测试）；
`crates/server-runtime/src/movement.rs`（控制器本体既有测试）。
