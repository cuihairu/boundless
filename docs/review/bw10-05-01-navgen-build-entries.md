# BW-10-05-01 核查 waypoint flood 与 Recast 的构建入口

**验收** 记录算法、格式及启用条件。本档是参考树侧（原版 BigWorld 14.4.1）的
NavGen 与导航资源生成的构建入口核查，为 BW-10-05-02/03（移植 flood 与 Recast
生成）与 BW-10-05-04（NavGen 图形工作流）提供算法、格式与启用条件锚点。

**参考锚点**：`/home/cui/workspaces/BigWorld`，HEAD `93bb0837`
（`bigworld-14.4.1-359-g93bb0837`）。BW-00-02-01 记录的 b119c389 为该档撰写时的
参考快照；本档行号以当前 HEAD 为准。

## 1. 两个生成族及其宿主

原版有两条独立的导航网格生成路径，产物在持久化后共用同一存储格式（见 §3）：

| 生成族 | 库 | 生成器 | 宿主 |
|---|---|---|---|
| waypoint flood / generator | `lib/waypoint_generator/` | `WaypointFlood` + `WaypointGenerator` | `tools/navgen/`（独立工具）与 `tools/common/`（worldeditor 后台处理器） |
| Recast | `lib/navigation_recast/` | `RecastGenerator` | `tools/common/`（worldeditor `RecastProcessor`） |

生成器选择是**每空间**的设置（见 §4.2），不是编译期二选一：同一个空间在
`navmeshGenerator` 为 `navgen` 时走 flood 路径、为 `recast` 时走 Recast 路径。

## 2. 算法

### 2.1 waypoint flood / generator（`lib/waypoint_generator/`）

- `WaypointFlood`（`waypoint_flood.hpp`）：对碰撞场景做**采样网格洪泛**，每个格点
  至多 `MAX_HEIGHTS = 16` 层高度；每格每层存一个 `AdjGridElt`——8 方向各 4 位的
  邻接位图（`all` 32 位；`u/ur/r/dr/d/dl/l/ul` 每向 4 位）。`IPhysics` 接口提供
  `getGirth`/`getScrambleHeight`/`findDropPoint`/`isUnblocked`（含
  `DROP_FUDGE = 0.1f` 台阶容差）/`adjustMove`。
- `WaypointGenerator`（`waypoint_generator.hpp`）：把洪泛得到的邻接位图转成
  **BSP 切分多边形**（`BSPNode`：`splitNormal`/`splitOffset`/`front`/`back`/
  `pointInNode` 查询），再 `streamline()` 合并、`determineEdgesAdjacentToOtherChunks`
  跨块边、`extendThroughUnboundPortals` 延伸、`calculateSetMembership` 划集合
  （按种子点/实体位置）。`readTGA` 支持从 TGA 灰度图读入（无碰撞场景时的替代输入）。
- 种子点：`ChunkWaypointGenerator`/`NavmeshProcessor` 取块内实体位置
  （`EditorChunkEntityCache::getEntityPositions`）；navgen 工具另支持
  `NavGenUDO` 种子（见 §4.4）。

### 2.2 Recast（`lib/navigation_recast/recast_generator.cpp`）

标准 Recast 流水线，常量在 `RecastConfigInternal` 构造（recast_generator.cpp:222-244）：

```
CELL_SIZE=0.23f  CELL_HEIGHT=0.2f  REGION_MIN_AREA=0  REGION_MAX_AREA=400
MAX_VERTS_PER_POLY=6  DETAIL_SAMPLE_DIST=6.f  EDGE_MAX_ERROR=1.3f
SAMPLE_MAX_ERROR=1.f  EDGE_MAX_LENGTH=12.f
```

流程（generate，recast_generator.cpp:247-327 组装 `rcConfig`，393-528 执行）：

```
rcCreateHeightfield
rcMarkWalkableTriangles( walkableSlopeAngle )   // 每三角形可走区
rcRasterizeTriangles → rcFilterLowHangingWalkableObstacles → rcFilterLedgeSpans
→ rcFilterWalkableLowHeightSpans → rcBuildCompactHeightfield
→ rcErodeWalkableArea( walkableRadius )
→ rcBuildRegionsMonotone / rcBuildDistanceField+rcBuildRegions   // useMono 选择
→ rcBuildContours → rcBuildPolyMesh( MAX_VERTS_PER_POLY ) → rcBuildPolyMeshDetail
```

`rcConfig` 派生量（recast_generator.cpp:250-327）：`cs=CELL_SIZE`、`ch=CELL_HEIGHT`、
`walkableHeight=ceil(agentHeight/ch)`、`walkableClimb=floor(maxAgentClimb/ch)`、
`walkableRadius=ceil(agentRadius/cs)`、`maxEdgeLen=EDGE_MAX_LENGTH/cs`、
`maxSimplificationError=EDGE_MAX_ERROR`、`minRegionArea/mergeRegionArea`、
`detailSampleDist=DETAIL_SAMPLE_DIST`、`detailSampleMaxError=ch*SAMPLE_MAX_ERROR`、
`borderSize=walkableRadius+3`、块 `width=height=tileSize+2*borderSize`。

可走参数来自 `RecastConfig`（recast_generator.hpp）：`maxWalkableSlopeInAngle`/
`maxAgentClimb`/`agentHeight`/`agentRadius`/`scaleNavigation`/`gridSize`，由
`RecastProcessor` 从 **Girth** 与空间设置注入（recast_processor.cpp:111-113）。

`useMono`（recast_processor.cpp:115 传入）选区域算法：true→
`rcBuildRegionsMonotone`，false→`rcBuildDistanceField`+`rcBuildRegions`。

产物 `rcPolyMeshDetail` 随后 `convertRecastMeshToNavgenMesh(girth, chunk, navgenMesh)`
转成 navgen 格式网格（recast_processor.cpp:124-127）。

## 3. 输出格式（flood 与 Recast 共用）

导航网格持久化为块 `.cdata` 内的二进制节 **`worldNavmesh`**（`BinaryBlock`，
tag `BinaryBlock/IWaypointView`；Recast 侧 tag 为 `navmesh`，布局相同）。
`.chunk` 节引用它：`<worldNavmesh resource="<chunk>.cdata/worldNavmesh"/>`
（waypoint_view.cpp:275-276）。运行时工厂 `navmeshFactoryLink("worldNavmesh", ...)`
（chunk_waypoint_set_data.cpp:26）。

内容为**按 girth 串联的记录流**（写：`IWaypointView::asBinary`，
waypoint_view.cpp:308-404；读：`ChunkWaypointSetData::readWaypointSet` +
`navmeshFactory`，chunk_waypoint_set_data.cpp:398-418）。每条记录：

```
int32  version            // 必须 == 0（读侧 MF_ASSERT，chunk_waypoint_set_data.cpp:405）
float  girth
int32  numWaypoints       // = 本 girth 的多边形数
int32  numEdges           // = 顶点总数
// 每 waypoint（navPolyEltSize=12 字节）：
float  minHeight, maxHeight, int32 vertexCount
// 每顶点（navPolyEdgeEltSize=12 字节）：
float  x, z, int32 adjacency
```

`adjacency` 语义（waypoint_view.cpp:361-367）：>0 = 相邻多边形索引（重映射），
`65535` = 邻接另一 chunk 的门户，≤0 = 仅邻 vista（`~-aNavPoly`）。坐标经
`transformToLocal`（Matrix::identity，即块局部系）。写侧按
`numWaypoints*12 + numEdges*12 + 16` 逐记录推进。

写侧合并策略（`IWaypointView::saveOut`，waypoint_view.cpp:171 起）：新建记录流与
旧 `worldNavmesh` 合并——保留**非默认 girth** 的旧记录，替换当前 girth 记录
（`removeAllOld` 为真时全部替换）。因此一个 `.cdata/worldNavmesh` 可含**多个
girth** 的记录（运行时逐条解析，chunk_waypoint_set_data.cpp:398-418 循环）。

Recast 原始 `rcPolyMeshDetail`（`RecastGenerator::navmesh()`）是进程内中间产物；
**持久化格式统一为 navgen 记录流**（经 `convertRecastMeshToNavgenMesh`）。

## 4. 启用条件

### 4.1 构建配置（CMake，Windows-only）

- 根 CMake 非 Windows 即 `FATAL_ERROR`（见 BW-00-01-02）；配置经
  `BW_CMAKE_TARGET` 选择 `BWConfiguration_<name>.cmake`（CMakeLists.txt:126-140，
  INCLUDE 在 :170）。配置类型 `Debug;Hybrid`（CMakeLists.txt:22）。
- **`BW_CMAKE_TARGET=navgen`**（`build/cmake/BWConfiguration_navgen.cmake`）：定义
  `_NAVGEN`；库成员含 `waypoint_generator`；二进制成员 `navgen  tools/navgen`。
- **`BW_CMAKE_TARGET=tools`**（`build/cmake/BWConfiguration_tools.cmake:53-54`）：
  库成员含 `navigation_recast`、`waypoint_generator`、`tools_common`（供
  worldeditor/工具侧后台处理）。
- `tools/navgen/CMakeLists.txt`：`BW_ADD_EXECUTABLE(navgen WIN32 ...)` +
  `BWStandardMFCProject`——**Windows MFC 图形应用**（对应"原版 NavGen 有 Windows
  图形入口"）。链接 `waypoint_generator`、`chunk_scene_adapter`、`duplo`、`physics2`、
  `terrain`、`romp`、`opengl32`（`BW_SPEEDTREE_SUPPORT` 时另链 `speedtree`）。
  输出 `game/tools/navgen/<platform>/navgen(_d)`。
- `lib/waypoint_generator` 与 `lib/navigation_recast` 各自的 `CMakeLists.txt` 为
  标准 `BW_ADD_LIBRARY`；recast 侧 INCLUDE_DIRECTORIES 指向
  `third_party/recastnavigation/{Recast,Detour,DebugUtils}/Include`，链接
  `detour`、`recast`。

### 4.2 每空间设置 `navmeshGenerator`

- 解析：`BaseChunkSpace::navmeshGenerator(string)`（base_chunk_space.cpp:753-785）：
  `"none"`→`NAVMESH_GENERATOR_NONE`，`"recast"`→`NAVMESH_GENERATOR_RECAST`，
  空串或 `"navgen"`→`NAVMESH_GENERATOR_NAVGEN`，其余值 WARNING+回落 `NAVGEN`；
  已设值时改设报 ERROR。
- 枚举：`base_chunk_space.hpp:204-206`（`NONE`/`NAVGEN`/`RECAST`）。
- 默认：构造时 `NAVMESH_GENERATOR_NONE`（base_chunk_space.cpp:74）。
- 写入：`GeometryMapping::bind` 读空间设置串 `navmeshGenerator`
  （geometry_mapping.cpp:111）→ `pSpace->navmeshGenerator(...)`；worldeditor 打开
  世界时若未设 tag 则写 `"navgen"` 兼容旧世界（world_manager.cpp:3703-3707）；
  new_space 对话框新建空间写 `"navgen"`（new_space_dlg.cpp:418）。
- 运行时门（客户端读路径）：`ChunkSpace::addMapping` 读
  `clientNavigation/enable`（默认 false，chunk_space.cpp:514）；`navmeshFactory`
  在 `!isNavigationEnabled() || generator==NONE` 时跳过（chunk_waypoint_set_data.cpp:328-331）。

### 4.3 每块 dirty 触发

`EditorChunkNavmeshCacheBase`（tools/common/editor_chunk_navmesh_cache_base.cpp）：
`load` 读 `.cdata` 的 `navmeshDirty`（bool 二进制节）；`dirty()` 在
generator==NONE 时恒 false（:272）；`recalc()` 按 `navmeshGenerator()` 分发
`NavmeshProcessor`（NAVGEN，:300-305）或 `RecastProcessor`（RECAST，:306-311）；
完成后写 `worldNavmesh`（:131）+ `navmeshDirty`。触发为后台
`ChunkProcessorManager`（`IDLE_NAV_CALC_TIME=1.0` 秒空闲才重算，:30）。

### 4.4 girth 设置

- 来源：`Girths` 构造读 `helpers/girths.xml`（`AutoConfigString
  "editor/girthSettings"` 默认，girth.cpp:35-43）；缺文件 CRITICAL_MSG。
  旧路径 `bigworld/tools/misc/navgen_settings.xml` 已废弃（girth.cpp:39-42）。
- `Girth` 参数（girth.cpp:13-28）：根值 `girth`，`width`/`height`/`depth` 必填，
  `maxSlope` 默认 45°、`maxClimb` 默认 `height/3`、`always` 可选；非法尺寸
  CRITICAL_MSG。
- navgen 工具加载（navgen.cpp:3720-3741）：`Girths` → `g_girthSpecs`（按 girth 值
  建索引）、`g_girthsAlwaysGenerate`（带 `always`）、`g_girthsViewable`；
  **girth 0.5 必须在列**否则 CRITICAL_MSG（navgen.cpp:3735-3738）。
- 每块生成 girth 列表：`compileGirthsList`（navgen.cpp:1600-1625）=
  `g_girthsAlwaysGenerate` ∪ 覆盖该块的 `NavGenUDO::GirthSeeds`（girth+generateRange
  范围判定）；worldeditor 侧用 `girths()` 静态空表（editor_chunk_navmesh_cache_base.cpp:21-23，
  注：worldeditor 侧 girth 列表另行由空间编辑器注入，本核查未覆盖其注入点）。
- `NavGenUDO`（tools/navgen/navgen_udo.hpp）：世界内放置的种子 UDO
  （`GirthSeed{position,girth,generateRange}`），静态 `s_girthSeeds`。

## 5. 资源缺口（影响 BW-10-05-04 图形工作流与本批验证）

参考 res 树（`programming/fantasydemo/res/`，仅 `scripts/`+`server/`）不含：
`helpers/girths.xml`、`resources.xml`（`editor/girthSettings` 键）、任何空间/世界
（无 `.space`/`.chunk`/`.cdata`）、无 `helpers/` 目录。因此：
- navgen 工具启动即 CRITICAL_MSG（缺 girths.xml，girth.cpp:39-42）；
- 无空间可打开、无块可生成——BW-10-05-04 图形工作流与 BW-10-07 的"固定测试地图"
  需先由资产管线/内容侧提供一个含 girths.xml + 空间（含坡、洞、障碍、两相邻块）
  的演示世界，不在参考仓库交付面内。

## 6. 构建入口核查结论

- **flood 路径**：`BW_CMAKE_TARGET=navgen`（独立工具）与 `BW_CMAKE_TARGET=tools`
  （worldeditor 后台），均经 `waypoint_generator` 库；算法=采样网格洪泛
  （`WaypointFlood`，16 层/8 向邻接）+ BSP 多边形化（`WaypointGenerator`）。
- **Recast 路径**：`BW_CMAKE_TARGET=tools`，经 `navigation_recast` 库；算法=标准
  Recast 流水线（参数见 §2.2），产物转换回 navgen 网格格式。
- **格式**：统一为块 `.cdata/worldNavmesh` 的 version-0 记录流（§3），读侧校验
  `version==0`；多 girth 记录可同文件。
- **启用条件**：Windows CMake 配置（`navgen`/`tools`）+ 每空间 `navmeshGenerator`
  设置（`none|recast|navgen`，默认 `none`，worldeditor 兼容默认 `navgen`）+
  每块 `navmeshDirty` 后台触发 + `clientNavigation/enable` 运行时门（客户端）+
  `helpers/girths.xml` girth 列表（0.5 必含）。
