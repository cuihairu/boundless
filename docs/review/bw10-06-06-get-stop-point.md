# BW-10-06-06 实现 getStopPoint（同一位置、方向和停止距离输入返回原版停止点）

**验收**：同一位置、方向和停止距离输入返回原版停止点。

## 参照面链

`EntityNavigate::getStopPoint`（entity_navigate.cpp:681-692，脚本脸只做
-1→maxAoIRadius 后委托）→ 系统面
`NavmeshEntityNavigationSystem::getStopPoint`
（navmesh_navigation_system.cpp:112-324）。逐跳查询用
`getNavigatePosition`（:550-629），即 06-01 已交付的 `navigate_position`。

系统面顺序：

1. `source = entity_.position()` 先取。
2. `!isRealToScript()` → TypeError
   `"...::getStopPoint(): This method can only be called on a real entity."`。
3. `srcLoc = createPathNavLoc(space, position, girth)` 非法 → ValueError
   `"Could not resolve source position"`（**无句点**）。
4. `!pChunkSpace()` 检查——**死码**（createPathNavLoc 已解引用过空间），
   不移植（登记）。
5. `dstLoc` 非法 → ValueError `"Could not resolve destination position."`
   （**有句点**），随后 `dstLoc.clip()`。
6. 跳走：`position = source`、`stepPoints = [source]`、`safetyCount =
   500`、`skippedFirstPoint = false`；
   `clearCache()` 在参照里仍是注释掉 → 端口不清。
7. 循环 `(|position.x-dst.x|>0.01 || |position.z-dst.z|>0.01) &&
   (--safetyCount>0)`：`getNavigatePosition(srcLoc, dstLoc,
   maxSearchDistance, &position, &passedActivatedPortal, girth)` 失败 →
   `Py_RETURN_NONE`（不是错误）。每跳 push `stepPoints`。
8. `passedActivatedPortal`（`has_chunk_item` 门户，见
   chunk_waypoint_set_state.cpp:213-223）：算 2D 行程 dist；
   `dist > nearPortalDist` → 远门户臂，否则近门户臂。

## 验收（实证，portal 三链夹具）

夹具：`square_set(origin_x)` = waypoint crate `one_set` 字节模板
（navigator.rs 测试）顶点平移；`portal_chain(n)` = n 片单位正方形集
（x 0/2/4 互不重叠）+ 互斥 chunk identity + 每对一条
`permissive + has_chunk_item` 门户（=「activated」）。

**逐跳实证（关键，勿脑内 trace）**：A→B→C 链、src (0.4,0.4,0.5)：
- hop1 → **(2.0, 0.6, 0.5)**，flag true——落在**边界穿越点**（B 左缘
  x=2），不是 connect 的 entry label 点（2.5,0.6,0.5）；
- hop2 → **(4.8, 0.6, 0.5)** = 终点（findPath 的 waypointsEqual(dst) 臂），
  2 跳收敛。

由该几何钉死（stopDist 默认 1.5 / nearPortalDist 1.8 之外的组合）：

- **远门户回退**（2 链，near 1.8 / stop 1.0）→ `(2.3833332, 0.6, 0.5),
  false`：stop = step[1] + 归一化反向 dir × prop，prop=(1.0−0)/2.4。
- **近门户不忽略** → `(source), false`（原样源点）。
- **近门户被忽略** → 走完 → `(destination), true`——**返回的是原始输入
  Vector3，y 不抬**（0.4 而非网格高 0.6）。
- **跨段回退**（3 链 near 3.0 / stop 3.5 / ignore=true，显式限长 100.0）：
  portal1（dist 1.6 ≤ 3.0）近→跳过；portal2（cum 4.4 > 3.0）远→回退，
  seg2 2.8 记 prevBackDist、seg1 断于 3.5 → prop (3.5−2.8)/1.6 →
  `(1.5625, 0.6, 0.5), false`——**prevBackDist 唯一的非零用法**。
- **stopDist 超出全链**（3 链 near 1.0 / stop 2.0）→
  `StopDistTooLong`（"Programmer logic error (1)."）；实证首臂的 stepPoints
  只有 [start, hop1]（seg1=1.6 < 2.0）才回不到 stopDist。
- **ignore 首个却太远** → `IgnoreFirstTooFar`（"told to ignore first
  portal..."）。
- **跳过一个后又遇近门户**（3 链 near 6.0 / ignore=true，两程 1.6、4.4
  皆 ≤ 6.0）→ `SecondClosePortal`（"Programmer logic error (2)."）。
- **无门空间**（slope 全长）→ `((7,3,7)), true` 原始输入；
  **shell 跨层无路由** → `None`（非错误）；**ghost** → NotReal；
  **girth 0.5 无记录匹配** → SourceUnresolved。

## 怪癖（钉死）

1. **远门户臂的两查**：`!skipped && ignore` → ValueError；
   `skipped && !ignore` 的 "Programmer logic error." 由布尔不变式保证永
   不成立（flag 只在 ignore 臂下被置）→ 不移植（登记）。
2. **回退循环的 `stepPoints[-1]` UB**：`backDist < stopDist` 才报错；
   恰等时参照 `int i` 落到 `stepPoints[-1]` 越界读，端口把 `i == 0` 也折
   进 `StopDistTooLong`。
3. **无门答案返回原始 destination**（不 clip、不抬 y）；近门户答案返回
   原始 source；只有回退答案取 stepPoints 的抬高 y。
4. **y 只进 hop 点**：回退答案的 y 是 stepPoints[i].y（makeMaxHeight 后
   的 0.6），与输入 y 0.4 不同——但无门/近门户臂的 y 是输入原值。
5. **`getNavigatePosition` 的 flag**：端口补 `navigate_position_portal`
   （带 flag 的同体实现），trait 的 `navigate_position` 委托之并丢弃
   flag（控制器不需要）。
6. `SafetyCountExceeded`（500 跳）防御臂：收敛路线永不触达，保留。
7. `DestinationUnresolved` 同 06-02/06-03 的防御臂：任何让目的非法的
   girth 也先让源非法（find-closest 兜底），不新增探针。

## 未盖行（共 10 行，99.47%）

06-02~06-05 已登记的防御集 6 行（DestinationUnresolved 返回 + message
臂、canNavigateTo 目的地非法臂、06-01 失败路线永不重入循环守卫 2 行）+
06-06 新登记 4 行（`DestinationUnresolved` 返回与 message 臂、
`SafetyCountExceeded` 返回与 message 臂）。

## 门禁

- `cargo test -p server-runtime`：**1147 过 + 1 忽略**（含 6 条新
  getStopPoint 测试，模块共 26）。
- `cargo test --workspace`：**4447 过 / 0 失败**。
- `cargo fmt -p server-runtime -- --check`、
  `cargo clippy -p server-runtime --all-targets -- -D warnings` 全绿。
- 覆盖（llvm-cov 0.9.1 `--lcov`，DA max 合并，`cargo clean -p` 后测）：
  `entity_navigate.rs` **99.47%**（1869/1879）。

**过程坑（留档）**：clippy 的 "unnecessary parentheses" 把
`(A||B) && block` 改成 `A || B && block` 会**静默改语义**——A 真时
短路跳过 `--safetyCount`，上限彻底失效。参照的分号是
`(fabs||fabs) && (--safetyCount>0)`，`&&` 分组必须保留。

证据测试：`crates/server-runtime/src/entity_navigate.rs`
（get_stop_point 脸 + GetStopPointFailure + portal_chain 夹具 + 6 测试）。
