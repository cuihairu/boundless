# BW-10-06-05 实现 navigatePathPoints（成功路径与无路径错误对照）

**验收**：成功路径与无路径错误对照。

## 参照面链

`EntityNavigate::navigatePathPoints`（entity_navigate.cpp:1005-1040）→
系统面（navmesh_navigation_system.cpp:425 附近 `navigatePathPoints`
脸）→ `Util::navigatePathPoints`（:15-75）→
`Navigator::findFullPath`（navigator.cpp:177-235）→
`addWaypointPathPoints`（:26-47，匿名命名空间）。

1. ghost 检查：**只 WARNING 不拒绝**——
   `"EntityNavigate::navigatePathPoints: This method was called on a
   ghost.\n"` 后照走（06 簇唯一不拒 ghost 的脸）。
2. `isEqual( maxSearchDistance, -1.f )` → `maxAoIRadius()`（警告之后、
   网格工作之前）。
3. `srcLoc = createPathNavLoc(...)`（系统面）——**会 clip**；源注释
   "Don't clip the source point" 是陈旧的（参照实际路径照 clip）。
   checkFirstPosition=false → 源点不前置进结果。
4. `Util::navigatePathPoints`：`findFullPath` 答假 → ValueError
   `"Full navigation path could not be found"`；答真返回点表。
5. `Navigator::findFullPath`：两端点 bare-space 构造；
   `if (src.valid() && dst.valid())` 门（任一非法直接答假）；两端各
   clip 一次；**scratch NavigatorCache**（"We don't clobber the
   entity's current navigator cache"——成员缓存不动）；
   循环 `searchNextWaypoint(cache, current, dst, maxSearchDistance,
   blockNonPermissive=true)` → 每跳 `addWaypointPathPoints` 后
   `current = path.pDest()->navLoc()`；`waypointsEqual(dst)` → 追加
   `dst.point()` 收官；搜索失败 → 清空点表、答假。
   参照局部 `ChunkWPSetState dstSetState( dst )` 写而不读——未移植
   （登记）。
6. `addWaypointPathPoints`：拷贝路径、`while isValid: push
   pNext()->navLoc().point(); pop()`——起点态跳过；端口用索引走
   `reverse_path[len-2]..reverse_path[0]` 等价同序（避免给
   ChunkWaypointStatePath 加 Clone）。

## 验收（slope/shell 资产，05-05/06 字节）

- **成功路径（slope，(1,0.5,1)→(7,3,7)）**：点表 =
  `[(3,3.5,3), (7,3.5,7)]`——**穿越点在前、终点在后**。关键语义：
  `isGoal` 打在**穿越态**上（05-05 已钉死的怪癖），链末态是边界穿越点
  而非终点，`findFullPath` 的 `waypointsEqual(dst)`（按 waypoint 判）
  仍真 → 追加 `dst.point()`。与 06-02 `findPath` 的 "Use dst as point
  might not match" 替换臂不同——findFullPath **不替换**，穿越点保留。
  显式限长 100.0 同表（-1 只是换 maxAoIRadius 的哨兵）。
- **同 waypoint（(1,0.5,1)→(2,0.5,2)）**：单态 walk 被 `init` 补到
  ≥2 态 → 链推源自身点 (1,1.5,1)，再追加终点 (2,1.5,2) =
  `[(1,1.5,1), (2,1.5,2)]`。
- **scratch 缓存**：全程走完 navigator 成员缓存仍空
  （`cached_waypoint_path().is_empty()`）。
- **无路径错误对照**：shell 地板→桥跨层 `Err(PathNotFound)`，message
  `"Full navigation path could not be found"`；委托空间空网格（两端
  非法 → valid 门答假）同错；girth 0.5 无记录匹配同错。三者错误对象
  相同、无文本差异。
- **ghost**：`Ok` 且 warnings =
  `["EntityNavigate::navigatePathPoints: This method was called on a
  ghost."]`，路径照答（slope 同表）。

## 怪癖（钉死）

1. **ghost 只警告不拒绝**：06 簇 navigate/navigateStep 都拒，
   navigatePathPoints（与 canNavigateTo 的静默 None 一样特殊）只
   WARNING 后照答——端口按 navigation_controller.rs 先例建模为返回
   `Vec<&'static str>` 警告。
2. **穿越点保留、终点追加**：`isGoal` 落在穿越态 → 链末点=边界穿越点
   (3,3.5,3)，`dst.point()` 追加其后；findPath 的终点替换臂不在此脸。
   「一跳路线答两遍终点」的直觉是错的（那是 findPath 单步语义）。
3. **同 waypoint 起点也进表**：`init` 补态让单态 walk 推出一个点 =
   源自身点。
4. **系统面源点 clip**（陈旧注释 "Don't clip the source point" 与实际
   路径矛盾，端口照 clip）。
5. **scratch 缓存**：findFullPath 明确不 clobber 成员缓存（端口
   NavigatorCache::default() 局部）。
6. **无 real/载具检查**：本脸连 ghost 都不拒，更无载具拒绝。
7. 端口委托空间建模：`MeshNavigator::new(Vec::new(), GRID_SIZE)` 空
   集合 → navloc 非法 → valid 门答假 → 同 PathNotFound。

## 未盖行（防御/永不重入，共 5 行，99.67%）

与 06-02/06-03/06-04 登记相同的防御集：`DestinationUnresolved` 返回 +
message 臂、`canNavigateTo` 的目的地非法臂、06-01 失败路线永不重入
循环守卫 2 行。06-05 新增代码全覆盖（NavigatePathPointsFailure 的
message 臂由无路径对照测试击中）。

## 门禁

- `cargo test -p server-runtime`：**1141 过 + 1 忽略**（含 2 条新
  navigatePathPoints 测试，模块共 20）。
- `cargo test -p waypoint`：全绿（find_full_path + 跨 chunk 测试）。
- `cargo test --workspace`：**4441 过 / 0 失败**。
- `cargo fmt -p server-runtime -- --check`、
  `cargo fmt -p waypoint -- --check`、
  `cargo clippy -p server-runtime --all-targets -- -D warnings`、
  `cargo clippy -p waypoint --all-targets -- -D warnings` 全绿。
- 覆盖（llvm-cov 0.9.1 `--lcov`，DA max 合并，`cargo clean -p` 后测）：
  `entity_navigate.rs` **99.67%**（1488/1493）。

证据测试：`crates/server-runtime/src/entity_navigate.rs`
（navigate_path_points 脸 + NavigatePathPointsFailure + 2 测试）；
`crates/waypoint/src/navigator.rs`（find_full_path +
add_waypoint_path_points）。
