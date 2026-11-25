# BW-10-04-02 实现 NavLoc 定位与投影

**验收**：多层位置和边界与原版一致。

## 参照源码

`/home/cui/workspaces/BigWorld/programming/bigworld/lib/`：

- `waypoint/navloc.{hpp,cpp}` — `NavLoc` 全套面（ctor 三参、`valid`/`pSet`/`waypoint`/
  `point`/`gridSize`/`isWithinWP`/`makeMaxHeight`/`waypointsEqual`/`containsProjection`/`desc`）。
- `waypoint/chunk_waypoint.cpp:105-122`（`contains`）、`137-172`（`containsProjection`）、
  `311-317`（`makeMaxHeight`）。
- `waypoint/chunk_waypoint_set_data.cpp:579-624`（`ChunkWaypointSetData::find` 双分支）。
- `waypoint/mapped_vector3.hpp`（`GeoSpaceVector3` / `WaypointSpaceVector3`，显式 Vector3 构造）。

## 移植面

`crates/waypoint/src/navloc.rs`（新）+ `chunk_waypoint.rs` 三面 +
`chunk_waypoint_set_data.rs::find`：

| 参照 | 移植 | 逐句要点 |
| --- | --- | --- |
| `NavLoc()` | `NavLoc::new()` / `Default` | 无 set、waypoint 0、点零、grid 0 |
| `NavLoc(guess, point)` | `NavLoc::from_guess` | `point_.y += 0.01f`；`guess.containsProjection(point_)` 命中则沿用 `guess.waypoint()`，否则 `set.find(point, false)`；`MF_ASSERT_DEBUG(guess.valid())` → `assert!` |
| `valid()` | `valid()` | 参照 `pSet_ && pSet_->chunk()`；端口无 chunk 装配 → 判 `p_set` 在（登记） |
| `pSet`/`waypoint`/`point`/`gridSize` | 同名访问器 | `p_set` 为 `Option<Rc<ChunkWaypointSetData>>`，保指针身份 |
| `isWithinWP()` | `is_within_wp()` | `pSet_ && waypoint_ >= 0` 双守卫，点仍在本 waypoint |
| `makeMaxHeight(Vector3&)` | `make_max_height` | 参照经 `WorldSpaceVector3` 映回 waypoint 空间；端口 waypoint 空间即同一坐标系（登记） |
| `waypointsEqual` | `waypoints_equal` | `pSet_ == other.pSet_ && waypoint_ == other.waypoint_` → `Rc::ptr_eq` + 索引相等（两份独立解码数据不等） |
| `containsProjection` | `contains_projection` | `(point_ − point).lengthSquared() < 0.00001` 短路，否则 `wp.containsProjection` |
| `desc()` | `desc()` | set:waypoint + 点 + 逐边顶点 `(...)` 逗号串；`waypoint == -1` 不印顶点表；无效臂 ` (invalid) ` |
| `ChunkWaypoint::contains` | `ChunkWaypoint::contains` | `minHeight_−0.1`/`maxHeight_+0.1` 字面 0.1 带（开比较）后 `containsProjection` |
| `ChunkWaypoint::containsProjection` | `ChunkWaypoint::contains_projection` | 首边取 `edges_[edgeCount_−1]`，逐边 `c = xd*v − zd*u`，`c > −0.01` 不成立即拒 |
| `ChunkWaypoint::makeMaxHeight` | `ChunkWaypoint::make_max_height` | `point.y = maxHeight_` |
| `ChunkWaypointSetData::find` | `ChunkWaypointSetData::find` | `ignore_height=false`：首个 `contains` 命中即返；`true`：投影命中且高度落在带内即返，否则记 `|y − (min+max)/2|` 最小者，全不中返 −1 |

## 样本非手造

`docs/review/fixtures/waypoint/gen_waypoints.cpp` 新增探针段，全部由**原版对象**直跑打印
（`gen_waypoints.log`，重跑 `md5 2d17221302e02a4afa2fc39d889743c3` 的 260B `world_navmesh.bin` 不变）：

- `CONTAINS <set> <wp> x y z 0/1` × 11（含边界 x=0 的 −0.01 fudge 答 1、高度带外 1.95/2.05 答 0、
  集 1 的 quad D 内外对照）
- `CONTAINSPROJ <set> <wp> x z 0/1` × 3（内部 1、贴角 1、投影外 0）
- `FIND x y z idx` × 9（含带外 −1）
- `FINDIGN x y z idx` × 4（多层最佳高度层：(1,5)→0、(2.5,5)→1、(2.5,1.05)→1、(1,1)→0）

回放测试 `crates/waypoint/tests/navmesh_bin.rs` 逐行重放上述 27 条真值，加既有
HDR/SET/WP/EDGE/ADJ/CHUNKBOUND/VISTA 全量对拍。

### 关键发现：引擎 navpoly 为顺时针绕向

原版 `containsProjection` 要求每条边 `c = xd*v − zd*u > −0.01`（内点在右）——即 **x-z 平面
顺时针**。首版夹具的逆时针四边形令**全部** CONTAINS/FIND 探针答 0/−1（真值即裁决，非移植缺陷）。
夹具四点多边形整体反向（顶点表与相邻表同步反向）后探针全数生效；`navmesh_bin.rs` 的
顶点表/边序/邻接断言随 CW 布局一并更新（共享角 (2,2) 现为顶点索引 1，(4,0) 为 4；
A 的边界边是第 0 边、B 回链 A 是第 1 边、C 的边界边是第 0 边）。

## 边界登记

1. `NavLoc(ChunkSpace*, …)` / `NavLoc(Chunk*, …)` 不移植：依赖 `findChunkFromPointExact`、
   `ChunkNavigator::find`、`MappedVector3`（chunk 车道 / 04-04/05 搜索面）。
2. `NavLoc(guess, point)` 的**全空间兜底** `*this = NavLoc(pSet->chunk()->space(), …)` 不移植：
   set 内搜索未命中时端口保 waypoint −1，参照跳去全空间搜索（04-04/05）。
3. waypoint −1 的 guess 走 `pSpace->collide`（障碍最近碰撞把 y 抬到面上）不移植：依赖
   `ChunkSpace::collide`（物理/空间面）。
4. `valid()` 的 `pSet_->chunk()` 判空不移植（无 chunk 装配）；端口判 set 在。
5. `desc()` 参照印 chunk 标识符与裸 set 指针；端口印 `baseIndex_` 且不印指针。
6. `clip()` / `clip(Vector3&)` 不移植（属 04-04 Chunk 内搜索面）。
7. waypoint 空间坐标映射为同一坐标系（参照 `WorldSpaceVector3` 经 chunk 变换）。
8. `MF_ASSERT_DEBUG` → `assert!`（端口无 DEBUG 分级）。
9. 参照 `contains`/`containsProjection` 的 0.1/−0.01 字面量逐字保留（非命名常量）。

## 门禁数字

- `cargo test -p waypoint`：lib **20** + 集成 **4** 全绿（含 27 条定位/投影/FIND 真值重放）。
- llvm-cov 行覆盖：`chunk_waypoint.rs` **172/172 = 100.00%**、
  `chunk_waypoint_set_data.rs` **400/400 = 100.00%**、`navloc.rs` **264/264 = 100.00%**，
  合计 **836/836 = 100.00%**。
- `cargo fmt -p waypoint` 0 漂移；`cargo clippy -p waypoint --all-targets -- -D warnings` 0。
- `cargo check --workspace` 过（2 条既有 manifest 警告非本任务引入）。
- 夹具 `world_navmesh.bin` 260B md5 `2d17221302e02a4afa2fc39d889743c3`；
  `gen_waypoints.log` 重跑 `RESULT OK`。