# BW-10-04-01 解码 waypoint set 和邻接 — 复核证据

- **任务编号**: BW-10-04-01（lane `lane/bw10-terrain`，todo.md 行 1370）。
  **验收条款**: 旧图节点与连接正确恢复。
- **参照权威树**: `/home/cui/workspaces/BigWorld/programming/bigworld/lib/`。
- **原版源码**:
  - 边与邻接编码: `lib/waypoint/chunk_waypoint.hpp:25-26, 64-113` ——
    `EdgeIndex` = `uint16`、`WaypointIndex` = `int32`；`Edge`
    两个成员 `vertexIndex_` / `neighbour_`；
    `neighbouringWaypoint()`（hpp:88-92）`neighbour_ < 32768 ? neighbour_
    : otherChunk`；`adjacentToChunk()`（hpp:101-104）
    `neighbour_ >= 32768`——**上界 `<= adjacentRangeEnd` 的判据被注释掉**，
    故 vista 编码（`~(-n)` 的顶位 cover 旗标）同样答 true；常量
    `adjacentRangeStart = 32768`、`adjacentRangeEnd = 65535`、
    `otherChunk = -1`（`chunk_waypoint.cpp:49-51`）；
    `HEIGHT_RANGE_TOLERANCE = 0.1f`（cpp:56）。
  - 中心点: `calcCentre`（`chunk_waypoint.cpp:365-387`）——
    逐边 `centre += (end + start) / 2 * length; totalLength += length;`
    再 `centre /= totalLength`；顶点经
    `provider.vertexByIndex(edges_[i].vertexIndex_)` 取，下一条边是
    `edges_[(i + 1) % edgeCount_]`。
  - 顶点供给面: `lib/waypoint/chunk_waypoint_vertex_provider.hpp` ——
    唯一纯虚 `vertexByIndex(EdgeIndex)`；实现方
    `ChunkWaypointSetData`（set_data.hpp:48-51，`MF_ASSERT(index <
    vertices_.size())`）。
  - 解码: `readWaypointSet`（`chunk_waypoint_set_data.cpp:677-743`）——
    `NAVPOLY_ELEMENT_SIZE = sizeof(float)+sizeof(float)+sizeof(int32)`
    （=12）；两游标：waypoint 控制流（`minHeight/maxHeight/vertexCount`）
    在前，`pEdgeData = pData + numWaypoints * 12` 盲加指针；
    边流每条 `float x, float y, int32 neighbour`（注释
    "'adj' already encoded as we like it"），`nedge` 分配游标按
    `vertexCount` 前推并切成 `DependentArray`；
    `VertexIndices`（cpp:462-530）以 `hash_string` 哈希 `Vector2` 的 8 个
    原始字节去重，`MAX_SIZE = (1<<(16))-1 = 65535`，超过则
    `CRITICAL_MSG` **打印后继续**（下标 `static_cast<EdgeIndex>` 回绕）；
    每 waypoint `calcCentre` + `visited_ = 0`；末尾
    `MF_ASSERT(numEdges == 0)`。
  - 多图遍历: `navmeshFactory`（cpp:755-800）——
    `index = 1; // yes, it starts from 1`，逐条记录读
    `int32 aVersion / float aGirth / int32 numWaypoints / int32 numEdges`
    后 `MF_ASSERT(aVersion == 0)`，建
    `ChunkWaypointSetData(index)`、`girth(aGirth)`、`readWaypointSet`，
    `index += numWaypoints`。
  - 绝对边下标: `getAbsoluteEdgeIndex`（cpp:663-672）
    `&edge - edges_`，`MF_ASSERT(index <= USHRT_MAX)`。
- **移植语义**（逐句核对后落点，新 crate `crates/waypoint`）:
  - `chunk_waypoint.rs`: `Edge { vertex_index, neighbour }` +
    三常量 + `HEIGHT_RANGE_TOLERANCE`；`neighbouring_waypoint()` /
    `adjacent_to_chunk()` 逐句照搬（含上界判据缺失的
    参照行为）；`ChunkWaypoint { min_height, max_height, centre,
    edge_start, edge_count, visited }` 对应
    `minHeight_/maxHeight_/centre_/edges_(DependentArray)/edgeCount_/
    visited_(mutable)`；`calc_centre(edges, provider)` 长度加权中点平均；
    `ChunkWaypointVertexProvider::vertex_by_index` 断言越界。
  - `chunk_waypoint_set_data.rs`: `ChunkWaypointSetData`
    （girth/waypoints/共享 edges/numEdges/baseIndex/vertices）；
    `read_waypoint_set` 两游标解码（控制流 + 边流）、顶点位模式去重、
    `VertexIndices` 溢出打印后继续；`parse_world_navmesh` 对应
    navmeshFactory 的多图循环（`baseIndex` 从 1 起累加）；
    `absolute_edge_index` 对应 `getAbsoluteEdgeIndex`。
- **边界登记**:
  1. **盲指针算术 → 截断上报**: 参照
    `pEdgeData = pData + numWaypoints * 12` 与 `consume<T>` 全是无界
    裸指针读（截断输入即越界 UB）。移植只护栏这一处指针运算，其余
    逐标量消费一律回报 `NavmeshDecodeError::Truncated`——与 physics
    `BspLoadError::Truncated` 同一登记。
  2. **MF_ASSERT → `assert!`**: version==0、`numEdges == 0`、
    绝对边下标 `<= USHRT_MAX`、`vertexByIndex` 越界四类断言平移为
    Rust panic（同 foundation `ConvexHull::new` 的 `numPlanes <= 32`
    惯例）。
  3. **顶点数超出声明边数**: 参照越过 `new Edge[numEdges]` 分配写入
    （UB），随后才由 `MF_ASSERT(numEdges == 0)` 收束；移植在写入前
    panic 出明确信息——断言构型下两者都是中止，登记。
  4. **顶点下标回绕**: 参照 `static_cast<EdgeIndex>(vertices_.size())`
    先回绕再判 `> MAX_SIZE`，只打 `CRITICAL_MSG` 并继续；移植用
    `eprintln!` 复现同一消息与「继续」语义（本仓库无共享消息宏，
    asset-pipeline 亦用 `eprintln!`）。
  5. **顶点去重键**: 参照 `hash_string` 哈希 8 原始字节、比较用浮点
    相等，故 `-0.0`/`0.0` 哈希不同却被当作不同键（尽管浮点相等）；
    移植以位模式 `u64` 为键复现该行为。
  6. **`DependentArray` → `(edge_start, edge_count)`**: 参照是
    「共享分配 + 指针/计数」，移植为 set 的 `Vec<Edge>` + 偏移/计数；
    `calc_centre` 因此需要共享边切片入参（它读
    `edges_[i].vertexIndex_`）。
  7. **`numEdges` 是预算计数器**: 按消耗递减，声明值与实际顶点数不符时
    两个方向各有一条收束路径（超额 → 分配溢出拒绝；不足 → 断言），
    与参照 `MF_ASSERT(numEdges == 0)` 语义一致。
  8. **顶点表按集重建**: 参照 `vertices_` 属 set data，故每个 set 的
    下标从 0 重新开始（夹具 SET1 的 4 个顶点重新编号，测试已钉住）。
  9. **不移植的面**: `WaypointStats` 计数（ctor/dtor/readWaypointSet 的
    addWaypoint/addEdges/addVertices、removeEdgesAndVertices）属统计
    单例；`navmeshFactory` 的 population 注册表 + 互斥锁 + chunk
    挂载 + 导航开关门 + 空 navmesh 分支属 chunk 装载面（04-04/04-05
    与其所在车道）。
- **验收直接测试化**: 「旧图节点与连接正确恢复」→
  `tests/navmesh_bin.rs` 回放原版引擎夹具：260 字节
  `world_navmesh.bin` 经 `parse_world_navmesh` 解码，与原版读者打印的
  `gen_waypoints.log` 逐行对拍（`HDR` 版本/girth/waypoints/edges、
  `SET` 计数与累加 `baseIndex` 1 → 4、`WP` 高低与 `calcCentre` 中心、
  `EDGE` 顶点下标/坐标/原始 neighbour、`ADJ` 集内链接与 vista → −1、
  `CHUNKBOUND` 65535 → true、`VISTA` FFFA/FFFD/FFF8 原样还原）；
  外加顶点去重面（set 0 六个唯一顶点，set 1 四个）与集内连接断言
  （A↔B、B↔C、C↔B、A/C 末边为边界）。
- **样本非手造**: `docs/review/fixtures/waypoint/gen_waypoints.cpp` 链接
  参照 el7 预编译库，驱动**原版** `IWaypointView::asBinary` 写出器
  （四多边形两 girth 集）与**原版**
  `ChunkWaypointSetData::readWaypointSet` 读者并打印探针行。
  编译标志同 cmake `flags.make`（`-DCODE_INLINE -DCSTDMF_IMPORT
  -DMF_CONFIG='"hybrid"' -DMF_SERVER -DMF_USE_ASSERTS -D_HYBRID
  -DBW_BUILD_PLATFORM='"el7"'`）；链接
  `-Wl,--start-group -lwaypoint -lchunk -lscene -lresmgr -lmoo
  -lmath -lphysics2 -lterrain -lcstdmf -lpng -lz -Wl,--end-group`
  （lib 组解析 `PlaneEq`/`Portal2D` 来自 libmath.a；`PlaneEq::param/
  intersect`、`Portal2D` 构造析构均在 libmath.a，先前判断有误已更正），
  并直接编译 `lib/waypoint_generator/waypoint_view.cpp` 以提供
  `IWaypointView::asBinary`（el7 无 libwaypoint_generator.a）。
  产物 260 字节，md5 `1265d4f348eb925699fda07904e5d389`；探针以
  `RESULT OK` 收尾。生成器二进制 `gen_waypoints` 不入库（同
  `bsp/gen_bsp`、`gen_portal` 惯例）。
- **门禁数字**:
  - `cargo test -p waypoint`: 12 lib + 2 integration = **14 passed**,
    0 failed。
  - `cargo clippy -p waypoint --all-targets -- -D warnings`: 0 error。
  - `cargo fmt -p waypoint -- --check`: 干净。
  - `cargo check --workspace`: 无 error。
  - `cargo llvm-cov --lcov -p waypoint`: chunk_waypoint 133/133 = 100%、
    chunk_waypoint_set_data 351/351 = 100%。
