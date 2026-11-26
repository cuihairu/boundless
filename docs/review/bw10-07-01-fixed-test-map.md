# BW-10-07-01 固定测试地图和期望值

**验收**：含坡、洞、障碍和两个相邻 Chunk。

## 地图

`docs/review/fixtures/waypoint/gen_fixed_map.cpp` 用**未改动**的原版 el7
库搭出真实双 Chunk 世界并倾倒期望值：

- **chunk A（set 0）** x∈[0,0.5]：
  - A0w（全局 1）x∈[0,0.2] z∈[0,0.5] 高度带 0.2..0.6；
  - A0e（全局 2）x∈[0.3,0.5] z∈[0,0.5] 高度带 0.2..0.6；
  - **洞** = 条带 x∈[0.2,0.3]（两侧面向边全是 vista）；
  - A1（全局 3，6 顶点）x∈[0,0.5] z∈[0.5,1.0] 高度带 **0.6..1.0 = 坡**
    （0.6 相触保持相邻；南边三段切分 adj-A0e / 洞 vista / adj-A0w）。
- **chunk B（set 1）** x∈[0.5,1.0]：
  - B0（全局 4）南条带 z∈[0,0.4]（北边三段切分 adj-B2w / 障碍 vista /
    adj-B2e）；B1（全局 5）北条带 z∈[0.6,1.0]（南边三段切分
    adj-B2e / 障碍 vista / adj-B2w）；
  - B2w（全局 6）x∈[0.5,0.65]、B2e（全局 7）x∈[0.85,1.0]，z∈[0.4,0.6]；
  - B3（全局 8）**障碍** x∈[0.65,0.85] z∈[0.4,0.6] 高度带 **1.1..1.5**
    （0.1 的带隙 0.6 vs 1.1 切断高度相邻）且四边全 vista → 图上孤立。
- **门户 A|B** 于 x=0.5，双向 permissive，PlaneEq((1,0,0),0.5)，
  矩形 u∈[0.47,0.53] v∈[0.10,0.90]，uAxis/vAxis/origin 如 05 簇先例。
- girth 0.6 全图；多边形 x-z 平面 CW 绕向（边方向 西 +z / 北 +x /
  东 −z / 南 −x；顶点 i 的 adjacency = 自 i 起的边对面的邻居）。

编译法与 gen_waypoints_cross.cpp 相同（el7 库 + cmake-entitydef 的
flags.make 组；`waypoint_view.cpp` 须一并编入——`asBinary` 不在任何
el7 库里；`-DMF_SERVER` 必须，moo_math.hpp 否则要 d3dx9math.h）。

## 期望值（真值日志 `gen_fixed_map.log`，139 行）

- `WROTE` 584 字节 `world_navmesh_fixed_map.bin`
  （md5 `224042030baeefda5aacdefaa881b5ad`）；
- `HDR/SET/WP/EDGE`：set0 3 waypoint 14 边（base 1）、set1 5 waypoint
  24 边（base 4）；坡/环/障碍的邻接表逐边钉死（FFFF 边界、
  FFFE vista = −1 取负顶位形）；
- `PORTAL/CONNTO/EDGELABEL`：**原版 `bind()` 的接线**——A→B 过边 6
  （A0e 东→B wp0）与边 10（A1 东→B wp1）；B→A 过边 0（B0 西→A wp1）
  与边 12（B2w 西→A wp1）；**B1 西边（6）没拿到标签**——
  部分门户矩形（v∈[0.1,0.9]）下原版 bind 的求交遍历停在 B2w 的边，
  非对称性按原值钉死；
- `ITER`：8 个 waypoint 邻居迭代器全部天生即死（count 0，04-05 先例）；
- `NAVLOC`：六个探针点落位——洞心 (0.25,0.4,0.25) 与障碍脚下
  (0.75,0.4,0.5) 都走 find-closest 平手（前者 A0w/A0e 距 0.05、
  后者 B0/B2w/B2e 距 0.1），**首个迭代者胜**（洞心答 A0w 非 A0e、
  障碍脚下答 B0 非 B2w——后者实证推翻预测）；障碍顶 (0.75,1.3,0.5)
  按自身带 1.1..1.5 解析进 B3（可定位但不可达）；位于点带 +0.01
  抬升（0.41/0.81/1.31）；
- `XPATH`：CROSS 四态（A0w → 坡角 (0.2,1.0,0.5) → A0e (0.3,0.6,0.5)
  → 门户面 B0 (0.5,0.6,0.366667)）——洞逼着去 B 也翻坡；
  CROSSBA **XPATHNONE**（穿越门户态不再展开，04-05 怪癖）；HOLE 三态；
  OBST 三态（南条带绕行）；WALLTOP **XPATHNONE**；
- `SETSEARCH/SETHOP`：AB0/AB1/BA0 各两态一跳，permissive 对
  两种 block 旗标都答；AB0/AB1 过 portal 0、BA0 过 portal 1；
- `FPBEGIN/FPOK/FPNONE/FPEND`：原版 findPath——CROSS/CROSSR/HOLE/
  HOLER/OBST 全 FPOK（next = 路径第一跳，y 带 smoothPath 经 guess
  构造器的 +0.01 抬升，如 1.010000），WALLTOP FPNONE；
- `FPLOC/FPOP/FFP/FFPNONE`：原版 findFullPath 在本夹具上
  **全部 FFPNONE**——它的空间构造器把两端点解析成 null
  （`FPLOC … 0 0`）：夹具的 chunk 挂在自己的 ChunkNavigator 上、
  没注册进空间，`findChunkFromPointExact` 找不到 chunk，`src.valid()
  && dst.valid()` 门在搜索前就关了。这是夹具世界接线的属性、
  不是算法的属性（见下）。

## 回放测试（`crates/waypoint/tests/fixed_map.rs`，9 条）

1. `the_fixture_decodes_two_sets_as_logged` —— HDR/SET/WP/EDGE 逐行
   （带/中心/邻接表/边界与 vista 编码）。
2. `the_bind_wiring_restores_connections_and_labels` —— 端口 `connect()`
   复现 CONNTO/EDGELABEL 四条 + **B1 西边无标签的非对称**钉死
   + 背链每邻居一条。
3. `the_neighbour_iterator_matches_the_reference_walk` —— 8 个迭代器
   全部天生即死。
4. `the_reference_resolver_drops_the_probe_points_as_logged` ——
   六探针落位 + 平手首迭代者胜 + from_guess 的 +0.01 抬升。
5. `the_chunk_level_paths_cross_hole_and_obstacle_as_logged` ——
   CROSS 四态 / CROSSBA none / HOLE / OBST / WALLTOP none。
6. `the_set_level_search_crosses_the_permissive_pair_both_ways` ——
   AB0/AB1/BA0 + 门户身份 + shell 旗标。
7. `the_navigator_answers_the_reference_findpath_probes` ——
   findPath 六探针（含 CROSSR 缓存命中臂）；**FPOK 的 y 差异**：
   原版 `saveWayPath` 在 `removeDuplicates` 前跑 `smoothPath`
   （string-pulling），漏斗把每个状态经 guess 构造器重建 → y +0.01
   （1.010000）；端口按登记边界暂缓该面（`chunk_waypoint_state_path.rs`
   模块头），next 钉未抬升的 y（1.0），set/waypoint/x-z 全同。
   并钉 `get_cached_waypoint_path`：缓存路径 = A* 链去头状态。
8. `the_navigator_answers_the_reference_findfullpath_probes` ——
   参照自身在本夹具 FFPNONE（空间构造器 null，见上）→ 该面钉的是
   **端口路线对照日志已钉死的链**：HOLE 三点 + 裁剪终点
   （clip 的 makeMaxHeight 尾部把 y 抬到 waypoint 高度带上限，
   06-01 怪癖：0.41→0.6）、CROSS 四点、OBST 三点（绕行 y=0.6）、
   WALLTOP 无路清空。
9. `the_whole_world_resolves_through_one_girth_search` ——
   `find_by_girth` 全集合形态（07-03 服务端加载消费的形状）。

## 怪癖（钉死）

1. **find-closest 平手 = 首个迭代者胜**：洞心答 A0w（非更近侧的
   预测错误）、障碍脚下答 B0（非 B2w——B0 同样 0.1 远且先迭代）。
2. **原版 bind 的门户求交不对称**：B1 西边与门户矩形相交
   （z∈[0.6,0.9] 段）却没拿到标签——求交遍历停在 B2w 的边。
3. **FPOK 的 +0.01 y 抬升来自 smoothPath 的漏斗重建**（经
   `NavLoc(navLoc, newPoint)` guess 构造器，navloc.cpp:60），
   非来自 findPath 本身（findPath 直接取
   `pNextWaypointState()->navLoc()`）；原始 A* 态（XPATH）两版都是
   1.0。端口登记暂缓该面。
4. **findFullPath 在夹具上全 FFPNONE**：空间构造器需要
   `findChunkFromPointExact`（completed 的 chunk），而夹具的 chunk
   未注册进空间；端口 `find_full_path` 吃的是调用方递来的 navloc
   （参照注释 "the caller hands the same unclipped navlocs in"），
   算法面按日志已钉死的链回放。
5. **findFullPath 推入的终点是裁剪后的点**：`dst.clip()` 的
   makeMaxHeight 尾部把 y 抬到 waypoint 高度带上限（0.41→0.6）。
6. **位于点 +0.01**（locate 系构造器 navloc.cpp:36/:60 与 guess
   构造器）；XPATH 原始态不带（A* 直接吃态点位）。

## 门禁

- `cargo fmt -p waypoint -- --check` PASS；
- `cargo clippy -p waypoint --all-targets -- -D warnings` 0 error
  （2 条 workspace manifest 未用依赖告警为 main 既有）；
- `cargo test -p waypoint`：**114 过 / 0 失败**（85 单测 + 9+9+3+8
  集成）；
- `cargo test --workspace`：**4476 过 / 0 失败**；
- 覆盖（llvm-cov 0.9.1 `--lcov`，`cargo llvm-cov clean` 后跑，DA
  max 合并）：`navigator.rs` **99.58%**（未盖 2 行 = 两处收尾
  `}`）、`chunk_waypoint_state_path.rs` 99.34%、`astar.rs` 98.88%、
  `chunk_waypoint_state.rs` 98.80%、`chunk_waypoint_set_state.rs`
  99.09%、`chunk_waypoint_set.rs` 99.71%、
  `chunk_waypoint_set_state_path.rs` 98.32%、其余 7 文件 100%。

## 证据

- 夹具源 `docs/review/fixtures/waypoint/gen_fixed_map.cpp`；
- 真值日志 `docs/review/fixtures/waypoint/gen_fixed_map.log`
  （stdout 139 行；INFO/WARNING 走 stderr，05-06 教训照旧）；
- 资产 `docs/review/fixtures/waypoint/world_navmesh_fixed_map.bin`
  （584 字节，md5 `224042030baeefda5aacdefaa881b5ad`）；
- 回放测试 `crates/waypoint/tests/fixed_map.rs`（9 条）。

生成器二进制与 assert 日志不入库（车道铁律）。
