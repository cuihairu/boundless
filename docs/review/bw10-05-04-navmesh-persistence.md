# BW-10-05-04 分片一：navmesh 持久化（asBinary / saveOut 合并）

> **BW-10-05-04 实现原版 NavGen 图形工作流**。**验收** 同图可选择目标、生成和保存。

**分片状态**：本片交付工作流的**保存臂**——记录流写入器
`IWaypointView::asBinary` 与 `saveOut` 的旧记录合并算术。分片二（见
下）已交付 `WaypointGenerator` BSP 多边形化主体（generate /
generatePoints / generatePolygons / generateAdjacencies / joinPolygons
/ streamline / calculateSetMembership）。分片三（见下）交付
`tools/common/navmesh_processor.cpp` 的 per-girth 工作流环与
`editor_chunk_navmesh_cache_base` 保存面——至此本席 05-04 面齐
（可选择 girth 目标 → 逐 girth 生成 → 合并保存）。**双交付警示**：
门户两臂与 `readTGA` 读入侧已由 lane/bw02-mercury e9e9cf5 交付
（嫁接地形席 c3b005a），bw16-18 席亦交付 workflow.rs GirthWorkflow
环——合并时主席择一。BW-10-05-03 Recast 移植范围仍挂起待用户拍板。

## 产物对照（验收面）

参照 harness `docs/review/fixtures/waypoint/gen_view.cpp` 把**未改动**的
原版 `lib/waypoint_generator/waypoint_view.cpp`（`cmp` 逐字节验证一致）
编进影子桩（`/tmp/viewstub`：chunk/resmgr 面全部内存化），跑 5 多边形
2 集合的测试场景，倾倒 `VIEW/ASBINARY/R/P/V/HX/CDATA1/CDATA2/CHUNK` 进
`docs/review/fixtures/waypoint/gen_view.log`（已提交，含每 16 字节一行
的 `HX` 十六进制逐字节对照）。Rust 侧
`crates/waypoint-gen/src/view/artifacts.rs` 同场景同倾倒格式重跑，
**逐行全等**（记录流字节级一致：ASBINARY 260 字节
fnv `9277bbe01734b419`、CDATA2 合并流 304 字节 fnv
`16029c0e097e371b`）。

场景覆盖 `asBinary` 的**全部邻接分支**（邻接域是 1 基全局多边形号）：

| 顶点 | 原始 adj | 输出 | 分支 |
|---|---|---|---|
| P0 v0 | 1（自身） | 0 | 同集合重映射 `indexMap[1]=0` |
| P0 v1 | 3（P3，异集合） | 0 | `std::map operator[]` 幻影插入默认 0 |
| P0 v2 | 0 + 跨块 | 65535 | `adjToAnotherChunk` |
| P0 v3 | -1 | -2 | vista `~ -adj` = adj-1 |
| P1 v0 | 2（P2） | 1 | 同集合重映射 `indexMap[2]=1` |
| P1 v1 | 7（>5） | 7 | 越界原样透传（三分支都不中） |
| P1 v2 | -5 | -6 | vista |
| P3 v3 | 5（P4 同集合） | 1 | 重映射 `indexMap[5]=1` |
| P4 v1 | 1（P0 异集合） | 0 | 幻影插入 |

girth 回退：`getGirth(0)=0` → 落 `defaultGirth 0.8`；`getGirth(1)=1.2`。
记录按集合号升序（参照 `BW::map` 序），每记录
`[int32 version 0][float girth][int32 nwp][int32 ne][全部 ctrl][全部
data]`——ctrl/data 两段**先后打包**（waypoint_view.cpp:394-400 的
memcpy 序），不是逐多边形交错。

`saveOut` 两趟：

- **趟 1**（无 cdata）：从零建 `.cdata`，写入与 ASBINARY 全同的流；
  `.chunk` 节旧节清空（boundary/navPolySet 删除），写入
  `worldNavmesh/resource = gen_view.cdata/worldNavmesh`。
- **趟 2**（`removeAllOld=false` + 旧流 [0.8 nwp1ne1][1.5 空][0.6
  nwp0ne1]）：合并结果 304 字节 = 新流 260 + 保留 16 + 28——**0.8 旧
  记录（默认 girth）被新流取代**，1.5/0.6 按旧序跟随其后。

## 参照源码

| 参照 | 位置 |
|---|---|
| `PairOfStreams` + `NavPolySets_asBinary_map` | waypoint_view.cpp:288-306 |
| preflight：girth 回退、`indexMap[p+1]=count++` | waypoint_view.cpp:313-327 |
| 主循环：ctrl `mhv.y<<Mhv.y<<vertexCount`、data `nc.x<<nc.z` | waypoint_view.cpp:330-385（346-355,358-366） |
| 邻接三分支 + 越界透传 | waypoint_view.cpp:368-374 |
| 记录打包 `[version][girth][nwp][ne][ctrl][data]` | waypoint_view.cpp:388-404（394-400 memcpy 序） |
| `saveOut` 旧流索引 + bit31 标记 + keepSize | waypoint_view.cpp:190-218（211,212,217） |
| 合并追加 `oldParts[i]>>31` / `& ~(1<<31)` | waypoint_view.cpp:227-241 |
| `saveOut` 无 cdata 建节、`.chunk` 旧节清扫、resource 串 | waypoint_view.cpp:243-281 |
| `VertexRef::getVertex` 网格缩放 | waypoint_view.hpp:83-118 |

## 移植面

| 参照 | Rust |
|---|---|
| `IWaypointView::asBinary` | `view::as_binary`（`view.rs:65`） |
| `NavPolySets`（`BW::map<int,PairOfStreams>`） | `BTreeMap<i32, SetStream>`（同序） |
| `PolygonRef`/`VertexRef` 读面 | `NavView` trait + `NavVertex`（世界坐标已含 `getVertex` 缩放） |
| 邻接三分支 + 65535 常量 | `ADJ_ANOTHER_CHUNK`（`view.rs:31`） |
| `saveOut` 合并臂 | `view::save_out_records`（`view.rs:189`） |
| 旧流解析（16B 头 + elt 计数步进） | `parse_old_parts`（`view.rs:156`） |
| bit31 丢弃标记 | 直接按 girth 判（语义同 `oldParts[i]>>31` 测试） |

## 测试与门禁

- `cargo test -p waypoint-gen`：**43 通过 / 0 失败**（40 旧 + 1 产物逐行
  对照 + 2 合并单元测：保留序断言、全默认 girth 旧流/`removeAllOld`
  提前返回）。
- 覆盖（llvm-cov 0.9.1，`--lcov` DA 计数）：`view.rs` **100.00%**
  （90/90）；`view/artifacts.rs` 98.18%（5 行 DA:0 = 306-315 比较失败
  报告臂，测试永不走进）；`flood.rs` 99.19% 不变。
- `cargo test --workspace`：**4392 通过 / 0 失败**；`cargo test -p
  test-support` 绿；`cargo fmt --check -p waypoint-gen` 绿；
  `cargo clippy -p waypoint-gen --all-targets -- -D warnings` 绿。

## 登记边界

- **MF_ASSERT 怪癖**：dev 构建下 `MF_ASSERT(offset <= INT_MAX)`
  （waypoint_view.cpp:212）对旧流里任何默认 girth 记录都会触发——
  bit31 标记后的 offset（如 40 | 0x80000000）大于 INT_MAX。release
  构建该断言编译掉，合并按预期丢弃默认 girth 记录。移植采用 release
  语义，harness 用 `-D_RELEASE` 编译。
- **`writeToSection`（旧 XML 面）未移植**：navgen GUI 写 `.navpoly`
  XML 的遗留路径，headless 工作流不经过；如后续 05-05/06 需要再评估。
- **resmgr/chunk 桩面**：`BinaryBlock`/`BinSection`/`BWResource`/
  `DataSection`/`Chunk` 在 harness 里是内存桩（`/tmp/viewstub`），只
  服务编译原版源码与驱动逻辑；`saveOut` 的磁盘格式（BinSection 序列化）
  与 BWResource 缓存不在移植面内。桩 `DataSection::save` 语义为内存
  重登记。
- **MemoryOStream `<<` 是裸 memcpy**（binary_stream 模板 addToStream），
  x86 上即小端——Rust 用 `to_le_bytes()` 同值。
- **截断旧流**：参照对 nwp/ne 超出缓冲是越界读（UB）；Rust 侧解析
  `try_into`/切片会 panic。登记不改（真产物不含截断流）。
- **跨席已知红**：workspace 全量 clippy 红在 `message_logger`
  （bw16-18 合并遗留），不在本席依赖链。

## Harness 复现

```sh
BW=/home/cui/workspaces/BigWorld/programming/bigworld
mkdir -p /tmp/viewstub/{cstdmf,resmgr,chunk,waypoint_generator}
cp $BW/lib/waypoint_generator/waypoint_view.{hpp,cpp} /tmp/viewstub/waypoint_generator/
# 影子桩：pch.hpp（把 memory_stream 链提升到全局作用域，避免
# waypoint_view.cpp:285 的文件中段 include 嵌套 BW::BW）、
# chunk/chunk.hpp（owns+space+transformInverse/centre/binFileName/
# resourceID/identifier）、resmgr/{binary_block,datasection,bin_section,
# bwresource}.hpp（内存树 + BWResource 路径->节 store；桩类需
# incRef/decRef/refCount 三件套以配 SmartPointer）
g++ -std=c++14 -O2 -o /tmp/viewstub/gen_view \
  docs/review/fixtures/waypoint/gen_view.cpp \
  /tmp/viewstub/waypoint_generator/waypoint_view.cpp \
  -I /tmp/viewstub -I /tmp/viewstub/waypoint_generator -I $BW/lib \
  -DCODE_INLINE -D_RELEASE -DHANDLE=void* -DINVALID_HANDLE_VALUE=NULL -DBW_GUARD= \
  $BW/build/el7/lib/libcstdmf.a $BW/build/el7/lib/libmath.a -lpthread
/tmp/viewstub/gen_view 2>/dev/null > docs/review/fixtures/waypoint/gen_view.log
```

（stderr 走原版 INFO_MSG；stdout 才是 log。`cmp` 验证
`/tmp/viewstub/waypoint_generator/waypoint_view.cpp` 与参照逐字节一致。）
产物二进制不入库。

---

# 分片二：WaypointGenerator BSP 多边形化（generate → streamline → set）

## 产物对照（验收面）

参照 harness `docs/review/fixtures/waypoint/gen_generate.cpp`：**未改动**
的原版 `waypoint_flood.cpp` + `waypoint_generator.cpp` +
`waypoint_view.cpp`（`cmp` 逐字节验证一致）全链路驱动——原版 flood 灌
05-02 两场景（slope 0.5 步进斜坡；shell 室内外壳 + 室内地坪 + 室内桥），
网格 memcpy 进原版 `WaypointGenerator`，跑 `init → generate() →
asBinary → streamline() → calculateSetMembership() → asBinary`，倾倒
`GENERATED/GENASBIN/STREAMLINED/SETMEM` 与最终记录流（`HX` 每 16 字节
一行）进 `docs/review/fixtures/waypoint/gen_generate.log`（228 行，已
提交）。Rust 侧 `generator/artifacts.rs` 同场景同倾倒格式重跑，**逐行
全等**：slope 终流 136 字节 fnv `3a23247435af66a6`；shell 终流 152 字节
fnv `e01640188f0c6115`（两记录 girth 0.8，nwp 1 ne 4 各一）。

| 阶段 | slope | shell |
|---|---|---|
| FILL | 1 种子 72 格 | 3 种子 21/6/12 格（SCENE 39） |
| GENERATED | 2 片（vc 6+6，streamline 前含重复顶点） | 6 片（**P3 vc 0 空片**） |
| GENASBIN | 184 B fnv `92d674f4a42fa41e` | 328 B fnv `988ecf841c555b54` |
| STREAMLINED | 2 片 vc 4+4 | 5 片（空片被剔） |
| SETMEM | 2 片 1 集合 | 2 片 2 集合（0.5 地坪 set 1、1.5 桥 set 2；3 片 set 0 被物理剔除） |
| 终流 | 136 B fnv `3a23247435af66a6` | 152 B fnv `e01640188f0c6115` |

asBinary 邻接重映射同分片一口径：slope 互惠对 `V2 adj 1`（P0→P1，
`indexMap[2]=1`）↔ `V11 adj 0`（P1→P0，`indexMap[1]=0`），其余无邻
顶点输出 -1。

## 参照源码

| 参照 | 位置 |
|---|---|
| `PointDef`/`EdgeDef` 排序梯 + `nearlySame`/`ptNearEnough` | waypoint_generator.cpp:39-186 |
| `init`/`generate`/`initBSP`/`nodesSame` | waypoint_generator.cpp:339-476 |
| `processNode`（og 直用 `hgtGrids_[og][ptindex+dx[a]+dy[a]]`，窗口 `h∈[y1,y2]`） | waypoint_generator.cpp:477-702（558-568） |
| `doBestHorizontalSplit` | waypoint_generator.cpp:703-805 |
| `averageHeight`/`findDispoints`/`calcSplitValue`/`splitNode` | waypoint_generator.cpp:806-1159 |
| `refineNodeEdges`/`reduceNodeHeight` | waypoint_generator.cpp:1160-1281 |
| `calcEdge`/`generatePoints`/`generatePolygons` | waypoint_generator.cpp:1407-1704 |
| `generateAdjacencies`（高度**重叠**过滤，非包含） | waypoint_generator.cpp:1705-1795（1769-1770） |
| `isConvexJoint`/`findPolygonVertex`/`fixupAdjacencies`/`joinPolygon`/`joinPolygons` | waypoint_generator.cpp:2064-2369 |
| `streamline`（剔空片 `polygons_.erase`） | waypoint_generator.cpp:2733-2826（~2752） |
| `calculateSetMembership`（尾段剔 set 0 片） | waypoint_generator.cpp:2827-3040（~3001） |
| `AdjGridElt::angle` 原值存邻居槽号 | waypoint_flood.hpp:17-22 |

## 移植面

| 参照 | Rust |
|---|---|
| `WaypointGenerator` + 全管线 | `generator::WaypointGenerator`（`generator.rs`，2056 行） |
| `SplitDef`（`ph`/`se` union） | `SplitDef.ph` 单字段 + `VERTICAL_SPLIT = 8`（union 两视读同内存） |
| `processNode` 显式索引栈 | `process_node`（`generator.rs:548`） |
| `BSPNode` 指针树 | `BspNode` Vec 池 + 子索引 |
| `generateAdjacencies` | `generate_adjacencies`（`generator.rs:1486`） |
| `joinPolygon(s)`/`fixupAdjacencies`/`isConvexJoint`/`findPolygonVertex` | `join_polygon`（`generator.rs:1573`）等四件 |
| `streamline` / `calculateSetMembership` | `streamline` / `calculate_set_membership`（`generator.rs:1821`） |
| `asBinary` | 复用分片一 `view::as_binary`（generator 自实现 `NavView`） |
| `readTGA`（:252）/门户两臂（:2370/:2628）/per-girth 工作流环 | 未移植（余片） |

## 测试与门禁

- `cargo test -p waypoint-gen`：**67 通过 / 0 失败**（43 旧 + 24 新：
  产物全等对照 + 管线单元/怪癖测试——join 合并修复与跳过臂、
  split 上限行走、refine 退化扫描回退、孤立格与零深度节点跳过、
  moat 场景、set 成员重访、比较器失败报告等）。
- 覆盖（llvm-cov 0.9.1，`--lcov` DA 计数）：`generator.rs` **99.31%**
  （1291/1300；9 行 DA:0 = 100/721/801/803/827/829/847/848/1478，全部
  为体已覆盖的嵌套控制流收尾 `}`——bw08 已档的 llvm-cov 永零区）；
  `generator/artifacts.rs` **100%**（602/602）；`flood.rs` 99.19% 不变
  （05-02 基线）；`flood/artifacts.rs` 95.65%→**100%** 与
  `view/artifacts.rs` 98.18%→**100%**（真值比较器提函数 +
  `should_panic` 探针，行为覆盖非死码豁免）。
- `cargo test --workspace`：**4416 通过 / 0 失败**；`cargo fmt --check`
  绿；`cargo clippy -p waypoint-gen --all-targets -- -D warnings` 绿。

## 登记边界

- **`og` 原值怪癖**：`AdjGridElt::angle` 存的是邻居的**槽号原值**
  （非编码角度），`processNode` 直接 `hgtGrids_[og][ptindex+dx[a]+dy[a]]`
  索引（waypoint_generator.cpp:558-568）。0 = 无邻；flood 灌层自
  MAX_HEIGHTS-1（15）向下分配槽，故 0 槽永不撞真层。手写测试网格须按
  此约定落层（artifacts 测试把可行高度放槽 1、angle 写 1）。
- **邻接高度过滤是重叠不是包含**：`generateAdjacencies` 仅当
  `maxHeight < minHeight || minHeight > maxHeight` 才 continue
  （waypoint_generator.cpp:1769-1770），即高度区间有交即候选。
- **`ptNearEnough` 高度窗 +1.5/−0.5**（waypoint_generator.cpp:143）：
  种子落点与多边形高差出窗即不中标（shell 里 y=10 种子落点够不到
  0.0 地面片，故 SETMEM 仅剩两片）。
- **空片读 v0 = release UB**：`joinPolygon` 对 `vertexCount == 0` 的
  多边形读 v0（waypoint_generator.cpp:2126 起）；shell 场景真造出一片
  vc 0（GENERATED P3）且原样进 GENASBIN（nwp 6）。Rust 侧守卫为
  (0,0) 顶点，登记不改（真产物可含空片）。
- **streamline 剔空片**：vc 0 多边形在 streamline 丢弃
  （`polygons_.erase`，shell 6→5）。
- **`calculateSetMembership` 尾段剔 set 0 片**：集合指派完成后 set 0
  多边形 `polygons_.erase` 物理剔除（shell 5→2）；无 frontier → 最大
  bbox 单集合；未互惠邻接清零。
- **joinPolygon 顶点角语义**：顶点 angle = 它**闭合**的那条边方向
  （南 6/东 0/北 2/西 4），`v.adj` = 自 v 起边的对面多边形；同角连段
  = 一条合并段，`singleAdjacency` 要求整段所有顶点同 adj>0。
- **`doBestHorizontalSplit` 调用约定**：调用方已弹出索引，栈到达时为
  空；落空（bestSplit < 0）时子节点 `waypoint=false`、不回压。
- **死列行为**：不可行列使 `calcSplitValue` 落空 → 节点不成为
  waypoint（gap 列不产多边形）；空节点 `averageHeight = (0.0, 0)`。
- **MF_ASSERT**：release 语义（harness 用 `-D_RELEASE` 编译），同
  分片一。

## Harness 复现

```sh
BW=/home/cui/workspaces/BigWorld/programming/bigworld
mkdir -p /tmp/genstub/{cstdmf,resmgr,chunk,waypoint_generator}
cp $BW/lib/waypoint_generator/waypoint_{flood,generator,view}.{hpp,cpp} \
  /tmp/genstub/waypoint_generator/
# 影子桩同分片一 /tmp/viewstub，另需 05-02 的
# cstdmf/{bw_set,bgtask_manager}.hpp 桩与 flood 补丁（见
# docs/review/bw10-05-02-flood-generation.md 的 Harness 复现）
g++ -std=c++14 -O2 -o /tmp/genstub/gen_generate \
  docs/review/fixtures/waypoint/gen_generate.cpp \
  /tmp/genstub/waypoint_generator/waypoint_flood.cpp \
  /tmp/genstub/waypoint_generator/waypoint_generator.cpp \
  /tmp/genstub/waypoint_generator/waypoint_view.cpp \
  -I /tmp/genstub -I /tmp/genstub/waypoint_generator -I $BW/lib \
  -DCODE_INLINE -D_RELEASE -DHANDLE=void* -DINVALID_HANDLE_VALUE=NULL \
  -DBW_GUARD= \
  $BW/build/el7/lib/libcstdmf.a $BW/build/el7/lib/libmath.a -lpthread
/tmp/genstub/gen_generate 2>/dev/null \
  > docs/review/fixtures/waypoint/gen_generate.log
```

（stderr 走原版 INFO_MSG；stdout 才是 log。`cmp` 验证
`/tmp/genstub/waypoint_generator/` 三对源文件与参照逐字节一致。
产物二进制不入库。）

---

# 分片三：NavmeshProcessor per-girth 工作流环 + navmesh cache 保存面

编辑器侧导航工作流的最后一环：`tools/common/navmesh_processor.cpp`
的 `processInBackground`（**逐 girth 循环**——每个 girth 独立重灌
chunk、重生成多边形、`asBinary(identity, girth)` 追加进同一 blob），
加上 `tools/common/editor_chunk_navmesh_cache_base.cpp` 的缓存状态机
（load → dirty → recalc → calcDone → saveChunk/saveCData——**保存**
腿的落盘面）。移植面在 `crates/waypoint-gen/src/navmesh_processor.rs`
与 `navmesh_cache.rs`，全部架在既有
`FloodPhysics`/`FloodChunk`/`GenChunk`/`WaypointFlood`/`WaypointGenerator`/`as_binary`
缝上。

## 产物对照（验收面）

编辑器 harness 面依赖 editor chunk 框架（ChunkSpace/BgTaskManager/
物理查询全链），不在影子桩可达范围；本片证据取**组合正确性**：

- **环 = 单 girth 串接**：同场景跑 girth 序 `[0.5, 0.8]` 的环，断言
  逐字节等于各自单 girth 独立跑 `as_binary` 的串接
  （`the_record_bytes_equal_the_standalone_single_girth_run`）——
  即环没有引入任何额外状态；单 girth 的字节正确性已由分片一/二
  对原版 LOG-IDENTICAL 对照钉死。
- **记录走查算术** = 参照 `parseOldParts`（waypoint_view.cpp:197-218
  同式）：`16 + numWaypoints*12 + numEdges*12`（numEdges = 顶点总数，
  即 data 段长——非逐边 4 字节）。
- **girth 语义**：defaults（maxSlope 45 / maxClimb = height/3）、
  显式覆盖、`depth==0` 合法但 `radius = min(width,depth)*0.5` 塌为 0、
  非法宽高/负深走 CRITICAL_MSG 断言（`girth.cpp:13-28`）。
- **cache 状态机**：load 缺失即 dirty、干净块无 blob 亦 dirty
  （`:64-67`）；dirty bool 按二进制单字节读（`len == sizeof(bool)`）；
  calcDone 身份检查（`currentProcessor_ == processor`）、成功存储并
  清 dirty + 入 unsaved 列表、失败保 dirty 丢结果；saveCData 写
  `navmeshDirty` bool（与 navgen 的 outputDirtyFlag 匹配）+ 非空
  blob、NONE 全不写；saveChunk 产
  `<id>.cdata/worldNavmesh` resource 串。

## TDD 痕迹

环面先写 6 测对桩实现确认 **RED**（5 失败：拼接/字节等价/两处
abort/girth 校验），后换真实现转 **GREEN**；cache 面实现与测试同批
落地（状态机纯函数，参照行为单映射）。

## 测试与门禁

- `cargo test -p waypoint-gen`：**85 通过 / 0 失败**（69 旧 + 16 新：
  环 6——双 girth 串接、字节等价独立单跑、girth 序间 abort 停环、
  首 girth 前 abort 产出空、flash-flood 臂跳过 entity 点、girth
  defaults/校验；cache 10——load 三态、NONE 永不 dirty、recalc→
  calcDone 成败两臂、陈旧 processor 忽略、Recast 路由、saveCData
  三态、saveChunk resource 串）。
- 覆盖（llvm-cov 0.9.1，`--lcov` DA-max (SF,line) 合并，
  `cargo llvm-cov clean` 先行）：`navmesh_processor.rs` **98.34%**
  （414/421；7 行 DA:0：156 fill 臂逐种子 abort 中断返回、166
  nshrink 循环体——无测传 nshrink>0、226 环内 flood 失败返回、
  328/330/342 测试夹具未走臂）；`navmesh_cache.rs` **99.25%**
  （132/133；85 = 枚举不可表示的 `None` 臂，注释已注明参照
  ERROR_MSG 臂语义）。
- `cargo test --workspace`：**4567 通过 / 0 失败**（118 测试二进制）；
  `cargo fmt -p waypoint-gen -- --check` 绿；
  `cargo clippy -p waypoint-gen --all-targets -- -D warnings` 绿。

## 参照源码

| 参照 | 位置 |
|---|---|
| `processInBackground` per-girth 环（abort 领检、flood、16 层拷贝、generate、asBinary 追加） | navmesh_processor.cpp:47-102（54-58 领检、63 边 determination 物理、77-92、94-97） |
| 每 girth 新建 `PhysicsHandler(space, girth)` | chunk_flooder.cpp:283（flood 内）、navmesh_processor.cpp:63（边 determination） |
| `floodChunk` 入口（reset reso 0.5 硬编码 ±2reso、种子序中心最后、flashFlood 臂、entity 点仅真灌臂、逐种子 abort、postfilter/shrink） | chunk_flooder.cpp:274-409（98-111、226-237、294、302-307、319-322） |
| flashFlood 门（chunk 空间列持有/地形高度+洞图） | chunk_flooder.cpp:417-480 |
| `Girth` 解析与校验（defaults 45 / height/3、`depth<0` 与 `width/height<=0` 才 CRITICAL） | girth.cpp:13-28 |
| cache load（dirty bool 单字节、干净无 blob 即 dirty） | editor_chunk_navmesh_cache_base.cpp:44-90（64-67） |
| saveChunk（`id.cdata/worldNavmesh` resource 串） | editor_chunk_navmesh_cache_base.cpp:93-106 |
| saveCData（删两节、`navmeshDirty` bool 对齐 navgen outputDirtyFlag、非空才写 blob、NONE 不写） | editor_chunk_navmesh_cache_base.cpp:109-134 |
| `dirty()`（NONE → false） | editor_chunk_navmesh_cache_base.cpp:266-277 |
| `recalc`（清 blob + dirty + 按 generator 建 processor） | editor_chunk_navmesh_cache_base.cpp:279-330 |
| `calcDone`（身份检查、成功存储清 dirty 入 unsaved、失败保 dirty） | editor_chunk_navmesh_cache_base.cpp:341-364 |
| `extendThroughUnboundPortals` 注册 no-op | waypoint_generator.cpp:2370-2607 |

## 移植面

| 参照 | Rust |
|---|---|
| `NavmeshProcessor::processInBackground` | `navmesh_processor::process_in_background` |
| `Chunk::floodChunk` 调用体 | `navmesh_processor::flood_chunk` |
| flashFlood 门 | 调用方传入 `Option<f32>`（Some=flash 臂跳 entity 点与逐种子 abort；None=真灌臂） |
| 每 girth 新建 `PhysicsHandler` | `GirthPhysicsSource::physics_for(&Girth) -> &mut dyn FloodPhysics` |
| `Girth`/`GirthSettings` 解析 | `Girth`/`GirthSection` + `from_section`（defaults + CRITICAL 断言） |
| `EditorChunkNavmeshCacheBase` 状态机 | `navmesh_cache::NavmeshCache`（`load`/`dirty`/`recalc`/`calc_done`/`save_chunk`/`save_cdata`） |
| 生成器三态 `NAVMESH_GENERATOR_*` | `NavmeshGenerator{None,Navgen,Recast}` |
| `NavmeshProcessor`/`RecastProcessor` 指针身份 | `Processor{Navmesh,Recast}` 变体比较（同一时刻仅一个在途） |
| `extendThroughUnboundPortals` | 注册 no-op（同 navgen 面口径） |

## 登记边界

- **flashFlood 门收窄为 `Option<f32>`**：参照门内是 chunk 空间列持有
  + 地形高度 + 洞图查询（chunk_flooder.cpp:417-480），属 chunk 物理
  域；移植面将其建模为调用方预算好的门结果（Some=门过高 flash 直灌
  顶高）。真 chunk 列域已在 chunk 席面注册，不在此重复。
- **逐种子 abort 只在真灌臂**（chunk_flooder.cpp:319-322 在 fill 腿），
  flash 臂不查——abort 计数语义按臂分叉，测试注释已钉。
- **`writeTGAs=false`/`progress=NULL`**：参照环调用即此常量，fill 的
  −1 进度臂与 TGA 写出不在移植面（与 05-02 口径一致）。
- **`GirthPhysicsSource` 具名 trait 的原因**：`FnMut` 闭包不能把借用
  返回进捕获盒（E0521 生命周期），参照「每 girth 新建 handler」语义
  以 trait 表达。
- **`radius()` 随 `depth==0` 塌 0**：`min(width,depth)*0.5`（girth.cpp
  原式），`depth==0` 合法但半径归零——非移植 bug，参照行为。
- **记录走查 `numEdges` = data 段顶点总数**：首版测试把 numEdges 当
  4 字节边条目步进导致越界误报，已按参照 parseOldParts 修正。
- **`calcDone` 身份检查**：参照比指针；移植比 `Processor` 变体
  （同一时刻仅一个处理器在途，语义等价）。
- **双交付警示（主席合并择一）**：门户两臂与 readTGA 读入侧 =
  lane/bw02-mercury e9e9cf5（无 per-girth 环）；per-girth 环完整面 =
  bw16-18 席 workflow.rs GirthWorkflow（portal 带符号半空间/双推回退
  缺口可达）与本席 navmesh_processor.rs——同面多交付，合并时择一。
- **BW-10-05-03 Recast 移植**：仍挂起待用户拍板；`Processor::Recast`
  路由已留位（recalc 按空间设置建对应 processor，参照 `:292-312`），
  Recast 处理器本体不在本片。
