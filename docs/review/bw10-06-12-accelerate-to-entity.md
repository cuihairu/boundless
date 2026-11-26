# BW-10-06-12 实现 accelerateToEntity（目标移动及销毁时的控制器结果与原版一致）

**验收**：目标移动及销毁时的控制器结果与原版一致。

## 参照面链

`EntityNavigate::accelerateToEntity`（entity_navigate.cpp:421-456）：两
查按序——`!entity_.isRealToScript()` → TypeError 无前缀共享文本 →
facing 越界 → TypeError **`"Facing must one of the value 0, 1 or 2"`**
（参照自带 typo）。全过后
`new AccelerateToEntityController( destinationEntity, acceleration,
maxSpeed, range, facing )` → `addController(pController, userArg)` →
id。**无载具检查、无 range 校验**；与 accelerateToPoint 同样只有两查
（端口用 type alias `AccelerateToEntityFailure =
AccelerateToPointFailure` 落实共享）。

控制器本体 `AccelerateToEntityController`
（accelerate_to_entity_controller.cpp:31-199）**BW-08-02-03 已交付**
（movement.rs：首拍整 range 随机偏移、恒 stop-at-destination、
close_enough 先于迈步且量 raw 目标、目标消失 onMoveFailure），本项只
补脚本脸。

## 怪癖（钉死）

1. **两查序**：real 先于 facing（ghost + 坏 facing → NotReal 赢）。
2. **close_enough 量 raw 目标、先于迈步**（同 06-09）：朝
   target+offset 走的实体，自身与 raw 目标距离一落进 range 即停——
   实证：起点 0、目标 (5,0,0)→tick 20 挪 (10,0,0)、range 1.0、
   rng 0（offset +x 整 range）、acc 2/max 2/hz 10——追到 x=**9.2**
   停（remaining 0.8 < 1.0，**未到偏移接近点 (11,0,0)**）、faceMovement
   精确转向目标、单条 `onMove`（id 5/userArg 7）。
3. **目标销毁 → `onMoveFailure` 自取消**（非 onMove），实体位置不动
   ——首拍即 None 也如此（offset 已算，无副作用）。
4. **恒 stop-at-destination**：路径脸/点脸有 stop 开关，entity 脸没
   有——制动曲线永远参与。

## 验收（实证）

- **目标移动**：mid-chase 重定位后重定向，tick 内 close_enough 停
  x=9.2（range 1.0 内、remaining 0.8），onMove 一次，自取消。
- **目标销毁**：`None` → onMoveFailure（id 5/userArg 3）、位置零位
  移、自取消。
- **拒绝**：facing 3 → `BadFacing`（typo 逐字）；ghost → `NotReal`
  无前缀逐字。

## 未盖行（共 10 行，99.60%）

与 06-02..06-11 相同的防御集（行号随插入平移）：
`DestinationUnresolved` 返回 + message 臂 ×2 处、`canNavigateTo`
目的地非法臂、06-01 失败路线永不重入循环守卫 2 行、
`SafetyCountExceeded` 返回 + message 臂。**06-12 新增代码全覆盖**
（type alias 复用的两条 message 臂已由 06-10/06-12 拒测击中）。

## 门禁

- `cargo test -p server-runtime`：**1162 过 + 1 忽略**（含 2 条新
  accelerateToEntity 测试，模块共 41）。
- `cargo test --workspace`：**4462 过 / 0 失败**。
- `cargo fmt -p server-runtime -- --check`、
  `cargo clippy -p server-runtime --all-targets -- -D warnings` 全绿。
- 覆盖（llvm-cov 0.9.1 `--lcov`，DA max 合并，`cargo clean -p` 后测）：
  `entity_navigate.rs` **99.60%**（2494/2504）。

证据测试：`crates/server-runtime/src/entity_navigate.rs`
（accelerate_to_entity 脸 + AccelerateToEntityFailure alias + 2 测试）；
`crates/server-runtime/src/movement.rs`（控制器本体既有测试）。
