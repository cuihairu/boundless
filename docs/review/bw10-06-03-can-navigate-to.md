# BW-10-06-03 实现 canNavigateTo（可达与不可达样本结果吻合）

**验收**：可达与不可达样本结果吻合。

## 参照面链

`EntityNavigate::canNavigateTo`（entity_navigate.cpp:957-976）：

1. `isEqual( maxSearchDistance, -1.f )` → `CellAppConfig::maxAoIRadius()`，
   **先于** real 检查（与 navigate/navigateStep 同序）。
2. 非 real → TypeError（同文本）。**没有载具检查**——与 navigate /
   navigateStep 不同，坐在载具上的实体在本面照走。
3. 委托 `navigationSystem().canNavigateTo(...)`（系统面是 `const`）。

系统面（navmesh_navigation_system.cpp:431-518）：

1. `!pChunkSpace()` → ValueError
   （`NavmeshEntityNavigationSystem::canNavigateTo(): Physical space of
   entity does not support navigation queries`）。
2. 源 `createPathNavLoc`（空间构造器 + clip）——非法 → **`Py_RETURN_NONE`**
   （注释里的 "Could not resolve source position" 只存在被注释掉的
   DEBUG_MSG 里）。
3. 目的地**裸空间构造器**，先验合法性**后** `clip()`——非法 → 同样
   `Py_RETURN_NONE`。
4. 同 waypoint 短路：`wp = dstLoc.point()` + **`srcLoc`**.makeMaxHeight。
5. `findPath( blockNonPermissive=true, …, Navigator::READ_CACHE )`，
   且参照把「门开着时存下的路径可能已过时」的 `clearCache()` **注释掉了**。
6. 找到 → `wp = dstLoc.point()` + **`dstLoc`**.makeMaxHeight；找不到 →
   `Py_RETURN_NONE`。返回 `Script::getData( wp )`，无路径即 None。

本项与 06-01/06-02 的差别全在「怎么答」：navigateStep 走
`getNavigatePosition`（跳点）并把实体 y 落位；本面答**终点**（clip 后
的 dst），且不动实体。

## 验收（slope/shell 资产，05-05/06 字节）

- **可达样本**：实体 wp1 (1,0.5,1) → 终点 (7,3,7)：终点 clip 到
  (7,3.5,7)，dstLoc.makeMaxHeight 保住 3.5 → 答
  `Some((7,3.5,7))`（脚本拿到的「最可达位置」）。
- **同 waypoint 短路**：终点 (2,0.5,2) 也落 wp1 → 答
  `Some((2,1.5,2))`（clip 抬到 maxHeight 1.5，源抬高同值）；此臂**不
  写缓存**（缓存仍空）。
- **READ_CACHE 的两个怪癖臂**：
  - **空缓存照填**：读缓存只在「已有路径且源/目的地状态不匹配」时保护
    `pCache_`（`navigator.cpp` 缓存臂）；空缓存走 `searchNextWaypoint`
    进成员缓存，故首次成功探针后 `cached_waypoint_path()` 非空。
  - **错配对走 scratch**：实体移到 (7,3,7)、终点 (1,0.5,1)（wp0→wp1
    反向）与缓存的源/目的状态都不匹配 → 搜索进 scratch 缓存，答
    `Some((1,1.5,1))`，而**成员缓存一字未动**（路径与首次一致）。
- **缓存命中无视限长**：首探针（-1）后，同对端点改 `maxSearchDistance`
  = 1.0（≤ gridSize，本应拦下 8.9 m 候选）仍答
  `Some((7,3.5,7))`——命中路径只 `moveStateAlongPath`，不带限长
  （`navigator.rs` `search_next_local_waypoint` 的命中臂）。
- **不可达样本**（三例皆 None，不报错）：
  - shell 地板 (2.5,0.5,4.5) → 桥 (3,1.5,3.5) 跨层无邻接 → None。
  - girth 0.5 无记录匹配 → 源非法 → **静默** None（对比 navigateStep 的
    "Could not resolve source position."）。
  - 新网格 + `maxSearchDistance` 1.0（无缓存可借）→ AStar 跳过 8.9 m
    候选 → None。
- **拒绝面**：ghost → `NotReal`（TypeError 文本逐字）；非 chunk 空间 →
  `NoSpace`（canNavigateTo 版 ValueError 文本逐字）；**载具实体不拒**，
  照答 `Some((7,3.5,7))`。

## 怪癖（钉死）

1. **无载具检查**：navigate/navigateStep 拒绝的本面照走。
2. **非法端点答 None 不报错**（与 06-02 的报错版同链不同出口）。
3. **同 waypoint 臂用 src 抬高、找到臂用 dst 抬高**（同一 waypoint 时
   两者同值，差别只在参照逐字）。
4. **READ_CACHE 空缓存照填**、**错配对走 scratch 不动 pCache_**。
5. **缓存命中路径不带限长**：命中即走，1.0 这类限长被无视（与 06-02
   的 ReadWriteCache 不复验同源，本面经 `searchNextLocalWaypoint` 命中
   臂落地）。
6. **`clearCache()` 在参照里注释掉了**，端口同样不留清空动作。
7. 目的地非法臂**防御不可达**：源/目的共用同一 girth 过滤，源合法 ⇔
   目的合法（与 06-02 的 DestinationUnresolved 同因），保留逐字并档。

## 未盖行（防御/永不重入，共 5 行，99.51%）

`DestinationUnresolved` 返回 + 其 message 臂（06-02 登记）；
`canNavigateTo` 的目的地非法臂（怪癖 7）；06-01 跨层失败测试中「首个
重查询即败」永不重入的循环守卫 2 行。

## 门禁

- `cargo test -p server-runtime`：**1136 过 + 1 忽略**（含 3 条新
  canNavigateTo 测试，模块共 15）。
- `cargo test --workspace`：**4436 过 / 0 失败**。
- `cargo fmt -p server-runtime -- --check`、
  `cargo clippy -p server-runtime --all-targets -- -D warnings` 全绿。
- 覆盖（llvm-cov 0.9.1 `--lcov`，DA max 合并，`cargo clean -p` 后测）：
  `entity_navigate.rs` **99.51%**（1025/1030）。

证据测试：`crates/server-runtime/src/entity_navigate.rs`
（can_navigate_to 脸 + 3 测试）。
