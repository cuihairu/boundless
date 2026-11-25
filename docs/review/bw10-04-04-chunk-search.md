# BW-10-04-04 实现 Chunk 内搜索

**验收**：固定起终点路径点逐点比对。

## 参照源码

`/home/cui/workspaces/BigWorld/programming/bigworld/lib/`：

- `waypoint/astar.hpp:36-56,60-79,220-231` + `astar.ipp:37-197,208-217`
  （泛型 A*、`IntState`、`cmp_f`、search/first/next）。
- `waypoint/chunk_waypoint_state.hpp:26-115` + `chunk_waypoint_state.cpp:29-235`
  （A* 状态、goal 判定、`getAdjacency` 交叉数学、`setConnection`/`getConnection`）。
- `waypoint/chunk_waypoint.cpp:21-46`（`closestPointOnLineSegment`）、
  `174-195`（`distanceSquared`：clip + 三参 `Math::clamp(minHeight_, point.y, maxHeight_)`）、
  `217-318`（`clip`：绕向外走 + 最近边段投影 + `makeMaxHeight`）、
  `365-387`（`calcCentre`）。
- `waypoint/navloc.cpp:183-215`（`NavLoc::clip`/`makeMaxHeight`）。
- `waypoint/chunk_waypoint_set_data.cpp:626-647`（`find(chunk, point,
  bestDistanceSquared)` —— 04-03 登记延后的 clip 基 `find_closest` 本项补齐）。

## 移植面

`crates/waypoint/src/`：`chunk_waypoint.rs`（`clip`/`distance_squared`）、
`navloc.rs`（`from_parts`/`clip_in_place`）、`chunk_waypoint_state.rs`（新）、
`astar.rs`（新）、`chunk_waypoint_set_data.rs::find_closest`（升级）：

| 参照 | 移植 | 逐句要点 |
| --- | --- | --- |
| `ChunkWaypoint::clip` | `ChunkWaypoint::clip` | 绕向外判定 `edgeVec.crossProduct(pointVec) > 0` 即 `xd*v − zd*u > 0`（containsProjection `c > −0.01` 的符号镜像）；出界点全边段循环保最小 `lengthSquared` → 最近投影；尾随 `makeMaxHeight` 恒置 `maxHeight_`。**登记**：chunk 包围盒回拉分支（首条非 chunk 邻接边中点内推）属 chunk 面，identity 映射下探针点全在盒内，盒建为无界 |
| `ChunkWaypoint::distanceSquared` | `distance_squared` | clip 点 x-z + `clipPoint.y = clamp(minHeight_, point.y, maxHeight_)`（**clamp 读原始点**，clip 已置 maxHeight 后被三参 clamp 覆盖）；`(point − clipPoint).lengthSquared()` |
| `find(chunk, point, bestDistanceSquared)` | `find_closest`（升级） | 逐 waypoint `distance_squared`、严格 `>` 界、进参出参同一变量；04-03 的中心基近似退役——引擎真值 `PATHBEGIN CB dst=0`（A 1.25 < C 1.375 < B 2.5）端口现可复算 |
| `ChunkWaypointState` | 同名结构 | `navLoc_`/`distanceFromParent_`/`crossedPortal_`；goal-only 构造（waypoint −1 + clip）；`isGoal`：goal waypoint −1 答集合同一性（`Rc::ptr_eq`），否则同 set-and-waypoint 对；`compare` 同集比 waypoint、异集比指针 |
| `getAdjacency` | `get_adjacency` | 集合/waypoint 守卫、`crossedPortal` 早出；in-set 邻居重建 NavLoc；交叉数学逐式照抄（`cp1 = del.x*(p1.y−src.y) − del.y*(p1.x−src.x)`，`cp1>0 && cp2<0` → `t = cp1/(cp1−cp2)` 插值；否则端点和 `len(p,dst)+len(p,src)` 严格 `<`）；连接点 `(way.x, cw.maxHeight_, way.y)`（identity 映射，登记）；`clip_in_place` 后量 `distanceFromParent` |
| `setConnection`/`getConnection` | `ChunkWaypointStateSearchData` | 键 = 有序 (set 指针, waypoint) 对，值 = 边两顶点下标；**setConnection 在 getAdjacency 内、close 重复判定之前执行**——被拒状态同样覆写其边顶点对（last-writer-wins，参照同此）；miss 答.sentinel（65535, 65535）。**登记**：参照散列裸指针，端口键 Rc 地址——同一身份，不同分配 |
| `AStar::search` | `AStar::search` | 重入清空（"Ensure that recalling this doesn't recycle old data"）；起点 g=0、f=h=distanceToGoal、只入 open 不入 close；线性 min-f 扫描 + `cmp_f`（`almostEqual(f)` 时 `g` 大者优先，否则 f 小者）；isGoal → 父链回溯（参照回链 pChild 后 first/next 正向遍历，端口收 path 等价）；邻接循环：`getAdjacency` 假→跳、`compare(start)==0` 跳、`checkMaxDist && distanceToGoal(start) > maxDistance` 跳；close 重复：旧 `f <= f` 保留，否则 close/open 双删后重用节点；祖先环守卫（节点不挂链）；赋 ext/g/h/f/parent 后 `close_.insert + open_.insert` |

**登记边界（分配面，同答案）**：参照 `open_` 是 `BW::set<IntState*>` 按指针序线性扫描，端口 `Vec` 按插入序——`(f, g)` 全等的并列由地址序改插入序裁决（分配相关，非契约）；`IntStateHash`（257 桶）→ `HashMap<(Rc 地址, waypoint), 节点>`（其 `ext.compare` 检查的同一身份对）；`StateScopedFreeList`（alloc/free/placement-new 重用）→ arena `Vec`（每状态恰析构一次）。

**登记边界（参照守卫臂）**：祖先环守卫与 `search_key` 空集臂在欧氏 h + 正步长下不可达（沿直线的 f 遥测恒等，回环步长恒 ≥ h 缩减）——参照注释 "Sometimes they enter a loop" 同为防御臂，照抄保留不造路径；`getAdjacency` 的 `waypoint == −1 && !waypointAdjToChunk` 终 else 臂参照同为死臂（neighbouringWaypoint −1 蕴含 chunk 邻接）。

## 样本非手造

`docs/review/fixtures/waypoint/gen_waypoints.cpp` 探针段（`gen_waypoints.log`；blob
md5 `2d17221302e02a4afa2fc39d889743c3` 不变）：BWResource 栈初始化 + XMLSection
settings + 真 Chunk/GeometryMapping/ChunkWaypointSet/ChunkNavigator 装配，6 组固定
起终点（AC/AB/BA/CB/AA/BC，src/dst 坐标与生成器常量逐字一致），原版
`NavLoc(Chunk*, point, girth)` + `ChunkWaypointState` + `StateScopedFreeList` +
原版 `AStar::search` 打印真值：

- `PATHBEGIN <name> <srcwp> <dstwp>` × 6 —— 原版构造器解析的起终点 waypoint
  （`CB dst=0` 即 clip 基 `findClosest` 的裁决）
- `PATH <name> <x> <y> <z> <wp>` 共 15 步 —— `%.6f` 路径点逐点真值
  （AC 2 步、AB 3、BA 3、CB 2、AA 1、BC 2）
- `PATHEND <name>` × 6

**生成器勘误（freelist 双析构）**：参照 `StateScopedFreeList::free()` 先急切
`~IntState()` 再挂链，列表析构又 `delete` 同批节点——每节点多 decRef 一次集合
引用。真 chunk 侧持有集合引用故引擎无感；最小夹具（base 2）耗尽引用致探针段
SIGSEGV。生成器以 `pWaySetA->incRef()` ×128 钉活（**算法未动**，PATH 真值不受影响），
并移除全部调试插桩后复验 RESULT OK + md5 不变。

## 验收直接测试化

`tests/navmesh_bin.rs` 逐行重放全部真值：

- `the_reference_path_probes_are_replayed_point_by_point` —— 6 组探针逐点重放：
  每步 waypoint 下标 + 坐标与 `PATH` 行比对（1e-6 打印舍入容差），15 步全过
  =「固定起终点路径点逐点比对」
- `the_reference_probe_waypoints_resolve_through_find_and_find_closest` ——
  `PATHBEGIN` 12 个 waypoint 全部经端口 `find`（精确）→ `find_closest`
  （clip 基回退）复算对账——04-03 登记延后的升级由引擎真值钉死

单元面：`clip` 三态（内点 0 / 高度夹带 0.25、4.0 / 出界边段 1.0、2.0）、
`distance_squared` 带高夹、`find_closest` 端口样例钉 0.2 投影值、
`ChunkWaypointState` 交叉点 z=1.25（t=0.375）/ 更近顶点 / goal-only 交叉 /
连接表 last-writer-wins / 边界 nudge 直测、AStar 三夹具（真值 SET0 复刻 +
三带图 min-f 扫描越过插入序 + L 形图低价后到邻接替换 open 态）。

## 门禁

lib 50 + 集成 8 全绿；llvm-cov 行覆盖 crate 合计 1848/1857 = **99.52%**，
无文件 < 98%（`astar.rs` 98.43%、`chunk_waypoint_state.rs` 99.36%，余 4+2 行
为上述登记守卫臂/04-05 面）；clippy `-D warnings` 0、fmt 0 漂移；
`cargo check --workspace` 过、test-support 593 绿。
