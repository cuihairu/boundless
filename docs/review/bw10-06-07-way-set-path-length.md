# BW-10-06-07 实现 waySetPathLength（跨两个 waypoint set 的路径长度返回值与原版一致）

**验收**：跨两个 waypoint set 的路径长度返回值与原版一致。

## 参照面链

`EntityNavigate::waySetPathLength`（entity_navigate.cpp:987-1005）→ 系统面
`NavmeshEntityNavigationSystem::waySetPathLength`
（navmesh_navigation_system.cpp:536-541）→
`navigator_.getWaySetPathSize()`（navigator.cpp:368-371）→
`pCache_->getWaySetPathSize()`（navigator_cache.cpp）。

1. 脚本脸只有一查：`!entity_.isRealToScript()` →
   `ERROR_MSG( "EntityNavigate::waySetPathLength: This method can only
   be called on a real entity." )` 后 **`return 0`**——不抛异常，脚本
   拿到 0。参照注释 `TODO: Move this to RealEntity`（端口留档）。
2. **无载具检查**（navigate/navigateStep 都拒，本面不查）。
3. 系统面直通 `getWaySetPathSize()`：缓存里 way-set path 的状态数
   （"length of the most recently determined navigation path, in
   chunks"——按 set 计数）。
4. 端口建模：ghost 不抛错 → 按 `navigate_path_points` 先例返回
   `(answer, warnings)`，ghost 臂答 `(0, [ERROR 文本])`；real 臂答
   `(navigator.way_set_path_size(), [])`。

## 验收（实证）

夹具：06-06 的 `portal_chain(2)`（两片单位正方形集 + permissive +
has_chunk_item 门户）与 slope 资产。

- **跨两 set（验收核心）**：fresh 缓存 → **0**；A→B 一跳
  `navigate_position`((0.4,0.4,0.5)→(2.8,0.4,0.5)) 答穿越点
  (2.8,0.6,0.5) 后 → **2**（"A then B"）；`clear_cached_paths()` 后 →
  **0**。
- **同 set 不建 way-set path**：slope (1,0.5,1)→(7,3,7)（单记录单
  set）→ way-set 缓存保持 **0**，但 waypoint path 照常缓存 1 点——
  `searchNextWaypoint` 只在 `!same_set(src, dst)` 时跑 set 级 A*
  （navigator.cpp:441 门），同 set 路线根本不触 `saveWaySetPath`。
  （直觉的"同 set 答 1"是错的，实证钉死。）
- **ghost** → `(0, ["EntityNavigate::waySetPathLength: This method can
  only be called on a real entity."])`。

**夹具坑**：`square_set` 模板的 girth 是 **0.6**（模板第二个 f32 字
段），不是 slope 资产的 0.8——portal_chain 探针必须传 0.6，否则端点
navloc 非法触发 MF_ASSERT 门（06-06 测试全部隐式用 0.6，本项首次踩
明）。

## 怪癖（钉死）

1. **ghost 不抛异常**：ERROR_MSG 后 `return 0`——与 06-02/06-06 的
   TypeError 拒绝不同；端口按 navigatePathPoints 的 warnings 管道建模
   （`(0, warnings)`）。
2. **同 set 路线答 0**：way-set path 只在跨 set 搜索时构建；"1" 的直
   觉来自状态级 `init` 的补态（那条会 pad 到 ≥2 态），set 级
   `ChunkWaypointSetStatePath::init` **不补态**，且同 set 根本不进
   搜索。
3. **无载具检查**：本面连 ghost 都不抛，更无载具拒绝。
4. `getWaySetPathSize` 未入 server-runtime 的 `Navigator` trait（控制
   器不用它）——脸面沿 `navigate_path_points` 先例直接吃
   `&MeshNavigator`，`MeshNavigator` 上补公开薄封装
   `way_set_path_size()` 委托 waypoint crate 既有的
   `get_way_set_path_size()`（06 簇前已交付，navigator.rs:399）。

## 未盖行（共 10 行，99.48%）

与 06-06 完全相同的防御集（行号因插入平移）：
`DestinationUnresolved` 返回 + message 臂 ×2 处（06-02/06-06）、
`canNavigateTo` 目的地非法臂 ×2、06-01 失败路线永不重入循环守卫 2 行、
`SafetyCountExceeded` 返回 + message 臂。**06-07 新增代码全覆盖**
（ghost 警告臂与 real 臂由两条新测试击中）。

## 门禁

- `cargo test -p server-runtime`：**1150 过 + 1 忽略**（含 3 条新
  waySetPathLength 测试，模块共 29）。
- `cargo test --workspace`：**4450 过 / 0 失败**。
- `cargo fmt -p server-runtime -- --check`、
  `cargo clippy -p server-runtime --all-targets -- -D warnings` 全绿。
- 覆盖（llvm-cov 0.9.1 `--lcov`，DA max 合并，`cargo clean -p` 后测）：
  `entity_navigate.rs` **99.48%**（1925/1935）。

证据测试：`crates/server-runtime/src/entity_navigate.rs`
（way_set_path_length 脸 + MeshNavigator::way_set_path_size + 3 测试）。
