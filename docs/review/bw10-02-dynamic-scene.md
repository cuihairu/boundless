# BW-10-02-03 注册动态对象 — 复核证据

- **任务编号**: BW-10-02-03（lane `lane/bw10-terrain`，todo.md 行 1342）。
  **验收条款**: 移动和移除后查询结果匹配。
- **参照路径勘误**: 同 BW-10-02-01，参照权威树
  `/home/cui/workspaces/BigWorld/programming/bigworld/lib/`。
  巡检单所写路径不含此文件——动态提供者在 `lib/space/`（非
  `lib/compiled_space/`）：`dynamic_scene_provider.{hpp,cpp}`、
  `dynamic_scene_provider_types.hpp`。
- **原版源码**:
  - 提供者: `lib/space/dynamic_scene_provider.cpp` —— `removeObject_internal`
    (625-653)、`updateObject` (190-202)、`onPostTick` (330-…)、`resizeScene`
    (504-…)、`insertObject_internal` (572-600)、`updateObject_internal`
    (603-623)；`Detail::removeNodeContents` (237-247)、
    `Detail::removeObjectNode` (249-259)、`Detail::insertObjectNode`。
  - 句柄池: `lib/object_pool.hpp`（`Handle<24,8>`：24 位 index + 8 位
    generation 压 u32；`HandleTable` FIFO free list，`DEFAULT_GROW_BY=50`，
    create 取队首**不** bump generation，release bump（1<<8 回绕）压队尾；
    invalid==invalid 与 generation 无关）。
  - 八叉树动态面: `lib/math/loose_octree.ipp` —— `initialise` (210-230，
    MF_ASSERT depth≥1 + 无存量数据)、`fetchLeaf` (339-…，child 几何：
    halfSize=worldSize/(2<<depth)、offset=quarterSize、四向 side 位
    x∈{0,1}/y∈{0,4}/z∈{0,2}、childDepth==maxDepth 带顶位叶标志)、
    `insert` 的 inside-out 拒收、`findData` 只走不建、
    `updateNodeBound`/`updateHeirarchy` 祖先界修复（nodes==1 且
    maxDepth>0 时 updateHeirarchy 早退）。
- **Boundless 模块**: `crates/compiled-space` 新增 `dynamic_scene`
  （`DynamicSceneProvider` + `HandleTable`/`DynamicObjectHandle`）+
  `loose_octree` 增设动态半面 `DynamicLooseOctree`；`crates/foundation`
  增 `BoundBox::add_bound_box`（boundbox.ipp:211-231：inside-out 源跳过、
  inside-out 自身吸附、逐分量 min/max）。

## 移植语义（逐句核对）

1. **延迟提交**：`updateObject` 只写 `latest*` 三表 + 置脏
   （`numDirtyObjects_` 一次性 ++），查询面读 `current` 表——staged 状态
   对查询不可见，直到 `onPostTick`。`onPostTick` 无脏即早退；否则两树
   `reinitialise`、清 contents、把脏句柄 latest→current 拷回，再对**全部**
   句柄重插（collision 先、visibility 后，`update_heirachy=false`），最后
   `update_heirarchy`。
2. **句柄池不回收怪癖**：`removeObject_internal` 从不调用
   `dynamicObjectHandles_.release()`（原注释 "Leave the data in the slots
   behind since they'll be used for something else soon" 即在 640 行处）——
   被移除句柄表上仍 valid、index 单调增长永不复用。按原样移植并在
   `removed_handles_keep_pool_validity_and_indices_are_not_recycled` 钉死。
   `HandleTable::release` 本身的 generation bump 语义仍完整实现并测试
   （wrap、FIFO、stale 检测 = release 过期句柄 MF_ASSERT 炸）。
3. **removeObject 立即生效**：与 update 不同，remove 直接从两树摘除
   （叶 contents swap-remove，缺句柄 MF_ASSERT 炸），对象/句柄表
   swap-pop + lookup 修补（被移除 lookup 置 `size_t(~0)`）。update 后
   remove 在 post-tick 前发生时，脏旗被移除吸收
   （`an_update_then_remove_before_post_tick_is_absorbed`）。
4. **resizeScene 深度公式**：nodeSize = hint>0 ? hint : 20.0，
   maxDimension = max(width.x, height.y, depth.z)，
   maxDepth = clamp(log2ceil(maxDimension/nodeSize), 1, 8)；旧 contents
   换出后重建并按旧 contents 重插。log2ceil 按 `math_extra.cpp:104-119`
   位技巧（x≤1→0，否则 `32-(x-1).leading_zeros()`）。
5. **构造即两树**：默认构造以 INITIAL_WORLD_SIZE=2000/depth=1 bootstrap
   两树（对齐原版成员初始化），`partitionSizeHint_=-1`。
6. **inside-out 可见盒不入查询**：insert 拒收 inside-out AABB
   （`insert` 对 inside-out 返回 INVALID_NODE），对象仍在表内但查询不到
   （`inside_out_visibility_bounds_never_answer_queries`）。

## 边界（本任务未移植，登记）

- `BW::SceneObject` 本体——以 `DynamicObjectId = u32` 占位（objects_ 表
  元素语义），场景对象真实结构属实体层。
- `getView` / `onSetScene` / `tick` / `updateAnimations` / `onPreTick` /
  `collide`(Vector3 与 WorldTriangle 两重载) / `intersect`
  (SceneIntersectContext) —— 碰撞细分与视口面属后续任务
  （BW-10-02-04 委托接口、BW-10-02-05 投影验证）。
- 查询面移植的是 region 语义（visibility 树 → contents → 逐对象
  intersects 过滤，动态树 8 槽全扫不早停——与静态压缩子表「首 INVALID
  即停」不同，均按源对拍）。

## 验收测试

- `queries_match_after_movement_and_removal`：三对象西/东布局 → update+post
  tick 移 b 到东 → 西空、东 {2,3} → remove c → 东 {2}，往返两次结果稳定
  ——验收条款「移动和移除后查询结果匹配」直接测试化。
- 句柄池 5 测（FIFO 回收、generation 8 位回绕、默认步长扩容、invalid
  index 校验、invalid 相等忽略 generation）。
- 八叉树动态面 15 测（initialise 双断言、fetchLeaf 参照几何、球插入半径
  立方扩、祖先界修复、updateHeirarchy 合并与独根早退、叶引用翻译、
  findData 不建节点、reinitialise/reset 语义）。
- 提供者 14 测（含延迟提交可见性、update+remove 吸收、resize 深度、
  hint 缩放、inside-out 拒查、invalid 句柄忽略）。
- `log2ceil_matches_the_reference_bit_trick` 位技巧对拍；
  `add_bound_box_merges_and_snaps_like_the_reference`（foundation）。

## 门禁数字

- `cargo test -p compiled-space`：lib 116 + scene_bin 7 + space_bin 4 全绿。
- `cargo test -p foundation`：lib 203 全绿。
- `cargo clippy -p compiled-space -p foundation --all-targets -- -D warnings`：0。
- `cargo fmt -- --check`（两 crate）：0 漂移。
- `cargo llvm-cov -p compiled-space`：行覆盖 **2983/3002 = 99.37%**；
  其中 dynamic_scene.rs 695/708 = 98.16%、loose_octree.rs 690/690 = 100.00%。

## 状态

待 push 后 CI 复核（与三条跨席基线红对集，见 memory ci-fleet-gotchas §9）。
