# BW-14-02-04 移植 Controller/Extra 样例 — 复核证据

- **任务编号**: BW-14-02-04（todo.md 行 1746）
- **验收**: 注册、回调及卸载可验。
- **Boundless 模块**: `crates/server-runtime/src/cellapp_extension.rs`
  （`crates/physics/src/world_triangle.rs` 增补 `bounce`）
- **平台**: Linux x86_64
- **交付 commit**: `8e6db9f`

## 原版形状

参考 `programming/bigworld/examples/cellapp_extension/`（13 文件 1142 行，
`BW_ADD_LIBRARY(... MODULE ...)` 加载时解析的共享库）由四个 Extra 与两个
Controller 组成，是 CellApp 的 Controller/Extra 机件的现成范例：

| 原版位置 | 形状 |
|---|---|
| egextra.cpp:35-46 | `EgExtra::helloWorld()` 打印 `egextra: hello world`；`addEgController(userArg)` = `entity_.addController(new EgController, userArg)`，返回 ControllerID |
| egcontroller.cpp:15-30 | `IMPLEMENT_CONTROLLER_TYPE(EgController, DOMAIN_REAL)`；`startReal` → `CellApp::instance().registerForUpdate(this)`；`stopReal` → `MF_VERIFY(deregisterForUpdate)`；`update()` 打 TRACE |
| speed_extra.cpp:50-77 | `trackSpeed(enable)`：`!entity_.isReal()` → TypeError 返回 false；`enable == isEnabled()` → WARNING 后返回 true；enable → `addController(new SpeedController, 0)`；disable → `pController_->stop()` |
| speed_extra.cpp:106-133 | `writeToStream`：`timeOffset = lastTime_!=0 ? timestamp()-lastTime_ : 0`，`stream << speed_ << velocity_ << timeOffset`；`readFromStream` 读回后 `if (timeOffset != 0) lastTime_ = timestamp()+timeOffset`（**对端机器计数器不同**） |
| speed_extra.cpp:150-165 | `onMovement(oldPosition)`：`lastTime_!=0 && thisTime>lastTime_` 时 `delta=(thisTime-lastTime_)/stampsPerSecondD()`、`velocity_=(position-old)/float(delta)`、`speed_=float(velocity_.length()/delta)`；末尾无条件 `lastTime_=thisTime`（故首次移动只记戳） |
| speed_extra.cpp:167-181 | `movementCallback`：`SpeedExtra::instance.exists(*pEntity)` 才调 `onMovement`；`StaticIniter` 挂 `g_entityMovementCallback` |
| speed_controller.cpp:37-95 | `DOMAIN_GHOST\|DOMAIN_REAL`：`startGhost` → `SpeedExtra::instance(entity)`（建实例）；`startReal` → `setController(this)`；`stopReal` → `setController(NULL)`；`stopGhost` → `instance.clear(entity)`；`writeGhostToStream`/`readGhostFromStream` = 基类 + extra 状态 |
| bounce_grenade_extra.cpp:94-197 | 无 ChunkSpace → TypeError；外层循环 `vel[1] += -9.8f*dTime*0.5f`；内层 `newPos=endPos+vel*dTime`、`distance=cs->collide(endPos,newPos,ct)`、`resTriangle=ct.triangle()`；命中则 `proportion=(distance-radius)/vel.length()` 负截 0、`endPos+=vel*proportion`，未达 `maxBounces` 时 `resTriangle.bounce(vel,1.0f); vel=elasticity*vel; endPos+=vel*(dTime-proportion)`；静息判据 `(endPos-worldPos).length()/dTime < 0.25` 或 `maxSamples>0 && loopCount>=maxSamples`；收尾 `vel.length()<0.25 → vel=(0,vel[1],0)`；返回 `(worldPos, loopCount*timeSample)` |
| dump_controllers_extra.cpp:90-203 | `visitControllers(callback)`：`PyCallable_Check` 不过 → TypeError；逐 controller 回调 `callback(typeName, exclusiveID)`，返回 false 即提前终止；返回 `!hasErrorOccurred()` |

物理缝：`WorldTriangle::bounce`（`worldtri.cpp:504-512`）——
`normal.normalise(); proj = normal.dotProduct(v); v -= (1+elasticity)*proj*normal`。
`physics::world_triangle::WorldTriangle` 原先只有 `normal()`，本项补齐 `bounce`。

## 端口语义

`cellapp_extension.rs` 按上述五类逐一落地，外加一个 `SampleEntity` 测试宿主代行
参考的 Entity + CellApp 两半（控制器集与 exclusive 注册表、extra 注册与槽、
`Updatables` 更新注册、时钟）——原版样例是运行中的 CellApp 驱动的共享库，
端口没有 CellApp 主循环，故以宿主驱动，这正是「注册、回调及卸载可验」的落地方式。

控制器生命周期事件经 `pump` 翻译成样例回调：`Started` 跑 `startReal`
（`EgController` 注册更新、`SpeedController` 建 extra 并接管），`Stopped` 跑
`stopReal`/`stopGhost`（注销更新、清控制器、毁 extra）。`SpeedController` 是
ghost+real 双域，宿主无 ghost 实体，故其 `Started` 一并跑两个半。

| 面 | 端口语义 |
|---|---|
| 注册 | `add_eg_controller(userArg)` 走 `Controllers::add_controller`，real 域得 `Started{initial:true}` 并进 `Updatables`；`track_speed(true)` 装 `SpeedController` 并置 extra 槽；`register_exclusive` 的类得 `0x8000` 起的 id |
| 回调 | `call_updatables` 逐 tick 驱动 `EgController::update`（计数）；`move_to` 只在 extra 存在时回调 `on_movement`（复刻 `movementCallback` 的存在性检查）；`visit_controllers` 逐个回调 (typeName, exclusiveID)，false 提前终止 |
| 卸载 | `unload` = `controllers.destroy()` + `pump` + `extras.clear_all()`：全部 Stopped 逐一流水（注销更新、毁 extra），两次卸载安全 |
| 弹球 | `bounce_grenade(space, …)` 完整循环：半重力、命中比例负截 0、`bounce(vel,1.0)` 后再乘弹性、`maxBounces`/`maxSamples` 帽、静息阈值 0.25、水平速度截零，返回 `(落点, loopCount*timeSample)`；无 space → `SampleError::NoPhysicalSpace` |
| ghost 流 | 24 字节：`speed` f32 + `velocity` 3×f32 + `timeOffset` u64（LE）；读侧 `timeOffset!=0` 时 `last_time = now + offset`；短流拒绝 |

## 有意偏离

1. **`debug_extension.cpp`（BWDebug `calcStreamedProperties`）未移植**：它依赖
   cell-data 属性流基础设施（`CELL_DATA` 属性统计），不在本项的
   Controller/Extra 面内。
2. **`PyCallable_Check` 校验以类型化 `FnMut(&str, ControllerId) -> bool`
   不可表达**：端口回调恒报成功（等价于原版「未发生错误」）。
3. **`stampsPerSecondD()` 取常量 `1_000_000`**：原版读 CPU 计数器频率，端口固定
   一个刻度——该值只以比值出现，不影响速度数学。
4. **碰撞缝抽象为 `CollideSpace::collide(from, to) -> (f32, WorldTriangle)`**：
   原版 `ChunkSpace::collide(startPos, endPos, closestTriangle)` 的返回值即段上
   距离（`0.0` 为空）；测试宿主给一个 `Floor`（解析命中距离 + 世界坐标三角形）。
5. **两个防御臂不可达**（按档记档）：`pump` 中「Started 而容器里已无该控制器」
   与 `visit_controllers` 中「live 表在容器里查不到」——公开路径无法构造。

## 门禁（本项）

| 门禁 | 结果 |
|---|---|
| `cargo test -p server-runtime` | **1150 passed / 0 failed / 1 ignored**（新增 19 本项 + 复活 11 `controller_extra`） |
| `cargo test -p physics` | **81 passed / 0 failed** |
| `cargo test --workspace` | **exit 0，3525 passed / 0 failed / 104 ignored**（109 个绿目标） |
| `cargo clippy -p server-runtime -p physics --all-targets` | 0 finding 落在本批文件（`server_app.rs` 4 条与 `asset-pipeline`/`blowfish` manifest 为工作区既有） |
| `rustfmt --edition 2024 --check`（本批 2 文件） | 0 diff（幂等） |
| `cellapp_extension.rs` 行覆盖 | **99.72%**（706 行 / 2 行防御臂，≥98%） |
| `controller_extra.rs` 行覆盖 | **100.00%**（365 行） |
| `world_triangle.rs` 行覆盖 | **99.80%**（512 行 / 1 行 bw10 既有 Möller–Trumbore 臂） |

新增验收测试（19 条，全部锚定原版行号）：

1. `eg_extra_greets_and_attaches_once` — 问候原文串 + extra 单次创建/幂等/清空；
2. `add_eg_controller_starts_and_ticks` — 注册即 `Started`、`userArg` 落地、更新注册、
   三次 tick 计数 3；
3. `del_eg_controller_deregisters_from_the_update_loop` — `MF_VERIFY` 注销、计数冻结；
4. `track_speed_refuses_on_a_ghost` — ghost 实体拒绝对照原文 TypeError；
5. `track_speed_enable_disable_round_trip` — 启停闭环（建 extra → 接管 → 停 → 毁 extra）；
6. `track_speed_is_idempotent_at_both_ends` — 两端幂等不产生第二个控制器；
7. `on_movement_measures_velocity_between_stamps` — 首次只记戳、次测
   velocity=(4,4,0)、speed=|v|/0.5≈11.3137；
8. `speed_ghost_state_round_trips_through_the_stream` — 24 字节流 round-trip、
   对端时钟偏移还原、短流拒绝、零偏移不移动 `lastTime`；
9. `bounce_grenade_without_a_space_is_refused` — 原文 TypeError；
10. `bounce_grenade_comes_to_rest_on_a_floor` — 弹跳衰减至静息、落点近地；
11. `bounce_grenade_stops_at_max_samples` — 恰 3 循环 ×0.1s，仍在空中；
12. `bounce_grenade_holds_at_the_bounce_cap` — `maxBounces=1` 早停、落点近地；
13. `bounce_grenade_clamps_a_hit_inside_the_radius` — 命中距离小于半径 → 比例截 0；
14. `visit_controllers_reports_name_and_exclusive_id` — (typeName, exclusiveID) 逐条、
    提前终止、卸载对未类型化控制器无副作用；
15. `defaults_mirror_the_constructors` — `Default`==`new()` + 机件公开面（`id()`/`is_empty()`）；
16. `an_untracked_entity_ignores_movements` — 无 extra 不回调；
17. `a_second_speed_controller_stop_finds_the_extra_gone` — 原始重复注册下停止顺序；
18. `extra_slots_are_stable_and_independent` — 四个 extra 槽稳定且互不干扰；
19. `unload_unwinds_every_registration_and_is_safe_twice` — 注册全解、二次卸载安全。

`physics` 侧 1 条：`bounce_reflects_the_velocity_about_the_plane` — 弹性 1 完全反射、
0.3 保留 30%、切向分量不变。

## 顺带修复（同类合并静默丢失）

`crates/server-runtime/src/lib.rs` 在主席合并 `e16e751` 时被自动解到 main 一侧，
丢掉了 bw19-docs 新增的 8 条 `pub mod`，使得那次并入的 `.rs` 文件成了
**孤儿**（存在磁盘、从未编译、自带测试从未运行）：共 22 个模块
（`cell`、`controller_extra`、`destroy_real`、`controller_stream`、`offload*`、
`ghost_data`、`witness_cache` …）。本项只复活移植所需的 `controller_extra`
（仅依赖 `std::collections`，自足），其余 21 个留给主席处理——这是与
`crates/mercury` 18 个孤儿模块同类的第三次静默丢失实例，已登记在本席的
合并回归记忆档。另注：`controller_stream` 依赖 `movement::Vec3`，而 main 的
`movement.rs` 并无该定义（bw19 侧有），属「文件并入了但同级文件的形状已变」的
情形，不可一并声明。

## 状态

已验收：注册、回调及卸载可验——五类样例面（`EgExtra`/`EgController`、
`SpeedExtra`/`SpeedController`、`BounceGrenadeExtra`、`DumpControllersExtra`）
在移植后的 Controller/Extra 机件上端到端落地，注册得事件与更新注册、回调按原版
逐字语义（速度数学、ghost 流偏移、弹球循环、逐控制器回调）驱动，卸载把全部
注册解干净且二次卸载安全。
