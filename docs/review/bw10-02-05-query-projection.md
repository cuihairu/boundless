# BW-10-02-05 验证渲染与查询投影 — 复核证据

- **任务编号**: BW-10-02-05（lane `lane/bw10-terrain`，todo.md 行 1345）。
  **验收条款**: 同一编译空间两种消费者均可加载。
- **参照权威树**: `/home/cui/workspaces/BigWorld/programming/bigworld/lib/`。
- **原版源码**:
  - 查询投影: `lib/scene/collision_scene_view.{hpp,cpp}` ——
    `ICollisionSceneViewProvider`（hpp:24-35）`collide(start, end, sp, cs)`
    ray/prism 两面；`CollisionSceneView : SceneView<...>`（hpp:37-46）
    收集 provider 面，`collide`（cpp:15-32 ray / cpp:34-51 prism）逐个
    问询、**首个答 true 即以 `cs.dist_` 收束**（走完则答初始 dist）。
    `lib/scene/intersect_scene_view.{hpp,cpp}` `intersect`
    （cpp:14-31）每个 provider 都跑、回答数**求和**。
  - 结果集: `lib/scene/intersection_set.{hpp,cpp}` ——
    `LookUpTable<SceneObjectList>` 桶按类型存取，`insert`
    （cpp:28-31）`checkIndex` 增长到最大下标
    （`cstdmf/lookup_table.hpp:106-115`，空洞桶计入 `maxType`）；
    `objectsOfType` const/mutable 两面（cpp:33-41）；`clear`
    （cpp:49-56）逐桶清空、桶数不变；`numObjects`（cpp:58-67）
    求和；`maxType`（cpp:69-72）即桶数。
  - 上下文: `lib/scene/scene_intersect_context.{hpp,cpp}` ——
    `INCLUDE_DYNAMIC_OBJECTS(1<<0)`、`INCLUDE_STATIC_OBJECTS(1<<1)`、
    `ONLY_SHADOW_CASTERS(1<<2)`。
  - 事件视图: `lib/scene/object_change_scene_view.{hpp,cpp}` +
    `lib/scene/scene_event_view.{hpp,cpp}` ——
    `ObjectsEventArgs` 携带 announcing provider + 对象批
    （hpp:19-24）；三个具名委托列表 added/removed/changed
    （cpp:11-30）；`notifyObjects*` 以 `(scene(), args)` 逐个回调
    （cpp:32-63）；`SceneEventView::add/removeProvider` 刻意 no-op
    （"Ignore all providers in an event view"，cpp:9-17）。
  - 动态场景三面: `lib/space/dynamic_scene_provider.{hpp,cpp}` ——
    `getView`（cpp:128-141）把 `this` 以
    `ITickSceneProvider`/`ICollisionSceneViewProvider`/
    `IIntersectSceneViewProvider` 三身份递出；`onSetScene`
    （cpp:143-157）拉取并缓存 view/operation、注册 tick listener、
    解绑置空；`addObject`（cpp:160-171）插入**后**通知 added；
    `removeObject`（cpp:174-187）先解析对象再 swap、移除**前**
    通知 removed；`tick`（cpp:304-318）空对象早退 + MF_ASSERT
    已绑场景 + TickOperation 整片驱动；`onPreTick`（cpp:321-324）
    无事；`onPostTick` rebuild 后
    `MF_ASSERT(updatedObjects.size() <= numDirtyObjects_)` +
    notifyObjectsChanged + 清零；`updateAnimations`（cpp:405-419）
    同 tick 面；`collide`（cpp:421-446）MF_ASSERT 场景 + op +
    树查询候选交 `collideReferences`（cpp:255-300 解析链
    `objects_[objectLookup_[idx]]`、`object.isValid()` 才问）；
    `intersect`（cpp:449-496）`!includeDynamicObjects` 早答 0、
    visibility 树 hull 走、逐对象 insideOut 跳过 +
    `hull.intersects(bb)` 过滤、insert + 计数、
    `MF_ASSERT(numIntersected <= objects_.size())`；
    `insertObject_internal`（cpp:572-600）两棵树各插；
    `updateObject_internal`（cpp:603-623）staged 表 + dirty 旗标
    只计一次。
  - 通用树走: `lib/space/loose_octree.ipp:391-628` ——
    `HullIntersectShape`（节点盒逐平面支点判、全含平面位清零、
    子节点继承屏蔽位）与 `RayIntersectShape`
    （`intersectsRay(start, stop-start, hitPoint)`；
    ORIGIN_INSIDE 即含；INTERSECTION 须
    `(hitPoint-start).lengthSquared() < endRayDistSqr`）；
    动态树 `pushValidChildren` 跳过 INVALID 槽位。
  - 数学支撑面: `lib/cstdmf/convex_hull.hpp`
    （`intersectsBox(bb, flags)` 支点负角拒绝 + 位掩码进出）、
    `lib/cstdmf/boundbox.cpp` `clip`（310-388，逐轴 sMin/eMax
    夹逼、零位移轴半开端点判）、`intersectsRay`（117-149，
    候选面探测 + 其余两轴严格 `<`）、`intersectsRay(origin, dir,
    coord)`（215-299，候选平面走、`IR(f)` 位技巧、maxT 选面 +
    符号位拒绝 + 逐轴 epsilon）。
- **移植语义**（逐句核对后落点）:
  - `crates/scene` 新投影件：`collision_scene_view.rs`（ray/prism
    逐 provider 走、首个 true 以 `cs.dist` 收束）、
    `intersect_scene_view.rs`（计数求和）、`intersection_set.rs`
    （insert 增长到最大下标、mutable 面 `operator[]` 同长、
    clear 不缩桶数）、`scene_intersect_context.rs`（三常量 +
    include 动/静开关）、`object_change_scene_view.rs`
    （三具名委托列表，`(scene, args)` 回调；事件视图忽略
    provider）。物理扫描件 `SweepShape/SweepParams/CollisionState`
    与 physics 移植逐字共享，视图把裸 `(start, end)` 交 provider。
  - `crates/compiled-space/src/dynamic_scene.rs`：
    `DynamicSceneFaces`（`Weak` 回边）实现 tick/collision/intersect
    三身份面 + `DynamicSceneTickListener` 独立 Rc 形状面；
    `shared()` 装配双 Weak；tick/update_animations 空对象早退 +
    断言已绑 + 先取 `tick_operation` Rc 再借对象片；
    collide ray 走 `intersect_ray_segment`、prism 走
    `intersect_aabb(shape_box)`，候选项过
    `collide_references`（解析链 + `is_valid()` + 派发闭包）；
    intersect `!include_dynamic_objects` 早答 0、hull 走 + insideOut
    跳过 + `hull.intersects_box` 过滤 + 尾断言；add 通知在插入后、
    remove 先解析对象再 swap 前通知、`on_post_tick` 断言
    `updated <= num_dirty` 后 notify + 清零。
  - `crates/compiled-space/src/loose_octree.rs`：
    `intersect_hull`（栈携 `(nodeRef, u16 平面位)`，根 `!0`，全含
    平面位清零子节点继承）与 `intersect_ray_segment`
    （`endRayDistSqr` 内含判据），动态树跳 INVALID 槽位。
  - `crates/foundation/src/math/convex_hull.rs`：
    `intersects_box_flags` 负角拒绝 + 位掩码进出
    （`valid_planes_mask` 收敛入位）。
  - `crates/foundation/src/math/boundbox.rs`：`clip`
    （sMin/eMax 夹逼、负向轴换支分支、零位移轴半开判定）、
    `intersects_ray`（候选面探测）、`intersects_ray_hit`
    （候选平面走 + `to_bits()` 对应 `IR(f)` + maxT 选面 + 符号位
    + epsilon）。
  - `crates/scene/src/provider.rs`：`SceneBoundFaces`
    （tick_view/tick_operation/collision_operation/object_change_view
    四面）+ `SceneProvider::tick_listener/on_scene_bound/
    on_scene_unbound` 三钩；`Scene::add_provider` 物化四面 →
    `on_scene_bound` → 注册 tick listener → 推入视图 → 通知；
    `remove_provider` views 先撤 → listener 摘除 →
    `set_scene(None)` → `on_scene_unbound`。
- **边界登记**:
  1. **Scene-push 接线**（核心适配）: 参照 `onSetScene` 经 `Scene*`
     回指针**拉取** view/operation；Rust 借用规则禁止 provider 反拉
     → `Scene::add_provider` 在推入前物化 `SceneBoundFaces`（惰性
     view 恰好错过新 provider，与参照有效次序一致），再交
     `on_scene_bound` 存储、注册 listener。移除次序 views →
     collection → listener → 解绑逐句对齐。
  2. **验证渲染（cull/FrustumHull）不移植**: 本项只落查询投影
     （collision/intersect 两消费面）与事件视图；视锥剔除面
     （`FrustumHull`/cull view）随渲染车道另落。
  3. **未绑定时通知跳过**: 参照解绑后 `pObjectChangeView_` 置空、
     通知路径会空指针；移植未绑定时安静跳过通知、查询照常走树
     （解绑面 `bound_faces = None` 后 tick/collide 走参照的
     MF_ASSERT 翻译，两个 should_panic 钉住）。
  4. **绑定时取操作 vs 逐次取**: 参照每次 tick/collide 经 getView
     重取 lazy 单例；移植绑定时一次存入 `SceneBoundFaces`
     （同一惰性单例，语义等价，登记）。
  5. **IntersectionSet const 读不增长**: 参照 const `at` 经 mutable
     成员也会撑表（读越界 type id 抬高 `maxType`）；移植 const 面
     答空切片、桶数不动。
  6. **通知 provider 身份 = SceneToken**: 参照传 `this`
     `SceneProvider*`；移植传注册 token（arg.provider）。
  7. **面持 Weak 回边**: provider 释放后留在视图里的面安静应答
     （升级失败 no-hit / 0 / 早退）；参照即悬垂指针，登记为注册
     适配。
  8. **RuntimeTypeID = u8 按上下文计数**: 桶增长到最大下标、空洞
     桶计入 `maxType`（与参照 `checkIndex` 增长一致）；字面量
     >255 不可能（每上下文计数远小于上限）。
- **验收直接测试化**: 「同一编译空间两种消费者均可加载」→
  `dynamic_scene.rs::one_compiled_space_serves_the_collision_and_intersect_consumers`
  （同一 `Scene` 挂共享 provider：collision view 与 intersect view
  各收一面；ray 命中 0.4、内部起点 0、脱靶 -1、prism -1；
  6 平面 hull 交出 1 对象、排除动态上下文答 0；added/changed/
  removed 三列表通知齐；tick + post-tick rebuild + 移动后换位查询
  + update_animations 不通知 + 双重 stage 只计一脏）。新增
  `the_faces_answer_quietly_once_the_provider_is_dropped`（Weak 面
  全路径）、`queries_and_removals_defend_against_degenerate_bounds`
  （inside-out 过滤 + INVALID 槽位移除）、
  `a_visited_node_answers_only_the_objects_the_query_overlaps`
  （同叶邻居过滤）等。
- **样本非手造**: 本项无二进制夹具面（lib/scene + space 动态场景
  为纯运行时逻辑）；数值样本取自参照几何推演——ray 命中 0.4 为
  `(4-0)/10` 夹逼参数、prism 断言对 `SweepParams::full_distance()`
  归一（非裸段长）、hull 平面 d 值按「内部 = 负半空间」约定构造、
  `clip` 各分支端点由 sMin/eMax 夹逼参数手工复核。
- **门禁数字**:
  - `cargo test -p scene`: 38 passed, 0 failed。
  - `cargo test -p compiled-space`: 127 lib + 7 space_bin +
    4 scene_bin, 全绿。
  - `cargo test -p foundation`: 217 passed（math 104）, 全绿。
  - `cargo clippy -p scene -p compiled-space -p foundation
    --all-targets -- -D warnings`: 0 error。
  - `cargo fmt -p scene -p compiled-space -p foundation
    -- --check`: 干净。
  - `cargo check --workspace`: 无 error。
  - `cargo llvm-cov --lcov`: scene 11 文件最低 98.5%
    （provider 526/527=99.8%、collision_scene_view 129/131=98.5%、
    其余 100%）；compiled-space dynamic_scene 1409/1416=99.5%、
    loose_octree 738/741=99.6%；foundation boundbox 462/467=98.9%、
    convex_hull 152/152=100%。
