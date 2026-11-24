# BW-10-06-01 实现 navigate（固定路线到达时触发对应回调）

**验收**：固定路线到达时触发对应回调。

## 参照面链

`EntityNavigate::navigate`（entity_navigate.cpp:597-637）：

1. `isEqual( maxSearchDistance, -1.f )` → `CellAppConfig::maxAoIRadius()`，
   **在任何拒绝之前**解析（所以 `-1` 的 ghost 也先换好限长再被拒）。
2. 非 real → `PyErr_SetString( PyExc_TypeError, "This method can only be
   called on a real entity." )`。
3. 在载具上 → `PyErr_SetString( PyExc_ValueError, "Cannot call
   Entity.navigate() for an entity on a vehicle, either alight from the
   vehicle first, or call Entity.navigate() for the vehicle." )`。
4. `navigationSystem().createController( ... )`（navmesh_navigation_system.cpp:86-93）
   = `new NavigationController( ... )`，随后 `entity_.addController`。

参照的 `createController == NULL → EnvironmentError` 臂只有分配失败能到，
端口构造器不可能失败，按 `NAVIGATION_CANCELLED` 状态保真先例丢弃（模块
doc 已登记）。

## MeshNavigator — `Navigator` 边界落在真网格上

server-runtime 已有 `navigation_controller.rs` 的 `Navigator` 边界（BW-08
交付）；本项在 `entity_navigate.rs` 用 waypoint crate 的两级搜索把它实现
成 `MeshNavigator`（向下边依赖，与 db/mercury 同例；workspace `Cargo.toml`
补 `waypoint` 成员映射）：

- **`createPathNavLoc` 脸**（`navloc_at`）：`find_by_girth` 选 set（girth
  过滤 + find-closest 兜底）→ `NavLoc::from_parts` → `NavLoc::clip()`。
  空间构造器存的是**未抬升**点（navloc.cpp:31-58，`+0.01` 抬升只为 chunk
  查找），clip 再把存点拉进所在 waypoint。`navloc.rs` 补公开 `clip()`
  薄封装（`clip_in_place` 的公开脸），并补一条直测（invalid clip 不动点）
  后 navloc.rs 覆盖回 100%。
- **`getNavigatePosition` 脚**（navmesh_navigation_system.cpp:550-627）：
  同 waypoint 短路（`waypointsEqual` → `dst.point()` + `src` 的
  `makeMaxHeight`）；否则 `findPath( blockNonPermissive=true, …,
  CacheState::ReadWriteCache )`（navigator.hpp:49 的默认缓存），hop 点
  y = `max( makeMaxHeight 抬升后, 找到的 y )`，半合法 hop（waypoint < 0）
  的抬升取自 src。
- **`pSet()` 脸**：`waypoint_set` 返回 set 指针身份（`Rc::as_ptr`）。
- **缓存脸**：`getCachedWaypointPath` / `clearCachedWay{,Set}Path` 委托
  waypoint 的 `Navigator`。

`MF_ASSERT( srcLoc.valid() && dstLoc.valid() )` 保留为断言（控制器经
`clip_to_mesh` 保证两端合法）。

## 验收路线（slope 资产，`gen_generate.log` HX 导出字节）

05-05/05-06 已证端口资产与原版逐字节同；本项从 log HX 重建 slope 记录，
`MeshNavigator` 驱动 `NavigationController`：

- 起点实体 7 在 (1, 0.5, 1)（wp1），`navigate((7,3,7), v=2, closeEnough
  0.4, userArg 42, controllerId 4, maxSearchDistance=-1)`。
- **clip 脸**：终点 (7,3,7) 落 wp0，x/z 不动、y 抬到 maxHeight 3.5 →
  控制器目的地 (7, 3.5, 7)。
- **固定路线**：首跳 = 网格穿越点 (3, 3.5, 3)（05-05 S1 同路线），随后
  直线走向终点。
- **到达**：参照在**行走中途**完成——`|x-dst.x| < closeEnough &&
  |z-dst.z| < closeEnough` 一旦成立立即回真（不吸附到终点）；实测停在
  (6.677, 3.5, 6.677)，落窗内且走在抬升后的 3.5 高度。`onNavigate`
  回调带 entity 7 / controller 4 / userArg 42，控制器随后取消。
- **失败面**（shell 资产）：地板 (2.5,0.5,4.5) → 桥 (3,1.5,3.5) 跨层无
  邻接，首个重查询 `onNavigateFailed` {7, 5, 7}，实体未动。
- **脚本拒绝面**：ghost → `NotReal`（TypeError 文本逐字）、载具 →
  `OnVehicle`（ValueError 文本逐字）；`-1` → `maxAoIRadius`（450），
  显式 30 保真；velocity 2 @ 10Hz → metresPerTick 0.2。

## 怪癖（钉死）

1. **clip 抬 y**：`ChunkWaypoint::clip` 尾部 `makeMaxHeight`
   （chunk_waypoint.rs:184-237 端口逐字），水平已在内则只抬高度——
   (1,0.5,1)→(1,1.5,1)、(7,3,7)→(7,3.5,7)。
2. **find-closest 兜底让远点仍合法**：girth 匹配时再远的点也解析到最近
   waypoint，「离网 None」脸只能用**无记录匹配的 girth**（0.5 对 0.8 记录）
   触发；`MF_ASSERT` 探针同理（girth 0.5 让两端 navloc 全非法）。
3. **中途完成不吸附**：完成判据打在移动后的位置上，closeEnough 窗口一
   合即停，终点 y 是行走高度（3.5）而非起点原值。
4. **`-1` 先于拒绝**：限长解析在 real/vehicle 检查之前。
5. **EnvironmentError 臂丢弃**（分配失败不可达，先例登记）。
6. **空间构造器存未抬升点**，clip 才抬；`from_guess` 的 `+0.01` 是另一条
   构造路径的事。

## 门禁

- `cargo test -p server-runtime`：1128（1127 过 + 1 忽略），含 6 条新
  entity_navigate 测试。
- `cargo test --workspace`：**4426 过 / 0 失败**（4420 + 6）。
- `cargo fmt -p server-runtime`、`cargo fmt -p waypoint` 全绿；
  clippy `-D warnings` 两 crate 全绿。
- 覆盖（llvm-cov 0.9.1 `--lcov`，DA max 合并，`cargo clean -p` 后测）：
  `entity_navigate.rs` **99.47%**（378/380，未盖两行 = 失败路线测试中
  「首个重查询即败」故永不重入的循环守卫）；`navloc.rs` **100%**
  （299/299）。crate 全文件最低 98.32%（chunk_waypoint_set_state_path.rs，
  既有水位）。
- 顺手清债（独立 style 提交）：`cargo fmt -p server-runtime` 暴出 4 个
  未触文件的历史 fmt 差（watcher/forwarding.rs、watcher/mod.rs、
  lib.rs replay_data 排序、tests/db_backend_isolation.rs 导入序）与
  server_app.rs 5 处 clippy（bool_assert_comparison ×2、
  unnecessary_cast ×3，新 clippy 1.100 收紧）。

证据测试：`crates/server-runtime/src/entity_navigate.rs`（模块 + 6 测试）。
