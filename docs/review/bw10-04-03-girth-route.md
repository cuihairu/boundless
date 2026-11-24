# BW-10-04-03 按 girth 选择路径

**验收**：窄门对不同角色返回正确可达性。

## 参照源码

`/home/cui/workspaces/BigWorld/programming/bigworld/lib/`：

- `waypoint/chunk_navigator.cpp:135-250`（`ChunkNavigator::find` 两趟结构）。
- `waypoint/navigator_find_result.hpp:21-53`（`NavigatorFindResult`）。
- `cstdmf/stdmf.hpp:606-609`（`almostEqual` 默认 `epsilon = 0.0004f`，严格小于）。
- `waypoint/chunk_waypoint_set.hpp`（`find` 重载签名）。
- `waypoint/chunk_waypoint_set_data.cpp:626-647`（`find(chunk, point, bestDistanceSquared)`）。
- `waypoint/girth_grids.hpp` + `girth_grid_element.hpp` + `girth_grid_list.{hpp,cpp}`
  （GirthGrids 加速，登记不移植，见下）。
- `waypoint/chunk_waypoint.cpp` `distanceSquared`（clip 基距离，登记延后 04-04）。

## 移植面

`crates/waypoint/src/girth.rs`（新）+ `chunk_waypoint_set_data.rs::find_closest`：

| 参照 | 移植 | 逐句要点 |
| --- | --- | --- |
| `almostEqual(a, b)` | `almost_equal` / `GIRTH_EPSILON` | `fabsf(a-b) < 0.0004f` 严格小于；f32 下 `almost_equal(0.6, 0.6004)` 为真（差值恰小于 epsilon），边界在 ~0.6005 |
| `NavigatorFindResult` | 同名结构 | `pSet_`/`waypoint_`/`exactMatch_` 三访问器对；**手动 `Default`**（waypoint 初值 −1，derive 会给 0） |
| `ChunkNavigator::find` 第一趟 | `find_by_girth` 前段 | girth 过滤后 `set.find(point, ignoreHeight)` ≥ 0 即返 **exactMatch = true**（ignoreHeight 的最佳层命中同样算 exact——参照循环在该分支直接 return） |
| `ChunkNavigator::find` 第二趟 | `find_by_girth` 后段 | 全不中才走：`bestDistanceSquared` 自 `FLT_MAX` 起逐 set `find_closest`，答 **exactMatch = false** |
| `ChunkWaypointSetData::find(bestDistanceSquared)` | `find_closest` | 严格 `<` 比较，初值 −1；**登记**：参照先 clip 点进多边形再量距，端口量 waypoint 中心 + 高度夹进 `[min,max]`（序不变，clip 属 04-04） |

## 样本非手造

`docs/review/fixtures/waypoint/gen_waypoints.cpp` 新增探针段，全部由**原版对象**直跑打印
（`gen_waypoints.log`；260B `world_navmesh.bin` md5 `2d17221302e02a4afa2fc39d889743c3` 不变）：

- `GIRTHMATCH a b 0/1` × 5 —— 原版 `almostEqual`：`0.600/0.600→1`、`0.600/1.000→0`、
  `0.600/0.6004→1`（f32 差值恰小于 epsilon）、`0.600/0.6003→1`、`1.000/1.000→1`
- `GIRTHFIND searchGirth setGirth x y z matched idx` × 4 —— `{0.6,1.0}×{0.6,1.0}` 网格，
  同一点 (1,1,1)：匹配集答 waypoint（窄 0.6 → 0；宽 1.0 → −1，该集四边形在别处），
  不匹配集**不搜索**（生成器哨兵 −99，非参照语义）
- `GIRTHFINDIGN girth x y z idx` × 2 —— ignoreHeight：(2.5,5,1) 窄 0.6 → 1、宽 1.0 → −1

回放测试 `crates/waypoint/tests/navmesh_bin.rs` 新增两条：

- `the_reference_girth_probes_are_answered` —— GIRTHMATCH 逐条对 `almost_equal`；
  GIRTHFIND 的 matched 位对 girth 过滤、−99 对「不搜索」语义、匹配行经 `find_by_girth`
  重放（窄门对不同角色返回正确可达性即在此对拍：同一点 0.6 答 0、1.0 答 −1）
- `the_reference_girth_ignore_height_probes_are_answered` —— GIRTHFINDIGN 逐条重放

单元测试 `girth.rs`（`two_layer_sets` 两层夹具：窄 0.6 下层方块 0..1、宽 1.0 上层同足印 2..3）：
窄门只对窄角色 exact 可达 / 宽角色回落最近点非 exact / 双不匹配 girth 答空 / 空集答空；
`chunk_waypoint_set_data.rs` 补 `find_closest` 双向（近者保持、远者不夺位 / best 已更优则全不中）。

## 边界登记

1. `GirthGrids` 加速结构不移植（`girth_grids.*`、`girth_grid_list.*`：仅 outside chunk
   建 12×12 网格 + `findMatchOrLower` 扩环搜索 + `s_visitedWaypoints_` 簿记）——它是同一答案
   的缓存，线性路径语义等价；其构建依赖 chunk localBB 与 `Clip` 基 `distanceSquared`
   （04-04 Chunk 内搜索面）。
2. `find_closest` 的距离量法为**登记近似**：参照 `distanceSquared` 先 `clip` 点进多边形
   再量 `(point − clipPoint)²`（并夹高度）；端口量 waypoint 中心 + 高度夹 `[min,max]`。
   clip 点恒在多边形内，故**对任一 waypoint 均外部的点，距离排序不变**；内部精确距离
   留待 04-04 的 `clip` 面。
3. `ChunkNavigator` 的 chunk 装配（`Chunk` ctor、`ChunkWaypointSet` 包装、`neighbourEdges_`）
   不建模：端口以 set 切片 + girth 过滤承载同一选择语义（跨 Chunk 面 04-05）。
4. `GIRTHFIND` 的 −99 是生成器「该对不匹配故未搜索」的探针哨兵，非参照返回值。
5. `find`/`find_closest` 为非 const 成员——生成器侧用非 const `ChunkWaypointSetData*` 数组。
6. `FLT_MAX` 初值与严格 `<`：首个 waypoint 恒夺位；`best` 已优于全部 waypoint 时答 −1
   （参照同语义，单测双向覆盖）。

## 门禁数字

- `cargo test -p waypoint`：lib **25** + 集成 **6** 全绿（含 11 条 girth 真值重放 + 既有全量）。
- llvm-cov 行覆盖：`chunk_waypoint.rs` **172/172**、`chunk_waypoint_set_data.rs` **433/433**、
  `girth.rs` **155/155**、`navloc.rs` **264/264**，合计 **1024/1024 = 100.00%**。
- `cargo fmt -p waypoint` / `-p test-support` 0 漂移；
  `cargo clippy -p waypoint --all-targets -- -D warnings` 0、
  `cargo clippy -p test-support --all-targets -- -D warnings` 0。
- `cargo check --workspace` 过；`cargo test -p test-support` **593** 绿
  （**补登记**：`waypoint` crate 入 layer registry DOMAIN 层——04-01 建 crate 时漏登，
  本次 `workspace_rules_hold` 揭出，`layer_of` 域臂与 `layer_registry_assigns_every_current_member`
  清单同步补上）。
- 夹具 `world_navmesh.bin` 260B md5 `2d17221302e02a4afa2fc39d889743c3` 不变；
  `gen_waypoints.log` 重跑 `RESULT OK`。
