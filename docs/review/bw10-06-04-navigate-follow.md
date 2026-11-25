# BW-10-06-04 实现 navigateFollow（目标移动后跟随轨迹对照）

**验收**：目标移动后跟随轨迹对照。

## 参照面链

`EntityNavigate::navigateFollow`（entity_navigate.cpp:872-922，参照
docstring 已标 deprecated、建议 navigateStep）——**全部实现在脚本脸，
没有系统面**：

1. `isEqual( maxSearchDistance, -1.f )` → `CellAppConfig::maxAoIRadius()`
   （先于一切检查）。
2. `!PyObject_IsInstance( pEntity, &Entity::s_type_ )` → TypeError
   `"parameter 1 must be an Entity instance"`。
3. `pOtherEntity->isDestroyed()` → ValueError
   `"cannot follow a destroyed entity"`。
4. 空间不同 → ValueError `"followed entity is not in the same space"`。
5. `almostZero( maxSearchDistance )`（stdmf.hpp:616，默认 ε=0.0004，
   `-ε < f && f < ε`）→ ValueError `"maxSearchDistance cannot be 0.0"`。
6. 偏移点：`totalYaw = 目标自身 yaw + angle`，
   `offset = ( distance·sin(totalYaw), 0, distance·cos(totalYaw) )`，
   `position = 目标位置 + offset`。
7. **委托 navigateStep(position, velocity, maxMoveDistance,
   maxSearchDistance, faceMovement, girth, userArg)**——无自己的控制器
   创建；拒绝与返回值（控制器 id）原样透传。

## 验收（slope 资产，05-05/06 字节）

- **跟随轨迹**：跟随者在 (1,0.5,1)（wp1），目标 (1,0.5,1) yaw 0，
  distance 1.0/angle 0 → 偏移点 (1,0.5,2)（wp1 内）→ 同 waypoint 短路
  → 控制器目的地 (1,1.5,2)、id=4；0.2 m/tick 走完，`onMove{7,4,42}`。
- **目标移动后**：目标移到 (1.5,0.5,1.5) 再调一次 → 复用臂只
  setDestination：id 仍 4、userArg 仍 42，新目的地 (1.5,1.5,2.5)；
  走完第二段，第二条 `onMove`——两条回调同控制器 id，轨迹连续。
- **偏移几何**：angle π/2、distance 0.5、目标 yaw 0 → 偏移点
  (1.5,0.5,1) → 目的地 (1.5,1.5,1)；目标改朝 yaw π 后同一脚本角 →
  偏移点 (0.5,0.5,1) → 目的地 (0.5,1.5,1)。**totalYaw = 目标 yaw +
  脚本 angle**，目标自身朝向旋转偏移方向。
- **拒绝面（按参照顺序）**：非 Entity → TypeError 文本逐字（先于限长
  检查，0.0 限长配非 Entity 仍报 Entity 错）；已销毁/异空间 → 两条
  ValueError 文本逐字；限长 0.0003（< ε）→
  `"maxSearchDistance cannot be 0.0"`；限长 0.0005（> ε）通过检查后
  **绑定集合内搜索** → 委托臂 NoPath；ghost → 委托臂 NotReal（跟随脸
  自己不查 real，最后才由 navigateStep 拒）。

## 怪癖（钉死）

1. **无系统面**：navigateFollow 全在脚本脸，是 navigateStep 的薄包装
   （目标实体四查 + 偏移几何）。
2. **几乎零限长的检查在四查之后**：-1 先换成 maxAoIRadius，显式 0.0
   才会被 ε 拒（换成 450 后 0.0 永不可能）。
3. **本脸不查载具/real**：navigate/navigateStep 都拒的载具实体在这里
   由委托臂才拒（顺序上还排在四查之后）。
4. **偏移只有 x/z**，y 取目标自身 y；cos 分量进 z、sin 进 x（BigWorld
   yaw 绕 y、0 朝 +z）。
5. **出界点的层选择**（实证，slope 是沿 z 全长的 x 向条带：P1
   x∈[0,3] h0-1.5 / P0 x∈[3,7] h1.5-3.5）：
   - `contains` 层带带 ±0.1 容差（P1 实际 [-0.1,1.6]）；
   - 层带不满足即落入 `find_closest`，其距离 = **点 clip 进 waypoint
     后的 3D 距离（含 y 钳制）**——点 (1,3.0,7) 到 P1 的 clip 点
     (1,1.5,7) 距 1.5，比到 P0 的 clip 点 (3,3.0,7) 距 2 近，所以解析
     **wp1**；
   - `find_closest` 用严格 `<`，**d² 平手时先遍历的 waypoint（wp0）胜**：
     (1,3.5,7) 两片 clip 距离同为 2 → wp0。
   - 这解释了为什么「跨层探针的目标 y 要 3.5」：y 3.0 仍解析回源
     waypoint 走短路，绑定限长根本到不了搜索。
6. 委托臂的错误对象包在 `NavigateStep(inner)` 里，`message()` 透传内层
   文本。

## 未盖行（防御/永不重入，共 5 行，99.63%）

与 06-02/06-03 登记相同的防御集：`DestinationUnresolved` 返回 + message
臂、`canNavigateTo` 的目的地非法臂、06-01 失败路线永不重入循环守卫
2 行。06-04 新增代码全覆盖。

## 门禁

- `cargo test -p server-runtime`：**1139 过 + 1 忽略**（含 3 条新
  navigateFollow 测试，模块共 18）。
- `cargo test --workspace`：**4439 过 / 0 失败**。
- `cargo fmt -p server-runtime -- --check`、
  `cargo clippy -p server-runtime --all-targets -- -D warnings` 全绿。
- 覆盖（llvm-cov 0.9.1 `--lcov`，DA max 合并，`cargo clean -p` 后测）：
  `entity_navigate.rs` **99.63%**（1346/1351）。

证据测试：`crates/server-runtime/src/entity_navigate.rs`
（navigate_follow 脸 + FollowTarget + 3 测试）。
