# BW-16-03-02 原版导入名称与路径 — 脚本类基类型证据

- **任务**: BW-16-03-02（todo.md「实现原版导入名称和路径。验收 Demo 无需改导入」）
- **卡点勘误回溯**: 2026-11-25 勘误定位真卡点 = `BigWorld.Entity/Base/Proxy`
  脚本绑定类型未落地（python-host BIG_WORLD_ITEMS 零行，原版 demo 实体脚本
  `class X(BigWorld.Proxy)` import 即断）；解锁条件 = BW-05..08 脚本类型面
  （最后一项 BW-08-06-02 已于 2026-11-26 00:36 勾账）。

## 1. 交付面

`crates/python-host/src/script_types.rs`（新模块）：三个 `Py_Header` 类基的
移植，按原版四个注册点的 app 集合装进 `BigWorld` 模块：

| 类型 | 参照声明 | 注册点 | 本 port 角色 |
|---|---|---|---|
| `Entity` | client `Py_Header(PyEntity, PyObjectPlus)`（client/py_entity.hpp:34）；cell `Py_Header(Entity, PyObjectPlus)`（server/cellapp/entity.hpp:143） | client/script_bigworld.cpp:986-988；server/cellapp/cellapp.cpp:3888-3893；server/tools/bots/entity_type.cpp:110-111 | client、cell、bot |
| `Base` | `Py_Header(BW_NAMESPACE Base, PyObjectPlus)`（server/baseapp/base.hpp:60） | server/baseapp/script_bigworld.cpp:2140-2141 | base |
| `Proxy` | `Py_Header(Proxy, Base)`（server/baseapp/proxy.hpp:76） | server/baseapp/script_bigworld.cpp:2142-2143 | base（站在 `Base` 上） |

- database app 不注册任何类（server/dbapp/dbapp.cpp:326 只 import）——
  审计的缺席契约对 database/editor/mystery 角色钉死三类的缺席。
- `Py_TPFLAGS_BASETYPE`（pyobject_base.hpp:59，宏体全带）经
  `TypeSpec::subclassable()` 进入 spec flags（binding.rs 新增 `flags` 字段与
  `extends()` 基类型builder；capi.rs 新增 `Py_tp_base`=48 与
  `Py_TPFLAGS_BASETYPE`；`PyType_FromSpec` slot 表按需追加 `Py_tp_base`，
  无基不下空槽——CPython 对该槽不判 NULL）。
- `Proxy` 的 `tp_base` = `Base` 类型对象（参照 `pyobject_base.hpp:67`
  `tp_base = &Super::s_type_`）。
- 构造保持拒绝（`_pyNew`，pyobject_plus.cpp:148-152 的原话报错）。**参照
  同形**：脚本派生类继承拒绝——引擎建实例走 `PyType_GenericAlloc`
  直配脚本类 + `callMethod("__init__")`
  （server/baseapp/entity_type.cpp:569、server/baseapp/base.cpp:794），
  不过 `tp_new`；该分配面归 BW-14-01-04（entitydef→python-host 依赖不在此
  行加）。

## 2. manifest 单源

`manifest.rs`：`Scope` 新增 `EntityApps`/`BaseApp` 两个变体——粗粒度
`Server`/`Client` 无法表达「cell 有 Entity 而 baseapp 没有」的注册面而不
过度声明（会打碎 BW-03-04-01 的 `dir(BigWorld)` 角色场景）。BIG_WORLD_ITEMS
新增三行 `Kind::Class`（Entity=entity-apps、Base/Proxy=baseapp，全 Ported 带
引用）。`carries_role` 按上述表落门（转 pub 供 script_types 复用），audit
的 gated 集扩两变体，`ported_surface_for_role` 自动带出（单源）。

## 3. 运行证据（全绿）

- python-host 154 测 0 败（新增 8 场景 per 进程 + 2 单元）：
  - **验收主证**：三方角色（cell/base/client）12 份生成夹具脚本
    （docs/review/fixtures/fantasydemo_def/res/scripts/{cell,base,client}）
    逐份 `import` 不改一字，各自 class 语句对注册基 issubclass 且
    `__bases__[0] is` 同一类型对象，`__name__` 相符。
  - `Proxy.__bases__ == (BigWorld.Base,)`、`Base.__bases__ == (object,)`、
    `Entity.__bases__ == (object,)`（PyObjectPlus 未移植=登记 delta）。
  - 直构造拒绝报原话 `X() Cannot directly construct objects of this type`；
    脚本类 `Account(BigWorld.Proxy)` 同拒绝（参照继承链同形）；
    `BigWorld.Proxy.__init__ is object.__init__`（夹具 idiom 的解析）。
  - database/mystery 角色三类全缺（审计缺席契约活面双证：manifest::check()
    + `dir(BigWorld)` 探针）。
  - 角色表面场景（BW-03-04-01 七角色）零改动自过——manifest 行落了，
    场景跟着对上：cell/bot 带 Entity，base 带 Base+Proxy，client 带 Entity，
    database/editor 无。
- 覆盖率（llvm-cov 0.9.1）：`script_types.rs` 行 **98.69%**（229 行 3 未盖
  =三处真防御臂：类型创建 panic、set_attr 失败清错、probe NUL panic），
  region 100%。crate 总体 95.13%（urlrequest/script_events/resmgr 长期
  防御臂未盖为既有状态）。
- fmt/clippy：python-host 全 target 新码零警（余一条 needless_borrow 为
  bw_profile.rs:997 继承债）。

## 4. 边界

本行交付导入面：类基可被脚本 `class` 语句继承、模块导入不因基缺失而断。
def 驱动的实体类型装载（EntityType 读 .def、引擎 `PyType_GenericAlloc`
建实例、`__init__` 注入）归 BW-14-01-04（entitydef→python-host 依赖在那行
加）；`Service`/`UserDataObject`（baseapp 同链注册）未移植，登记为后续
delta。
