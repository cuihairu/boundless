# BW-13-05-06 插件宿主 SDK 启用组合核对

**验收** 每个启用组合有样本或缺口。本档盘点参考树 `tools/plugin_system/` 的 SDK 面、
全部宿主与插件、逐组合给样本或缺口，并与新树 `crates/plugin-host`（BW-13-06-03，
`962fcba`）及资产管线交付（BW-13-01-03/04、BW-13-02-01）对账。

## 1. SDK 本体：`tools/plugin_system/`（三文件）

- `plugin.hpp`：`PLUGIN_INIT(fn)` / `PLUGIN_FINI(fn)` 宏；`_DEBUG` 下符号名变
  `PluginInit_d` / `PluginFini_d`（宿主按构建类型查对应符号——与 DLL 的
  `DEBUG_OUTPUT_NAME "<proj>_d"` 输出名约定成对，见 §3）。
- `plugin_loader.hpp/.cpp`：`PluginLoader::loadPlugin` 开 `<exeDir>/<name>.dll`
  （`::LoadLibrary`，plugin_loader.cpp:57，**全文件无 dlopen/POSIX 分支**）、解析
  init 符号、`init(host)` 返 true 才入表，否则调 `PLUGIN_FINI` + `FreeLibrary` +
  返 NULL；`unloadPlugin` 先 fini 再 FreeLibrary。
- **无独立 CMake 目标**：`tools/plugin_system/` 无 `CMakeLists.txt`，源文件被宿主
  目标直接吸收（jit_compiler/batch_compiler/test_compiler 的源清单一一列入）。

## 2. 宿主清点（全树引用 `plugin_system` 的仅 26 文件，全在资产管线工具家族）

| 宿主 | 目标类型 | 与 loader 关系 |
|---|---|---|
| `batch_compiler` | `BW_ADD_ASSETPIPELINE_EXE`（无 WIN32 → 控制台 exe） | `class BatchCompiler : public …, public PluginLoader`（batch_compiler.hpp:14，多继承自持 loader） |
| `jit_compiler` | `BW_ADD_ASSETPIPELINE_EXE jit_compiler WIN32`（GUI exe） | `main_window`/`config_dialog` 构造收 `const PluginLoader &`（main_window.cpp:23、config_dialog.cpp:12），按配置加载转换器 DLL |
| `TestCompiler`（资产管线单测宿主） | 测试专用 | `class TestCompiler : public AssetCompiler, public PluginLoader`（test_compiler.hpp 多继承）；单测加载 `testconverter_d` 失败回退 `testconverter`（unit_test/test_compiler.cpp:14-16）并 CHECK 加载/卸载循环 |
| `AssetCompiler`/assetprocessor（生产宿主） | 资产管线编译器 | 按转换规则名加载各转换器 DLL |

**编辑器宿主缺口**：`tools/worldeditor/`、`tools/modeleditor/` 的 CMakeLists 与源
**零引用** plugin_system（grep 全清）；`worldeditor_ui` 为空壳目录（BW-13-06-01 档
`929f572` 佐证）。参考编辑器不宿主任何插件，plugin_system 仅活于资产管线工具家族。

## 3. 插件侧：`BW_ADD_ASSETPIPELINE_DLL` 转换器（9 个）

`BWMacros.cmake:329-341`：`BW_ADD_LIBRARY(… SHARED)` + `DEBUG_OUTPUT_NAME
"<proj>_d"`（与 `_DEBUG` 符号变体成对）+ 落盘
`<BW_GAME_DIR>/tools/asset_pipeline/<BW_PLATFORM>`——与 loadPlugin 的
`<exeDir>/<name>.dll` 相对发现路径对齐。九个目标（各自 `plugin_main.cpp` 导出
`PLUGIN_INIT` 且 include `plugin_system/plugin.hpp`）：

`test_converter`（**样本插件**，`compiler/test_converter/`）+ 8 个生产转换器：
`texformat_converter`、`texture_converter`、`bsp_converter`、`primitive_processor`、
`visual_processor`、`space_converter`、`hierarchical_config_converter`、
`effect_converter`。

**反向插件**：`bw_world_machine_import/wm_plugin_shell.hpp`——BW 代码作为**外部宿主
World Machine** 的插件（`__declspec(dllexport)` `SetupGlobalAccess` /
`GetHeaderVersion` / `GetDevPlugin(const i, DevPluginStruc*)`，对 WM 私有头
`core\Plugin_Header.h` 编程）。宿主不在仓、仅 Windows DLL 面。

## 4. 启用组合 × 样本/缺口（验收总表）

| 组合 | 平台 | 判定 |
|---|---|---|
| batch_compiler × 转换器 DLL | 参考 Windows-only（根 CMake 非 Windows FATAL，bw00-01-02 清册；el7 RPM tools 无 batch_compiler 二进制） | **样本在树**（9 DLL + MI loader）；缺口：真机验证随 BW-15-03 Windows runner。Rust 侧 batch compiler 已交付（BW-13-01-03） |
| jit_compiler × 转换器 DLL | 同上（WIN32 GUI） | **样本在树**；缺口同上。Rust 侧 jit compiler 与转换器注册已交付（BW-13-01-04，`06bc88a`） |
| assetprocessor × 9 转换器 | 跨平台工作流 | **样本已交付**：Rust 工作流可跑且产物可被原版消费（BW-13-02-01） |
| TestCompiler 单测宿主 × `test_converter` 样本 | 参考 Windows（LoadLibrary） | **样本在树**：test_converter DLL + 单测加载/卸载循环（:14-16 带回退）。Rust 对应样本已交付：`crates/plugin-host` `tests/loader.rs` 14 用例 + `build.rs` 编真实 cdylib 供宿主打开（100% 行覆盖，`962fcba`） |
| worldeditor / modeleditor × plugin_system | — | **缺口**：零引用，编辑器无插件宿主面，无可移植对象 |
| World Machine × wm_plugin_shell | 外部宿主、Windows DLL | **缺口**：WM 宿主不在仓不可运行（BW-13-05-04 同因卡点）；Rust 无对位面 |
| Rust plugin-host × cdylib 样本 | unix 门控（dlopen） | **样本已交付**（14 用例）；缺口：Windows 宿主后端随 BW-15-03（与参考的 Unix 缺口互补——参考 loader 仅 Windows、移植仅 Unix，两侧各盖自家可测平台） |

## 5. 结论

插件宿主 SDK 的**全部启用组合均已核到**：四个资产管线家族宿主各有样本（参考侧
DLL 集在树、Rust 侧 loader/注册/工作流已交付）；编辑器宿主与 World Machine 反向
插件两组合为真缺口（无可运行宿主），已按上表登记归因（BW-15-03 / BW-13-05-04）。
SDK 本体（符号宏 + exe 相对路径 + `_d` 双名约定）在 `crates/plugin-host` 逐条
复现，加载成功与失败行为 14 用例可复现（BW-13-06-03 档）。
