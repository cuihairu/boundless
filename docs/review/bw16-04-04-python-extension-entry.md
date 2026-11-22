# BW-16-04-04 定义 Python 扩展入口 — 入口、导入失败定位与状态回滚

- **任务编号**: BW-16-04-04（lane BW-16 客户端与发布物，todo.md 行 1413）
- **验收**: 导入失败可定位且状态可清理。
- **结论**: 满足并勾行。Python 扩展入口（`Interpreter::init` 的 init 序列及其末段的 `import bw_site`）已随 7d031b1 合入并落地：导入失败**可定位**（专用 `InitError::BwSiteImportFailed` 变体，对齐参考 `script.cpp:617` 的 `"Script::init: Unable to import bw_site"`）且**状态可清理**（失败即回滚，`is_initialized()` 为 `false`）。两臂各有 `#[ignore]` 场景实测通过。

## 1. 入口与定位

`Interpreter::init`（`crates/python-host/src/interpreter.rs:105-190`）复刻参考 `Script::init` 的完整序列：解释器/子解释器建立 → `sys.path` 装配 → `bigworld` 模块与组件 → stdout/stderr 钩子 → excepthook → 内建脚本装载 → **末段** `import bw_site`（行 176）。

末段失败走 `InitError::BwSiteImportFailed`（行 70），语义即参考 `script.cpp:617` 处 `ScriptModule::import("bw_site", ...)` 返回 false、`Script::init` 记日志并返回 false 的路径。`import_bw_site()`（行 392-395）经 `capi::PyImport_ImportModule(c"bw_site")` 导入，与参考的模块导入同源。

## 2. 状态可清理（回滚）

失败即在 `init` 尾段（行 177-182）回滚：`INITIALIZED` 复位为 `false` 并 `Py_Finalize`。因此调用方观测到的是「入口失败且从未置起」，而非半初始化状态。

## 3. 实测（两臂，均绿）

每个 `#[ignore]` 场景独立进程运行（CPython 3.13+ 拒绝在已 `Py_Finalize` 的进程上二次初始化，参考侧同样不重复初始化，`script.cpp` `Script::createInterpreter`/`Script::fini` 语义）；父测试 `interpreter::tests::every_scenario_exits_cleanly` 逐场景 out-of-process 驱动。

| 场景 | 断言 | 结果 |
|---|---|---|
| `interpreter::tests::scenario_missing_bw_site_fails_init` | `Interpreter::init(&InitConfig::default())` 在**无** res 树时返回 `Err(InitError::BwSiteImportFailed)`，且 `!Interpreter::is_initialized()` | ok |
| `interpreter::tests::scenario_bw_site_is_imported_at_the_tail_of_init` | res 树携带 `scripts/common/bw_site.py` 时，init 末段导入之，`sys.bw_site_imported` 为 `True` | ok |

```
$ cargo test -p python-host --lib -- --exact interpreter::tests::every_scenario_exits_cleanly
test interpreter::tests::every_scenario_exits_cleanly ... ok
test result: ok. 1 passed; 0 failed

$ cargo test -p python-host --lib -- --exact --ignored interpreter::tests::scenario_missing_bw_site_fails_init
test interpreter::tests::scenario_missing_bw_site_fails_init ... ok
test result: ok. 1 passed; 0 failed

$ cargo test -p python-host --lib -- --exact --ignored interpreter::tests::scenario_bw_site_is_imported_at_the_tail_of_init
test interpreter::tests::scenario_bw_site_is_imported_at_the_tail_of_init ... ok
test result: ok. 1 passed; 0 failed
```

## 4. 边界

本项只覆盖**入口**及其失败定位与清理，即验收所述范围。`python/` 模块目录的**内容**（各扩展模块的逐类/逐函数迁移）属 BW-03-03/04（boundless3 lane），不在本项验收内；入口已就绪，随其交付即可挂接。