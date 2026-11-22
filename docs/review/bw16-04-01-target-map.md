# BW-16-04-01 原版目标 → 新版模块/测试路径登记

**验收** 每个目标有模块与测试路径。本档登记原版构建目标（lib 库、服务端角色、
服务端工具、工具链、客户端）在新版树中的模块与测试位置；**未建目标**标注其计划
归属与解锁项，使 BW-16 开篇「登记原版构建目标、功能编号、新 crate/模块及测试位置」
的映射面完整，且可随各解锁项增量补齐。

**参考锚点**：`/home/cui/workspaces/BigWorld`，HEAD `dc3b0b59`（`bigworld-14.4.1-369-gdc3b0b59`）。
原版构建目标名 = `programming/bigworld/{lib,server,tools,client}/<dir>` 目录名
（`BW_ADD_LIBRARY(<dir>)` / `BW_ADD_EXECUTABLE(<dir>)`，目录名即目标名）。

## 1. 已合入目标（有模块与测试路径）

### 1.1 库（`lib/*` → `crates/*`）

| 原版目标 | 新版模块 | 测试路径 | 备注 |
|---|---|---|---|
| `cstdmf` | `crates/foundation` | 内联 `#[test]`（`crates/foundation/src/*`） | 基础工具/日志/时间 |
| `math` | `crates/foundation` | 同上 | `Matrix`/`Vector*` 等（BW-01） |
| `network` | `crates/mercury` | 内联（`crates/mercury/src/*`，323 测） | 流编解码 5/5（BW-01-02）；`platform::{interface,socket}` 面在禁区分块 |
| `entitydef` | `crates/entitydef` | `crates/entitydef/tests/*`（3 文件）+ 内联（238 测） | BW-04 全勾 |
| `entitydef_script` | `crates/entitydef` | 同上 | Python 绑定面 |
| `pyscript` | `crates/python-host` | 内联（145 测，含 62 场景 out-of-process） | BW-03-01 全勾 |
| `script` | `crates/python-host` | 同上 | `Script::init` 序列（BW-03） |
| `resmgr` | `crates/resources` | 内联（`crates/resources/src/*`，212 测） | `BWResource`/`DataSection`（BW-01-04） |
| `asset_pipeline` | `crates/asset-pipeline` | 内联（62 测） | BW-13-01-01/02/03 已由 tools 席位勾 |
| `server`（`lib/server`） | `crates/server-runtime` | 内联（134 测） | 角色生命周期面随 BW-06..10 |
| `testing` | `crates/test-support` | 内联（113 测） | 共享测试框架（BW-18）+ 分层检查器（BW-16-01-01/02-05） |
| `unit_test_lib` | `crates/test-support` | 同上 | 同上 |
| `db`/`db_storage`/`db_storage_mysql`/`db_storage_xml`/`sqlite` | `crates/db` | 内联（4 测） | **部分**：后端矩阵（MySQL/XML/SQLite，BW-15-05-03）、PostgreSQL（BW-15-05-04）待 |

### 1.2 服务端角色（`server/*` → `apps/server`）

原版 8 角色**各自成 app**；新版以**单二进制 8 角色**（`boundless-server <role>`，
BW-00-03-05）承载：

| 原版目标 | 新版模块 | 测试路径 |
|---|---|---|
| `baseapp` | `apps/server` | `apps/server/tests/role_lifecycle.rs`（8 角色启动/停止） |
| `baseappmgr` | `apps/server` | 同上 |
| `cellapp` | `apps/server` | 同上 |
| `cellappmgr` | `apps/server` | 同上 |
| `dbapp` | `apps/server` | 同上 |
| `dbappmgr` | `apps/server` | 同上 |
| `loginapp` | `apps/server` | 同上 |
| `reviver` | `apps/server` | 同上 |

（原版角色**业务逻辑**——登录链路、实体、AOI——未移植，属 BW-06..10；`role_lifecycle.rs`
只覆盖启动/停止面。）

### 1.3 服务端扩展与工具

| 原版目标 | 新版模块 | 测试路径 | 备注 |
|---|---|---|---|
| `baseapp_extensions`/`cellapp_extensions`/`dbapp_extensions` | `crates/python-module`（`boundless.so`）+ `crates/python-host` 宿主 | `crates/python-module/tests/import.rs` | **部分**：模块面（BW-00-03-03）绿；扩展**内容**（各角色 API，BW-03-03/04）待 |
| `process_defs` | `crates/entitydef` bin | `crates/entitydef/src/bin/process_defs.rs` + install 布局自检（BW-04-04-01） | CLI 与参考逐项对齐（BW-14-04-01） |
| `plugin_system`（tools） | `crates/plugin-host` | `crates/plugin-host/tests/*`（1 文件）+ 内联 | `PluginLoader` 移植（BW-13-06-03） |
| `assetprocessor`/`batch_compiler`（tools） | `crates/asset-pipeline` | 内联（`asset_compiler.rs`/`batch_compiler.rs`/`task_processor.rs` 等，62 测） | BW-13-01-01/02/03 已由 tools 席位勾 |

## 2. 未建目标（有归属，无模块/测试路径）

| 原版目标 | 计划归属 | 解锁项 | 现状 |
|---|---|---|---|
| `client`（`bwclient`） | `crates/`（渲染栈） | BW-11 | 参考客户端 Windows-only（BW-15-06-01） |
| `appmgr`/`memhook`/`moo`/`romp`/`chunk`/`chunk_loading`/`chunk_scene_adapter`/`compiled_space`/`model`/`particle`/`speedtree`/`terrain`/`post_processing`/`scaleform`/`fmodsound`/`web_render`/`ual`/`gizmo`/`guimanager`/`guitabs`/`controls`/`input`/`camera`/`duplo`/`scene`/`space`/`physics2`/`navigation_recast`/`open_automate`/`emptyvoip`/`connection`/`connection_model`/`waypoint`/`waypoint_generator`/`visual_manipulator`/`bwentity`/`delegate_interface`/`ashes` | `crates/`（客户端/渲染/工具库） | BW-11 / BW-13 | 未建 |
| `modeleditor`/`worldeditor`/`worldeditor_ui`/`particle_editor`/`modeleditor_core`/`editor_shared` | `crates/` + `apps/editor` | BW-13（编辑器立项） | 未建；`apps/` 现仅 server（BW-16-04-02/03 同卡） |
| `navgen` | `crates/`（导航） | BW-13 | 未建；OpenGL 预览后端见 BW-15-06-01 |
| `visualexporter`/`animationexporter`/`mayavisualexporter` | `crates/`（导出器） | BW-13-02 | 未建；宿主矩阵见 BW-15-06-02 |
| `bots` | `crates/`（Bots） | BW-12-04 | 未建 |
| `bwmachined` | `crates/`（machined） | BW-15-02 | 未建；RPM 基线见 BW-15-02-05 |
| `message_logger`/`bw_profile` | `crates/`（工具） | BW-15-02 | 未建 |
| `clear_auto_load`/`consolidate_dbs`/`remove_db`/`snapshot_helper`/`sync_db`/`transfer_db` | `crates/`（DB 工具） | BW-09 | 未建 |
| `bw_lsp`/`jit_compiler`/`bw_world_machine_import`/`offline_processor`/`resourcechecker`/`visual_studio`/`exporter_common`/`common` | `crates/`（工具） | BW-13 | 未建 |
| `urlrequest` | `crates/` | BW-09 | 未建 |

## 3. 覆盖与卡点

- **已合入**：17 个库目标、8 个角色目标（单二进制面）、3 个扩展目标（模块面）、
  4 个工具（`process_defs`/`plugin_system`/`assetprocessor`/`batch_compiler`）——
  **均已有模块与测试路径**（§1）。
- **未建**：客户端、39 个客户端/渲染/工具库（含 `appmgr`/`memhook`/`urlrequest`）、
  5 个编辑器目标、3 个导出器、Bots、machined、日志/DB 工具、其余工具——
  **无模块与测试路径**（§2），各随 BW-09/11/12-04/13 立项解锁。
- **部分**：`db`（后端矩阵）、3 个扩展（扩展内容）——模块面已有，内容/后端待解锁项。

**卡点**：§2 目标的「模块与测试路径」依赖对应席位立项（BW-09/11/12-04/13）；§1.2
角色业务逻辑依赖服务端运行时（BW-06..10）。本项按「已合入部分勾证据、未建部分记
归属与解锁项」的登记面交付，验收「每个目标有模块与测试路径」待 §2 全部立项后闭合。

## 4. 边界

本档只**登记**目标映射与测试路径，不新建 crate、不改代码；原版目标清单以
`programming/bigworld/{lib,server,tools,client}/` 目录为准（与 BW-15-06-01/02 的
后端/宿主登记同源）。