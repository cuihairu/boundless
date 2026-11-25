# BW-10-04-05 实现跨 Chunk waypoint set 搜索

- **任务编号**：BW-10-04-05（todo.md:1508）。
- **验收**：同图 Portal 访问顺序吻合 —— `SETHOP` 真值行的 portal 列逐
  hop 逐字复现（AC0 走 0→2、CA0 走 3→1、BC0 走 2、AC1/AB1 在封
  non-permissive 后 `SETSEARCHNONE`）。

## 参照源码

`/home/cui/workspaces/BigWorld/programming/bigworld/lib/waypoint/`：

- `chunk_waypoint_set.cpp:33-107` 构造面（`neighbourEdges_` 三探针高度 +
  `v` 中点）；`connect`（cpp:293-324，首连接入表+目标侧 `addBacklink`、
  同集异 portal NOTICE 且**存储 portal 不变**、edgeLabel 无条件覆写）；
  `removeOurConnections`（cpp:120-149）、`deleteConnection`
  （cpp:151-195）、`removeOthersConnections`（cpp:198-225）、
  `addBacklink/removeBacklink`（cpp:228-252）。
- `waypoint_neighbour_iterator.cpp:12-70`：构造播种
  `currentEdgeIndex_ = adjacentRangeEnd(65535)`，`advance()` 先测
  `< edges_.size()` 再自增 —— 新生迭代器**天生已结束**。
- `chunk_waypoint_set_state.cpp:70-214`
  `ChunkWPSetState::getAdjacency`：门序 = 非容许且 blockNonPermissive →
  false → 继承 blockNonPermissive → 反向 portal 扫描 → 回链扫描 →
  `passedShellBoundary = pFromChunk != pToChunk && (!from外 || !to外)`
  （`pFromChunk = pSet_->chunk()`、`pToChunk = pPortal->pChunk`）→
  activated 读两侧 `hasChunkItem()` → 目标集卸载守卫 → goal 同集短路
  position 否则最近 `entryPoint_` 标签（严格 `<`）→
  `distanceFromParent_ = this->distanceToGoal(neigh)`（quirk：记的是
  父到 goal，非父到子）。
- `chunk_waypoint_state.cpp:92-118` 跨集臂：`edgeLabel(cwe)` 供目标
  集/waypoint，`crossedPortal_ = true`，`!pSet->chunk()` 拒绝；交叉点
  后 `wway(way.x, cw.maxHeight_, way.y)` —— 邻居点**骑源 waypoint 的
  maxHeight**。
- `astar.hpp/astar.ipp`：`AStar` 模板对两个 state 实例通用（本次抽出
  `AStarState` trait：`SearchData`/`Key` 关联类型 + close 哈希键泛化）。

## 移植面（本次落树）

- `crates/waypoint/src/chunk_waypoint_set.rs`（新）：`Portal
  { permissive, has_chunk_item, to_chunk: Option<usize> }`、`EdgeLabel`、
  `NeighbourEdge`、`ChunkWaypointConns` 与 `connect/delete_connection/
  remove_our_connections/remove_others_connections/add_backlink/
  remove_backlink` 全家、`WaypointNeighbourIterator`（含天生已结束怪癖）。
- `chunk_waypoint_set_state.rs`（新）：`ChunkWPSetState` + `AStarState`
  实现（Key=集合地址）。
- `chunk_waypoint_set_data.rs`：连接态落为数据记录上的 `pub(crate)
  RefCell` 字段（同 crate 合法）。
- `chunk_waypoint_state.rs`：跨集臂接 `edge_label`，`astar.rs` 泛化。

## 样本非手造

`gen_waypoints_cross.cpp`（571 行，勘误后落树版）对 el7 预编译库重编：

```sh
g++ -O1 -std=c++11 -fPIC \
  -I"$BW/lib" \
  -DCODE_INLINE -DMF_CONFIG='"hybrid"' -DMF_SERVER -DMF_USE_ASSERTS \
  -D_HYBRID -DBW_BUILD_PLATFORM='"el7"' \
  gen_waypoints_cross.cpp "$BW/lib/waypoint_generator/waypoint_view.cpp" \
  -L"$BW/build/el7/lib" -Wl,--start-group \
  -lwaypoint -lchunk -lscene -lresmgr -lmoo -lmath -lphysics2 -lterrain \
  -lcstdmf -lpng -lz -Wl,--end-group -o gen_cross
```

- **不可加 `-DCSTDMF_IMPORT`**：IMPORT 下 `bw_string.hpp` 的
  `extern template class basic_string<...,BW::StlAllocator>` 抑制本地
  实例化，而 `libcstdmf.a` 无显式实例化 TU，`BW::string` 构造与 `_Rep`
  成员全部链接失败；去掉后宏落回 `template class` 本地实例化。
- 复跑产物 md5 `537a2f64b3865145af9ab82a719eec22`，88 条真值行逐字
  复现（横幅 8 行走 stderr，落树 log 在 stdout）。
- 512 次 `incRef()` 钉住三集合：参照 freelist 对回收态二次析构（`free()`
  先 `~IntState`、链表析构再 `delete`），每次 A* 多掉一次 set 引用 ——
  与 `gen_waypoints.cpp` 同坑，算法面不受影响。

## 验收直接测试化

`tests/cross_chunk_search.rs` 5 例，真值 log 驱动：

1. 解码面对账 `HDR/WP/EDGE`（中心、min/max 高、顶点坐标、FFFF 邻接
   码）。
2. 布线面：按 `CONNTO`（portal 及 `to_chunk` 由此推导）+`EDGELABEL`
   逐行 `connect()` 重放，断言连接表、标签表、backlink（a:[B]、
   b:[A,C]、c:[B]）。
3. `ITER` 四条走查全 count 0（天生已结束）。
4. `XPATH` 六探针：`find_by_girth` 复算起终 waypoint 与真值一致后跑
   chunk 级 A*；AB/BA/BC 交叉点 (1/3, 0.6, 0.321429)/(1/3, 0.6,
   0.321429)/(2/3, 0.6, 0.369048) 逐位吻合；CB/AC `XPATHNONE`
   ——**`crossedPortal_` 一跳即封**（跨 portal 后邻接全拒，两跳路径
   不存在）；AA 单态。
5. `SETSEARCH` 五探针（**验收**）：SETHOP 的 set/位置/shell/activated/
   **portal 序号**逐列对账 —— AC0: A(0.15,0.41,0.4) → 经 portal 0 落
   B 标签最近点 (1/3,0.5,0.25) → 经 portal 2 落 C goal 位；CA0 反向
   3→1；BC0 单跳 2；AC1/AB1 封非容许后 `SETSEARCHNONE`。

单元面（lib 74 例新增 9）：构造面三探针高度/`v` 回绕/断言重入 panic、
跳过集内边、connect/backlink/NOTICE 覆写、delete（含未建 neighbour
edges 的 miss 容忍）、remove_our/others（含陈旧 backlink ERROR 臂）、
标志往返、迭代器新生即死与播种走查（集内→容许 portal→尽；非容许/
无标签/-1 三拒）、`ChunkWPSetState` 全门序（非容许门、activated 矩阵、
单向拒、卸载拒、无标签拒、最近标签、goal 短路、distanceFromParent
quirk）、默认态 compare/is_goal/search_key、goal-only 标签 0.2 nudge
（z 2.0→2.2）与卸载目标拒。

## 登记分歧

- **容器模型**：`connections_` BW::map→`BTreeMap<usize 地址,
  (Rc<集>, Rc<Portal>)>`；`edgeLabels_` unordered_map→`BTreeMap<
  EdgeIndex, _>` 升序。CA0 探针起点特意偏 z=0.6 避开 B 东侧两标签
  等距平局（unordered 分配序 vs BTree 升序会分叉）；等距平局时本口
  取更小 abs edge，已登记。
- **`connectionPortal` 不插表**：参照 `BW::map::operator[]` miss 会插
  入 null，本口答 `None` 且不动表。
- **`Portal` 建模**：`to_chunk: Option<usize>` 承 `pPortal->pChunk`
  （shell 公式 to 侧读 portal 而非目标集挂点，保持忠实）；shell 的
  to 侧 outside 标志读目标集的 `is_outside_chunk()`（本 crate 无 Chunk
  注册表，目标集即 portal 所指 chunk 的挂载者）。
- **回链扫描被吸收**：参照反向 portal 扫描走目标 **chunk** 边界表、
  回链走目标**集**连接表（两面可分叉）；本口两者同读一张连接表，
  portal 命中⟹回链成立，`getAdjacency` 的回链 `return false` 臂按
  构造不可达，保留以对齐参照门序（覆盖 99.74%，该行登记）。
- **chunk 面归 chunk 车道**：`bind()/readyToBind()/toss()`（portal 投
  影、`findExact`、ChunkNavigator、overlappers 走查）与跨集臂的
  chunk-box clamp（`inABit=0.01`，本夹具全部落在 [0,1]³ 内为 no-op）
  不落本口；`connect()` 为布线面，由真值重放驱动。
- **双交付**：terrain 席 `1e126c6`（feat）+`b7055f3`（tick）同面交付
  —— 本席按其勘误版夹具独立复现（md5+88 行逐字）后自口落树，主席
  合并时择一。

## 门禁

- `cargo test -p waypoint`：lib 74 + 集成 5 + navmesh_bin 8 全绿。
- llvm-cov 0.9.1（`cargo clean -p waypoint` 后
  `--package waypoint --lcov`，同 SF 多测试二进制 DA 按 (SF,行) max
  合并）：`chunk_waypoint_set.rs` 602/603=99.83%、
  `chunk_waypoint_set_state.rs` 380/381=99.74%、
  `chunk_waypoint_state.rs` 431/432=99.77%、`astar.rs` 349/353=98.87%、
  余 `girth/navloc/chunk_waypoint/chunk_waypoint_set_data` 100%——无文件
  <98%。余 10 行登记：astar 187/205/206/211（dup 重开 open 未命中臂 +
  祖先环守卫，欧氏 h 下不可达，04-04 起登记）、set_state 208（上列
  吸收臂）、state 187（`neighbouringWaypoint()<0 ⟺ adjacentToChunk()`
  同界，参照自身死码臂）、set 442（llvm-cov 对 `else if` 块尾大括号的
  区域伪 0，臂体 430-441 皆有计数）。
- clippy `-p waypoint --lib --tests` 0 自有告警；fmt 仅叶子文件全路径
  0 漂移。
- `cargo test --workspace` exit 0。
