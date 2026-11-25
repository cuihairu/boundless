# BW-10-01 Chunk 加载与空间索引 — 复核证据

- **任务编号**: BW-10-01-01..03（lane `lane/bw10-space`，todo.md 行 867-869）
- **原版构建目标/源码**:
  - 标识/坐标: `programming/bigworld/lib/chunk/chunk_format.hpp`（`ChunkFormat::outsideChunkIdentifier`）、`programming/bigworld/lib/chunk/chunk_terrain.cpp`（`outsideChunkIDToGrid`、`charToHex`）、`programming/bigworld/lib/chunk/geometry_mapping.cpp`（`gridFromChunkName`、`HexLookup`、`gatherChunks`、`GeometryMapping`）、`programming/bigworld/lib/chunk/chunk.cpp`（`isOutsideChunk`、`Chunk::load`、`loadInclude`、`readMooMatrix`）、`programming/bigworld/lib/chunk/base_chunk_space.hpp`（`DEFAULT_GRID_RESOLUTION`、`MIN/MAX_CHUNK_HEIGHT`）。
  - 边界/Portal: `programming/bigworld/lib/chunk/chunk_boundary.cpp` / `chunk_boundary.hpp`、`programming/bigworld/lib/chunk/portal.cpp`。
  - 流式装载: `programming/bigworld/lib/chunk/chunk_manager.cpp`、`programming/bigworld/lib/chunk/chunk_space.cpp`（`findOrCreateChunk`、`gridBounds` 扫描、`bind` 序列）。
- **Boundless 模块**: `crates/chunk`（`identifier` / `mapping` / `chunk` / `boundary` / `space`）+ `crates/chunk/tests/space.rs`。
- **平台**: Linux x86_64，Rust 工具链 1.100.0，llvm-cov 0.9.1。
- **输入夹具**:
  - `docs/review/fixtures/chunk_space/res/space_00010001/`（最小手造样本，见下节「夹具来源登记」）。
- **原版结果**（源码核对 + 参考格式文档）:
  - 室外块标识 = 8 位小写 hex 坐标 + 尾 `o`；室内块 = 8 位任意 hex + 尾 `i`；特殊目标 heaven/earth/invasive/extern。
  - 宽空间前缀：`|x|` 或 `|z|` 触发 4/8/12 位三级哈希子目录（`001x001x/sep/…`，勘误 2026-11-24：原记 `001xx001xx` 多一 x——参考 `%03xx%03xx/sep/` 实为 1 个字面 x，见 `chunk_format.hpp:35-47`），`singleDir` 关闭。
  - 网格↔坐标：`outsideChunkIDToGrid` 对 bit15 符号扩展；`gridFromChunkName` 只取末 9 字符；`outsideChunkIdentifier` 用 u16 回绕算术。
  - 室外块 transform/包围盒由网格坐标 × gridSize 派生；室内块读 `<transform>`/`<boundingBox>`，首个 `<model>` 在无 `<shell>` 时作 shell。
  - 对象集合 = 逐子节点 tag（shell/transform/boundingBox 不计入），`<include>` 递归展开。
- **新版结果**:
  - `cargo test --workspace` 全绿（chunk 23 lib + 9 集成；layering 注册通过）。
  - 标识↔坐标往返扫掠覆盖全 i16 双轴（2048 点）；前缀三级、单目录、回绕、大小写分歧各成一测。
  - `GeometryMapping::from_settings` 读 `chunkSize`/`bounds`/`singleDir`；`gather_chunks` 枚举 18×2 室外格 + 1 室内块 = 37。
  - 夹具文件解析：原点格 4 对象（2 model + light + sound）、前缀远格 (17,0)、室内块 transform/bbox/3 对象，逐项断言。
  - 流式装载：scan 候选序（半径序 + 越界剔除 + 幂等）、绑定序列 `[00000000o,00000001o,00010000o,00010001o]`、双向对移、拒载/拒绑/重入幂等。
  - llvm-cov 0.9.1：全 crate 行覆盖 98.62%、`space.rs` 97.85%、`boundary.rs` 98.48%、`identifier.rs` 100%。
  - clippy `-D warnings`（scoped `-p chunk --all-targets`）0 findings；`cargo fmt -p chunk --check` 0 漂移。
- **证据链接**: `crates/chunk/src/{identifier,mapping,chunk,boundary,space}.rs`、`crates/chunk/tests/space.rs`、`docs/review/fixtures/chunk_space/`。
- **状态**: 待 push 复核。

## 夹具来源登记（BW-10-01-01）

任务书允许「无原版转换器样本则最小手造样本并如实登记来源」。本树内无原版转换器产出的样本 chunk 目录（`programming/bigworld` 树未含 sample 资源），故手造 `space_00010001` 如下：

- **坐标系**: 与参考一致，x 右 z 下（`z` 即网格 y），gridSize=100（`base_chunk_space.hpp` `DEFAULT_GRID_RESOLUTION`）。
- **settings.xml**: `chunkSize=100`、`bounds` 矩形 0..17 × 0..1、`singleDir=false`。bounds 语义按 `GeometryMapping`（`geometry_mapping.cpp:156-236`）——本地格矩形含两端点。
- **标识/前缀**: 格 (17,0) 触发 4 位前缀级（`001x000x/sep/00110000o`，勘误 2026-11-24 按 `chunk_format.hpp:35-47` 的 `%03xx%03xx/sep/` 修正 x 数），与 `ChunkFormat::outsideChunkIdentifier` 输出一致；其余格平铺（`00000000o`..`00010001o`）。
- **对象集**: 每室外格 1-2 个 `<model>`/`<entity>`；原点格另含 `<light>`/`<sound>`。室内格 `00000000i.chunk` 采用编辑器形态（`editor_chunk_cache.cpp:1167-1179`）：`<transform>` row3=150 0 250、`<boundingBox>` −10..10、`<shell>` + model + entity + light。
- **内容字段**: `<resource>`、`<position>`、`<transform>`、`<id>`/`<type>`/`<instantiate>` 均按 `chunk format.txt` 与 `chunk.cpp` 的解析路径填写，取值任意但合法（解析器只关心结构，不解释资源本体）。

## BW-10-01-01 派生痕迹

- `outside_chunk_identifier(x,z,single_dir)`（`identifier.rs`）复刻 `chunk_format.hpp:21-54`：u16 回绕判三级前缀；测试钉死 (17,0)→`001x000x/sep/00110000o`、(4096,4096)→三级 `1xxx1xxx/sep/10xx10xx/sep/100x100x/sep/…`（勘误 2026-11-24：三级分别 3/2/1 个字面 x，对齐 `chunk_format.hpp:35-47` 与 `asset_processor_script.cpp:916-926`）、(−4097,−4097)→`exxxexxx/sep/efxxefxx/sep/effxeffx/sep/efffefffo`、(−32768,32767)→`8xxx7xxx/sep/80xx7fxx/sep/800x7ffx/sep/80007fffo`。
- `grid_from_chunk_name`（`identifier.rs`）复刻 `geometry_mapping.cpp:404-419`：只取末 9 字符 + `HexLookup`（双大小写、失败→None）；拒绝 `i` 尾/短名/非法 hex。
- `outside_chunk_id_to_grid`（`identifier.rs`）复刻 `chunk_terrain.cpp:567-591`：8 字符、bit15 符号扩展、`charToHex` 仅数字+小写（大写→0）。
- `GeometryMapping::from_settings`/`in_local_bounds`/`in_world_bounds`/`gather_chunks`（`mapping.rs`）复刻 `geometry_mapping.cpp:63-101/294-309/470-484`；`gather_internal_chunks` 复刻 `Util::gatherInternalChunks`（`geometry_mapping.cpp:422-447`），深度扫 `*i.chunk`（15 字符名）。
- `Chunk::new`/`Chunk::load`/`load_include`（`chunk.rs`）复刻 `chunk.cpp:157-247/370-537/555-640`：室外块从网格派生 transform+bbox（含 `MIN/MAX_CHUNK_HEIGHT` ±10000）；室内块读 `<transform>`/`<boundingBox>`；`<include>` 经 resolver 递归展开；shell/首 model（无 shell 时）不计入对象集。

## BW-10-01-02 派生痕迹

- `Portal::from_section`/`from_plane_points`/`from_parts`（`boundary.rs`）复刻 `chunk_boundary.cpp:358-595` 两个构造器：label/internal/permissive/chunk/point/uAxis 读取、`vAxis = normal × uAxis`、`origin = normal*d/|n|²`、`lcentre = uAxis*avg.x + vAxis*avg.y + origin`，以及 winding 检查（`testPlane` 法向与边界法向反向时就地 `reverse`）。
- `PortalTarget` 复刻 `pChunk` 的特殊指针编码（`Portal::NOTHING/HEAVEN/EARTH/INVASIVE/EXTERN` = 0..4，`chunk_boundary.hpp:247-255`）；`has_chunk`/`is_connecting_to_special` 对应 `hasChunk`/`isConnectingToSpecial`。
- `ChunkBoundary::from_section`（`chunk_boundary.cpp:59-104`）复刻 normal/d 读取（非单位法向归一化并把 d 同步缩放）与 portal 列表（earth→boundPortals_，其余→unboundPortals_）。
- `create_boundaries`（`boundary.rs`）复刻 `createBoundaries` 的室外分支（`chunk.cpp:695-806`）：right/left/bottom/top/back/front 六个边界，plane/uAxis/points/邻居目标逐字转写（right=x=0→(x−1,z)，min 边→extern；left=x=gridSize→(x+1,z)，max 边→extern；back z=0→(x,z−1)；front z=gridSize→(x,z+1)；bottom→earth；top→heaven）。
- `create_boundaries_from_shell` 复刻室内分支（`chunk.cpp:808-850`）：shell（或旧式首个 `<model>`）的 `.visual`（后备 `.static.visual`）资源取 `<boundary>` 段；`createBoundarySections`（由三角形生成边界）属 visual 管线，未移植——登记。
- `form_boundaries`（`chunk.cpp:856-908`）：`good = len ≥ 4`、零法向退化、`bounds_`/`joints_` 分类（unbound 非 internal→bound；仅 bound（earth/heaven）→joint+bound；无 portal→bound）。
- 集成测试 `outside_chunk_boundaries_match_the_reference_grid` 钉死 (0,0)/(17,0)/(17,1) 的邻居目标、六个 plane/uAxis/vAxis/lcentre/points；`inside_chunk_boundaries_come_from_the_shell_visual` 经 `hut_shell.visual` 夹具验证室内边界、winding 反序修正、`inside` 投影语义与 joints/bounds 分类。
- 顺带补 `PlaneEq::new(normal, d)`（`foundation/src/math/plane.rs`，对应 `planeeq.ipp:29-51`）——边界构造需要 (normal,d) 形态，原 port 仅有 Default + init(p0,p1,p2)。

## BW-10-01-03 派生痕迹

- `ChunkSpace`（`space.rs`）复刻 `ChunkSpace`/`ChunkMap` 与流式装载切片：`point_to_grid`/`grid_to_point`（`base_chunk_space.hpp:181-189`）、`find_chunk_by_grid`（`chunk_manager.cpp:2199`）、`add_chunk`（`chunk_space.cpp`）。
- `ChunkSpace::scan`（`chunk_manager.cpp:2132-2222`）复刻 `scan()` 的候选收集：`GridComp` 按偏移平方半径排序、世界距离超 `maxLoadPath` 即断、`inWorldBounds` 过滤、`gridToLocal`（本 port 恒等，变换映射属 BW-10-02+）、`findChunkByGrid`→`new Chunk`→`addChunk`、仅收集未装载块、`MAX_WORTHY_CHUNKS=50` 上限。
  - **登记分歧**: 参考 `std::sort` 在同半径层内顺序未定，本 port 以（半径，偏移 x，偏移 z）作确定性 tie-break，测试只断言半径序与单元集合而非逐层脆序。
- `ChunkSpace::bind`/`bind_back` 复刻 `Chunk::bind`/`bindPortals`（`chunk.cpp:1039-1288`、`1741-1774`）：先按 `chunk.cpp:3339` 给全部 portal 写世界 `centre`；heaven 立即绑定；目标块已绑定才连接；连接双向（`Chunk::bind(Chunk*)` 只移走首个匹配 portal）；`bindPortal` 移动 bound/unbound 列表；`is_bound` 置位并记录 `bind_order`。
  - **登记分歧**: extern/invasive/Nothing 连接的解析需空间列（`resolveExtern`/`guessChunk`）与 `ChunkOverlappers`（场景适配面，BW-10-02），此处保持未绑定；同步 `load_and_bind` 取代 `checkLoadingChunks` 的异步完成序（单线程装载器按启动序完成）。
- 集成测试 `streaming_scan_loads_the_grid_nearest_first` 钉死 (0,0) 相机、半径 200 的候选序（含越界单元格剔除与二次扫描幂等）；`streaming_bind_sequence_connects_neighbours` 钉死绑定序列 `[00000000o,00000001o,00010000o,00010001o]`、双向连接（left/front/back 对移）、exter extern 保持未绑定、`centre=transform∘lcentre` 与重入幂等；`inside_chunk_binds_to_a_bound_outside_chunk` 钉死室内块外部 portal 绑定到已绑定室外块、未装载邻居保持未绑定、未装载/缺失块拒绝绑定；`space_map_accessors_track_created_chunks` 覆盖 `find_chunk_by_grid`/`add_chunk`/`chunk_mut`/`chunks`/缺失文件与未装载拒绝。
- lib 单测补 `bind_back` 双 portal 只移首个、混合 joint 保留 extern 未绑定、`MAX_WORTHY_CHUNKS` 截断（>50 格扫描）。
- 覆盖率（llvm-cov 0.9.1，`cargo llvm-cov -p chunk --summary-only`）: `space.rs` 行 97.85%、全 crate 行 98.62%（≥98% 纪律通过；剩余未覆盖行为控制流闭括号记录条目）。

## BW-10-01-04 派生痕迹（卸载 Chunk 及场景对象）

- **验收**: 查询结果和资源释放符合基线（todo.md 行 1462）。
- `Chunk::removable`（`chunk.rs`，`removable_` 默认 true，`chunk.cpp:190`）：
  上闸/锁定块对卸载趟拒绝（`chunk_manager.cpp:2241`）；`Chunk::new` 默认置 true。
- `ChunkSpace::unbind`（`chunk.cpp:1292-1458` `Chunk::unbind(bool cut)`）：
  逐 joint 边界的 **bound** portal 活下标遍历（摘除后同下标继续，幸存 portal 才前移）：
  ① 室内块 heaven → 自解绑；② 其余非 chunk 目标（室外 heaven/earth/extern/invasive）
  一律 `!p.hasChunk()` continue **保持绑定**——室外块 heaven 在 unbind 后仍 bound
  是基线怪癖，测试钉死；③ chunk 连接：cut 时目标重写（`!isOutside && pChunk->isOutside`
  → INVASIVE 壳提示，否则 NULL/Nothing），`unbindPortal` 摘除，再递归
  `pOnlineChunk->unbind(this, cut)`，且 **室外块切室内邻居时强制 cut=true**；
  终置 `is_bound=false`。缺失块返 false。
- `unbind_portal`（`chunk_boundary.cpp:185-191`）：摘除的 portal **压到 unbound 列表尾部**
  ——位置即后续 bind 的首配位，lib 单测钉死 `[extern, …]` 尾插序。
- `unbind_back`（`chunk.cpp:2083-2136` `Chunk::unbind(Chunk*, bool)`）：反向链只找
  joints 边界里**首个**指向 requester 的 bound portal；cut 时目标重写同上 + **internal 特例**
  ——`if (pit->internal)` 整条边界**逐出 joints**（`joints_.erase(bit)`）而 portal
  原地保持 bound（边界成孤儿但列表不动），lib 单测钉死两腿分岔；
  找不到/缺 owner 静默（参照为 ERROR——port 经公共面链接恒对称，同 01-03 bind_back 缺 owner 先例，登记）。
- `unload_chunk`（`chunk.cpp:914-984` `Chunk::unload`）：**先查 bound**（照旧拒绝，
  参照 ERROR→端口返 false）、`!loaded` 静默无操作；释放=清 items/`bounds_`/`joints_`，
  **boundaries_ 不清**（块仍被空间任命，重载经 `form_boundaries` 重形边界）；
  `loaded_=false`。绑定中卸载与未装载卸载两拒绝腿集成测试钉死。
- `unload_farthest`（`chunk_manager.cpp:2236-2366` scan() 的卸载切片）：
  相机**格角** `(gridToPoint(cameraGridX), 0, gridToPoint(cameraGridY))`（非相机点）；
  每趟至多 `MAX_UNLOAD_CHUNKS=1`（非编辑器档，`chunk_manager.cpp:92-95`）；
  候选门序：bound+removable → 原点平面距离² `≤ minUnloadPath²` 跳过（滞回）→
  室内块被**已装载**覆盖室外块罩住则跳过（`outsideChunkIdentifier(pos)` checkBounds=true 腿）→
  `AABB::intersects(cameraGridPos, CAMERA_NEARBY_WIRED_BIAS=20)`（`boundbox.ipp:313-317`
  逐轴 `v ≥ min−bias && v < max+bias` 半开）跳过；中选者 `unbind(false)+unload`，
  **块留在 map**（仍由 chunk space 任命）。室内壳被未装载覆盖块罩住时不拦——独立单测钉死。
  - **登记分歧**: ① 参考 load 与 unload 同在 `scan()` 一趟，port 保持 01-03 的同步双面
  （`scan` 管装载候选、`unload_farthest` 管卸载候选），测试分别驱动；
  ② 参考 `ChunkMap` 迭代桶内逆序，port 钉 BTreeMap 标识序（同 01-03 tie-break 类）；
  ③ 重叠壳在装检查（`ChunkOverlappers`，BW-10-02 场景适配面）trivially 通过；
  ④ 参照 `Chunk::unbind` 找不到反向 portal 走 ERROR，port 静默（见上）。
  ⑤ 强制 cut 递归腿（室外→室内 `(true, Some(false)) => true`）在当前公共面不可达
  （室外块边界永不指向室内块——invasive 解析归 BW-10-02），space.rs 该行未覆盖，
  余 644/647=99.54%。
- 集成测试 `unloading_farthest_releases_chunks_and_a_reload_rebinds`：
  四绑定块 + 相机 (50,0,50)/滞回 50 → 首趟卸 `00000001o`（资源清空、boundaries 六条
  与 heaven 绑定保留、留 map、`find_chunk_by_grid` 仍命中）、邻居 `00000000o` b5/
  `00010001o` b0 双向退回 unbound 且目标原样；次趟卸 `00010000o`；滞回放宽 200 后空趟；
  重载+重绑恢复双向连接、对象集回归、`bind_order` 追加至 5。
  `unbind_cut_marks_inside_shells_invasive_and_flattens_outside_links`：室内块 cut=true
  → +x 出口 INVASIVE、heaven 退绑、earth 保持；室外块 cut=true → 两侧 chunk portal
  扁平化 Nothing（heaven/earth 保持 bound）、未绑定 portal 不改写、邻居仍 bound。
- lib 单测：`unbind_portal` 尾插序、`unbind_back` internal/normal/invasive 三腿 +
  静默无操作、滞回/偏置/覆盖/可移除/单趟上限、`intersects_with_bias` 半开边界、
  未覆盖室内壳随趟卸载。
- 覆盖率（llvm-cov 0.9.1，`cargo clean -p chunk` 后逐行 max 合并）: `space.rs`
  644/647=99.54%、`chunk.rs` 237/240=98.75%、`boundary.rs` 454/456=99.56%、
  `identifier.rs` 100%、`mapping.rs` 118/119=99.16%——≥98% 纪律全过。
  `cargo test -p chunk` 29 lib + 11 集成全绿；`cargo test --workspace`
  **4729 passed / 0 failed（exit 0）**；fmt 包级 `--check` 0 漂移；clippy
  `-p chunk --lib --tests` 0 chunk 侧告警。

## BW-10-01-05 派生痕迹

空间索引 = 焦点网格（focus grid）+ 凸包树（hull tree）+ 点查询。参照源码行号以本版为准：

| 移植 | 参照 | 行 |
| --- | --- | --- |
| `LineEq` | `lib/math/lineeq.hpp/.ipp` | 全文 |
| `Portal2D::combine/contains/setDefaults` | `lib/math/portal2d.cpp` | 89-183 / 255-274 / 47-62 |
| `PlaneEq::basis/param/project/intersect/isInFrontOf(Exact)` | `lib/math/planeeq.cpp/.ipp` | 92-115 / 122-152 / 24-76 / 197-214 |
| `HullTree`（arena + mark 链） | `lib/physics2/hulltree.cpp` | 63-70 / 107-274 / 280-467 / 475-499 |
| `Column` / `ColumnGrid` | `lib/chunk/base_chunk_space.cpp`、`server_chunk_space.hpp/.cpp` | 377-600 / 626-652 / 718-744；34-90 / 50-113 / 143-176 |
| `Chunk::centre/volume/completed/focus_border` | `lib/chunk/chunk.cpp` | 217,238,388 / 3506-3512 / 1466-1468,183,186 / 1547-1553 |
| `ChunkSpace` 焦点面与点查询 | `chunk_space.cpp`、`server_chunk_space.cpp` | 714-810 / 1306-1371；50-113 |
| 列删除→smudge | `base_chunk_space.cpp` | 393-411（`~Column`）+ 41-43（`~HullChunk`） |

### 语义逐条

- **`LineEq`（lineeq.hpp/.ipp）**：法向 `(v0.y-v1.y, v1.x-v0.x)` 归一化、`d = n·v0`；`intersect = (other.d − d·(on·n))/(on×n)`、`param(t) = (n.x·d+n.y·t, n.y·d−n.x·t)`、`project`、`isMinCutter = n×cn > 0`、`isParallel = |n×on| < 0.001`、`isInFrontOf = n·p > d`。
- **`PlaneEq` 扩展**：`basis` 取 `|n|` 最大轴为主（`>` 链向低索引收敛）、xdir 解平面方程、ydir = n×xdir；`param(0,0)` = `n·d/|n|²`；`intersect` 按 planeeq.cpp:24-76 全链（近平行→零线哨兵，`almostZero(lenSq, 0.0001)`）；`isInFrontOf` 带 `almostEqual` 0.0004 ε，`isInFrontOfExact` 无 ε。
- **`Portal2D`**：`combine` 逐边剪裁（PORTAL_EPSILON=1e-10 跳微小边、交点 clamp01 插入、`inside` 首位回绕、尾部过滤 outside==0）；**多边形回绕方向按参考 `setDefaults`（(-1,1),(1,1),(1,-1),(-1,-1)）顺时针（y-up）**——`n=(seg.y,−seg.x)` 内半平面只对该序成立；`contains` 全边 `>=` 判内。HullTree 的 outline 是普通 `addPoint` 列表，不经过 `combine`——`Portal2D` 作为补充面保留并注册。
- **`HullTree`**：
  - 参考 mark 链（哨兵 `(HullTree*)1`）移植为 per-`add` 的 `MarkState`（marked/in_chain/chain 三平行数组，节点新建时 `grow`），调用结束全部清零——与参考「每次 add 后所有 visited 节点 marked_ 归零」不变量一致。
  - **split block（hulltree.cpp:345-394）在参考中被注释掉（死代码）**：跨越 divider 的 outline 以完整副本传给两个孩子（`inFront = outline; outBack = outline;`）。移植保留 pass-through——移植死算法会改变结果。
  - 标记位：1=back、2=front、256/512=「此侧永不标」门、65536=本 hull 新节点；coplanar 臂（any_side 全假）按 `sameDir` 标 `BACK<<sameDir`、异侧清位+上锁、`|MARK_NEW` 后返回；guess 臂（单侧 + 非新节点）按 `pointingSameWay != anySide[1]` 决定「另一侧可能仍在内（标 back）」或「证明不在内（清 back+上锁）」；递归臂先清标再上锁，新孩子 `FRONT|NEW`。
  - outline 剪裁逐字：平行同向 `param(0)` 在前 → 忽略；被完全截断 → min=1/max=-1/break；`min >= max` 跳过；**只推 max 端**（min 端参考中注释掉）。
  - `testPoint` 单路径下降，逐节点取所在侧 tags 链接，参考头插序 → 移植末尾 `reverse`。
- **`Column`**：`addChunk` 先查 `shutTo_`；室外块进 `pOutsideChunk_` 槽，**新块只在静态对象数严格更多时替换**，holdings 无论替换与否都记原加入块；室内块进 hull 树。`findChunk` 从 `pOutsideChunk_` 起步，链上首个结果必胜（`pResult == pOutsideChunk_` 含 NULL==NULL），此后严格更大 volume 才替换；`findChunkExcluding` 同走法外加 `boundingBox.intersects(point)`（= port 半开 `contains`，boundbox.ipp:303-307）与 `pNot` 过滤。`openAndSee`（非 shutTo 才记 seen）/`shutIfSeen`（seen 含才 shut）逐字。
- **焦点流程**：`bind` 尾部 `noticeChunk`（= `blurredChunk`，仅绑定的块入列，chunk.cpp:1072）；`focus()`（server_chunk_space.cpp:50-113）逐个 blurred：3×3 `shutIfSeen` → `focus_chunk`（`Chunk::focus`：室外块按 centre 单格、室内块按 8 个 bbox 角去重多格；每格缺列即建、`add_chunk`、`focusCount_=1`，chunk.cpp:1536-1618）→ 3×3 `openAndSee`。`ignoreChunk`（chunk_space.cpp:1306-1353）对 3×3 邻域**删除列**（非标陈旧），持块经 `~HullChunk { pChunk_->smudge(); }`（base_chunk_space.cpp:41-43）模糊化——**故「列删除→smudge 持块」是忠实行为而非分歧**（01-04 节登记的「ignoreChunk 无移植面」自本项起失效）。`Chunk::smudge`（chunk.cpp:1619-1638）：`focusCount_ != 0` 才清零并入 blurred。
- **查询**（chunk_space.cpp:714-810）：`findOutsideChunkFromPoint`/`findChunkFromPoint` 先 `pt.y += 0.0001` 再 `column(pt,false)`；`findChunkFromPointExact` 对结果再过 `completed()`（`focussed()` = `focusCount_ > 0`，chunk.hpp:183/186）。
- **`recalcGridBounds`**（chunk_space.cpp:1077-1127 + server_chunk_space.cpp:143-176）：窗口取 mappings 本地格矩形包络，缩窗掉出的列按 `~Column` 语义 smudge 持块。
- **centre→格**（server_chunk_space.cpp:55-58）：`int(cen/gridSize)` 截断后 `cen < 0` 手工 −1（= floor）。

### 测试

- foundation：`lineeq` 7 项、`portal2d` 9 项（默认四边形/含边/重叠剪裁/不相交失败/微小边跳过/空剪剪刀不剪/erase）、`plane` +3 项（basis 单位且在面内 + param/project 往返、intersect 双面含线 + 自交零线、isInFrontOfExact 无 ε）。
- `hulltree` 9 项：空树、单 hull 内/外两侧、双 hull 重叠区双答、同 hull 二入（coplanar 臂）、平行同向截断、不相交不漏、divider ε 点、反向共面标 back 臂。
- `column` 8 项：室外进空列+点查询、大者替换/小者失手（holdings 3 条）、室内外共列（壳内 hull 胜/壳外室外胜）、excluding 的 pNot/bbox/无室外列回退、volume 偏好 + 陈旧 id 跳过、seen/shutTo 记账、网格窗移动与丢列。
- `space` +5 项：**验收主测** `the_focus_grid_answers_cross_chunk_and_empty_region_queries`（两室外块+一壳块 bind→focus 后：跨列各自命中、壳内 hull 胜、空白列/窗外 None、exact 完成门控）+ 聚焦前全 None；`unbinding_blurs_and_a_refocus_rebuilds_the_neighbourhood`（unbind 删列→邻居 smudge 入 blurred→refocus 重建）；`smudge_blurs_but_keeps_the_hull_findable`（plain 仍答/exact 拒答/二次 smudge 无操作/缺失块静默）；`shrinking_the_grid_drops_and_smudges`（负格 centre_grid 手工 −1、缩窗丢列+smudge）；`column_creates_on_demand_and_refuses_out_of_span`（含空 mappings 1×1 窗）。
- 覆盖率（llvm-cov 0.9.1，DA:0 计）：`lineeq.rs`/`plane.rs`/`portal2d.rs` 100%、`hulltree.rs` 99.67%、`space.rs` 98.97%、`column.rs` 98.88%（余未覆盖为防御性死臂：current 只会持有 map 内 id、`if let` 恒真的闭括号区域）。
- 门禁：`cargo test -p chunk`（53 lib + 9 集成）+ `-p foundation`（236）+ `-p test-support`（593）全绿；fmt scoped check 0 漂移；clippy `-D warnings` scoped 0 findings；`cargo check --workspace` 绿。

### 登记边界（本项）

- **障碍树**（`Column` 构造的 `pObstacleTree_`）无移植侧对象（BW-10-03 碰撞域），列只带 hull 树。
- **`jogForeignItems`**（室外列已有室外块时室内块加入会挪其外来物品）无物品嵌套面——登记。
- **焦点缓存刷新**（`Chunk::focus` 1608-1612 的 `focusCache_` 面）无缓存对象——登记。
- **stale-id 跳过**：参考列存裸 `Chunk*`，失效指针直接解引用；移植存标识并在 map 缺失时跳过（findChunk/findChunkExcluding/focus 的 blurred 陈旧项）。
- **ERROR_MSG 降级静默**：`ignoreChunk` 对未绑定块参考打 "Attempted to ignore offline chunk"（1329-1331），移植无声返回；`focus_chunk` 列缺失参考 `MF_ASSERT`（网格窗口应覆盖一切聚焦块），移植跳过。
- **`combine` 卫兵**：空 `pt1` 参考 `points_.front()` 越界读、剪空集参考 `inside[0]` 越界读，移植分别返回 pt2 自身计数 / false（HullTree 不经此路径）。
- **`std::sort` 确定性 tie-break、同步装载序**等沿袭 01-03 的登记不变。
