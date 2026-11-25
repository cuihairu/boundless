# BW-10-06-10 实现 accelerateToPoint（逐 tick 比较速度、位置和到达结果）

**验收**：逐 tick 比较速度、位置和到达结果。

## 参照面链

`EntityNavigate::accelerateToPoint`（entity_navigate.cpp:297-327，docstring
"equivalent to calling accelerateAlongPath( [destination], ... )"）：
`!entity_.isRealToScript()` → TypeError 无前缀共享文本；facing 整数
校验 `intFacing >= 0 && intFacing < FACING_MAX`（3）→ 越界 TypeError
**`"Facing must one of the value 0, 1 or 2"`（参照自带 typo，逐字）**；
`new AccelerateToPointController( destination, acceleration, maxSpeed,
facing, stopAtDestination )` → `addController(pController, userArg)` →
id。**无载具检查**（destination 契约=本地空间）。

控制器本体 `AccelerateToPointController` + `AccelerationCore`
（base_acceleration_controller.cpp / accelerate_to_point_controller.cpp）
**BW-08-02-03 已交付**（movement.rs：ramp/制动曲线/面朝分支/
streamIO 全测过），本项补脚本脸 + movement.rs 补 `FACING_MAX = 3`
常量（校验上界）。

## 怪癖（钉死）

1. **facing 校验顺序在 real 检查后**：ghost + 坏 facing → NotReal
   先赢。
2. **facing 值域 0..FACING_MAX（3）**：0=FACING_NONE 不改朝向、
   1=FACING_VELOCITY 朝速度、2=FACING_ACCELERATION 朝加速度；
   报错文本 "must one of" 无 "be"——参照 typo 照抄。
3. **实体 state.velocity 是采样速度，滞后一拍**（newPosition 语义：
   从位置采样差分重算，不是控制器内部 new_velocity）：逐 tick 追踪
   呈 0, 0.2, 0.2, 0.4, 0.4… 的双步 ramp——首拍答 0 是采样窗口使然
   非 bug；位置步长 0.02/0.04/0.04/0.06/0.06…（=前拍速度/10）。
4. **stopAtDestination=false 不进制动曲线**（creation 透传断言钉
   `controller.stop_at_destination`）。

## 验收（实证，velocity 2 / max 2 / hertz 10 / facing 1 / stop true）

- **逐 tick**：v(t) = 0.2·⌊t/2⌋ 至 2.0 封顶（tick 1 v=0、tick 4
  v=0.4、tick 10 v=1.0、tick 20 v=2.0）；x(tick 10)=0.6、
  x(tick 20)=2.2；tick 31 起制动（sqrt(2·d·a) 曲线咬合）、tick 38
  采样速度衰减 v=0.6→0、**tick 39 到达**——位置吸附 (5,0,0)、
  state.velocity == 0、`onMove` 一次（id 5/userArg 3）。
- **facing 0/3/-1**：0 透传 FACING_NONE+stop=false；3 与 -1 →
  `BadFacing`（message 逐字含 typo）；ghost → `NotReal` 无前缀。

## 未盖行（共 10 行，99.56%）

与 06-02..06-09 相同的防御集（行号随插入平移）：
`DestinationUnresolved` 返回 + message 臂 ×2 处、`canNavigateTo`
目的地非法臂、06-01 失败路线永不重入循环守卫 2 行、
`SafetyCountExceeded` 返回 + message 臂。**06-10 新增代码全覆盖**
（`AccelerateToPointFailure` 两 message 臂由拒测击中）。

## 门禁

- `cargo test -p server-runtime`：**1158 过 + 1 忽略**（含 2 条新
  accelerateToPoint 测试，模块共 37）。
- `cargo test --workspace`：**4458 过 / 0 失败**。
- `cargo fmt -p server-runtime -- --check`、
  `cargo clippy -p server-runtime --all-targets -- -D warnings` 全绿。
- 覆盖（llvm-cov 0.9.1 `--lcov`，DA max 合并，`cargo clean -p` 后测）：
  `entity_navigate.rs` **99.56%**（2281/2291）。

证据测试：`crates/server-runtime/src/entity_navigate.rs`
（accelerate_to_point 脸 + AccelerateToPointFailure + 2 测试）；
`crates/server-runtime/src/movement.rs`（FACING_MAX + 控制器本体
既有测试）。
