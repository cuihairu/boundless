# BW-13-06-03 核对 plugin_system 并复现加载行为

**验收** 加载成功与失败行为可复现。本档把原版 `tools/plugin_system/`（BigWorld 的
`PluginLoader` 动态插件宿主）核对到新版资源管线的插件宿主面，并以一个新建的
`crates/plugin-host` crate 复现其加载成功与失败行为（14 个验收用例，100% 行覆盖）。

**参考锚点**：`/home/cui/workspaces/BigWorld`，HEAD `93bb0837`
（`bigworld-14.4.1-359-g93bb0837`）。

## 1. 参考侧：`tools/plugin_system/`（Windows-only 插件宿主）

三个文件，宿主契约如下：

| 文件 | 内容 |
|---|---|
| `plugin.hpp` | `PLUGIN_INIT`/`PLUGIN_FINI` 宏——`_DEBUG` 下导出名变 `PluginInit_d`/`PluginFini_d`，否则 `PluginInit`/`PluginFini`；`PLUGIN_INIT_FUNC`/`PLUGIN_FINI_FUNC` = `extern "C" __declspec(dllexport) bool NAME( PluginLoader & )` |
| `plugin_loader.hpp` | `class PluginLoader`：`initPlugins()`/`finiPlugins()`/`loadPlugin(name)`/`unloadPlugin(HMODULE)`/`pluginNames()`；`typedef bool (*PluginInitFunc)(PluginLoader&)`；成员 `PluginList plugins_` + `PluginNameList pluginNames_`（平行双表） |
| `plugin_loader.cpp` | 行为实现（见 §2） |

## 2. 参考行为（`plugin_loader.cpp`，逐条复现）

- **`loadPlugin(name)`**：`LoadLibraryW(<exeDir>/<name>.dll)`；null → 失败。
  成功则 `GetProcAddress(h, PLUGIN_INIT)` 解析 init；init 存在且返回 **true** →
  `plugins_.push_back(h)` + `pluginNames_.push_back(文件名)` + 返回 h。否则（init
  不存在或返回 false）→ 若有 `PLUGIN_FINI` 则调用它、`FreeLibrary(h)`、返回 NULL。
- **`unloadPlugin(h)`**：`GetModuleFileName` 记日志；在 `plugins_` 找 h；调
  `PLUGIN_FINI`，返回 **false** → 返回 false（插件拒绝卸载，保持加载）；否则
  `FreeLibrary` + 从 `plugins_`/`pluginNames_` 删除 + 返回 true；h 不在表 → 返回 true。
- **`finiPlugins()`**：逆序 `unloadPlugin(plugins_[i-1])`（忽略每个结果）。
- **`initPlugins()`**：读 `<exeDir>/<exeBasename>_plugins.txt`（空白分隔名）；
  文件打不开 → `ERROR_MSG` 后返回（不加载）；否则逐个 `loadPlugin`。
- 消费方：8 个 asset_pipeline 转换器的 `plugin_main.cpp`
  （`texformat/texture/bsp/space/visual/primitive/hierarchical_config/effect_converter`）
  + `jit_compiler` + `batch_compiler`；参考自带复现用例
  `tools/asset_pipeline/compiler/unit_test/test_compiler.cpp`（加载
  `testconverter(_d)` 并 CHECK 加载/卸载循环）。

## 3. 新版核对：`crates/plugin-host`

新建 crate（workspace 成员，`crates/plugin*` 通配命中）把宿主契约移植到 Unix
`dlopen`/`dlsym` 原语（复用 `platform::mmap::DynamicLibrary`，BW-15-01-05）：

| 参考 | 新版 | 说明 |
|---|---|---|
| `PluginLoader` | `plugin_host::PluginLoader` | 同构：`init_plugins`/`fini_plugins`/`load_plugin`/`unload_plugin`/`plugin_names` |
| `PluginInitFunc`/`PluginFiniFunc` | `PluginInitFn`/`PluginFiniFn` = `unsafe extern "C" fn(*mut c_void) -> bool` | `host` 形参 = 参考的 `PluginLoader&`（不透明上下文，仅调用期有效） |
| `PLUGIN_INIT`/`PLUGIN_FINI`（release） | `INIT_SYMBOL`/`FINI_SYMBOL` = `"plugin_init"`/`"plugin_fini"` | `PluginLoader::new()` |
| `_DEBUG` → `PluginInit_d`/`PluginFini_d` | `DEBUG_INIT_SYMBOL`/`DEBUG_FINI_SYMBOL` = `"plugin_init_d"`/`"plugin_fini_d"` | `PluginLoader::with_debug_symbols()` |
| `loadPlugin` 失败返回 NULL | `Result<usize, LoadError>` | `LoadError::{Open, NoInit, InitVetoed}` 区分三种失败 |
| `.dll` 后缀 | `.so` 后缀 | 随 `dlopen`（Windows `LoadLibrary` 路径属 BW-15-03） |
| `plugins_`/`pluginNames_` 平行双表 | `plugins: Vec<LoadedPlugin>` + `names: Vec<String>` | 同构 |

**行为逐条复现**（`crates/plugin-host/tests/loader.rs`，14 用例，行覆盖 100%）：
- 加载成功：init 返回 true → 插件被保持并记录（`a_plugin_that_initialises_is_loaded_and_kept`）。
- 加载失败三种：库打不开 → `LoadError::Open`；无 init 导出 → `LoadError::NoInit`
  （并先调 fini）；init 返回 false → `LoadError::InitVetoed`（并先调 fini）
  ——`a_missing_library_is_an_open_failure`、`a_library_without_the_requested_init_is_rolled_back`、
  `a_plugin_whose_init_refuses_is_not_registered`。
- 卸载：正常释放；fini 返回 false → 拒绝卸载（插件保持）；fini 缺失 → 照常释放；
  未知索引 → no-op 成功——`unload_releases_a_loaded_plugin`、
  `a_plugin_that_refuses_to_unload_stays_loaded`、`a_plugin_without_a_fini_entry_point_unloads_anyway`、
  `unloading_an_unknown_index_is_a_no_op_success`。
- `fini_plugins` 逆序卸载：`finishing_unloads_in_reverse_load_order`；
  拒绝卸载者保持：`finishing_keeps_a_plugin_that_vetoes_unloading`。
- `init_plugins` 读插件清单文件（`<name>.so` 按目录解析）并报告逐名成功/失败：
  `the_plugin_list_loads_each_entry_and_reports_failures`；清单缺失 → 报错：
  `a_missing_plugin_list_is_an_error`。
- 调试符号对（`_d`）选择：`the_debug_symbol_pair_names_the_d_entry_points`；
  错误变体自述：`error_variants_name_their_failure_mode`。

**夹具**：`crates/plugin-host/fixtures/*.rs`（`plugin_fixture_ok`/`_alt`/`_fail`）由
`build.rs` 用 `RUSTC --crate-type cdylib` 编成真实 `.so` 供宿主打开——加载行为
对**真实可加载产物**复现，不靠桩。`ci/sdk.sh` 的 `plugin-host` 检测器随本 crate
落位由 SKIP 翻转为 PASS（`plugin host crate(s) present: plugin-host`）。

## 4. 归属结论

| 原版 | 归属 | 新版目标 |
|---|---|---|
| `tools/plugin_system/`（`PluginLoader` 宿主） | **资源管线插件宿主**（asset_pipeline 编译器按 `_plugins.txt` 加载转换器 DLL） | `crates/plugin-host`（已建，Unix `dlopen`）；asset_pipeline 编译器宿主接线（转换器注册）随 BW-13-02 立项；Windows `LoadLibrary` 后端随 BW-15-03 |

新版资源管线（`crates/asset-pipeline`）现有 `Converter` 插件接口
（`converter.rs`，移植自 `conversion/converter.hpp`）但无动态加载宿主；本 crate
补齐**宿主侧**，转换器注册协议随 BW-13-02 落位。
