# BW-14-01-04 — 运行 Guard 和 Space 脚本（script host 接线）

验收：**行为与基准相符**。参照 `BigWorld/programming/bigworld/server/baseapp/base.cpp`
与 `pyscript/` 的 `PY_METHOD` 装配，交付
`crates/server-runtime/src/script_host.rs`：把 base/cell 两个角色的解释器面
接到引擎簿记上，Guard/Space 脚本在引擎驱动的生命周期里运行。

## 交付面

- **纯簿记半**（无解释器即可测）：`BaseFlags`、`check_associated_cell_entity`
  + `associated_cell_entity_error`（`base.cpp:3717-3760` 五梯级逐级原文）、
  `prepare_for_cell_create`（`base.cpp:3269-3300`）、`cell_creation_result`
  （`base.cpp:3757-3790`）、`set_current_cell`（`base.cpp:1028-1044`）、
  `check_destroy_allowed` + `destroy_steps`（`base.cpp:1488-1560`）。
- **宿主半**：`ScriptHost`（flags/scripts/ptr_index/outbound/pending_registrations/
  global_bases/type_names/script_signals）与六个引擎半：
  `create_in_new_space`、`register_globally`、`on_register_reply`、
  `on_cell_creation_result`、`set_current_cell`、`destroy_cell_entity`、
  `destroy_base`。
- **驱动半（thread-local + 自由函数）**：`host_set_current_cell`、
  `host_on_register_reply`、`host_destroy_base` — 每次脚本派发都在
  `RefCell` 借用释放后进行（见下「重入契约」）。
- **解释器半**：`install_base_methods` 把四个方法（`createInNewSpace`/
  `registerGlobally`/`destroyCellEntity`/`destroy`）盖上 `BigWorld.Base`；
  `construct_base`（tp_init 槽调用 + `PyErr_Print` 尾）、`construct_cell`
  （按名找 `__init__`，失败 `PyErr_Print` 尾）、`import_entity_class`
  （引用原文三种拒绝文本）。

## 参照锚点

| 面 | 锚 |
|---|---|
| `py_destroyCellEntity` 语义 | `base.cpp:4037-4070` |
| get-pending 整梯旁路 | `base.cpp:4038-4041`：`!isGetCellPending() && !checkAssociatedCellEntity(...)` |
| `prepareForCellCreate` 双旗标 | `base.cpp:3292-3293`（isCreateCellPending_ + isGetCellPending_ 同置）|
| `py_createInNewSpace` `|O` 元数 | `base.cpp:3561` `PyArg_ParseTupleAndKeywords(args, kwargs, "\|O:...")` |
| `destroy` 守卫与步骤 | `base.cpp:3975-3995` + `base.cpp:1488-1560` |
| 注册回复（global_bases） | `global_bases.cpp:228-252`（base 已毁也照派回调）|
| `inDestroy_` no-op | `base.cpp:1490` |

## 关键裁定

1. **重复注册路径照走 `destroyCellEntity`**：`prepareForCellCreate` 同时置两个
   pending 旗标，故 get-pending 时 `py_destroyCellEntity` 的整个梯子被旁路——
   not raise，而是正常落闸（`inDestroy_` latch）+ 发 DestroyCellEntity 出站。
   幂等二调是 `isDestroyCellPending_` → silent `None`。
2. **重入契约**：每个脚本派发（onGetCell/onLoseCell、onDestroy、
   MakeGlobalHandler 回调）都必须在宿主 `RefCell` 借用释放后运行——fixture 的
   `onRegistered(False)` 调 `destroyCellEntity()`、`onLoseCell` 调 `destroy()`，
   借用下派发会死锁。形态：簿记 `with_host` 内做，Python 调用借外做。
3. **`PyInstanceMethod` 绑定形**：盖上实例方法后，C 调的 `args[0]` 是实例，
   脚本参数从 `args[1]` 起；`|O` 元数是对剩下的参数说的。实例经 `ptr_index`
   回解 base id。
4. **`PyCFunction_New` 存的是 `PyMethodDef` 指针**（不拷贝）：defs 表必须活得
   比函数对象久。port 用 `Box::leak`——正是参照 `static PY_METHOD[]` 表的契约。
   本地栈表首跑即 SIGSEGV（调时读悬挂 ml_meth）。
5. **`Py_tp_init = 60`**（`/usr/include/python3.14/typeslots.h` 实证）：
   槽号属稳定 ABI，**不跟 `Py_tp_new`=65 相邻**（66 是 `Py_tp_repr`）。
   猜邻接值首跑即静默垃圾（slot 指针落堆区而非 libpython 文本段；调返回
   -432590544）。
6. **派生检查文本**：`tp_name` 已是全限定名（`BigWorld.Base`），原文不再前缀
   `BigWorld.`——首测抓到 port 输出 `BigWorld.BigWorld.Base` 的真 bug。
7. **元数文本**：经典 `PyArg_ParseTupleAndKeywords` 的名词跟随格式允许的最大值
   （此处 1 → 恒单数），不是跟随实际给数——`takes at most 1 argument (2 given)`。
8. **bare 类不拒绝**：无自定义 `__init__` 的类继承 `object.__init__`，属性查找
   成功、调用成功——「no `__init__`」的 null 臂是防御性不可达。

## 与参照的差异（deltas）

- **重入契约是本 port 的设计**：参照 C++ 单线程手递引用计数无此问题；Rust 侧
  宿主是 `RefCell`，因此簿记/派发两段式。
- **cell `__init__` 失败 `PyErr_Print` 后继续**：与 base 构造同一尾（参照
  baseapp 构造路径对脚本错误打 console 后带病前进）。
- 解释器半的三个 C 调（`PyCFunction_New`/`PyInstanceMethod_New`/
  `PyObject_SetAttrString`）不带检查地跑：静态表、活函数、可写堆类型，与参照
  `PY_METHOD` 表的无检查一致。

## 测试与覆盖

- 9 个进程内测试：六个纯函数组 + 三个宿主/驱动守卫组（无解释器）。
- 15 个解释器场景，各起独立进程（解释器一进程一次 init 的老规矩）：
  bootstrap 出站序、重复注册→get-pending 旁路→DestroyCellEntity、毁后回回调、
  全生命周期、无 cell 响毁、cell 存在拒毁、不可调回调、Guard cell 角色构造、
  四方法元数原文、未注册实例四拒绝、注册/导入拒绝族、Base 缺席拒盖、
  onDestroy 内 destroy 的 no-op、失败注册回复带 False、cell `__init__`
  失败打印后继续。
- **llvm-cov 0.9.1：`script_host.rs` 1240/1260 = 98.41% 行覆盖**（child 场景
  进程继承 `LLVM_PROFILE_FILE` 的 `%p` 模式，profraw 合并后子进程覆盖计入）。
  余 20 零行：防御性不可达臂（allocate 失败、slot 缺失、null-init 查找）、
  大括号 artifact、测试自身的 panic 臂。

## TDD 偏差声明

本次交付**先实现后补红**：模块骨架与场景先行，覆盖率驱动的守卫测试组
（U1/U2 + S9-S14）后补。过程中补测仍抓到一个真 bug（第 6 条的
`BigWorld.BigWorld.Base` 重复限定），但"红-绿"次序未守住，特此声明。
