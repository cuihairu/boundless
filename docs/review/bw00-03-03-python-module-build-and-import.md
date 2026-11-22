# BW-00-03-03 配置 Python 模块构建与导入 — 记录

- **任务编号**: BW-00-03-03（lane BW-00-03 Boundless 构建入口，todo.md 行 117）
- **Boundless 模块**: `crates/python-module`（`crate-type = ["rlib", "cdylib"]`，`[lib] name = "boundless"`）。
- **验收**: 安装后能导入最小模块。

## 结论（可导入）

构建产出 `libboundless.so`（`cargo build -p python-module`）；安装步把其改名/复制为
`boundless.so`（CPython 扩展加载器只认 `boundless.so`，`lib` 前缀是 Unix 共享库约定，不是
Python 模块发现规则——原版 lib-dynload 里的 `.so` 同样不带 `lib` 前缀）。把 `boundless.so`
所在目录放上 `PYTHONPATH` 后，真解释器 `import boundless` 成功：

```
$ python3 -c "import boundless; print(boundless.__version__, boundless.version())"
OK 0.1.0 0.1.0
```

模块表面刻意最小：`__version__`（字符串常量）+ `version()`（返回同一版本串），可验证导入
不止于符号解析。**本机 CPython 3.14.9** 实测通过；跨版本（.python-version 钉 3.12）依
stable-ABI 设计成立（见下），本机无 3.12/3.13 解释器可执行，未实测。

## 交付物

| 文件 | 说明 |
|---|---|
| `crates/python-module/Cargo.toml` | 包 `python-module`，lib 名 `boundless`，`crate-type=["rlib","cdylib"]` |
| `crates/python-module/src/lib.rs` | `PyInit_boundless` 导出 + `MODULE_DEF`/`METHODS` + `version()` |
| `crates/python-module/src/capi.rs` | stable-ABI C 面：`PyModuleDef`/`PyMethodDef`/`PyModuleDefBase`/`PyModuleDefSlot`/`PyObject` 布局 + `PyModule_Create2`/`PyModule_AddStringConstant`/`PyUnicode_FromString` + `METH_VARARGS`/`PYTHON_API_VERSION` |
| `crates/python-module/tests/import.rs` | 集成测试：安装（复制改名）后真解释器导入 + 安装改名钉死 |
| `Cargo.toml` | workspace `members` 增加 `crates/python-module` |

## 与参考侧的对应

- 参考「Python 模块」构建 = `bwpython313`（`libbwpython3.13.a`）+ 标准库 `lib-dynload`
  `.so`（`game/bin/server/el7/third_party/python/lib/python3.13/lib-dynload/*.so`）。
- 参考引擎模块（`BigWorld`/`Math`/`ResMgr`…）**不是**可导入 `.so`——经 init-time job 内
  `PyImport_AddModule(name)` 注册进嵌入解释器（见 BW-03-03-01 清单：
  `lib/pyscript/script.cpp:1409`；`PyImport_AppendInittab` 全树唯一调用点是 WorldEditor
  工具构建 `tools/worldeditor/scripting/world_editor_script.cpp:2165`）。
- 故本项与 BW-03 分开：`python-host` 是**嵌入宿主**（角色进程内注册引擎模块），本项是
  **构建入口的可导入模块制品**（部署后放上 Python 路径即可 `import`）。验收对真解释器
  断言，不对嵌入解释器断言。

## 设计要点与真实缺陷（TDD）

1. **`static` 只读段 → SIGSEGV**（真实缺陷，gdb 定位）：初版 `MODULE_DEF` 用 `static`
   （不可变），落在 `.data.rel.ro`（运行时只读）。CPython `PyModuleDef_Init`
   （`moduleobject.c:47`）向定义写 `ob_refcnt`/`ob_type`/`m_index` → 首次写页错误。
   `readelf -sW` 证实符号在 `.data.rel.ro`（只读），改 `static mut` 后落入 `.data`（可写）。
   这**不是**能靠拷贝绕过的：`PyModule_Create2` 不拷贝定义，`m->md_def = module`
   （`moduleobject.c:221`）保留指针供 `_PyModule_IsExtension` 后续读 `md_def->m_methods`
   ——栈/堆拷贝会在 `PyInit_boundless` 返回后悬垂。故 `static mut MODULE_DEF`（进程寿命、
   可写、CPython 就地初始化）。
2. **安装改名是验收面**：`cargo build` 产出 `libboundless.so`，CPython 扩展加载器只找
   `boundless.{abi3,tag}.so` 与 `boundless.so`。安装步 = 复制/改名去 `lib` 前缀（与原版
   lib-dynload 同规）。测试 `the_install_step_renames_libboundless_so` 钉死该步。
3. **stable-ABI 设计**：只用 limited API 符号与 ABI 稳定布局（`PyModuleDef`/`PyMethodDef`/
   `PyObject` 头、`PyModule_Create2`、`PYTHON_API_VERSION=1013`）。struct 布局按本机
   `/usr/include/python3.14/{moduleobject,methodobject,object}.h` 对齐，字段同序同宽
   （`PyModuleDef_Base`=40B，`PyModuleDef`=104B，与 C 一致）。`libboundless.so` 不声明
   `#[link(lib=python...)]`——符号由宿主进程已在内存的 libpython 在 dlopen 时解析，与
   参考扩展模块同法。
4. **edition 2024 细节**：`#[no_mangle]` 需 `#[unsafe(no_mangle)]`；`concat!` 只收字面量
   （版本串用 `concat!(env!("CARGO_PKG_VERSION"), "\0")`）；`static` 需 `Sync`
   （`*mut`/`*const` 字段非 `Sync`，`unsafe impl Sync` 三处，注释声明 CPython 只读这些
   描述、`PyModule_Create2` 写的是拷贝/就地初始化后的定义而非共享表）。

## 测试与门禁

- `cargo test -p python-module`：**3 passed / 0 failed**（unit 1：版本串非空且 NUL 结尾；
  integration 2：`the_minimal_module_imports_after_install`、`the_install_step_renames_libboundless_so`）。
- `cargo fmt --check` 通过；`cargo clippy -p python-module --all-targets` **0 警告**
  （其余为既有 workspace 根 manifest 的 `unused workspace dependency`
  `asset-pipeline`/`python-host`，非本项引入——本项未加 workspace 依赖消费，仅列 members）。
- workspace 级 `cargo test --workspace` 当前**红**：`server-runtime`/`platform`/`test-support`
  为兄弟会话在途未提交改动（`platform::signals` 缺 `SIGRTMIN`/`SIGRTMAX`，见 `git status`），
  非本项引入；本项 crate 零外部依赖，独立绿。提交按 pathspec（见下）。

## 状态

- [x] 构建（`cargo build -p python-module` 产出 `libboundless.so`）
- [x] 安装（复制改名 `boundless.so` 到安装目录）
- [x] 导入（真解释器 `import boundless`，`__version__`/`version()` 可读）
- 跨版本（3.12 pin）stable-ABI 依构造成立，本机无 3.12/3.13 可执行，未实测（如实登记）。
