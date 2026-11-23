# BW-00-05-03 映射客户端和工具目标 — 记录

- **任务编号**: BW-00-05-03（lane BW-00-05 功能覆盖台账，todo.md 行 146）
- **验收**: 编辑器、导出器及离线工具均有索引。

## 结论

台账新增 **58 行 `origin=cmake`**（CMake/Windows 面，BW-00-01-02 清册），
总行数 79→137。编辑器（worldeditor/modeleditor/particle_editor）、导出器
（3 族 16 变体）、离线工具（offline_processor/batch_compiler/jit_compiler/
8 转换器/assetprocessor/navgen/res_packer*）全部有索引。*res_packer 为既有
make 行（BW-13-02-03）。

## 登记来源与方法

行来源 = 各 `BWConfiguration_*.cmake` 的 `BW_BINARY_PROJECTS`/`BW_LIBRARY_PROJECTS`
数组（`EXPORTER_PROJECTS` 单独 SET）。规则：**CMake-only 目标**（无
`Makefile.rules`）才新增行；与 Make 同名者共用既有 make 行（台账记功能覆盖，
不按构建系统重复）；纯第三方 vendored 工程（nedalloc/libpng/libpython/
libpython_tools/re2/nvmeshmender/recast/detour/zip）不建行（归 BW-00-01-04
第三方决策表）。任务号按 todo lane 源码列表归属。

新增 58 行分组：

| 面 | 行数 | 任务号 |
|---|---|---|
| 客户端 | 18（bwclient/emptyvoip/ashes/camera/chunk_scene_adapter/compiled_space/space/duplo/input/model/particle/post_processing/romp/web_render/open_automate/fmodsound/scaleform/asset_pipeline） | BW-11-01/04/05（fmodsound/scaleform 加 BW_*_SUPPORT 启用令牌） |
| 工具可执行 | 14（worldeditor/modeleditor/particle_editor/offline_processor/batch_compiler/jit_compiler/8 转换器） | BW-13-01/02/03/04（转换器按 BW-13-01-05/06 细分） |
| 工具库 | 19（compiler/conversion/dependency/discovery/compiled_space_writers/appmgr/guimanager/guitabs/controls/gizmo/ual/tools_common/editor_shared/modeleditor_core/navigation_recast/waypoint_generator/test_compiler/test_converter/asset_pipeline） | BW-13-01/02/03/04、BW-07（appmgr）、BW-10-05（nav 库） |
| 工具单测 | 2（compiler_unit_test/dependency_unit_test） | BW-13-02 |
| 导出器 | 3 族（animationexporter_max/mayavisualexporter_maya/visualexporter_max，note 枚举 16 变体） | BW-13-05-01/02 |
| 独立交付 | 3（assetprocessor dll/bwentity dll/navgen） | BW-13-02-01 / BW-04-04 / BW-10-05 |

## 状态与阻塞（全部 blocked）

新增行**全部 `blocked`**，note「CMake-only,Windows-only（BW-00-01-02）;
Linux 无原生 CMake 路径,验收待 Windows 真机（BW-00-02-05/BW-15-03）」——
原版这些目标仅在 Windows 构建，本机（Linux）无从对照验收；这是外部条件
（Windows 真机），与 BW-00-02-05（Windows 客户端基线）、BW-15-03（Windows
后端）的卡点一致。`bwentity` 附注 SDK 契约（BW-04-04）。

## 核对与别名

- 全部 111 个 CMake 数组工程名 + 4 拼接单测工程名：逐名覆盖（make 行 /
  新 cmake 行 / 第三方 skip / 别名），**0 未覆盖**；16 个导出器版本变体
  （max2009..2013 / maya2009..2014）经 3 个族行 note 逐一枚举，非逐名行
  但全部可追溯。
- **CMake↔Make 别名**（同一目标两名）：`bwmachined`(CMake)=`bwmachined2`(Make)、
  `cellapp_extension`(CMake)=`cellapp_example`(Make)——共用既有 make 行，不重复。
- `lib/visual_manipulator`（BW-13 源码列表点名）不是任一 BW_CMAKE_TARGET 工程
  （未在数组），属源文件非目标，不建行。

## 校验与门禁

- `python3 docs/review/tools/check_coverage_ledger.py`：**137 行全字段合法**，
  origin=make 79 项与清单双向 1:1、类型逐一吻合（cmake 行豁免清单核对），rc=0。
- `--schema-only`：rc=0；`py_compile` 过；`--scaffold` 空（make 面无缺）。
- 本项纯台账，未触 crate；workspace 级 cargo 测试未跑（兄弟会话在途债务无关）。

## 边界声明

本项完成 CMake/Windows 面索引（origin=cmake）；common/examples/FantasyDemo
（origin=resource）归 BW-00-05-04，发行物/SDK/缺失资源（origin=release/gap）
归 BW-00-05-05。

## 状态

- [x] 编辑器/导出器/离线工具全部有索引（58 新行）
- [x] 全部 CMake 工程名 0 未覆盖（含 16 导出器变体、4 单测工程、2 别名）
- [x] blocked 面如实注阻塞（Windows 真机外部条件）
- [x] 校验绿（137 行，rc=0）
