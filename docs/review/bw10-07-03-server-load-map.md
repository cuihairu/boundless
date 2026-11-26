# BW-10-07-03 从服务端加载该地图

todo.md:1532。验收条件：「坐标、地形和 Portal 状态吻合」。

交付日期 2026-11-25，branch `lane/bw10-terrain`。

## 范围

服务端装载面 = 资产解码（`ChunkWaypointSetData::readWaypointSet`，07-02 已重放）+ 每 chunk 一套挂到
自己的 navigator + 原版 `ChunkWaypointSet::bind()` 走线把接缝的 another-chunk 边穿过 permissive
门户对接。参照 harness 在 el7 真库上组出两个真 chunk（`Chunk::load(NULL)` 走 FNF 分支后用公开
`localBB`/`boundingBox` 落真盒）、挂 `ChunkWaypointSet` + `ChunkNavigator::add`、建门户对
（`ChunkBoundary` + `bindPortal(0)` + `joints().push_back`），跑真 `bind()`，倾倒 CONNTO /
EDGELABEL / ITER / NAVLOC / XPATH / SETSEARCH 六段真相。

## 参照侧产物（`docs/review/fixtures/waypoint/`）

| 文件 | 内容 |
| --- | --- |
| `gen_mapload.cpp` | harness：读入 07-02 导出的两个 .bin，组 chunk/门户，真 `bind()`，六段探针逐行打印 |
| `gen_mapload.log` | 183 行真相，`RESULT OK` |
| `gen_mapload.err` | 5 条 INFO + chunkA/chunkB 的 FNF WARNING（stderr 单独重定向，避免搅乱 stdout） |

真相要点（格式 `段 段内字段`）：

- `CONNTO 0 0 1 0` / `CONNTO 1 0 0 1`：A→B 过门户 0、B→A 过门户 1，各一条连接
- `EDGELABEL 0 23 1 5 (5.1,4.6,4.1)`、`0 16 1 7 (5.1,4.1,1.35)`：A 侧两条接缝边落 B wp5/wp7
- `EDGELABEL 1 30 0 3 (4.6,4.1,1.85)`、`1 21 0 4 (4.6,4.6,4.6)`、`1 18 0 4 (4.85,4.6,5.6)`：
  B 侧三条落 A wp3/wp4/wp4
- entry point = 探针点 `wsv`（边的 waypoint 空中点 + 工作探针高 `yValues[i]`，即属主 waypoint
  的 `wyavg = (wymin+wymax)*0.5 + 0.1`）——不是门户平面投影 `ltpv`（逐边核对过，见下）
- `ITER` 全零：`WaypointNeighbourIterator` 的卫序（`currentEdgeIndex_` 初始化为 65535，
  `advance()` 先测 < size 再自增——新生迭代器已 ended），**原版二进制即产出零邻居**，非接线缺口
- `NAVLOC` 9 条按归属 chunk 解析（AHOLE/BOBST 两探针无 poly 包含，走 closest 回退）
- `XPATH SEAM` 5 态跨缝、`SEAMBA` 2 态（目标=穿越落点）、`TOUR` XPATHNONE（单次穿越规则）
- `SETSEARCH` 5 组：SEAM/SEAMB/SEAMBA 双向穿缝（blockNonPermissive 不拦 permissive 门），
  SETHOP 的 hop 列给出门户号 0/1

## 关键参照行号

- `ChunkWaypointSet` 构造器的邻接边收集 `chunk_waypoint_set.cpp:30-72`：`wyavg`、三探针高
  `[wyavg, wymax+0.1, wymin-0.1]`、边中点（下一条边起点回绕）、重复绝对边号 `MF_ASSERT`。
  `adjacentToChunk()` = `neighbour >= 32768`（**vista 65530 与 boundary 65535 都算**）
- `readyToBind` `:369-409`：外 chunk 覆叠者已载（夹具为内 chunk，塌缩成单旗）、joints 里
  未绑门户的 `hasChunkOrIsNothing() || isInvasive()` 挡门（`hasChunkOrIsNothing =
  !isConnectingToSpecial()`，heaven/earth/extern 特殊门户不挡，chunk_boundary.hpp:271-273）、
  自家门户对端 chunk 已载
- `bind()` `:384-500`：逐条未绑邻接边 × 三探针高 → `hasChunk() && 对端 bb.intersects(wsv)` 门 →
  `Chunk::findBetterPortal`（`chunk.cpp:3576-3620`：平面距门（内 chunk 1 m）→ (u,v) 矩形 CW
  叉积全 > 0 内点测试 → 竞争按对端 chunk 小者（bb 三边长积）、再按平面距近者）→ `ltpv =
  origin + uAxis*(uAxis·lwv) + vAxis*(vAxis·lwv)` → `ChunkNavigator::findExact` →
  `connect(..., wsv)`（**entry 存探针点 wsv，非 ltpv**）
- `findExact` 线性面 `chunk_navigator.cpp:279-363`（无网格回退）：`minHeight > y + 0.1` 跳过 →
  包含且 `maxHeight + 0.1 >= y` 即刻精确答 → 否则记 `|maxHeight − y|` 最小者兜底
  （INVALID_HEIGHT 1e6）
- `getAdjacency` 的穿越臂 `chunk_waypoint_state.cpp:104-118`：邻域查 edgeLabel，
  `crossedPortal_` 置位；**该 state 不再扩张**（`:80-88` 的一次穿越规则）——故跨缝的
  chunk 级路径必须终于穿越落点（SEAMBA 的目标正是 B 侧 wired 边落的 A wp3），TOUR 想穿越后
  继续走即 XPATHNONE

## 验收三腿

1. **接线重放**：`crates/waypoint/src/chunk_bind.rs` 用端口自己的 `build_neighbour_edges()` +
   `bind()` 在真解码资产上跑，CONNTO 两条、EDGELABEL 五条（A 恰 {16,23}、B 恰 {18,21,30}，
   entry 逐分量吻合）、ITER 全零逐 waypoint 相等。
2. **导航重放**：`crates/waypoint/tests/genmap_load.rs` 的 NAVLOC 9 条（按归属 chunk
   `find_by_girth` 两相面）+ XPATH 6 组逐步（含 SEAM 跨缝 5 态、SEAMBA 2 态、TOUR NONE）+
   SETSEARCH 5 组 SETHOP（set 序、位置、shell/activated 旗、门户号全对上）。
3. **单测面**：chunk_bind.rs 7 个单测钉住构造器收集（vista/boundary 同收、三探针高、边中点）、
   接线（含 far=None 死门户跳过与二次 bind 的已绑边跳过）、三处 ready 门、findExact 三臂
   （精确/兜底/拒答 + girth 不匹配）、门户竞争三臂（无竞争/同 far 平面距/异 far 体积）、
   内点与平面距拒绝臂、findExact 拒答留边不绑臂。

## 新增代码

- `crates/waypoint/src/chunk_bind.rs`：`NeighbourEdge`（yValues/v/isBound）、`BindPortal`
  （平面/矩形/uAxis/vAxis/origin/far/flags/未绑旗/特殊旗/invasive 旗）、`BindChunk`
  （bb/outside/loaded/覆叠旗/门户/sets）、`BindSpace`；`ChunkWaypointSetData::
  build_neighbour_edges/neighbour_edges/ready_to_bind/bind`；自由函数 `find_exact`、
  `find_better_portal`（crate 私有）。
- `crates/waypoint/src/chunk_waypoint_set_data.rs`：新字段 `neighbour_edges: RefCell<
  BTreeMap<EdgeIndex, NeighbourEdge>>`（Default 派生覆盖）。
- `crates/waypoint/tests/genmap_load.rs`：真相重放（接线 + 导航两测）。

## 登记边界

- **仅恒等变换**：`MappedVector3` 的 world/chunk 换算走 chunk 变换；夹具 chunk 恒等，chunk/geo/
  waypoint 空间同坐标。端口不建非恒等 chunk 变换模型。
- **findExact 的 girth 网格分支不在范围**：网格只为外 chunk 存在（`ChunkNavigator::add` 对内
  chunk 在建网格前就返回），端口不建该索引；此处线性 `wpSets_` 面即内 chunk 的实际路径。
- **readyToBind 的外 chunk 覆叠门塌缩成单旗**（真门逐覆叠 chunk 查 loaded；夹具是内 chunk）。
- **跨集臂的 chunk 盒钳制未建模**（`chunk_waypoint_state.cpp:196-205`，先前已登记）：夹具图
  每次穿越都落在盒内。
- **SETHOP 的 position 是探针点抬升**（src/dst 的 NavLoc 点 +0.01），非连线交点——set 级搜索
  不做边投影。
- 门户矩形用 (u,v) 二元组建模（`findBetterPortal` 把矩形角点当平面 2D 坐标直用、探针点按
  uAxis/vAxis 投影——harness 把 (u,v) 烤进 Vector3 的 x,y 槽，行为已实证）。

## 门禁

- `cargo test -p waypoint`：125 passed / 0 failed（92 lib + 9 + 9 + 3 + 2 + 2（genmap_load）+ 8）
- `cargo test --workspace --no-fail-fast`：116 个 target 全绿（4488 passed / 0 failed）
- `cargo fmt -p waypoint -- --check`：clean
- `cargo clippy -p waypoint --all-targets -- -D warnings`：clean
- `cargo llvm-cov clean` 后 `cargo llvm-cov -p waypoint --lcov`（行覆盖）：
  chunk_bind.rs 99.55%（441/443）、chunk_waypoint_set_data.rs 100%（418/418，含新字段无新未盖行）、
  chunk_waypoint_state.rs 98.80%（410/415，本任务新增穿越臂全盖，余 5 行为先前交付即存在的臂）、
  navigator.rs 99.58%、girth.rs/navloc.rs/chunk_waypoint.rs 100%；TOTAL 99.49%

chunk_bind.rs 未覆盖的两行（:244/:318）是裸收尾 `}` 的区域合并伪迹——其包着的语句块全部执行
（`chosen = Some(...)` 与 `best = Some(...)` 所在臂均有测），与前几车登记的「else-if 收尾 } 伪迹」
同类，不做缩水。三处活拒绝臂（已绑边跳过、far=None 门户跳过、findExact 拒答）均有专测。
