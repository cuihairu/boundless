# BW-10-06-02 实现 navigateStep（单步目标、返回值与越界行为吻合）

**验收**：单步目标、返回值与越界行为吻合。

## 参照面链

`EntityNavigate::navigateStep`（entity_navigate.cpp:775-806）：

1. `isEqual( maxSearchDistance, -1.f )` → `CellAppConfig::maxAoIRadius()`，
   **先于**拒绝（与 navigate 同序）。
2. 非 real → TypeError（与 navigate 同文本）；载具 → ValueError
   （navigateStep 版文本逐字）。
3. 委托 `navigationSystem().navigateStep( pNavigateStepController_, ... )`
   （navmesh_navigation_system.cpp:328-434）。

系统面：

1. `!pChunkSpace()` → ValueError（`navigateStep(): Physical space of
   entity does not support navigation queries`）。
2. 源 `createPathNavLoc`（空间构造器 + clip）——非法 → "Could not
   resolve source position."。
3. 目的地走**裸空间构造器**，先验合法性**后** clip——非法 → "Could not
   resolve destination position."。
4. `getNavigatePosition` 无路 → "No path found."。
5. 裁剪/落位：`disp.y=0`、`wayDistance`、`dstY`（找到的步 y）、`srcY`
   （`srcLoc.makeMaxHeight`）；`wayDistance > maxMoveDistance` 时沿 disp
   裁到整步长，裁剪点落在源 waypoint 投影内则 `dstY = srcY`；
   `nextPosition.y = max(dstY, srcY)`；实体 **y 立即落位**
   （`setAndDropGlobalPositionAndDirection`），x/z 等控制器走。
6. 无挂接控制器 → `new NavigateStepController(...)` + `addController`；
   已有 → 仅 `setDestination`。返回**控制器 id**。

控制器本体（`MoveControllerCore::move_towards` 与
`NavigateStepController::{new,set_destination,update}`，
含 onMove 链、剩余 tick 预算、100 循环守卫转 onMoveFailure）已在
`movement.rs` 随 BW-08-02-03 交付；本项补上它一直缺的网格脸与注册钩
（`deregisterNavigateStepController` 只对 id 匹配时清除，
entity_navigate.cpp:93-100；持有即建模注册）。

## 验收（slope/shell 资产，05-05/06 字节）

- **单步目标**：wp1 (1,0.5,1) → wp0 (7,3,7) 是**一跳**路线：
  `findPath` 的 `nextWaypoint` 在 `next.waypointsEqual( dst )` 时直接
  取 dst（原版注释 "Use dst as point might not match"）——单步目标 =
  **(7, 3.5, 7) 整个终点**，不是穿越点 (3,3.5,3)。实体 y 立即落 3.5，
  x/z 保持；随后步进控制器 0.2 m/tick 直线 42 tick 走到
  (7,3.5,7)（贴齐），`onMove{7,4,42}` 恰一条、随即取消。
  （06-01 固定路线的首跳穿越点来自 NavigationController 的
  `generateTraversalPath` 缓存路径，与本面不同——两处对得起来。）
- **返回值**：首次创建回答 id=4；**复用臂只 setDestination**——id、
  userArg、velocity 保持原值（新脚本参数被忽略，参照仅在创建臂
  addController）；deregister id 不匹配不动、匹配清空后再步进则按
  调用方给的 id 新建（9）。
- **越界行为**：
  - ghost/vehicle/非 chunk 空间 → 三条错误文本逐字。
  - girth 0.5（无记录匹配）→ "Could not resolve source position."。
  - shell 跨层 → "No path found."。
  - **限长语义**：maxSearchDistance **大于 gridSize 时不约束集合内
    AStar**（navigator.cpp:433-438 "It makes no sense..."）——2.0 >
    1.0 时 8.5 m 路线照常解析；**≤ gridSize 才拦**——1.0 时候选
    距起点 8.9 m 被跳过 → NoPath。
  - **离网源仍合法**（find-closest 兜底）：实体 (50,50,50) 照样出步，
    裁剪点投不进任何 waypoint → 高度取找到步的 3.5；步长 1 m 沿
    45° 对角 → 每轴 0.7071，落 (49.2929, 3.5, 49.2929)。
  - **maxMoveDistance 裁剪**：disp (6,0,6)、wayDistance 8.49 > 1 →
    裁到 (1.7071, y, 1.7071)；裁剪点仍在 wp1 投影内 → 高度取源
    waypoint 的 1.5（而非终点的 3.5）。

## 怪癖（钉死）

1. **一跳路线的单步目标 = 终点 navloc**（findPath waypointsEqual 臂）。
2. **maxSearchDistance > gridSize → 集合内搜索不加限**；≤ 才拦。
3. **ReadWriteCache 的 findPath 不按新限长复验缓存**——成功路线把
   wp1→wp0 缓存后，再小的限长也吃缓存；失败搜索会 clear 掉 way path。
   （测试须先跑会失败的探针再跑会成功的。）
4. 复用臂忽略新 userArg/velocity/faceMovement（参照只 setDestination）。
5. 实体 y 立即落位、x/z 不动（走是控制器的事）。
6. DestinationUnresolved 为**防御臂**：源/目的共用同一 girth，源合法
   ⇔ 目的合法，参照同样不可达——保留并档（未盖行之一）。

## 未盖行（防御/永不重入，共 4 行，99.51%）

`DestinationUnresolved` 返回 + 其 message 臂（防御臂，见上）；跨层
失败测试中「首个重查询即败」永不重入的循环守卫 2 行。

## 门禁

- `cargo test -p server-runtime`：1134（1133 过 + 1 忽略），含 6 条新
  navigateStep 测试（模块共 12）。
- `cargo test --workspace`：**4432 过 / 0 失败**（4426 + 6）。
- fmt / clippy（`-D warnings`）全绿。
- 覆盖（llvm-cov 0.9.1 `--lcov`，DA max 合并，`cargo clean -p` 后测）：
  `entity_navigate.rs` **99.51%**（813/817）。

证据测试：`crates/server-runtime/src/entity_navigate.rs`
（navigate_step 面 + 6 测试）。
