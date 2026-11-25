# BW-10-04-06 返回 waypoint 不可达结果

**验收**：封闭 Portal 后，同一对起终点返回与原版一致的失败结果。

## 样本（truth from the original engine）

`docs/review/fixtures/waypoint/gen_waypoints_cross.cpp`（FP 段，565-634 行）链接原版 el7
库、复用 04-05 的三 chunk 接线世界，`Navigator navigator;` 单实例上原版
`findPath(src, dst, 1000000.f, block, next, act)` 十探针：

| 探针 | src→dst | block | 真值 |
| --- | --- | --- | --- |
| AC0 / AC0R | A(0.15,0.4,0.4)→C(0.85,0.4,0.5) | 0 | `FPOK 0.333333 0.600000 0.426190 0 1 0`（AC0R 为同参重放） |
| AC1 | 同上 | 1 | `FPNONE` |
| CA0 | C(0.85,0.4,0.6)→A(0.15,0.4,0.4) | 0 | `FPOK 0.666667 0.600000 0.500000 0 1 0` |
| CA1 | 同上 | 1 | `FPNONE` |
| AB0 | A(0.15,0.4,0.4)→B(0.5,0.4,0.25) | 0 | `FPOK 0.500000 0.410000 0.250000 0 1 0` |
| AB1 | 同上 | 1 | `FPNONE` |
| CB0 / CB1 | C(0.85,0.4,0.5)→B(0.5,0.4,0.75) | 0/1 | `FPOK 0.666667 0.600000 0.500000 0 1 0`（许可门户不挡） |
| AA0 | A(0.15,0.4,0.4)→A(0.2,0.4,0.6) | 0 | `FPOK 0.200000 0.410000 0.600000 0 0 0`（同集终点直答） |

真值日志 `gen_waypoints_cross.log`（96-126 行，`RESULT OK`；blob
`world_navmesh_cross.bin` md5 `537a2f64b3865145af9ab82a719eec22` 不变——FP 段只读不写）。
生成物 `gen_waypoints_cross`、assert 日志不提交（发电机二进制照例留场外）。

AC1/CA1/AB1 即验收句：`blockNonPermissive=true` 封掉 A|B 非许可门户对后，同一对
（与 AC0/CA0/AB0 逐坐标相同）起终点 `findPath` 答 false、`next` 原样不动。

## 参照源码

`/home/cui/workspaces/BigWorld/programming/bigworld/lib/`：

- `waypoint/navigator.cpp:118-159`（`findPath`：READ_CACHE 校验臂 + 主臂的
  `waypointsEqual → nextWaypoint = dst`（"Use dst as point might not match"））。
- `waypoint/navigator.cpp:411-481`（`searchNextWaypoint` 两级编排：异集先集级搜、
  `dstState = ChunkWaypointState(pNextSetState->navLoc().pSet(), dst.point())`、
  `maxSearchDistanceInSet = max > src.gridSize() ? -1 : max`、激活旗四连门）。
- `waypoint/navigator.cpp:499-527`（`searchNextLocalWaypoint`：命中挪游标 /
  miss 搜+存 / 搜败清缓存答 false）、`538-571`（`searchNextWaypointSet`：
  shell 旁路 + 命中 / miss 重搜 / 败清答 false）、
  `356-362`（`getCachedWaypointPath`）+ `34-47`（匿名 `addWaypointPathPoints`）、
  `368-389`（`getWaySetPathSize`/两 clear）。
- `waypoint/navigator.hpp:37-41`（`CacheState`）。
- `waypoint/search_path_base.hpp:17-165`（CRTP 基：`empty`/`isValid`/`pCurrent`/
  `pNext`/`pDest`/`clear`/`pop`/三判 + `174-208` `matches`）。
- `waypoint/chunk_waypoint_state_path.cpp:11-54`（`init`：`<2` 补 pLast、
  `fullPath_`/`stateIterOnFullPath_`）、`62-108`（`removeDuplicates`：重复点 +
  三共线中点，`size>2` 守卫）、`593-627`（`validateNavLoc`）、`633-671`
  （`moveStateAlongPath`）、`677-`（`isPartOfPath`）。
- `waypoint/chunk_waypoint_set_state_path.cpp:13-43`（`init` 无补态怪癖 +
  `passedShellBoundary_` 沿途 OR）。
- `waypoint/navigator_cache.cpp:31-100`（`saveWayPath`/`findWayPath`/两 set 面）。
- `waypoint/chunk_waypoint_state.cpp:29-43`（2 参 goal 构造，04-05 已交付）。

## 移植面

`crates/waypoint/src/` 新五模块 + `lib.rs` 登记：

| 参照 | 移植 | 逐句要点 |
| --- | --- | --- |
| `SearchPathBase`（CRTP） | `search_path_base.rs` trait | 存储归实现方（`reverse_path`/`clear_reverse_path`/`pop_back` + 虚 `states_are_equivalent`/`is_part_of_path`），其余默认体逐行；`matches` 三臂（next→pop、current、part），**"clear it now just for sanity" 注释但不清**——照抄 |
| `ChunkWaypointStatePath` | 同名 | `init`：<2 补 pLast（保 isValid）、`full_path_` 克隆、游标 0、`can_be_smoothed=true`；`remove_duplicates` 逐句（almostEqual 0.0004 分量、`normalise` 后共线判、中点删除置 `can_be_smoothed=false`）；`validate_navloc`：next 态 contains-projection 先、后 full 路自游标；`move_state_along_path`：跳到匹配态停在 next、越界读按早退钳制（参照 past-rend 解引用 UB，登记） |
| `ChunkWaypointSetStatePath` | 同名 | `init` 无补态 + shell 旗沿途 OR；`states_are_equivalent` 即集合同一性 |
| `NavigatorCache` | 同名 | `save_way_path` = `init`+`remove_duplicates`（`smoothPath` 延后，见下）；`find_way_path`/`find_way_set_path` = `matches`；两 clear、size、shell 旗 |
| `Navigator` | `navigator.rs` | `find_path`：READ_CACHE 三连校验（空缓存直落主臂）→ 草稿缓存搜（成功 `next = pNextWaypointState()->navLoc()`，**无** dst 交换——参照 145 行同序）；主臂经成员缓存搜，成功且 `next.waypoints_equal(dst)` 才 `next_waypoint = dst`。`search_next_waypoint`：断言双端 valid → 清旗 → 异集先 `search_next_waypoint_set`（败即 false）→ `for_destination(下一集, dst.point())` → `max > grid_size ? -1 : max` → 集内搜（败即 false）→ 激活旗 `pNextSetState` 存在 ∧ `pCache->pNextWaypointState()->navLoc().pSet() != src.pSet()` ∧ `passedActivatedPortal()` |
| `search_next_local_waypoint` | 同名 | 命中挪游标 / miss 搜+存 / 败清答 false；**参照 508 行挪的是 `pCache_`（成员）而方法其余读参数**——端口挪参数，经 `findPath` 观测等价（READ_CACHE 草稿缓存恒空、`matches` 永假，nudge 只在主臂可达且彼时参数即成员），登记 |
| `search_next_waypoint_set` | 同名 | shell 旗旁路缓存 → miss/旁路清两缓存重搜 → 败清 set 路答 false |
| 访问面 | 同名 | `get_cached_waypoint_path`（next 点集，不含当前态）、`get_way_set_path_size`、两 clear |

## 登记边界

- **smoothPath 延后（成路整形面）**：参照 `saveWayPath` 在 `init` 与
  `removeDuplicates` 之间调 `smoothPath`（funnel 拉绳，cpp:120-190 约 460 行）。
  它只改已找到路径的点形，不影响可达/不可达判定；且本车道真值世界所有 way path
  2-3 态，`smoothPath` 开头 `reversePath_.size() == 2` 早退原样返回（真值路径与
  不做 smooth 逐字节一致）。端口 `can_be_smoothed` 旗照管生命周期；funnel 主体
  随首个需要它的路径质量面落地。
- **`searchNextLocalWaypoint` 的成员游标怪癖**：如上，端口以参数 nudge，观测等价，
  直呼带已填草稿缓存的调用者会与参照分叉（成员游标 vs 草稿游标）——本端口无此
  调用面，登记。
- **`moveStateAlongPath` past-rend UB**：参照在无匹配时解引用 `reversePath_` 末端
  之后的迭代器；活跃路径永不触发（next 态恒停住循环）。端口钳制为早退，登记。
- **空间面延后**：`findFullPath`/`createPathNavLoc`/`findRandomNeighbourPointWithRange`
  需 `NavLoc(pSpace, …)` 与 `pSpace->collide` 面（04-02 起登记，随 chunk space 车道）；
  参照 `findFullPath` 内 `dstSetState` 局部为死代码不移植；`dumpCache` 为调试打印。
- **const 方法内可变缓存**：参照经 `pCache_` 指针在 const 方法里改缓存；端口
  `RefCell<NavigatorCache>`。
- 集内两级均成功才有真值；任一级假即 false——不可达结果即 `findPath` 答 false
  （AC1/CA1/AB1 三探针为证）。

## 测试与门禁

- 单元（`crates/waypoint/src/` 五新模块）：单节点补态、portal 态按集等价、
  去重三态、validate 三臂、游标三态 + 重叠态 nudge、缓存存/查/进/拒、set 路
  size/shell、READ_CACHE 三臂（空缓存落主臂 / dst 失配走草稿 / 成员缓存不动）、
  重复调用骑双缓存、激活旗（hasChunkItem + 双外 chunk）、集内级 miss、
  `same_set` 两空臂、None 旗 + 亚格距臂。
- 集成（`crates/waypoint/tests/cross_chunk_search.rs` 新
  `the_navigator_answers_the_reference_findpath_probes`）：十 FP 探针逐真值重放——
  六 FPOK 逐点/逐 waypoint/逐集合指针断言 + act 旗全 0，三 FPNONE 断 false 且
  next 不动，AC0==AC0R。
- 门禁：`cargo test -p waypoint` 84+6+8 全绿；`cargo fmt` 过；
  `cargo clippy -p waypoint --all-targets -- -D warnings` 过；
  llvm-cov（--lcov DA:0 计数）13 文件全 ≥98%（crate 总 99.36%；未盖行：
  navigator.rs:210 集级成功后集内失败——需断连双子图真值世界、
  chunk_waypoint_state_path.rs:65 双无集 portal 臂（无公共构造面）与 186 full 路
  walk 命中臂（需多 waypoint 集真值世界），均登记）；`cargo check --workspace` 过；
  `cargo test -p test-support` 593 全绿。
