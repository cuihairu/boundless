# BW-10-03-03 查询 Terrain2 主材质和碰撞标志 — 复核证据

- **任务编号**: BW-10-03-03（lane `lane/bw10-terrain`，todo.md 行 1355）。
  **验收条款**: 同一点返回相同标识。
- **参照权威树**: `/home/cui/workspaces/BigWorld/programming/bigworld/lib/`。
- **原版源码**:
  - 容器: `lib/terrain/terrain_data.hpp:126-137`（`DominantTextureMapHeader`
    32 字节：magic "mat\0"=0x0074616d、version、numTextures、
    textureNameSize、width、height、pad×2；`VERSION_ZIP=1`）。
  - 读端: `lib/terrain/terrain2/dominant_texture_map2.cpp` —— `load`
    (108-197：not binary 132、wrong map type 144、zero dims 152、
    名窗 strncpy/min(strlen) 对 160-170、图像子块 172-180、
    溢出 183-186)；save 面（EDITOR_ONLY，47-104）作布局权威。
  - 查询面: `lib/terrain/dominant_texture_map.cpp` —— `materialIndex`
    (152-162，`+0.5f` 后 `int()` 截断、`(width-1)` 比例、`&0x0F`)、
    `groundType` (165-173，同坐标 `>>4`)、`texture` (208-217，
    `MF_ASSERT(id < names.size())`)、`width/height` (243-256)。
  - 压缩子块: `lib/resmgr/binary_block.cpp` —— `compress`
    (256-343，magic1 "zip\0"=0x7a697000 + magic2 0x42af9021 +
    未压尺寸 u32 + zlib)、`decompress` (350-393，`MF_ASSERT
    uncompress==Z_OK`)、`isCompressed` (407-419，len>12 且双 magic)。
  - 图像面: `lib/moo/image.ipp` —— `get` 双坐标钳制 (347-354)、
    `rawDataSize`=w·h·sizeof(u8) (900-903)、`copyFrom` 逐行 memcpy
    (861-871)。
- **Boundless 模块**: `crates/terrain` —— `dominant_texture_map`
  （`DominantTextureMapHeader::parse`、`is_zip_block`/`zip_decompress`
  （BinaryBlock zip 子块）、`U8Image`、`DominantTextureMap2::load` +
  `material_index`/`ground_type`/`texture` 查询）。

## 移植语义（逐句核对）

1. **名窗语义**: 每名固定 `textureNameSize` 字节窗，`strncpy` 拷入后
   `buf[min(strlen(p), size)] = '\0'`——即窗内首个 NUL 截断，无 NUL
   则保整窗（160-170 的 strncpy/min 对）。逐句移植。
2. **图像子块双分支**: `isCompressed()`（len>12 且双 magic）→ zlib
   解压；否则原样接收（save 恒压缩但 load 容忍未压分支——单元测试
   钉死）。`rawDataSize > len` 报溢出原文，否则 `copyFrom` 逐行拷满
   w·h，尾部多余字节忽略。
3. **zip 解压怪癖**: 声明尺寸取自头第三字；zlib 失败或流长于声明
   → 参考 `MF_ASSERT` panic（移植端 panic，catch_unwind 钉死）；流短
   于声明 → 参考保留声明长度、尾部为未初始化内存（移植端零填——
   登记分歧，确定性化）。
4. **+0.5f 取整怪癖（对比洞图）**: 材质/地面查询的坐标映射是
   `(x/blockSize)*(width-1) + 0.5f` 再 `int()` 截断（朝零、半数向上），
   且比例基是 `width-1` 而非洞图的 `width`——两处不同，逐句照抄。
   16.66/16.67（50/3 跨越）一根头发丝翻格，由真值与单元双侧钉死。
5. **双 nibble 拆分**: 低 4 位=材质索引（`&0x0F`），高 4 位=地面
   （碰撞）类型（`>>4`）——同一字节两查询，编辑器侧
   `pValues[x] = index & 0x0F`（dominant_texture_map.cpp:120）与
   `setGroundType` 的高位写入互不侵扰。
6. **名查询断言**: `texture()` 的 `MF_ASSERT(id < textureNames_.size())`
   ——移植端 panic（catch_unwind 钉死）。
7. **错误原文四条**: not binary / wrong map type / zero width-height /
   image data size overflow，逐字核对。
8. **截断即报告（登记分歧）**: 头不足 32 字节、名表越过 blob、
   图像子块长度下溢处，原版直读 OOB；移植端报
   `DominantLoadError::Truncated`（与 compiled-space/height/hole
   reader 同一登记）。名表尺寸乘法按 u32 回绕照抄
   （`wrapping_mul`），溢出后落 Truncated。
9. **TerrainMap 构造断言**: `!almostZero(blockSize)`（epsilon 0.0004）
   ——与 height/hole 两图同款。

## 边界（本任务未移植，登记）

- **save 写端（EDITOR_ONLY）**: el7 预编译库无 EDITOR_ENABLED，
  `DominantTextureMap2::save` 不可链——夹具字节按其源码行逐行复刻
  （真 `DominantTextureMapHeader` 结构 + 真 `strncpy` 定长名窗 +
  真 `BinaryBlock::compress`），并经原版 `load` 全程读回验证
  （.bin 装载成功 + 2049 探针真值）。
- **materialKind(x,z)**: 依赖 `MaterialKinds` XML 配置单例
  （physics2/material_kinds.hpp/cpp，`get()` 未知名/未配置返回 0）
  ——配置资源层不在本任务移植面；索引/名称/地面类型三标识已全查。
- **层混合构造器**（`DominantTextureMap2(blockSize, layers, mult)`，
  编辑器侧逐层双三次混合求主导层）不移植——读端任务。
- **资源层**: section 查找与外层 zip 属资源层；移植面收 section
  二进制字节（`Option<&[u8]>`，None=not binary）。

## 验收测试（夹具非手造）

- **原版结构+压缩产出夹具**: `docs/review/fixtures/terrain/
  dominant_texture_map/gen_dominant_texture_map.cpp` 链接 el7 预编译库
  （编译/链接同 BW-10-03-01/02 配方）：6×4 图三材质名（grass/rock/
  sand，名窗 18 定长 NUL 填充），胞元字节 `(ground<<4)|index`
  （index=(x+2y)%3、ground=(x/2+y)%5）；头/名窗/压缩子块按 save 源码
  行复刻，`canZip(false)` 后经 `BinSection->asBinary()` 回读原样
  （生成器断言未再压缩）。
  - `dominant.bin` 122 B，md5 `8b19d5a8826c2460ab6c4f2d4c40eea2`。
  - 真值由原版 `load` + `texture`/`groundType` 打印
    （`gen_dominant_texture_map.log`）: 1 MAP + 3 NAME +
    **1024 TEX + 1024 GT**（10×10 网格 × 32 取整毛发探针全集——
    两轴跨点 (k−0.5)·bs/(dim−1) 的两侧、半数整点、负/越界钳制）。
- `tests/dominant_texture_map_bin.rs`：逐行重放全部真值（MAP 维数 +
  NAME 名窗往返 + 全部 TEX/GT 探针）——**2049 探针全过
  =「同一点返回相同标识」**；布局断言（头六字、rock 窗 NUL 填充、
  zip magic 对与 24 字节声明）。
- 单元 13 测：四错误原文、两截断（头短/名表越界）、zip 往返、
  未压分支、名窗 NUL 截断/满窗/超窗截断、+0.5 取整矩阵（跨点毛发、
  半数整点、两轴、负/远钳制）、坏 zlib panic、名越界 panic、
  U8Image 钳制、almostZero 构造断言。

## 门禁数字

- `cargo test -p terrain`：lib 44 + dominant_texture_map_bin 2 +
  height_map_bin 4 + hole_map_bin 3 全绿。
- `cargo clippy -p terrain --all-targets -- -D warnings`：0。
- `cargo fmt -p terrain`：0 漂移。
- `cargo llvm-cov -p terrain`：行覆盖 dominant_texture_map.rs
  **308/308 = 100.00%**、height_map.rs **460/460 = 100.00%**、
  hole_map.rs **471/471 = 100.00%**、png.rs **392/395 = 99.24%**
  （≥98% 线达标）。

## 状态

待 push 后 CI 复核（与四条跨席基线红对集，见 memory ci-fleet-gotchas §9）。
