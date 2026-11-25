# BW-10-04-07 限制 waypoint 搜索距离

**验收**：在原版阈值附近分别验证可达和超限结果。

## 样本（truth from the original engine）

`docs/review/fixtures/waypoint/gen_waypoints_cross.cpp` 新增 D 段（638-744 行）：
复用 04-05/04-06 的三 chunk 接线世界，**每探针一个全新 `Navigator navigator;`**
（把缓存排除在画面外），原版 `findPath(src, dst, limit, false, next, act)`
逐限值扫过两个原版阈值；另加 DCACHE 段（同一 Navigator 上换限值）。

真值日志 `gen_waypoints_cross.log`（196 行，`RESULT OK`；blob
`world_navmesh_cross.bin` md5 `537a2f64b3865145af9ab82a719eec22` 不变——
D/DCACHE 段只读不写；FP 段 125 行与 04-06 交付时逐字节一致）。
生成物 `gen_waypoints_cross` 不提交。

### 原版阈值（三处，真值逐一钉死）

1. **`checkMaxDist = maxDistance > 0.f`**（`astar.ipp:46`）——**0.0 与 -1 同为不限**。
   真值：每对 `0.0000` 与 `-1.0000` 答案相同；AA 连 `0.001` 也 OK。
2. **A\* 的邻接过滤**（`astar.ipp:125-131`）：
   `if (checkMaxDist && key.ext.distanceToGoal(start) > maxDistance) continue;`
   —— 候选到**起点**的 3D 距离（不是累计代价）。集级 goal 集候选的
   `position_` 就是 dst 点（`chunk_waypoint_set_state.cpp:168-171`
   "If we are in the same set as the goal then use it as the position"），
   所以**跨集查询的阈值 = |src→dst| 本身**：

   | 对 | 真值翻转 | \|src→dst\| |
   | --- | --- | --- |
   | AB | `DPNONE 0.3807` / `DOK 0.3808` | √0.145 = 0.380789 |
   | AC | `DPNONE 0.7071` / `DOK 0.7072` | √0.50 = 0.707107 |
   | CA | `DPNONE 0.7280` / `DOK 0.7281` | √0.53 = 0.728010 |
   | BC | `DPNONE 0.4300` / `DOK 0.4302` | √0.185 = 0.430116 |

   CA 与 AC 走同一串门户却翻在不同的值上——阈值跟着行距走，不是常数。
3. **集内 A\* 的同一过滤 + 集内钳制**（`navigator.cpp:455-460`
   `maxSearchDistance > src.gridSize() ? -1 : max`）：

   | 对 | 真值翻转 | 绑定面 |
   | --- | --- | --- |
   | BB（同集两 waypoint） | `DPNONE 0.3140` / `DOK 0.3141` | 集内过滤（共享边 z=½ 的交点候选 ≈0.3140064） |
   | AE（终点刚过 A\|B 门户） | `DPNONE 0.2656` / `DOK 0.2657` | 集内过滤（≈0.26565）；集级在 0.254 就已放行 |

   AE 是**集内过滤成为绑定面**的判据：0.254/0.26/0.265/0.2656 全
   `DPNONE`（集级 0.2534/0.2532 早已放行），0.267 起 `DOK`。

另：`GRID 100.0000` 探针——`pChunk[0]->space()->gridSize()` = **100**
（`NavLoc::NavLoc(pChunk, point, girth)` 存的是 space 的 grid 而非 girth，
`navloc.cpp:66-70`）。本表所有限值 ≤2.0 ≪100，故集内钳制臂在真值世界里
永不触发。

**同集同 waypoint**：AA 全部限值 OK（含 0.001）——起点即 goal：goal 的
`waypoint == -1` 与 `waypointsEqual` 两支都按**集同身份**答真
（`chunk_waypoint_state.hpp:48-56`），不展开、不过滤。

**DCACHE 段**（同一 Navigator 换限值，AC 对）：`-1.0` OK、`0.05` NONE、
`0.0` OK、`0.30` NONE——本世界所有集路径都跨 shell 边界（三 chunk 皆 inside），
参照 `searchNextWaypointSet` 因此**每次旁路集缓存重搜**，紧限值照样失败
（不是缓存越权答真）。这是 04-08 路径缓存失效面的入口。

## 参照源码

- `waypoint/astar.ipp:38-131`（`search`：`checkMaxDist`、邻接过滤三行、
  goal 判定先于展开）。
- `waypoint/navigator.cpp:411-481`（`searchNextWaypoint`：异集先集级搜、
  `maxSearchDistanceInSet = max > src.gridSize() ? -1 : max`）、`499-527`
  （`searchNextLocalWaypoint`）、`538-571`（`searchNextWaypointSet`）。
- `waypoint/chunk_waypoint_set_state.cpp:160-204`（集级邻接：goal 集候选取
  `goal.position_`，其余取最近 edge label 入口点；`distanceFromParent_` 用
  `distanceToGoal(neigh)` 即纯点距）。
- `waypoint/chunk_waypoint_state.hpp:48-56`（`isGoal` 两臂）、`78-82`
  （`distanceFromParent`/`distanceToGoal` = 点距）。
- `waypoint/navloc.cpp:66-95`（`NavLoc(pChunk, point, girth)`：
  `gridSize_( pChunk->space()->gridSize() )`、`point_.y += 0.01f`、按 girth
  定位 waypoint）。

## 移植面

距离限制两面在 04-06 已随 `AStar::search`/`Navigator` 落地
（`crates/waypoint/src/astar.rs:103,168`、`navigator.rs` 的 `max > grid_size`
钳制），本项是**验收与真值面**，并修掉回放侧一处真值偏差：

| 参照 | 移植 | 逐句要点 |
| --- | --- | --- |
| `NavLoc(pChunk, point, girth)` 的 `gridSize_` | `NavLoc::from_parts(set, waypoint, point, grid_size)` | 原端口把 `grid_size` 钉死 0.0——**回放世界因此失真**：`max > 0.0` 恒真，集内搜索被钳成不限，BB 的 `0.25` 探针在端口答真而原版答 `DPNONE`。补第 4 参后取真值 **100.0**（`GRID` 探针），20 调用点同步 |
| `checkMaxDist = max > 0.f` | `astar.rs:103` 既有 | 0.0 与 -1 同为不限（AA 0.001 OK、各对 0.0000 OK） |
| 邻接过滤 | `astar.rs:168` 既有 | `scratch.distance_to_goal(&start_state) > max_distance` |
| 集内钳制 | `navigator.rs` 既有 | 单元面补真：`the_none_flag_and_the_in_set_distance_clamp_arms_run` 加「超过 grid 钳成 -1 仍答真」与「紧限 0.1 答假」两臂，`navigator.rs` 覆盖回到 100% |

## 测试与门禁

- 集成（`crates/waypoint/tests/cross_chunk_search.rs` 新
  `the_navigator_answers_the_reference_distance_probes`）：**65 行 DOK/DPNONE
  表**逐真值重放——四个跨集对的 |src→dst| 翻转、BB/AE 集内翻转、AA 同集全绿、
  逐 DOK 断言 next 点/waypoint/集合指针/act 旗、逐 DPNONE 断言 next 未动；
  新 `a_changed_limit_on_the_same_navigator_always_researches` 重放 4 条 DCACHE。
- 门禁：`cargo test -p waypoint` **84 lib + 8 cross_chunk + 8 navmesh_bin**
  全绿；`cargo fmt -p waypoint -- --check` 干净；
  `cargo clippy -p waypoint --all-targets -- -D warnings` 过（两 `#[allow]`
  带理由：重放限值字面量恰好回显 1/√2、真值表保留为扁平行）；
  llvm-cov（--lcov DA:0 计数）**13 文件全 ≥98%、crate 总 99.53%**
  （`navigator.rs` 100%——04-06 遗留的 210 行被钳制臂补真）；未盖行照旧登记：
  `astar.rs [187,205,206,211]`、`chunk_waypoint_state.rs [113,215,218,249,315]`、
  `chunk_waypoint_set_state.rs [137,146,210]`、`chunk_waypoint_set.rs [266]`、
  `chunk_waypoint_state_path.rs [65,186]`（双无集 portal 臂）、
  `chunk_waypoint_set_state_path.rs [39,40]`；`cargo check --workspace` 过；
  `cargo test -p test-support` 630 全绿。

## 登记边界

- **集内钳制臂在真值世界不可区分**：space grid = 100，本表限值 ≤2.0，
  `max > grid_size` 恒假 → 集内搜索恒拿原始限值。臂本身由单元面覆盖
  （`the_none_flag_and_the_in_set_distance_clamp_arms_run` 的 1000.0 臂）；
  真值世界若要区分需 max>100，超出路径距离量级。
- **`moveStateAlongPath` past-rend、`smoothPath`、空间面** 等 04-06 登记的
  延后项不变。