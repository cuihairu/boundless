# OPT-03-01-02 优化 Python/native 转换

对应 todo.md OPT-03-01-02：**优化 Python/native 转换。验收：以前后对照验证耗时变化。**

交付面：
- `crates/python-host/tests/bridge_baseline.rs`（`b3728d6`）：基线 target 新增 **math 面**三条——`matrix_set_translation`（`PyObject_SetAttrString`，赋值走 `parse_vector_like`→`check_type` 的 TYPES 查表）、`matrix_get_translation`（getter 走 `vector_object`→`type_address` 的 TYPES 查表+`tp_alloc`）、`vector3_getitem`（`PySequence_GetItem`，下标走 `dim_of` 的 DIMS 查表）。值用**活的 Vector3 实例**而非元组——这是引擎最热的形态（脚本侧 `m.translation = v3`），`check_type` 命中快路径；结构断言随行（往返精确值、getter 必须回新副本、下标读回 1.0）。
- `crates/python-host/src/math/mod.rs` + `vector.rs`（`eac5ee7`）：`TYPES`/`DIMS` 两张注册表从 `OnceLock<Mutex<BTreeMap>>` 换成 **`AtomicPtr` 发布的写时复制快照**——读路径（`check_type`/`type_address`/`dim_of`，每条转换的最内层）只剩一次 Acquire load + BTreeMap 查找，全局互斥锁从热路径消失。
- 记录 TSV：`docs/review/fixtures/python-baseline/bridge-baseline.tsv` 已刷成优化后一轮（69 行，`math 3` 面）。

## 优化设计

- **读**：`type_snapshot()`/`dim_of` 对 `AtomicPtr` 做 `Acquire` load，快照一经发布不可变；返回 `&'static` 引用（快照故意泄漏，见下）。
- **写**：只在模块安装期发生（`install`→`remember_type`/`remember_dim`，单线程、任何脚本运行之前）。写方 load 旧快照→clone→插入→`Box::into_raw`→`Release` store。被换下的旧快照**故意不回收**——并发读者可能仍持有引用；每进程只写几笔，泄漏量是几张小表（与 `binding.rs` 泄漏描述符数组同一取舍）。
- **fini**：`math::reset` 把 `TYPES` 指针摆回 null、`vector::reset` 同样摆回 `DIMS`——读者看到空表，被换下快照同上不回收，下次 init 从新表起。
- `remember_type` 签名收窄为 `&'static str`：两个调用点本就传静态串（matrix.rs 的 `MATRIX_NAME`/`PLANE_NAME` 常量、`vector_name(dim)`），快照存字符串地址，免一次分配。

## 前后对照（验收证据，2026-11-26 本机）

**方法**：本机当时被其它席重载（load 28.6/14 核），两次独立测量窗整体漂移 ~300–400ns，单跑对照不诚实。故 A/B **交错**跑：两个变体各构建为独立二进制（pre-opt=`b3728d6` 树、post-opt=`eac5ee7` 树，`git checkout HEAD~1 -- crates/python-host/src/math/` 换腿），`taskset -c 5` 钉核，逐轮交替 15 轮，取**跨轮中位数**对账（debug build，ns）。

| 面 | 前 p50 | 后 p50 | Δ | 后/前 | 前 p99 | 后 p99 |
|---|---|---|---|---|---|---|
| `matrix_set_translation` | 1002 | 910 | **−92ns** | 0.908 | 1537 | 1360 (0.885) |
| `matrix_get_translation` | 1159 | 1057 | **−102ns** | 0.912 | 1635 | 1378 (0.843) |
| `vector3_getitem` | 346 | 254 | **−92ns** | 0.734 | 468 | 329 (0.703) |

结论：
1. 六项指标（三面 × p50/p99）**全部单调向好**，方向一致无反例；互斥锁摘除的收益量级 ~90ns/查（debug），与 uncontended mutex lock/unlock 的 debug 成本吻合。
2. 相对收益分层正确：`vector3_getitem`（−27%）最大——`dim_of` 是它唯一的 port 侧加价；两个 matrix 面（−9%）混着 `tp_alloc`/属性机等大头，注册表只占其中一角。
3. 标量转换面（int/float 17–34ns）不经过任何注册表，本优化不触碰也不应触碰——OPT-03-01-01 结论「标量面已贴 CPython 下限」保持成立。
4. 刷后的 fixture TSV 携带优化后的绝对数字（本轮 `matrix_set_translation` p50 895 / `matrix_get_translation` 1062 / `vector3_getitem` 248），与 A/B 后侧同量级。

## 复跑命令（canonical）

```sh
PYTHON_BRIDGE_OUT=<绝对路径> cargo test -p python-host \
    --test bridge_baseline the_bridge_baseline_records_all_faces -- --exact
```

TSV 追加落盘，重跑前先清文件。

## 门禁（全绿后落账）

- `cargo test -p python-host`：lib 151 绿（math 模块全部单测含 registry/reset 面）、`bridge_baseline` 1 绿。
- clippy `-D warnings`：净（workspace blowfish 未用警告是继承债）。
- rustfmt `--edition 2024 --check`：净。

## 坑集（复现与规避）

- **被重载的机器上单跑 A/B 是假对照**：load 28/14 时同一二进制两窗漂移 ~400ns，比被测效应大 4 倍。交错多轮+跨轮中位数+钉核（`taskset -c`）是对账形态；方向一致的单调性比绝对数字更可信。
- **`translation` 属性在 Matrix 不在 Vector3**：参考引擎 `Vector3` 只有 `isReference` 属性，访问走下标；`m.translation = v3` 才是最热赋值面。值须用活 Vector3 实例——传元组会先 `check_type` 失败再落元组解析，量的是冷路径。
- **getter 回的是新 Vector3 不是元组**：断言用 `PySequence_Size`/`PySequence_GetItem`，用 `PyTuple_Size` 拿 −1 且报错滞留到关机才喷（"bad argument to internal function"）。
- **A/B 二进制换腿法**：`git checkout HEAD~1 -- <scope>` 回退腿→构建→`cp` 出二进制→`git checkout HEAD -- <scope>` 复原；范围必须钉到文件（另一席的脏文件不能碰）。
- **`AtomicPtr` 在 `std::sync::atomic` 不在 `std::ptr`**；`PySequence_GetItem` 接 `PySsize` 下标不走对象索引。
