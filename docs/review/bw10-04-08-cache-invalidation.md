# BW-10-04-08 实现路径缓存失效

**验收**：Portal 或资源变化后重新选路。

## 样本（truth from the original engine）

`docs/review/fixtures/waypoint/gen_waypoints_cross.cpp` 新增 E 段（DCACHE 段之后，
入口即 04-07 证据档写明的 DCACHE 面）：**全程一个 `Navigator eNavigator`**，
真值日志新增 14 行（196 → 210，`RESULT OK`；FP/DCACHE 段与 04-07 交付时
逐字节一致；blob `world_navmesh_cross.bin` md5 `537a2f64b3865145af9ab82a719eec22`
不变——变体资源 `asBinary` 只在内存里走，磁盘 blob 不碰）。
生成物 `gen_waypoints_cross` 不提交（chunkBV 的 FNF 警告走 stderr 无缓冲，
`load(NULL)` 前加了一次 `fflush(stdout)` 让它落成整行）。

### 缓存生命周期（E1-E9，一 Navigator）

| 行 | 查询 | 真值 | 证明 |
| --- | --- | --- | --- |
| E1 | BB(-1) | `EOK (0.5,0.41,0.75) wp1` | way path 进缓存 |
| E2 | BB(0.25) | `EOK` 同上 | **缓存直接放行**——同一限值的新搜是 `DPNONE BB 0.2500`（D 段），缓存命中完全跳过 maxSearchDistance 过滤 |
| ECLEAR+E3 | `clearCachedWayPath()` 后 BB(0.25) | `ENONE` | 公开失效面强制重搜，重搜在旧几何上失败 |
| E4 | BB(-1) | `EOK` | 重搜入缓存 |
| E5 | BB(0.25)，dst (0.5,0.4,0.7) | `EOK (0.5,0.41,0.7)` | **点不参与缓存键**：`waypointsEqual` 只比对集指针+waypoint 序号（`navloc.hpp:60-61`），同集同 waypoint 新点照样放行，next 换成新 dst |
| E6 | BB(-1)，dst (0.5,0.4,0.3)→wp0 | `EOK (0.5,0.41,0.3) wp0` | **换终点拒配重选路**：`matches()` 终点臂拒绝，重搜在新终点上重答 |
| E7 | AC(-1) | `EOK (1/3,0.6,0.42619)` | 跨集查询走 shell-bypass 臂，**双缓存全清** |
| E8 | BB(0.25) | `ENONE` | **E2 的同一查询翻脸**——跨集（跨 portal）查询把 way path 缓存清了，0.25 重搜失败 |
| E9 | BB(-1) | `EOK` | 重新入缓存 |

### 资源变化（EHDR/EWP/E10，变体 B）

变体视图把 chunk B 重建为**不劈分的单 waypoint 条带**，`asBinary` 在内存里
出 blob，按同一记录布局解码 set 1，挂到自己的 chunk（`ChunkNavigator::add`，
照挂 freelist 引脚）。真值：

```
EHDR version 0 girth 0.600000 waypoints 1 edges 4
EWP 0 minh 0.200000 maxh 0.600000 edges 4 centre 0.500000 0.500000
EOK E10 0.2500 0.500000 0.410000 0.750000 0 3 0
```

E10 = E1-E9 的同一 Navigator（缓存里还握着 E9 刚存的 (B,wp1) 旧资源路径），
同一 BB 坐标、同一 0.25 限值：**set 列答 3**（吃旧缓存会答 1）——旧资源
集合对象与新状态永不相配（`waypointsEqual` 比集指针，重载出的集合是新
对象），拒配后在新资源上重搜，旧几何拒掉的限值在新资源上答真。

### Portal 变化面

参照里的"portal 变化失效"是**按旗标旁路**而非按键失效：
`searchNextWaypointSet`（`navigator.cpp:538-571`）先看
`cache.waySetPathPassedShellBoundary()`——缓存集路径只要跨过一次
室内外 portal 就**永远旁路**，每次查询双清重搜。本世界三 chunk 皆
inside，每个跨集 hop 都置旗，DCACHE 段（04-07）已证：同一 Navigator
换限值全重搜。E7→E8 把它推到面级：跨集查询顺带把集内 way path 也清了。

引擎自己的调用方是 `server/cellapp/navigation_controller.cpp:150`
（每走完一段路）与 `:233-234`（新终点）：两处都显式调
`clearCachedWayPath()`（后者再 `clearCachedWaySetPath()`）——
E3/E6 就是这两处的机制真值。

## 参照源码

- `waypoint/navigator.cpp:377-390`（`clearCachedWayPath`/`clearCachedWaySetPath`
  公开面，直落 `pCache_->clearWayPath()`/`clearWaySetPath()`）。
- `waypoint/navigator.cpp:499-527`（`searchNextLocalWaypoint`：命中
  `matches` 则 `moveStateAlongPath` 推进游标；未中则搜后 `saveWayPath`；
  搜败 `clearWayPath(); // for sanity`）。
- `waypoint/navigator.cpp:538-571`（`searchNextWaypointSet`：
  `bypassCache = cache.waySetPathPassedShellBoundary()`；旁路或未中即
  **双清**再 `astarSet.search`）。
- `waypoint/search_path_base.hpp:174-208`（`matches` 四臂：终点变即拒、
  src==next 则 pop 推进、src==current 放行、`isPartOfPath` 兜底）。
- `waypoint/navigator_cache.hpp:56-59`（`clearWaySetPath` 的注释原文：
  `blockNonPermissive` 两侧结果不同，所以需要清）。
- `waypoint/navloc.hpp:60-61`（`waypointsEqual`：
  `pSet_ == other.pSet_ && waypoint_ == other.waypoint_`——**集指针身份**，
  资源重载出的新集合对象天然拒配旧缓存状态）。

## 移植面

缓存失效各面 04-06 已随 `find_path`/缓存落地，本项是验收与真值面：

| 参照 | 移植 | 逐句要点 |
| --- | --- | --- |
| `matches` 四臂 | `crates/waypoint/src/search_path_base.rs:97` | 终点拒、next 推进（pop）、current 放行、part-of-path 兜底，逐臂在 04-06/04-07 单元面已覆盖 |
| 公开失效面 | `navigator.rs:309-315` | `clear_cached_way_path`/`clear_cached_way_set_path`，E3 行吃第一支 |
| 失败 sanity 清 | `navigator.rs:253` | 搜败 `clear_way_path()`，E3/E8 的 NONE 即此臂产物 |
| shell-bypass 双清 | `navigator.rs:270-281` | `bypass_cache || !find_way_set_path` → 双清重搜，E7→E8 吃此面 |
| `waypointsEqual` 身份键 | `navloc.rs` | `Rc::ptr_eq` 集指针 + waypoint 序号，E5（点不进键）与 E10（新对象拒配）两行吃此面 |

## 测试与门禁

- 集成（`crates/waypoint/tests/cross_chunk_search.rs` 新
  `the_cache_lifecycle_serves_invalidates_and_repaths`，E1-E10 十行全断言，
  `variant_b_bytes()` 手搭变体 blob）：E2/E8 同查询翻转、E3 公开面、
  E5 点不进键、E6 换终点重答、E10 set 列吃 `Rc::ptr_eq` 变体集。
- 门禁：`cargo test -p waypoint` **84 lib + 9 cross_chunk + 8 navmesh_bin**
  全绿；`cargo fmt -p waypoint -- --check` 干净；`cargo clippy -p waypoint
  --all-targets -- -D warnings` 过；llvm-cov（--lcov DA:0 计数）**13 文件全
  ≥98%、crate 总 99.53%**（3618/3635，本项零 src 改动，未盖行照旧登记：
  `astar.rs` 4、`chunk_waypoint_state.rs` 5、`chunk_waypoint_set_state.rs` 3、
  `chunk_waypoint_state_path.rs` 2、`chunk_waypoint_set_state_path.rs` 2、
  `chunk_waypoint_set.rs` 1）；`cargo check --workspace` 过；
  `cargo test -p test-support` 630 全绿。

## 登记边界

- **`clearCachedWaySetPath` 在真值世界不可区分**：本世界所有集路径都
  旁路集缓存（shell 旗恒真），清它无观测面。旗本身与双清臂由单元面
  覆盖（`navigator_cache.rs` 测试）；引擎调用方清两支的语义（E3/E6）
  已在真值世界钉死。
- **资源变化是身份级失效**：参照不感知资源内容，只靠集指针身份拒配。
  夹具模拟的是重载后的世界（新集合对象已挂上新 chunk），与
  `ChunkNavigator::add` 的换装路径同构；旧集合对象由缓存引用计数保活
  不崩，但永不被配对。
- 04-06/04-07 登记的延后项（`smoothPath`、`moveStateAlongPath`
  past-rend、空间面）不变。
