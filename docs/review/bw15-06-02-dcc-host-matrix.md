# BW-15-06-02 DCC 宿主矩阵：逐插件可用宿主

**验收** 逐插件记录可用宿主。本档登记原版三个 DCC 导出插件各自的目标宿主（应用、
宿主版本、插件入口 ABI、产物后缀、启用条件、输出目录），并给出外部依赖缺口，
使新版离线工具/导出器切分（BW-13-02）时能直接照此确定宿主矩阵。

**参考锚点**：`/home/cui/workspaces/BigWorld`，HEAD `93bb0837`
（`bigworld-14.4.1-359-g93bb0837`）。

## 1. 宿主矩阵总表

三个插件，**两个宿主**（Autodesk 3ds Max、Autodesk Maya），每个「插件 × 宿主版本」
组合是**独立构建目标**（每版本一个 `CMakeLists.txt`，调用共享项目宏）：

| 插件 | 宿主 | 插件类型 | 产物后缀 | 默认启用宿主版本 | 条件启用 |
|---|---|---|---|---|---|
| `visualexporter` | Autodesk **3ds Max** | `SceneExport`（文件导出器） | `.dle` | Max 2010、2011、2012 | Max 2009（仅 win32）；Max 2013（仅非 vc9 编译器） |
| `animationexporter` | Autodesk **3ds Max** | `SceneExport`（动画文件导出器） | `.dle` | Max 2010、2011、2012 | Max 2009（仅 win32）；Max 2013（仅非 vc9 编译器） |
| `mayavisualexporter` | Autodesk **Maya** | MLL 文件翻译器 + MEL 脚本 | `.mll` | Maya 2010、2011、2012、2013 | Maya 2009（仅 win32）；Maya 2014（仅 win64） |

**宿主覆盖并集**：3ds Max **2009–2013**（5 年版本，覆盖 3 个插件中的 2 个），
Maya **2009–2014**（6 年版本，仅视觉导出器）。

## 2. 启用条件（构建配置层）

`BW_CMAKE_TARGET=exporters` → `build/cmake/BWConfiguration_exporters.cmake`：

- 全局定义：`-DBW_EXPORTER`（:6）与 `-D_SECURE_SCL=1`（:11，注释说明：不用同
  版本的编译器/STL 会导致导出器访问 Max SDK 的 STL 容器时崩溃）。
- 关闭的可选依赖：`BW_FMOD_SUPPORT`/`BW_SPEEDTREE_SUPPORT`/`BW_UMBRA_SUPPORT`
  均 OFF（:1-3）。
- `EXPORTER_PROJECTS` 清单（:15-51）按平台条件追加：

| 条件 | 追加目标 |
|---|---|
| 无条件（默认） | `animationexporter_max2010/2011/2012`、`mayavisualexporter_maya2010/2011/2012/2013`、`visualexporter_max2010/2011/2012` |
| `BW_PLATFORM = win64`（:53-57） | + `mayavisualexporter_maya2014` |
| `BW_PLATFORM = win32`（:59-65） | + `animationexporter_max2009`、`mayavisualexporter_maya2009`、`visualexporter_max2009` |
| `BW_COMPILER_TOKEN != vc9`（:67-72） | + `animationexporter_max2013`、`visualexporter_max2013` |

- 同一配置还构建这些引擎库（:26-43）：`chunk`、`cstdmf`、`duplo`、`entitydef`、
  `input`、`math`、`model`、`moo`、`network`、`particle`、`physics2`、`pyscript`、
  `resmgr`、`romp`、`scene`、`script`、`space`、`terrain`；第三方（:46-50）：
  `libpng`、`libpython`、`nvmeshmender`、`re2`、`zip`。

**结论**：宿主矩阵不是运行时探测，而是**构建期按版本分叉**——同一份导出器源码
在多个宿主版本的 SDK 头/库下各编一份插件二进制。

## 3. 3ds Max 插件（visualexporter / animationexporter）

- 项目宏：`visualexporter/VisualExporter.cmake` 的 `BW_VISUAL_EXPORTER_PROJECT
  (_LIBRARY_NAME _MAX_VERSION)`；动画侧为 `animationexporter/AnimationExporter.cmake`
  的 `BW_ANIMATION_EXPORTER_PROJECT`。每版本目录仅一个 `CMakeLists.txt`，例如
  `max2010_exporter/CMakeLists.txt` 只有
  `INCLUDE( ../VisualExporter.cmake )` + `BW_VISUAL_EXPORTER_PROJECT( visualexporter_max2010 2010 )`。
- 版本差异落在各 `max*_exporter/CMakeLists.txt`：Max **2009** 置 `BW_NO_UNICODE ON`；
  Max **2013** 置 `BW_NO_UNICODE OFF`、追加 `third_party/3dsmax/2013/samples/
  modifiers/morpher/{include,lib}/{x32,x64}` 与 `morpher` 链接库、定义
  `_CRT_NON_CONFORMING_SWPRINTFS`。
- **插件 ABI**：`visualexporter/visualexporter.def` 与
  `animationexporter/animationexporter.def` 声明 Max SDK 库级导出
  `LibDescription @1`、`LibNumberClasses @2`、`LibClassDesc @3`、`LibVersion @4`
  （`SECTIONS .data READ WRITE`）；实现在 `expmain.cpp:124-155`
  （`__declspec( dllexport )` 四函数，视觉/动画两处同源）。插件类
  `class MFXExport : public SceneExport`（`visualexporter/mfxexp.hpp:154`）。
- 输出与后缀：`ADD_LIBRARY( … SHARED … )` + `SUFFIX ".dle"` + `PREFIX ""`，
  输出到 `game/tools/exporter/3dsmax<版本>{,x64}/`（win32 → `3dsmax2010`，
  win64 → `3dsmax2010x64`）。
- 链接：Max SDK `maxutil`、`mesh`、`bmm`、`maxscrpt`（仅 optimized）+ DirectX
  `d3d9`/`d3dx9`/`dxguid` + 引擎 `moo`/`geom`/`math`/`cstdmf`/`physics2`/`resmgr`
  + 第三方 `libpng`/`zip`/`re2`/`nvmeshmender`。
- 共享源：`tools/exporter_common/`（`bsp_generator`、`data_section_cache_purger`、
  `node_catalogue_holder`、`skin_splitter`），以及 `tools/resourcechecker/` 的
  `visual_checker`——两者由视觉与动画两个导出器共同编译进。

## 4. Maya 插件（mayavisualexporter）

- 项目宏：`mayavisualexporter/MayaVisualExporter.cmake` 的
  `BW_MAYA_VISUAL_EXPORTER_PROJECT( _LIBRARY_NAME _MAYA_VERSION )`；每版本目录
  `maya2009_exporter`…`maya2014_exporter` 各一 `CMakeLists.txt`。
- **插件 ABI**：`visualmain.cpp:38` `MLL_EXPORT MStatus initializePlugin( MObject obj )`、
  `:131` `uninitializePlugin( MObject obj )`（Maya 插件注册/注销），内部注册文件
  翻译器 `VisualFileTranslator`；随插件分发 MEL 脚本
  `visualfileexporterscript.mel`（宿主侧 UI）。
- 输出与后缀：`SUFFIX ".mll"` + `PREFIX ""`，输出到
  `game/tools/exporter/maya<版本>{,x64}/`。
- 头/库：`third_party/maya/<版本>/include` 与 `lib/{Win32,x64}`；编译定义
  `-DWIN32 -D_WINDOWS -D_USRDLL -DVISUALTEST_EXPORTS`。
- 源面：网格/骨骼/材质/约束/blendshapes/hierarchy（`mesh.cpp`、`boneset.cpp`、
  `bonevertex.cpp`、`material.cpp`、`constraints.cpp`、`blendshapes.cpp`、
  `hierarchy.cpp`）、视觉信封（`visual_envelope`、`visual_mesh`、`portal`）与
  设置持久化（`expsets*`）。

## 5. 外部依赖缺口

`third_party/` 下**没有 `3dsmax/` 也没有 `maya/` 目录**（快照内
`ls third_party/` 仅 CppUnitLite2、WTL、curl、mongodb、nedalloc、nvmeshmender、
nvtt、openautomate、openssl-*、png、putty、python、python_modules、re2、
recastnavigation、sqlite、zip 等）。cmake 期望的 `third_party/3dsmax/<版本>/` 与
`third_party/maya/<版本>/` 是**外部 DCC SDK**，不随仓库交付。因此本矩阵记录的是
**宿主清单与入口 ABI 契约**，插件二进制无法从本树直接构建；宿主侧实测
（`BW-15-06-03/04`）需先取得相应 DCC SDK 与宿主安装。

## 6. 新版归属

| 原版 | 归属 | 新版目标 |
|---|---|---|
| `visualexporter`、`animationexporter`（3ds Max `.dle`）、`mayavisualexporter`（Maya `.mll`） | **DCC 宿主插件**（Windows 宿主内加载，宿主 ABI 由各 DCC SDK 定义） | BW-13-02 离线导出工具；宿主插件层随 BW-13/BW-15-03 立项落地，Rust 侧无对应宿主插件 ABI（各 DCC SDK 均为 C++/COM），宿主适配层为 Windows-only |
| `tools/exporter_common/` | 视觉/动画/ Maya 三方共用导出源 | 见 BW-13-06-02 结论（BW-13 导出器共享子模块） |

新版无客户端渲染 crate、无 DCC 宿主适配 crate（`crates/` 现有 asset-pipeline、
contracts、entitydef、foundation、mercury、platform、python-host、python-module、
resources、server-runtime、test-support），故本矩阵为参考树侧清单，随
BW-13 导出器切分时逐项迁移。