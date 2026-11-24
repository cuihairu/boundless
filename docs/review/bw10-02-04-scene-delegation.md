# BW-10-02-04 实现委托接口与实体映射 — 复核证据

- **任务编号**: BW-10-02-04（lane `lane/bw10-terrain`，todo.md 行 1344）。
  **验收条款**: 实体可定位对应场景对象。
- **参照权威树**: `/home/cui/workspaces/BigWorld/programming/bigworld/lib/`。
- **原版源码**:
  - 类型系统: `lib/scene/scene_type_system.{hpp,cpp}` ——
    `RuntimeTypeID = uint8`、`GlobalUniqueTypeID = size_t`（hpp:16-17）；
    `TypeIDContext::getLocalID`（cpp:63-86）1 起连续分配，
    `MF_ASSERT(result != UNKNOWN)` 存满即断言；`getGlobalID`
    （cpp:88-101）越界答 `UNKNOWN`（0）；`generateSceneTypeID`
    （cpp:25-40）`hash_string(name)` 过全局上下文并记名表供
    `fetchTypeName`（cpp:42-46）；三个静态上下文
    （view/object/objectOperation，hpp:150-155）+ `SimpleMutex`。
  - hash: `lib/cstdmf/bw_hash.cpp:24-36` FNV-1 64 位
    （el7 x86-64 `size_t`=8：偏移 14695981039346656037、
    素数 1099511628211）；`bw_hash.hpp:23-37` `hash_combine`
    `seed ^= hash(v) + 0xBC6EF372 + (seed<<5) + (seed>>3)`。
  - SceneObject: `lib/scene/scene_object.hpp` —— handle/type/flags
    三元（hpp:16-136）；`operator==` 只比 handle+type（hpp:112-116，
    **flags 不参与**）；`hash<SceneObject>` 三者全折（hpp:262-273，
    零种子）；`isValid` 即 `handle_ != 0`（hpp:118-121）；
    `typeName`（hpp:123-128）object 上下文 `getGlobalID` 后查名表；
    `SceneObjectOperation<TypeHandlerInterface>`（hpp:146-254）
    `typeHandlers_` 按 object runtime id 存取，`addHandler` 重加即
    覆盖（hpp:247-251）、`removeHandler` 缺席为 no-op。
  - flags: `lib/scene/scene_object_flags.{hpp,cpp}` —— `StorageType
    = uint8`，五比特 `IS_DYNAMIC_OBJECT(1<<0)`、
    `IS_REFLECTION_VISIBLE(1<<1)`、`IS_CASTING_STATIC_SHADOW(1<<2)`、
    `IS_CASTING_DYNAMIC_SHADOW(1<<3)`、`IS_TERRAIN_OBJECT(1<<4)`；
    setter 为 clear-then-merge（cpp:30-34 及后续各对）。
  - Tick 视图: `lib/scene/tick_scene_view.{hpp,cpp}` —— `tick`
    （cpp:8-27）listeners onPreTick → providers tick → listeners
    onPostTick；`updateAnimations`（cpp:29-38）仅 providers；
    erase-remove 按指针值；`TickOperation::tick(objects, count)`
    （cpp:58-72）逐对象切一对象片问自家 type handler、
    `tick(typeID, ...)`（cpp:74-85）整片交一个 handler、缺席跳过。
  - 委托: `lib/scene/scene.{hpp,cpp}` + `lib/scene/scene_provider.{hpp,cpp}`
    —— `Scene::addProvider`（cpp:28-47）先断言「本 scene 未加过 +
    provider 当前未绑」，绑之、入列、通知全部既有 view；
    `removeProvider`（cpp:50-71）views 先撤、再出列、再解绑；
    `getView<InterfaceType>`（hpp:23-41 + createView 81-96）首问创建
    `pView->scene(this)` 后逐 provider `addProvider`，再问答同一视图；
    `getObjectOperation`（hpp:49-70 + 98-103）裸创建不接线；
    `SceneView<ProviderInterface>::addProvider`（hpp:80-89）问
    `provider->getView<ProviderInterface>()` 收非空；
    `SceneProvider::getView` 基类答 NULL（cpp:79-83）。
  - 实体映射链: `lib/space/dynamic_scene_provider.cpp` —— 注册
    `addObject` 落 `objects_` 并 `objectLookup_[handle.index_] =
    objects_.size()-1`；解析链 `objects_[objectLookup_[handle.index_]]`
    （cpp:183/289/366）；`removeObject_internal` swap-pop 后
    `objectLookup_[moved] = 旧位`、`objectLookup_[removed] = ~0`
    （池条目**不释放**）；查询 `intersection.insert` 逐个追加
    **不判重**。
- **移植语义**（逐句核对后落点）:
  - 新 crate `crates/scene`：`type_system.rs`（FNV-1/hash_combine/
    TypeIDContext 1 起连续/UNKNOWN 越界答 0/四静态上下文
    `LazyLock<Mutex<>>`）、`scene_object.rs`（三元、== 只比
    handle+type、hash 三元全折零种子、operations 按 id 存取）、
    `scene_object_flags.rs`（五比特 clear-then-merge）、
    `tick_view.rs`（pre→providers→post、updateAnimations 仅
    providers、逐对象切片派发）、`provider.rs`（Scene
    add-once/unbound 断言、remove 次序 views→collection→unbind、
    getView 惰性单例接线、getObjectOperation 裸单例、
    ask-and-collect 收集面）。
  - 实体映射：`crates/compiled-space/src/dynamic_scene.rs` 弃
    `DynamicObjectId = u32` 替身，`objects` 直存
    `scene::SceneObject`；`add_object(object, …)`、`object(handle)`
    解析链带「无效 handle」「已移除 handle」双断言；
    `query_region` 按树走查序**追加** `SceneObject`（不判重，
    参照 `intersection.insert` 语义）；swap-pop 处 lookup 记账与
    参照一致（移除 handle 表内保持 valid、槽位永不回收）。
  - 测试面：新增验收测试
    `an_entity_locates_its_scene_object_through_the_handle` ——
    实体构造带 flags 的 `SceneObject::typed::<Entity>(42, …)` 注册 →
    `object(handle)` 还原同 handle/type/flags → 查询结果同对象 →
    update+post-tick 后解析不变 → remove 后解析断言、表仍 valid。
- **边界登记**（偏离参照处，均在模块头声明）:
  1. `typeid(T*).name()` → `std::any::type_name::<T>()`：数值 id
     与 C++ 构建不同，进程内唯一/稳定语义一致；参照函数局部
     static 缓存 → 每次重算（同一名哈希入同一上下文，幂等）。
  2. 静态上下文/互斥：`SimpleMutex` + 函数局部 static →
     `LazyLock<Mutex<TypeIDContext>>`。
  3. `Scene*`/`ProviderInterface*` 裸指针图 → `SceneToken`
     （进程唯一 AtomicU64 自 1 起，0 即 NULL）+ `Rc` 面：所有权
     归属显式化，断言次序与派发次序不变。
  4. `getView` 擦除 `void*` → `SceneViewRef` 携带类型面
     （tick 先行；collision/intersect 面随查询投影项落地）。
  5. view 双存储：`Vec<Rc<RefCell<dyn ISceneView>>>` 迭代面 +
     `HashMap<u8, Rc<dyn Any>>`（内裹 `Rc<RefCell<T>>`）typed 取面
     —— `Rc<RefCell<dyn ISceneView>>` 无法 downcast 到具体视图。
  6. `getAs<T>` 不移植（无法自 handle 重建引用指针）；
     `SceneObject` 身份由注册点交入（参照从 `ObjectType*` 重解释）。
  7. provider `onSetScene` 需 `&mut self` 时经
     `ProviderSceneRef::replace` 先换后焚（闭包形式双借不可行）；
     scene 借位内的重入 hook 不支持（参照 C++ 无此约束，登记）。
  8. 动态场景的 view 接线/变更通知/collision+intersect 面 →
     BW-10-02-05（本项验收不涉及）。
- **验收直接测试化**: 「实体可定位对应场景对象」→
  `dynamic_scene.rs::an_entity_locates_its_scene_object_through_the_handle`
  （解析链 + 查询面 + flags 携带 + 移除后失联四断言）；既有
  BW-10-02-03 验收测试随载荷改写全数保留
  （`added_objects_answer_region_queries`、
  `queries_match_after_movement_and_removal` 等 12 项）。
- **样本非手造**: 本项无二进制夹具面（lib/scene 为纯运行时逻辑）；
  语义样本取自参照源码逐行——FNV-1 常数与 `hash_combine`
  常数按 el7 位宽钉死于测试，`TypeIDContext` 1 起连续/UNKNOWN
  语义、swap-pop 记账、FIFO 池序均有独立测试压住。
- **门禁数字**:
  - `cargo test -p scene`: 29 passed, 0 failed。
  - `cargo test -p compiled-space`: 117 lib + 7 space_bin +
    4 scene_bin, 全绿。
  - `cargo clippy -p scene -p compiled-space --all-targets
    -- -D warnings`: 0 error。
  - `cargo fmt -p scene -p compiled-space -p foundation
    -- --check`: 干净。
  - `cargo check --workspace`: 无 error。
  - `cargo llvm-cov --lcov -p scene`: 全五文件行覆盖 100%
    （provider 272/272、scene_object 175/175、scene_object_flags
    95/95、tick_view 201/201、type_system 127/127）。
