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
  - 宽空间前缀：`|x|` 或 `|z|` 触发 4/8/12 位三级哈希子目录（`001xx001xx/sep/…`），`singleDir` 关闭。
  - 网格↔坐标：`outsideChunkIDToGrid` 对 bit15 符号扩展；`gridFromChunkName` 只取末 9 字符；`outsideChunkIdentifier` 用 u16 回绕算术。
  - 室外块 transform/包围盒由网格坐标 × gridSize 派生；室内块读 `<transform>`/`<boundingBox>`，首个 `<model>` 在无 `<shell>` 时作 shell。
  - 对象集合 = 逐子节点 tag（shell/transform/boundingBox 不计入），`<include>` 递归展开。
- **新版结果**:
  - `cargo test --workspace` 全绿（chunk 14 lib + 3 集成；layering 注册通过）。
  - 标识↔坐标往返扫掠覆盖全 i16 双轴（2048 点）；前缀三级、单目录、回绕、大小写分歧各成一测。
  - `GeometryMapping::from_settings` 读 `chunkSize`/`bounds`/`singleDir`；`gather_chunks` 枚举 18×2 室外格 + 1 室内块 = 37。
  - 夹具文件解析：原点格 4 对象（2 model + light + sound）、前缀远格 (17,0)、室内块 transform/bbox/3 对象，逐项断言。
  - clippy `-D warnings`（scoped `-p chunk --all-targets`）0 findings；`cargo fmt -p chunk --check` 0 漂移。
- **证据链接**: `crates/chunk/src/{identifier,mapping,chunk,boundary,space}.rs`、`crates/chunk/tests/space.rs`、`docs/review/fixtures/chunk_space/`。
- **状态**: 待 push 复核。

## 夹具来源登记（BW-10-01-01）

任务书允许「无原版转换器样本则最小手造样本并如实登记来源」。本树内无原版转换器产出的样本 chunk 目录（`programming/bigworld` 树未含 sample 资源），故手造 `space_00010001` 如下：

- **坐标系**: 与参考一致，x 右 z 下（`z` 即网格 y），gridSize=100（`base_chunk_space.hpp` `DEFAULT_GRID_RESOLUTION`）。
- **settings.xml**: `chunkSize=100`、`bounds` 矩形 0..17 × 0..1、`singleDir=false`。bounds 语义按 `GeometryMapping`（`geometry_mapping.cpp:156-236`）——本地格矩形含两端点。
- **标识/前缀**: 格 (17,0) 触发 4 位前缀级（`001xx000xx/sep/00110000o`），与 `ChunkFormat::outsideChunkIdentifier` 输出一致；其余格平铺（`00000000o`..`00010001o`）。
- **对象集**: 每室外格 1-2 个 `<model>`/`<entity>`；原点格另含 `<light>`/`<sound>`。室内格 `00000000i.chunk` 采用编辑器形态（`editor_chunk_cache.cpp:1167-1179`）：`<transform>` row3=150 0 250、`<boundingBox>` −10..10、`<shell>` + model + entity + light。
- **内容字段**: `<resource>`、`<position>`、`<transform>`、`<id>`/`<type>`/`<instantiate>` 均按 `chunk format.txt` 与 `chunk.cpp` 的解析路径填写，取值任意但合法（解析器只关心结构，不解释资源本体）。

## BW-10-01-01 派生痕迹

- `outside_chunk_identifier(x,z,single_dir)`（`identifier.rs`）复刻 `chunk_format.hpp:21-54`：u16 回绕判三级前缀；测试钉死 (17,0)→`001xx000xx/sep/00110000o`、(4096,4096)→三级、(−4097,−4097)→`exxxexxx/…/efffefffo`、(−32768,32767)→`8xxx7xxx/…/80007fffo`。
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

（见对应 leaf 提交。）
