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
