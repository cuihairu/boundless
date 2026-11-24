# BW-10-03-02 读取 Terrain2 地形洞 — 复核证据

- **任务编号**: BW-10-03-02（lane `lane/bw10-terrain`，todo.md 行 1353）。
  **验收条款**: 洞查询与原版一致。
- **参照权威树**: `/home/cui/workspaces/BigWorld/programming/bigworld/lib/`。
- **原版源码**:
  - 容器: `lib/terrain/terrain_data.hpp:78-86`（`HolesHeader` 16 字节：
    magic "hol\0"=0x006c6f68、width、height、version——save 写 1，
    load 从不回读）。
  - 读端: `lib/terrain/terrain2/terrain_hole_map2.cpp` —— `load`
    (345-420，NULL 错误 408)、`holeAt(float,float)` (599-613)、
    holeAt 矩形 (615-642)、`recalcHoles` (556-591)、`create` (422-447)、
    `lock` (212-239)、`image` (241-262，`MF_ASSERT(lockCount_)`)、
    `unlock` (265-293)。
  - 载入侧 stride: 同文件 36-37（`HoleHelper` 构造
    `(width/8) + ((width & 3) ? 1 : 0)`）与 38 行的
    `MF_ASSERT(magic_==MAGIC)`。
  - pow2: `lib/math/math_extra.cpp:49-73`（`largerPow2` 位抹：
    0→1、>2^31 封顶并 WARNING）。
  - 位图面: `lib/moo/image.ipp` bool 特化 —— `get/set` 双坐标钳制
    (347-354 区)、`getRawRow(y)` 仅上侧钳制（`y>=height-1` 取末行，
    :915 区）、`resize` 填充 memset 0xff/0x00（尾位补 true，:302 区）、
    `isEmpty()`=data 空指针 (:233)、`clear()` 保留 width/height。
- **Boundless 模块**: `crates/terrain` —— `hole_map`（`HolesHeader::parse`、
  `BoolImage` 位图面、`TerrainHoleMap2::load/lock/unlock/create` +
  两面 `hole_at` 查询、`larger_pow2`）。

## 移植语义（逐句核对）

1. **载入侧 stride 怪癖（按原样移植）**: 读端行距
   `(width/8) + ((width&3) ? 1 : 0)` 对 width%8==4 的宽度比写端
   ceil(width/8) 少一字节——行被重采样；10%8=2 的夹具走干净路径，
   12 宽单元测试钉死重采样。`min(src_stride, rawRowSize())` 的服务端
   拷贝面照抄。
2. **magic 校验是 MF_ASSERT（panic）不是错误分支**（cpp:38）——移植端
   assert panic，`foreign_magic_panics_like_the_reference_assert` 钉死。
3. **NULL section 报原文**: "TerrainHoleMap2::load - NULL DataSection"
   （cpp:408）；`load(None)` 映射该路径。load 先复位全状态再解析
   （失败不留旧图）。
4. **状态推演**: `holesSize_=header.width_`、`holesMapSize_=
   largerPow2(width)`（cpp:372）；`largerPow2` 位抹 + 0→1 + 2^31 封顶
   全数照抄。拷贝后 `recalcHoles()`：全扫 `allHoles &= v`、
   `holes |= v`、`noHoles = !holes`；退化（全有/全无）清图但保留维数
   （`clear()` 语义）——再锁再编辑的退化重灌机制。
5. **点查询截断朝零**: `int32((x/blockSize)*width)` 的 C 强转对负数
   朝零（−5→格 0，非 −1）；先答 allHoles/noHoles 两快枝，再
   `image.get` 双坐标钳制（105/50→(9,5)）。
6. **矩形查询**: floorf 四边、含端闭区间双循环、首个洞即真；
   `(-50,-50,200,200)` 全图探针即此面。
7. **锁面**: MF_ASSERT 嵌套规则 `lockCount_==0 || readOnly_==readOnly
   || readOnly_==false`（写锁可叠任何锁、只读锁上叠写锁 panic，
   cpp:217）；首锁定 readOnly；空图且维数非零时
   `resize(width,height,allHoles_)` 重灌（memset 0xff=尾位补 true）。
   unlock 计数无符号回绕减（多余 unlock 不报错）、`!readOnly` 时
   recalc、随后 `readOnly_=true`（cpp:265-293）。
8. **create(size)**: holesSize=size、holesMapSize=pow2、宽高=size、
   无洞、图清空（cpp:422-447）。
9. **0x0 防御臂（照原样）**: 零维头跳过拷贝；参考 `new uint8[0]`
   仍是非空分配——空扫后 allHoles 与 noHoles 同真再清图；
   未 create/load 的新图写锁不重灌、unlock recalc 走空图臂
   （all=false/none=true）。
10. **截断即报告（登记分歧）**: 定长读越界处原版直读 OOB；移植端报
    `HoleLoadError::Truncated`（与 compiled-space/height map reader
    同一登记）。

## 边界（本任务未移植，登记）

- **写端与客户端面**: `save`/`HoleHelper`（含 DXT1 块打包）、
  `texture/createTexture/updateTexture` 不在移植面（服务端读端任务）；
  夹具由原版写端产出（见下）。
- **资源层**: `load(DataSection)` 的 section 查找与 `BinaryBlock`
  解压属资源层——移植面收已解压字节（`Option<&[u8]>`，None=NULL）。
- **生成器噪声（良性）**: `TerrainSettings::init(BLOCK_SIZE,NULL)` 在
  服务端构建打印 "Terrain version 100 is unsupported"——洞图面只取
  blockSize，不影响输出（.bin/.log 均为确定产物）。

## 验收测试（夹具非手造）

- **原版写读两端产出夹具**: `docs/review/fixtures/terrain/hole_map/
  gen_hole_map.cpp` 链接 el7 预编译库（`-lterrain -lmoo -lresmgr
  -lmath -lphysics2 -lscene -lcstdmf`，`-Wl,--start-group` 包组 +
  resmgr 重复；旗标同 BW-10-03-01）：原版 `save`=HolesHeader+位行经
  BinaryBlock 压缩入 section，dump 解压后载荷（36 B = 16 头 + 10×2 行）；
  真值由原版 `load`（走 section 压缩二进制的引擎真路径）+ `holeAt`
  两面打印（`gen_hole_map.log`）。三夹具:
  - `holes_mixed.bin` 36 B，md5 `3df27f6eebcb4f122792c69a31fcb0ac`
    （create+7 洞编辑+unlock recalc 混合）；
  - `holes_none.bin` 36 B，md5 `b756bd3c3c674d9c777b8df6b6792558`
    （create 即存——退化无洞）；
  - `holes_all.bin` 36 B，md5 `0286feaaf61a5e41aa92b5e238a2a467`
    （100 格全洞+unlock→recalc 清图置 allHoles；save 经
    lock(true) 以 allHoles_ 重灌 0xff）。
  - 真值规模: 3 条 MAP（六字段全对账）+ 317 条 HOLE（10×10 格心
    + 6 边缘/越界探针）+ 18 条 RECT（含单点、贴边、全图）。
- `tests/hole_map_bin.rs`：逐行重放全部真值（MAP 宽/高/holesSize/
  holesMapSize/noHoles/allHoles 六字段 + 全部 HOLE/RECT 探针）；
  布局往返（magic/宽/高/version 字节位、全 0xff/全 0 行区分）。
  **验收条款「洞查询与原版一致」= 338 条真值探针全过。**
- 单元 11 测：largerPow2 位抹、create 几何、NULL 原文、截断、
  magic panic（catch_unwind）、混合图点/矩形探针（含负坐标朝零、
  越界钳制）、退化往返、12 宽 stride 重采样、锁面编辑 recalc、
  嵌套写锁 panic、零维/新图防御臂。

## 门禁数字

- `cargo test -p terrain`：lib 31 + height_map_bin 4 + hole_map_bin 3
  全绿。
- `cargo clippy -p terrain --all-targets -- -D warnings`：0。
- `cargo fmt -p terrain`：0 漂移。
- `cargo llvm-cov -p terrain`：行覆盖 hole_map.rs **471/471 =
  100.00%**、height_map.rs **460/460 = 100.00%**、png.rs
  **392/395 = 99.24%**（余 3 行为夹具编码器 paeth b/c 臂与
  unreachable，非解码逻辑；≥98% 线达标）。

## 状态

待 push 后 CI 复核（与四条跨席基线红对集，见 memory ci-fleet-gotchas §9）。
