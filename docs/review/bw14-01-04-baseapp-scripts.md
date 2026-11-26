# BW-14-01-04 运行 Guard 和 Space 脚本 — 交付证据

**验收**：行为与基准相符。交付面 = `crates/server-runtime/src/baseapp_scripts.rs`（967 行）
+ 测试 `src/baseapp_scripts/tests.rs`（8 个 scenario 进程）。

## 交付范围

Guard/Space 等 `BigWorld.Base` 脚本的运行宿主面：

- **实体类型注册**（`entity_type.cpp:81-136`）：模块导入 → 同名类取回 → 无模块时
  Proxy 占位 → 派生校验。`newEntityBase`（`entity_type.cpp:566-590`）在脚本类上
  `PyType_GenericAlloc`、引擎态置于分配尾、随后以 `okIfNull` 调 `__init__`；`__init__`
  内自毁（Boom.py）拒绝创建（`base.cpp` `isDestroyed()` 后检）。
- **Base 方法面**（`base.hpp:203-254` 声明族）：`createInNewSpace`/`createCellEntity`/
  `destroy`/`destroyCellEntity`/`registerGlobally`/`registerException`/`cellData` 读写/
  `id`/`isDestroyed`/`baseType`/`className`/`cell`/`hasCell`/`databaseID`。`checkAssociatedCellEntity`
  四 ValueError 臂逐字；`prepareForCellCreate` 走 `EntityCreator` 模型。
- **globalBases** 腿：register 请求记录、ack 回执（成功/重复/异常三臂）、回调异常
  `ScriptErrorPrint` 吞错并落盘、订阅号 arm、`globals_subscript` 未知键 KeyError。
- **cell-side 构造腿**：`construct_cell_entity` 在脚本类上分配实例并调 `__init__`；
  无引擎 id 的实例上每个 Base 面一律拒绝（NULL 返回 → SystemError；cellapp 行
  持有该面）。

## 验收行为

场景 `the_scenarios_run` 按进程跑 8 个 scenario（CPython 每进程只允许一次解释器初始化）：

| scenario | 覆盖 |
|---|---|
| `scenario_space_boot_then_guard_attaches` | Guard `__init__` 全链（cellData 播种 + createCellEntity + DefaultSpace mailbox） |
| `scenario_register_failure_drives_destroy_cell_entity` | onRegistered(False) → destroyCellEntity |
| `scenario_the_error_arms` | destroy 拒绝参数/destroyed 臂、create 双 pending、registerGlobally 参数、unprintable KeyError、BadStr 渲染 |
| `scenario_the_import_arms` | 无模块 Proxy 占位、缺类、派生错、Ghost classOf 身份 |
| `scenario_cell_data_and_mailbox_faces` | cellData 播种全 PropValue 族、mailbox 视图 |
| `scenario_the_construction_and_exception_faces` | DestroyedInInit、cell-side 构造、id-less 面拒绝、池耗尽、InvalidType、Wail 回调异常、ip-0 contact、destroyCellEntity 重复静默 |
| `scenario_the_space_id_and_state_arms` | space-id create 成功+请求记录、无 spaceID 拒绝、NoCellData 面、arity、import NoScriptFace、Unscripted Proxy 占位 |
| `scenario_the_unknown_and_missing_faces` | 缺 cellData dict 双路拒绝（读+create）、ip-0 mailbox、未知 register ack、begin 失败臂 |

## 门禁

- `cargo test -p server-runtime`：1680 过 / 0 挂（9 忽略 = 8 scenario + 其他既有）。
- `cargo fmt --all -- --check`：幂等。
- `cargo clippy -p server-runtime --all-targets -- -D warnings`：本席文件零提及。
- `cargo llvm-cov -p server-runtime --lib`：baseapp_scripts.rs **913/967 = 94.42%**。

## 覆盖注记（登记死臂）

94.42% 低于常规 ≥98% 纪律，差额为结构性不可达防御臂，已在模块头登记：

1. C-API 失败分配/意外 NULL 守卫（~25 行）：解释器进程生命周期内这些对象恒存在。
2. `UnknownBase` 兜底（2 行）：销毁总是登记 id，destroyed 臂胜出。
3. ip-zero mailbox 臂（5 行）：`.cell` 在构造 mailbox 前先拒绝 ip-0 地址。
4. `install_proxy_face` 的 first-spec 分支（1 行）：harness 先装全规格 Base。
5. `swallow_error_text` 的 "unknown" 兜底（1 行）：`PyErr_Fetch` 取不到 value 不可复现。
6. 其余为紧邻不可达守卫的隐式行。

脚本可达的每个分支均在 scenario 中被驱动。
