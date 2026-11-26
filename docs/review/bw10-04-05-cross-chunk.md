# BW-10-04-05 实现跨 Chunk waypoint set 搜索

**验收**：同图 Portal 访问顺序吻合。

## 参照源码

`/home/cui/workspaces/BigWorld/programming/bigworld/lib/`：

- `waypoint/chunk_waypoint_set_state.hpp:26-81` + `chunk_waypoint_set_state.cpp:16-213`
  （`ChunkWPSetState` 三构造、`desc`、`compare`/`hash`/`isGoal`/`distanceToGoal`、
  `getAdjacency` 全门序）。
- `waypoint/chunk_waypoint_set.hpp:185-208`（`connectionPortal`/`connectionWaypoint`）+
  `chunk_waypoint_set.cpp:293-323`（`connect`：首连插入 + `pWaypointSet->addBacklink( this )`
  反向背链、同集异门 `NOTICE` 门不动、`edgeLabels_` 无条件覆写）、
  `228-249`（`addBacklink`/`removeBacklink`，miss 只 `ERROR`）、
  `116-143`（`removeOurConnections`，卸载面本项不建模）。
- `waypoint/waypoint_neighbour_iterator.hpp:1-50` + `cpp:13-77`
  （构造器 `currentEdgeIndex_( adjacentRangeEnd )` + `advance()` 增前守卫）。
- `waypoint/chunk_waypoint.cpp:44-45`（`adjacentRangeStart`=32768、
  `adjacentRangeEnd`=65535；`neighbouringWaypoint()`、`adjacentToChunk()` 上界注释掉——
  vista 边也按 chunk 邻接答）。
- `waypoint/chunk_waypoint_state.cpp:104-118`（chunk 级 `getAdjacency` 的 edgeLabel
  跨集臂：04-04 的答 false 桩本项填真）、`196-205`（chunk 盒 ±0.01 回拉，登记不建模）。
- `waypoint/navigator.cpp:270-300`（消费面：先 `ChunkWPSetState` 搜集跨 chunk 序列，
  再逐集 `ChunkWaypointState` 细搜）。
- `waypoint/astar.ipp:37-197,208-217`（04-04 泛型 A* 的第二实例化侧）。

## 移植面

`crates/waypoint/src/`：`chunk_waypoint_set_state.rs`（新）、`chunk_waypoint_set.rs`（新）、
`chunk_waypoint_set_data.rs`（连接/标签/背链/挂载身份四字段）、
`chunk_waypoint_state.rs`（edgeLabel 臂填真 + `AStarState` 实现）、`astar.rs`（泛型化）：

| 参照 | 移植 | 逐句要点 |
| --- | --- | --- |
| `ChunkWPSetState` | 同名结构 | 默认 + `NavLoc` 两构造；`compare` 集合指针差（只消费 `==0`）、`hash`/`search_key` 集地址（空集 0）、`isGoal` 集同一性（双空集同真）、`adjacenciesBegin/End` 即连接数、`distanceToGoal` 纯欧氏长度 |
| `getAdjacency` | `get_adjacency` | 门序逐句：越界假 → **非许可门**（仅 `blockNonPermissive_` 置位时拒）→ 邻态继承 block 标志 → 伙伴门扫描（登记，见下）→ 背链互连复核 → shell 判定 `fromChunk != toChunk && (!fromOutside \|\| !toOutside)` → 激活旗 `portal.hasChunkItem \|\| back.hasChunkItem`（仅 shell 内）→ **chunk-gone 守卫在挂邻态之后**（参照先设再查同序）→ 位置：goal 集 `position_ = goal.position_` 短路跳过标签扫描，否则 `dist = (entryPoint_ − position_).length()` 严格 `<` 取最近标签入口；`distanceFromParent_ = this->distanceToGoal(neigh)` |
| `ChunkWaypointSet::connect` | `ChunkWaypointSetData::connect` | 首连插入 + **背链落目标集**（`pWaypointSet->addBacklink( this )`，交付勘误一次：初版误落本方，参照 307 行拨正）；同集异门 `NOTICE_MSG` 打印 + 存储门保持首个；`edgeLabels_[index]` 无条件覆写（同门同边亦覆写） |
| `addBacklink`/`removeBacklink` | 同名 | 目标集 `push_back`；miss 查找删除 + `ERROR_MSG` 打印 |
| `connectionPortal`/`connectionWaypoint`/`edgeLabels_` | 同名 + `EdgeLabel` | `connectionWaypoint` 等价于 `edge_label(edge).set`；miss 臂即参照静态 `s_none`，端口答 `None` |
| `WaypointNeighbourIterator` | 同名 | 逐句移植含**增前守卫的参照怪癖**（见样本段）；体覆盖由同模块测试直种私有游标 |
| chunk 级 edgeLabel 臂 | `ChunkWaypointState::get_adjacency` | `edge_label(absolute_edge)` → `None` 答 false（s_none）；命中则 `neigh_set = label.set`、`navLoc_ = (label.set, label.waypoint, label.entryPoint)`、`crossedPortal_ = true`、chunk-gone 守卫；重建 NavLoc 时骑**源** waypoint 的 `maxHeight_`（参照 `wv` 沿 `cw.maxHeight_` 同式） |
| A* 泛型化 | `AStar<S: AStarState>` | 04-04 的具象 `AStar<ChunkWaypointState>` 抽成 trait（`SearchData`/`Key`/七方法），search 体逐行不变；`ChunkWaypointState`/`ChunkWPSetState` 各自实现，navigator 式两级搜索自此可用 |

**登记边界（容器模型，同答案）**：参照 `connections_` 是 `BW::map`（指针序遍历）→
`BTreeMap<usize, (Rc, Rc<Portal>)>` 按地址键（真值 CONNTO 打印序即指针序，端口 BTree
序不参与契约断言）；`edgeLabels_` 是 `std::unordered_map`（分配序遍历）→ `BTreeMap`
升序（CA0 平手裁决不同，见样本段勘误三——真值已按严格胜者布点，顺序无关）；
`backlinks_` 保持强引用 `Vec<Rc>`（参照 `ChunkWaypointSetPtr` 同为强句柄，环由卸载面破）。

**登记边界（建模面）**：
- 参照 `getAdjacency` 的伙伴门扫描目的地 chunk 边界门列表；端口读**反向连接的门户**
  替代——扫描仅有的两个可观测效果（miss 答假、`hasChunkItem` 供激活旗）恰好都是反向
  连接记录携带的，扫描未中 ⟹ 互连也未中（bind 建成的连接恒成对）。
- chunk 身份：参照读 `chunk()->id()`；端口由挂载方注入 `chunk_id`（`Option<usize>`，
  `None` 即参照的 `chunk()` 已卸载 = chunk-gone 守卫臂）；`desc` 的 chunk 标识符以
  `baseIndex` 顶替（`ChunkWaypointState::desc` 同式登记）。
- `ChunkWPSetState(pSet)` 构造器定位 `chunk()->centre()`，需 chunk 挂载面，延后
  chunk 车道；端口提供本项两消费面用到的默认与 `NavLoc` 构造。
- `Portal` 端口只保留 `permissive`/`hasChunkItem` 两旗——本项读面仅此二者；
  uAxis/vAxis/origin 几何属 chunk 车道。
- chunk 盒 ±0.01 回拉分支（04-04 登记）维持不建模。
- 空标签臂（`closestEdge` 未置答 false）参照同为防御臂：真 `connect` 恒写标签，
  公开面不可达；端口同臂保留，覆盖由测试清 crate 私有 map 直达。

## 样本非手造

`docs/review/fixtures/waypoint/gen_waypoints_cross.cpp`（`gen_waypoints_cross.log`；
blob `world_navmesh_cross.bin` 288 字节 md5 `537a2f64b3865145af9ab82a719eec22`）：
三集世界（A 单 waypoint 方块 x∈[0,⅓]、B 双 waypoint 条带 z=½ 对分、C 单方块
x∈[⅔,1]；girth 0.6、高 0.2–0.6、CW 绕向），真 `Chunk::load(NULL)` FNF 挂载分支 +
`ChunkWaypointSet` ×512 incRef 钉活 + 快构造 `PlaneEq((1,0,0), ⅓/⅔)` 矩形门户 +
`ChunkBoundary` + `bindPortal(0)` 进 `joints()`，**原版 `ChunkWaypointSet::bind()`
建成全部连接与边标签**（P_AB 对非许可、P_BC 对许可），原版 `ChunkWPSetState` +
原版 `AStar::search` 打真值。编译配方沿 04-01（flags.make 定义组 + `--start-group`
`-lwaypoint -lchunk -lscene -lresmgr -lmoo -lmath -lphysics2 -lterrain -lcstdmf` 链接组）。
真值清单：

- `HDR/SET/WP/EDGE` —— 三集解码（base 1/2/4、waypoint 1/2/1、B 条带 0001/0000
  互邻）；端口 `parse_world_navmesh` 逐行对拍（含 `wp.centre` 0.166667/0.5）。
- `PORTAL/CONNTO/EDGELABEL` —— bind 产物 4 门 6 连接 6 标签；`EDGELABEL` 入口点
  全是 bind 探针点（y=0.5 = 首次绑定的 wsv 高度，**非门户投影**——真值钉死）。
- `ITER` ×4 全 `count 0` —— **参照迭代器天生即死**：构造器种 `currentEdgeIndex_ =
  adjacentRangeEnd`(65535)，`advance()` 守卫 `while(!pNeighbourSet_ && currentEdgeIndex_
  < edges_.size())` **增前判定** → `65535 < 4` 假 → 循环体从不执行（uint16 回绕妙计
  被守卫位置挫败）；唯一消费面 navigator.cpp:278 的 for 模式也从不逐出。端口逐句
  复刻同怪癖，体覆盖由测试直种私有游标。
- `XPATH` AB/BA/BC 三跨成功（交点 ⅓/⅔ 处 0.321429/0.369048，手算 t=0.678571/0.261905
  吻合）、CB/AC `XPATHNONE`（交叉后状态拒再扩）、AA 单步。
- `SETSEARCH/SETHOP` ×5 —— **验收钉面**：AC0 过 [p0,p2]、CA0 过 [p3,p1]、
  BC0 过 [p2]、AC1/AB1 堵（`blockNonPermissive` 下 A 的唯一连接 p0 非许可）；
  B 途经态位置 = 距当前态最近的标签入口。

**生成器勘误**：① `AStar::first()/next()` 共享成员游标 `iter_`——探针段嵌套
`first()/next()` 走链会清掉外层走链（引擎 `next()` 空守卫故不炸只截断），改为单趟
收进数组再打印；② CA0 起 (0.85,0.4,0.5) 处 B 两标签入口 d² 精确平手（0.104211²
由 unordered_map 分配序裁决），起锚移 z=0.6 使 B1 东入口严格胜（0.0642 vs 0.164），
**算法未动**，端口 BTree 升序自此顺序无关；③ 生成器二进制与 assert 日志不入库。

## 验收直接测试化

`tests/cross_chunk_search.rs` 五测逐行重放真值：

- `the_fixture_decodes_three_sets_as_logged` —— HDR/SET/WP/EDGE 全对拍
- `the_bind_wiring_restores_connections_and_labels` —— CONNTO/EDGELABEL 经端口
  `connect()` 复建再回读（6 标签 set/waypoint/入口逐点、背链 1/2/1、同集异门门不动）
- `the_neighbour_iterator_matches_the_reference_walk` —— ITER ×4 即死复刻
- `the_chunk_level_paths_cross_the_portals_as_logged` —— XPATH 六探针逐点
- `the_set_level_search_matches_the_portal_access_order` —— SETHOP 五探针，途经
  门户用 `connection_portal` 指针同一性断言 =「**同图 Portal 访问顺序吻合**」

单元面：`connect` 首连/背链落目标/`removeBacklink` miss `ERROR`/`NOTICE`+标签覆写、
迭代器天生即死 + 直种游标走链（in-set 先于许可门户）、集级门序
（非许可仅置位拒、单向连接拒、未挂载拒、空标签拒、goal 集位置短路、最近标签 argmin、
shell 公式三态、激活旗三态含同 chunk 与双 outside）。

## 门禁

lib 64 + 集成 5（cross_chunk）+ 8（navmesh_bin）= 77 全绿；llvm-cov 行覆盖
crate 合计 2555/2568 = **99.49%**，无文件 < 98%（`astar.rs` 98.87%、
`chunk_waypoint_state.rs` 98.77%、`chunk_waypoint_set_state.rs` 99.08%、
`chunk_waypoint_set.rs` 99.71%，余 4 文件 100%）；clippy `-D warnings` 0、
fmt 0 漂移；`cargo check --workspace` 过、test-support 593+3 绿。
