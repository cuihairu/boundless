# BW-13-06-02 映射 editor_shared、exporter_common 与 tools/common

**验收** 共享代码依赖可追溯。本档登记这三个原版编辑器/导出器共享库的源码面、
消费方（依赖方向）与新版归属，使新树切分时共享代码的归属与依赖可回溯到原版
来源。

**参考锚点**：`/home/cui/workspaces/BigWorld`，HEAD `93bb0837`
（`bigworld-14.4.1-359-g93bb0837`）。

## 1. `tools/editor_shared/` —— 编辑器应用抽象接口层（库）

`tools/editor_shared/CMakeLists.txt` 按 `BW_IS_QT_TOOLS` 分派：
- **默认（MFC）**：`CMakeLists.mfc.txt`——`BW_ADD_LIBRARY(editor_shared …)`，
  `INCLUDE(BWStandardMFCProject)`；源含 `mfc/app/i_editor_app.cpp`、
  `mfc/cursor/{cursor,wait_cursor}.cpp`、`mfc/dialogs/{file_dialog,message_box}.cpp`、
  `mfc/menu_helper.cpp`。
- **Qt 工具（`BW_IS_QT_TOOLS`）**：`CMakeLists.qt.txt`——`BW_ADD_LIBRARY`
  （无 MFC）；源仅 `mfc/menu_helper.cpp` + 共享接口头。

**共享公共接口**（两类变体共用，`SHARED_SOURCE_FILES_SRCS`）：

| 接口头 | 内容 | 消费方 |
|---|---|---|
| `app/i_editor_app.hpp` | `IEditorApp`（编辑器应用抽象基类） | `modeleditor_core`（`IModelEditorApp : IEditorApp`）、`worldeditor`、`modeleditor`、`tools_common` |
| `gui/i_main_frame.hpp` | `IMainFrame`（主窗口框架接口） | 各编辑器主框架 |
| `pages/gui_tab_content.hpp` | `GuiTabContent`（属性页内容） | 编辑器属性页 |
| `cursor/cursor.hpp` | 光标/等待光标 | 编辑器工具 |
| `dialogs/file_dialog.hpp`、`dialogs/folder_guard.hpp`、`dialogs/message_box.hpp` | 文件/目录/消息对话框 | 编辑器工具（MFC 特化） |

**依赖方向**（grep 全树链接 `editor_shared` 的构建目标）：`modeleditor_core`、
`modeleditor`、`worldeditor`、`tools/common`（`tools_common`）——即共享层位于最底，
编辑器应用与 `tools_common` 均在其上；
`BW_IS_QT_TOOLS` 未在 `build/cmake/` 任何配置中置 ON（grep 全树），即默认构建走
MFC 变体。

**归属**：编辑器应用公共抽象层（无渲染/物理依赖，纯接口 + 平台对话框）。新版目标
= BW-13 编辑器 crate 的 `editor-app` 公共接口子模块（`IEditorApp`/`IMainFrame`/
`GuiTabContent` 契约）；对话框/光标为 MFC 特化，Windows-only，随 BW-15 同批。

## 2. `tools/exporter_common/` —— 3D 导出器共享源（无独立目标）

**无 `CMakeLists.txt`**——不是构建目标；其源被**各导出器可执行直接编译**。

| 源 | 作用 | 编译进 |
|---|---|---|
| `bsp_generator.{cpp,hpp}` | BSP 碰撞面生成 | `visualexporter`、`mayavisualexporter` |
| `data_section_cache_purger.{cpp,hpp}` | DataSection 缓存清空 | 导出器 |
| `node_catalogue_holder.{cpp,hpp}` | 节点目录持有（导出时树遍历） | `visualexporter`、`mayavisualexporter` |
| `skin_splitter.{cpp,hpp,ipp}` | 蒙皮顶点/索引切分 | `visualexporter`、`mayavisualexporter` |
| `vertex_formats.hpp` | 顶点格式声明（头文件） | `visualexporter`、`mayavisualexporter` |

消费方（按 include/引用）：`tools/visualexporter/`（`mfxexp.cpp`、
`visual_envelope.cpp`）、`tools/mayavisualexporter/`
（`visualfiletranslator.cpp`、`visual_envelope.cpp`）。

**归属**：3D 模型导出器共享层（Max/Maya 导出共用）。新版目标 = BW-13 导出器
（visualexporter/mayavisualexporter）共享子模块；`vertex_formats.hpp` 若被新版
模型格式 crate 复用则随资产管线归位。

## 3. `tools/common/` —— 编辑器/工具共享库（`tools_common`）

`tools/common/CMakeLists.txt:112`：`BW_ADD_LIBRARY( tools_common ${BLOB_SRCS} )`；
另为 navgen 提供 `chunk_flooder` 等（见 BW-10-05-01）。源面（按职责分组）：

| 职责组 | 代表源 | 消费方 |
|---|---|---|
| 编辑器块缓存 | `editor_chunk_cache_base`、`editor_chunk_entity_base`、`editor_chunk_model_base`、`editor_chunk_terrain_{base,cache,lod_cache}`、`editor_chunk_processor_cache`、`editor_chunk_navmesh_cache_base` | `worldeditor`、`modeleditor_core`、`particle_editor`、`lib/chunk`（editor 启用时） |
| 导航/网格生成 | `chunk_flooder`、`navmesh_processor`、`recast_processor`、`girth`、`physics_handler`、`waypoint_annotator`、`collision_advance` | `worldeditor`、`modeleditor_core`、`navgen`（经 `chunk_flooder`/`girth`） |
| 编辑器基础 | `base_camera`、`mouse_look_camera`、`orbit_camera`、`tools_camera`、`cooperative_moo`、`dxenum`、`romp_harness` | `worldeditor`、`modeleditor_core` |
| 面板/属性 | `base_panel_manager`、`base_property_table`、`property_list`、`property_table`、`material_properties`、`material_proxies`、`material_utility`、`popup_menu` | 各编辑器 |
| 工具设施 | `space_mgr`、`string_utils`、`utilities`、`resource_loader`、`compile_time`、`delay_redraw`、`bw_message_info`、`floor`、`lighting_influence`、`chop_poly`、`command_line` | 各编辑器/工具 |
| 脚本/适配 | `python_adapter`、`editor_views`、`editor_group`、`shader_loading_dialog` | 各编辑器 |

**依赖方向**（链接 `tools_common` 的目标，grep 全树）：`tools/worldeditor`、
`tools/modeleditor_core`（→modeleditor）、`tools/particle_editor`、`lib/ual`、
`lib/chunk`、`tools/navgen`（经 COMMON_SRCS 直接编译，见 BW-10-05-01）。
反向：`tools_common` 自身链接 `editor_shared`（tools/common/CMakeLists.txt:114-116
的 `BW_TARGET_LINK_LIBRARIES` 列表），故层级为 `editor_shared` ← `tools_common` ← 各编辑器。

**归属**：编辑器/工具共享层（渲染侧 `moo`/`romp`/`terrain` 依赖，Windows/GUI
为主）。新版目标 = BW-13 编辑器共享 crate；其中导航/网格组
（`chunk_flooder`/`navmesh_processor`/`recast_processor`/`girth`/`waypoint_annotator`）
与 BW-10 导航生成（`waypoint_generator` 库）同源，随 BW-10-05/06 落地时同步切分。

## 4. 依赖可追溯结论

| 原版库 | 形态 | 依赖方向（消费方） | 新版归属 |
|---|---|---|---|
| `tools/editor_shared/` | 库（MFC/Qt 双变体，默认 MFC） | modeleditor_core、modeleditor、worldeditor、tools_common | BW-13 编辑器 crate 公共接口子模块（`IEditorApp`/`IMainFrame`/`GuiTabContent`）；MFC 特化随 BW-15 |
| `tools/exporter_common/` | 无独立目标（源并入导出器） | visualexporter、mayavisualexporter | BW-13 导出器共享子模块（`vertex_formats.hpp` 视复用随资产管线） |
| `tools/common/` | 库（`tools_common`） | worldeditor、modeleditor_core、particle_editor、ual、chunk（editor 启用）、navgen | BW-13 编辑器共享 crate；导航/网格组与 BW-10 同步切分 |
