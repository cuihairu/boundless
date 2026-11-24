# BW-10-03-01 读取 Terrain2 高度图 — 复核证据

- **任务编号**: BW-10-03-01（lane `lane/bw10-terrain`，todo.md 行 1351）。
  **验收条款**: 格点和插值坐标吻合。
- **参照权威树**: `/home/cui/workspaces/BigWorld/programming/bigworld/lib/`。
- **原版源码**:
  - 容器: `lib/terrain/terrain_data.hpp:22-42`（`HeightMapHeader` 32 字节：
    magic "hmp\0"=0x00706d68、width、height、compression、version、
    minHeight f32、maxHeight f32、pad；`COMPRESS_RAW=0`）。
  - 读端: `lib/terrain/terrain2/terrain_height_map2.cpp` ——
    `load` (356-375)、`loadHeightMap` 三分支 (377-532)、
    `heightAt(float,float)` (702-763)、`heightAt(int,int)`；hpp 的
    `refreshInternalDimensions` (267-277)、`internalHeightAt` (299-302)、
    `DEFAULT_VISIBLE_OFFSET=2` (48)。
  - 量化: `lib/terrain/height_map_compress.cpp` —— `QUANTISATION_LEVEL=0.001`
    (24)、`quantise` (32-35，毫米格 ±0.5 截断=远离零)、`unquantise`
    (44-47)、`compressHeightMap` 的 'qpng' 标记 (192-198)。
  - 换算: `lib/terrain/terrain_height_map.hpp:16`（`DEFAULT_UNIT_SCALE=0.01`
    厘米→米）、`convertHeight2Float` (215-218)、`almostZero` 0.0004
    （`lib/stdmf.hpp:616`；`TerrainMap` 构造断言 `terrain_map.hpp:30-33`）。
  - 网格取值: `lib/moo/image.ipp:347-354`（`Moo::Image::get` 边界钳制）。
  - PNG 面: `lib/moo/png.cpp:324-395`（`decompressPNG` = 纯 libpng 行读、
    无 transform；`compressPNG` 261-267 两种形状：RGBA8 / GRAY16）。
- **Boundless 模块**: 新 crate `crates/terrain` —— `height_map`
  （`HeightMapHeader::parse`、`TerrainHeightMap2::load` + 网格/插值查询、
  `quantise`/`unquantise`）+ `png`（仅两种地形形状的最小 PNG 解码：
  RGBA8 与 GRAY16，非隔行，全 filter 组）。

## 移植语义（逐句核对）

1. **三载入分支**: v1 ABS_FLOAT 原样读 f32；v3 REL_UINT16_PNG 走 PNG；
   v4 ABS_QFLOAT = 'qpng' 标记 + 32bpp RGBA PNG（逐像素 int32 毫米）。
   v2 REL_UINT16 原版 `loadHeightMap` 不处理——落错误分支
   "Invalid TerrainHeightMap2 format %u."，按原样拒绝。
2. **v3 位重释怪癖**: 头内 min/max 是 float，此分支却按
   `*((int32*)&header->minHeight_)` 取原始位型作 int32 偏移
   （cpp:395-396、424）——相对高度加偏移在 int32 空间完成再乘 0.01。
   按原样移植并由
   `relative_uint16_adds_the_offset_in_int32_bit_space` 钉死。
3. **插值不是基类双三次**: `TerrainHeightMap2::heightAt(float,float)`
   override 为奇偶对角三角插值——`(xOff ^ zOff) & 1`（有符号 int32
   异或，负数同样成立）选对角；奇偶两枝各有 `(1.0-xf) > zf` /
   `xf > zf` 的三角划分与四个顶点公式，逐式照抄（cpp:702-763）。
4. **16 位 PNG 端序怪癖**: PNG 16 位样本线上为大端，但原版从不调
   `png_set_swap`，x86 主机上 `Moo::Image<uint16>` 按本机 LE 读——
   与 PNG 规范差一次字节交换。移植端 `sample16` 按 LE 组合保持
   位一致。
5. **min/max 反序交换**: 三个分支都做 `if (max < min) swap`
   （v1 cpp:432-433、v3 转换后、v4 同）；逐分支移植。
6. **lod 与边框几何**: `width()/height()` 返回含不可见边框的 poles 维；
   lod>0 时 `internalVisibleOffset` 塌缩为 0（cpp:63-92 构造），
   lod0 为 DEFAULT_VISIBLE_OFFSET=2；`internalBlocks = poles −
   2·off − 1`，spacing = blockSize / internalBlocks；
   `heightAt(int,int)` = 钳制 get(格点+off)。
7. **量化往返**: quantise 毫米格 ±0.5 截断（正 0.0015→2、负
   −0.0015→−2）；unquantise 是 f32 乘法——unquantise(1500) 落在
   1.5 上方 1 ulp（1.5000001），与原版 float 算术一致。
8. **截断即报告（登记分歧）**: 定长读越界处原版直读 OOB；移植端
   报 `LoadError::Truncated`（与 compiled-space section reader 同一
   登记）。quantise 的 C 强转对超范围值是 UB，`as` 饱和—— unreachable
   分支上的登记分歧。

## 边界（本任务未移植，登记）

- **写端**: `compressHeightMap` / `compressPNG` 不在移植面（读端任务）；
  夹具由原版写端产出（见下）。
- **资源层**: `load(DataSection)` 的 section 查找与 `BinaryBlock`
  解压（`pBin->isCompressed()`）属资源层——移植面收已解压字节。
- v3 无原版写端（全树 grep 无产生者）——legacy 只读格式；单元测试
  手工构帧钉死其读取语义。

## 验收测试（夹具非手造）

- **原版写端产出夹具**: `docs/review/fixtures/terrain/height_map/
  gen_height_map.cpp` 以 el7 预编译库链接原版
  `compressHeightMap`（libpng）产出两份 .bin；真值由原版
  `TerrainHeightMap2::load` + `heightAt` 两面打印
  （`gen_height_map.log`）。
  - `heightmap_qfloat.bin` 3530 B，md5 `4b8c8d85f088cbef710b16d27a76064d`
    （VERSION_ABS_QFLOAT 走 compressHeightMap）；
  - `heightmap_absfloat.bin` 8132 B，md5
    `32bd3624a6f15eb3279f841a580625cd`（VERSION_ABS_FLOAT 原样 f32）；
  - 编译旗标（cmake-entitydef flags.make 同款）：
    `-DCODE_INLINE -DCSTDMF_IMPORT -DMF_CONFIG='"hybrid"' -DMF_SERVER
    -DMF_USE_ASSERTS -D_HYBRID -DBW_BUILD_PLATFORM='"el7"'
    -I$BW/lib -I$BW/third_party`；链接序
    `-lterrain -lmoo -lresmgr -lmath -lphysics2 -lscene -lcstdmf
    -lpng -lz`（physics2+scene 供 WorldTriangle::intersects）；
    `BinSection(tag, BinaryBlock)` 供 DataSection。
  - 真值规模: 每夹具 4 条 MAP（poles/min/spacing/blocks）+ 82 条 GRID
    （`heightAt(int,int)` 步 5 及远角）+ 441 条 INTERP
    （`heightAt(float,float)` 步 5.0，%.6f）——两夹具共 8/164/882。
- `tests/height_map_bin.rs`：逐行重放全部真值——GRID 容差 1e-6
  （%.6f 打印舍入；-6.2230005 与 -6.223001 同一 f32）、INTERP 容差
  1e-4（打印舍入 + 可能的 FMA 收缩）；magic 三分诊（垃圾→NotHeightMap、
  空→Truncated、翻转字节→NotHeightMap、原样→过）；
  量化/绝对两夹具同源图——格点逐点 ≤5.1e-4、min/max 位相等。
  **验收条款「格点和插值坐标吻合」= 1046 条真值探针全过。**
- 单元 19 测：三分支载入（v1 原样+反序交换、v3 位重释+反序+维数
  不符拒绝、v4 qpng+反序+尺寸断言）、版本拒绝原文、两处截断、
  损坏 PNG→DecompressFailed、四臂插值矩阵（奇偶×三角划分×等值
  归属）、负坐标钳制、lod 边框塌缩、almostZero 构造断言、量化往返、
  Display 原文。
- PNG 解码 17 测：全 filter 组真编解码（RGBA8+GRAY16）、LE 怪癖钉死、
  走查八类缺陷（签名/边界/CRC/IHDR 长/压缩法/隔行/缺 IEND/空图）、
  尺寸不符与坏 filter 行。

## 门禁数字

- `cargo test -p terrain`：lib 19 + height_map_bin 4 全绿。
- `cargo clippy -p terrain --all-targets -- -D warnings`：0。
- `cargo fmt -p terrain`：0 漂移。
- `cargo llvm-cov -p terrain`：行覆盖 height_map.rs **460/460 =
  100.00%**、png.rs **373/377 = 98.94%**（≥98% 线达标）。

## 状态

待 push 后 CI 复核（与四条跨席基线红对集，见 memory ci-fleet-gotchas §9）。
