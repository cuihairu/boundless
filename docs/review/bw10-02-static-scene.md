# BW-10-02-02 注册静态场景对象 — 复核证据

- **任务编号**: BW-10-02-02（lane `lane/bw10-terrain`，todo.md 行 1340）。
  **验收条款**: 固定空间查询返回同一对象集合。
- **参照路径勘误**: 同 BW-10-02-01，参照权威树
  `/home/cui/workspaces/BigWorld/programming/bigworld/lib/`。
- **原版构建目标/源码**:
  - 提供者: `lib/compiled_space/static_scene_provider.hpp:80-115`（typedefs：
    `ObjectBounds=ExternalArray<AABB>`、`ObjectSphereBounds=ExternalArray<Sphere>`、
    `OctreeContents=ExternalArray<NodeDataReference>`、
    `OctreeNodeContents=LookUpTable<DataSpan,ExternalArray<DataSpan>>`）、
    `static_scene_provider.cpp:130-235`（`doLoadFromSpace` 全文：读序 octree →
    spans → contents → bounds → spheres → typeHeaders）。
  - 八叉树: `lib/math/loose_octree.hpp:33-44`（u16 node index、顶位叶标志、
    INVALID/ROOT 常量）、`hpp:161-172`（`isLeafRef`/`childIndexFromReference`）、
    `loose_octree.ipp:23-29`（Configuration 默认构造：depth 0/零中心/size 1/0）、
    `ipp:158-169`（`translateDataReferences`）、`ipp:439-460` + `ipp:576-628`
    （AABB 交查询驱动的一般栈遍历）、`ipp:551-566`（静态子表压缩后首 INVALID
    即停）、`ipp:662-665`（`empty()` = numDataEntries==0）。
  - 线格式: `lib/compiled_space/binary_format.cpp:122-137`
    （`read(StaticLooseOctree&)` = typed config + 5 个 ExternalArray：
    bounds 24/centers 12/nodes 16/dataRefs 4/parents 2 字节）、
    `binary_format.hpp:151-180`（`read(ExternalArray<T>&)` = typeSize+count+数据）。
  - 类型系统: `lib/scene/scene_type_system.hpp:16` —— **`RuntimeTypeID` 是
    `typedef uint8`**（本任务关键发现，见怪癖 §1）。
  - 模型数据: `lib/compiled_space/static_scene_model*`（`StaticModelData::read`/
    `Model` 84B 记录/资源引用 ExternalArray，BWSM 段）与写端
    `binary_writers/static_scene_model_writer.cpp`。
  - 球包围: `lib/math/sphere.hpp/.cpp:26-31`（`fromBoundBox`：中心=盒心，
    半径=对角线半长）。
- **Boundless 模块**: `crates/compiled-space`（新增 `loose_octree` /
  `static_scene` / `static_scene_model`）+ `crates/foundation/src/math/sphere.rs`
  （`Sphere::from_bound_box`）+ `crates/compiled-space/tests/scene_bin.rs`。

## 夹具来源登记（BW-10-02-02）

**`docs/review/fixtures/compiled_space/scene/scene.bin`（1886 字节，
md5 `fc54c56c64e07ec2318d112ea77f7f77`）是原版写端代码的原生产物**：

1. `gen_scene.cpp`（与样本同目录存档）直编原版 writer 源
   （binary_format_writer + string_table_writer + asset_list_writer +
   settings_writer + static_scene_writer + octree_writer +
   static_scene_model_writer，Linux g++ 对 `build/cmake-resmgr` 既有库链接，
   同 BW-10-02-01 方法），写出 5 段：BWST(262)/BWAL(40)/BWCS(28)/BWSS(936)/
   BWSM(476)。内容：10 条字符串、3 资产、5 个静态模型对象（缩放
   10/4/2/4/2，分布于四角+中心），模型资源 `models/box_{a,b}.model`
   （同目录 `models/` 存 m01–m05.xml 与 box visual/model）。
2. 写完后同程序用**原版读端**（`BinaryFormat::Stream::read` 重载 +
   octree intersect → span 展开 → 球过滤链）回放 8 个探针区域并打印，
   输出 `scene_out.txt`（126 行，md5 `182bfc8f4940acbf3dd5cb9595568fa1`，
   已归档）为移植对拍基准；TYPEHDR 行与 PROBE/PROBE_TYPES 行即真值。

## 原版结果（源码核对）

- **读序**（doLoadFromSpace）：octree →
  `OctreeContents`（LookUpTable.storage() 仍走 ExternalArray 重载）→
  contents → bounds → spheres → typeHeaders。
- **ExternalArray 线格式**：u32 typeSize（须 == sizeof(T)）+ u32 count +
  count×T 裸数据。
- **`read(StaticLooseOctree&)`**：config 作 typed read（u32 前缀 24 + 载荷），
  随后 5 个 ExternalArray。
- **静态存储拓扑**（真样本实测）：写端 DFS 排序 + 压缩子表 —— root 子表
  [8,6,4,1]+INVALID×4；内节点各持 1 个叶引用（node1→[LEAF|3,LEAF|2]、
  node4→LEAF|5、node6→LEAF|7、node8→LEAF|9）；dataRefs
  [INV,INV,4,0,INV,2,INV,3,INV,1]；parents [INV,0,1,1,0,4,0,6,0,8]；
  叶查询命中经 `translateDataReferences` 批量过 `dataReferences_` 翻译。

## 原版怪癖登记（逐面移植时保留并在代码处引用）

1. **TypeHeader 的 typeID 只有 1 字节**：`SceneTypeSystem::RuntimeTypeID` 是
   `typedef uint8`（scene_type_system.hpp:16），`TypeHeader` 实际布局
   `{u8 typeID_; 3 字节未初始化 padding; u32 numObjects_}`，写端不清零
   padding——真样本原始字节 `00 70 bb 96 05 00 00 00`（0x70BB96 为栈垃圾）。
   读端只取首字节 + [4..8] 的 u32。移植 `TypeHeader::from_le_bytes`
   按此实现并有专门测试
   `type_header_ignores_the_uninitialised_padding`。
2. 未知 typeID **非致命**：doLoadFromSpace 只打 warning
   "StaticScene binary contains unknown object typeID N." 后继续。
3. 模型 handler 失败**非致命**：读坏只追加 warning，场景其余部分照常可用。
4. `Configuration` 默认值不合 derive 语义：参考构造是 depth 0/零中心/
   **size 1.0**/0（`loose_octree.ipp:23-29`），Node 默认全 INVALID——移植手写
   `Default` 而非 derive（derive 会给 0.0/child[0;8]，已测试钉死）。
5. 球半径=对角线半长：`fromBoundBox` 中心=盒心、半径=|(max-min)|/2
   （fixture 实测半径 14.177447/5.744563/1.5，与 sqrt(804)/2 等吻合）。
6. 元素解析不可失败：参考以固定 stride memcpy 搬运 ExternalArray 元素、
   无逐元素校验；移植在 `read_external_array` 保证 count×elem_size 字节后用
   `as_chunks` 均分，元素解析以 expect 守卫（非死 `?` 分支）。

## 新版结果

- `cargo test -p compiled-space`：**lib 85 + scene_bin 7 + space_bin 4 全绿**；
  `cargo test -p foundation`：**lib 202 全绿**（含新 sphere 测试）。
- `cargo fmt -p compiled-space --check`、`cargo fmt -p foundation --check`
  0 漂移；`cargo clippy -p {compiled-space,foundation} --locked --all-targets
  -- -D warnings` 0 findings。
- llvm-cov 0.9.1（cargo llvm-cov，执行测试后统计）：**行覆盖
  1962/1968 = 99.70%（门槛 ≥98%）**；余 6 行（static_scene.rs 2、
  static_scene_model.rs 4）为 region 折算（JSON 导出无独立 count=0 行段），
  如实登记不再追。
- 集成测试以真样本逐面核对：段表 5 段全 v1 与长度 [262,40,28,936,476]；
  字符串 10 条全量；octree config（maxDepth 2/numData 5/中心 (-1.5,4.75,0)/
  worldSize 88）与 10 节点五数组全量（含上述拓扑）；spans [i,i]×5、
  contents [0..4]、typehdr [{0,5}]（含 padding 怪癖）；object_bounds 5 个、
  球中心精确+半径 1e-4 容差；5 个 model 变换/资源引用 [3,4,6,7,8,9,3,4,3,4]/
  动画/flags/dyes 与 `model_resource_strings`/`model_scene_index`。

## 验收核对（固定空间查询返回同一对象集合）

- `fixed_regions_return_the_reference_object_sets`：8 个探针区域
  （whole/m01–m05/empty/two，坐标与 gen_scene.cpp 逐字一致）结果与
  `scene_out.txt` PROBE 行真值全量对拍（objects 集合按 BTreeSet 比较）。
- `repeated_queries_return_the_same_object_set`：whole 与 narrow 两区域各
  重复查询 4 次集合逐次相等——验收条款直接测试化。

## 证据链接

- `crates/compiled-space/src/{loose_octree,static_scene,static_scene_model}.rs`、
  `crates/foundation/src/math/sphere.rs`、
  `crates/compiled-space/tests/scene_bin.rs`。
- 夹具：`docs/review/fixtures/compiled_space/scene/{scene.bin,scene_out.txt,
  gen_scene.cpp,octree_statics.cpp,shim_max.hpp,models/}`。
- 原版活基准：`programming/bigworld/build/cmake-resmgr/`（产样链接）、
  `target/bwsample/scene_out.txt`（运行副本，归档同 md5）。

- **状态**: 待 push 复核。
