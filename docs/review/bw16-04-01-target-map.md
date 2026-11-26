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
| `network` | `crates/mercury` | 内联（`crates/mercury/src/*`，377 测） | 流编解码 5/5（BW-01-02）；`platform::{interface,socket}` 面在禁区分块 |
| `entitydef` | `crates/entitydef` | `crates/entitydef/tests/*`（3 文件）+ 内联（257 测） | BW-04 全勾 |
| `entitydef_script` | `crates/entitydef` + `crates/python-host/src/script_types.rs` | 同上 + python-host 内联 | **部分**：defs 解析/存储面；`BigWorld.Entity/Base/Proxy` 脚本类型已落地（`422d275`，BW-16-03-02：`Py_tp_base` 继承 + `tp_new` 拒绝语义）；entity-script 导入面待 |
| `pyscript` | `crates/python-host` | 内联（154 测，含 62 场景 out-of-process） | BW-03-01 全勾 |
| `script` | `crates/python-host` | 同上 | `Script::init` 序列（BW-03） |
| `resmgr` | `crates/resources` | 内联（`crates/resources/src/*`，233 测） | `BWResource`/`DataSection`（BW-01-04） |
| `asset_pipeline` | `crates/asset-pipeline` | 内联（439 测，含 §1.4 exporter 族） | BW-13-01..03 + 05 exporter 族在树 |
| `server`（`lib/server`） | `crates/server-runtime` | 内联（1698 测） | 角色生命周期面随 BW-06..10 |
| `chunk` | `crates/chunk` | 内联 | BW-10-01（标识符/边界/portal/流式绑定） |
| `testing` | `crates/test-support` | 内联（638 测） | 共享测试框架（BW-18）+ 分层检查器（BW-16-01-01/02-05） |
| `unit_test_lib` | `crates/test-support` | 同上 | 同上 |
| `db`/`db_storage`/`db_storage_mysql`/`db_storage_xml`/`sqlite` | `crates/db` + `crates/db-postgres` | 内联（db 543 测 + db-postgres 131 测） | 后端在树：SQLite（rusqlite）+ PostgreSQL（BW-09-06/07，PG18 真机）；MySQL/XML 面=BW-15-05-03 store 四模式 TSV |

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
| `client`（`bwclient`） | `crates/client` | 内联 | BW-11（窗口/设备/渲染后端，`render/` 建模参考 `IDirect3DDevice9` 生命周期） |
| `assetprocessor`/`batch_compiler`（tools） | `crates/asset-pipeline` | 内联（`asset_compiler.rs`/`batch_compiler.rs`/`task_processor.rs` 等） | BW-13-01-01/02/03 已由 tools 席位勾 |
| `res_packer`（tools） | `crates/asset-pipeline/src/res_packer.rs` | 内联（packers/`PackedSection::convert`/CLI 面） | BW-13（BW-16-04-02 的工具族消费面） |

### 1.4 刷新入库（2026-11-25 自 §2 迁入；2026-11-26 自 §1.5 并轨迁入 exporter 族与 launcher）

| 原版目标 | 新版模块 | 测试路径 | 备注 |
|---|---|---|---|
| `terrain` | `crates/terrain` | 内联（92 测） | BW-10-02/03（`EditorTerrainBlock2`/`TerrainSettings`） |
| `physics2` | `crates/physics` | 内联（114 测） | `WorldTriangle` 等（BW-14-02-04） |
| `scene`/`chunk_scene_adapter` | `crates/scene` | 内联（40 测） | BW-13-06-04（`7f06848`，11 模块 100%） |
| `compiled_space` | `crates/compiled-space` | 内联（138 测） | `space_converter` 的 compiled-space 写面 |
| `space`（lib/chunk space 面） | `crates/chunk/src/space.rs` | 内联（chunk lib 186 测） | BW-10-01（`ChunkSpace`/焦点网格/blur-focus 泵；main `623714d` 的 grid_traversal/obstacle 换代面同树） |
| `waypoint` | `crates/waypoint` | 内联（136 测） | BW-10-04/05（两层检索，真机复测 `643c2b6`） |
| `waypoint_generator` | `crates/waypoint-gen` | 内联（88 测） | navgen 生成面（girth workflow/flood/view，`977f555`；main `b99cb0e` 的 navgen_map 面同树在——05-04 双交付待主席择一） |
| `navgen` | `crates/asset-pipeline/src/navmesh_processor.rs` + `crates/waypoint-gen` | 内联（`79f47c4`） | **部分**：生成/锁块/缓存处理机在树（55 测）；Windows MFC 入口外壳未建（真卡点=无 EL7 binary，BW-13-02-05）；OpenGL 预览后端见 BW-15-06-01 |
| `bots` | `crates/load` + `apps/bots` | load 内联（169 测）+ `apps/bots` | BW-10-07-05/06（`2fd88e4`/`7560866`）；scenario runner（cross-cell/reconnect/long-stability）归 lane/bw17-load（BW-17-02-06..08） |
| `message_logger` | `crates/message_logger` | 内联（160 测） | BW-12 |
| `bw_profile` | `crates/profiler` | 内联（43 测） | BW-12-03（帧/计数器兼容层 + `_bw_profile`） |
| `clear_auto_load`/`consolidate_dbs`/`remove_db`/`snapshot_helper`/`sync_db`/`transfer_db` | `crates/db/src/tools/*.rs`（七件已声明已编译） | 内联（clear_auto_load 28 测/remove_db 12 测/consolidate 74 测等） | BW-09-05 簇；顶层散文件五份孤儿归主席清理 |
| `urlrequest` | `crates/python-host/src/urlrequest.rs` | 内联 | BW-09 |
| `jit_compiler`/`offline_processor`/`resourcechecker` | `crates/asset-pipeline/src/{jit_compiler,offline_processor,resource_checker}.rs` | 内联 | BW-13 |
| `particle`/`camera`/`post_processing`/`web_render`（面） | `crates/client/src/{particle,camera,post_processing,web_render}.rs` | client 内联（865 测） | BW-11（`c5a70f6` 合入）；`web_render` 等三方 SDK 集成点在 `conditional_integrations.rs` 登记缺席（BW-11 SDK 矩阵） |
| 编辑器核（`modeleditor_core`/`worldeditor` 核面） | `asset-pipeline::{model_editor,material_editor}` + `chunk::{chunk_editor,chunk_placer,snaps,undoredo,space_publish}` + `client::{animation_editor,particle_editor,simple_gui}` | 内联 | **部分**：编辑核面在树；GUI 窗口面未建（headless 外壳二进制 `apps/editor` 已建 `02bedbb`，Windows/macOS 入口待 BW-15-03/04） |
| `common`（tools/common） | `chunk::undoredo` + `asset_pipeline::navmesh_processor` 等按面散布 | 内联 | **部分**：按面落位，无独立 crate |
| `bwmachined` | `crates/server-runtime/src/machined.rs`（408 行，daemon+discovery 客户端）+ `crates/contracts/src/machined.rs`（3527 行消息契约） | 两模块均 `pub mod` 在活树（`lib.rs:54`/`lib.rs:7`）；server-runtime 内联 3 测（`#[test]` @295/328/356） | BW-05-01-06（`PORT_MACHINED=20018`，真实 SOCK_DGRAM 经 `platform::socket`）；2026-11-25 勘误：旧 §2 记「未建」已过时——模块与测试路径在树；RPM 布局基线见 BW-15-02-05 §2.2 |
| `animationexporter` | `crates/asset-pipeline/src/animation_exporter.rs`（1791 行） | 内联（19 测） | `95a6df2` 在树；lane/bw15-platform `e299957`（2995 行+验收 2 测）同面双交付，主席择一 |
| `visualexporter` | `crates/asset-pipeline/src/visual_exporter.rs`（2570 行） | 内联（31 测） | `0364a43` 并轨入库（2026-11-26 自 §1.5 迁入） |
| `mayavisualexporter` | `crates/asset-pipeline/src/maya_visual_exporter.rs`（5893 行） | 内联（48 测） | `0538e8c` 在树；lane/bw15-platform `maya_exporter.rs`（`c37ba81`，MayaIo trait 面）同项双交付，主席择一 |
| `bw_world_machine_import` | `crates/asset-pipeline/src/world_machine_import.rs`（1355 行） | 内联（19 测） | `a14093c`（=`8322e9d` cherry-pick）并轨入库（2026-11-26） |
| `launcher`（tools/bwlauncher） | `crates/launcher` | `crates/launcher/tests.rs` + 内联（45 测） | `e18dc94`（BW-13-05-05，100%）并轨入库（2026-11-26） |
| `client`（可执行入口，Linux 腿） | `apps/client`（`boundless-client`） | `crates/client/tests/map_display_baseline.rs`（2 测） | `02bedbb`（BW-15-02-03 固定地图显示：map_view+软件设备+TGA，六绑/8 帧二跑同） |
| `modeleditor`/`worldeditor`（可执行外壳，Linux 腿） | `apps/editor`（`boundless-editor`） | `apps/editor/tests/edit_reload.rs`（2 测） | `02bedbb`（BW-15-02-03 固定地图编辑：ed_save 移 tree00 模型+重载验证，二跑逐字节同；Windows/macOS 外壳仍待 BW-15-03/04） |

### 1.5 并轨台账（2026-11-26 清空）

原 §1.5 四行（`animationexporter`/`visualexporter`/`mayavisualexporter`/`bw_world_machine_import`，
另 `launcher`）均已入库，2026-11-26 迁入 §1.4（模块与测试路径见上行）。唯一遗留不阻塞登记：
exporter 族个别目标存在 lane 候选双交付（animation `e299957`、maya `c37ba81`），择一归主席裁，
登记面以在树代际为准（早期引用 hash `f4bd3b2`/`c784ed3` 的在树落点以上行标注为准）。

## 2. 未建目标（有归属，无模块/测试路径）

| 原版目标 | 计划归属 | 解锁项 | 现状 |
|---|---|---|---|
| `appmgr`/`moo`/`romp`/`chunk_loading`/`model`/`speedtree`/`ual`/`gizmo`/`guimanager`/`guitabs`/`controls`/`input`/`duplo`/`navigation_recast`/`connection`/`connection_model`/`bwentity`/`delegate_interface`/`visual_manipulator`/`ashes` | `crates/`（客户端/渲染/工具库） | BW-11 / BW-13 | 未建（`moo` 的设备生命周期面已由 `crates/client/src/device` 等建模，完整渲染器待 BW-11；`navigation_recast` 用户挂起 BW-10-05-03；三方 SDK 面 `fmodsound`/`scaleform`/`emptyvoip`/`memhook`/`open_automate` 在 `conditional_integrations.rs` 登记缺席） |
| `bw_lsp`/`visual_studio`/`exporter_common` | `crates/`（工具） | BW-13 | 未建 |

（编辑器 GUI 外壳行已于 2026-11-26 自 §2 迁出：`apps/editor` 可执行外壳
`02bedbb` 建成（headless Linux 腿），GUI 窗口面/Windows·macOS 入口余量并入
§1.4 该行备注。）

## 3. 覆盖与卡点

- **已合入（§1.1–1.4）**：库目标与工具面大半在树并有测试路径——`cstdmf`/`math`/`network`/
  `entitydef`/`pyscript`/`script`/`resmgr`/`asset_pipeline`/`server`/`chunk`/`testing`/`db`/
  `terrain`/`physics2`/`scene`/`compiled_space`/`space`/`waypoint`/`waypoint_generator`/`navgen`（部分）/
  `bots`/`message_logger`/`bw_profile`/DB 六工具/`urlrequest`/`jit_compiler`/`offline_processor`/
  `resourcechecker`/`res_packer`/`particle`/`camera`/`post_processing`/`web_render`（面）/编辑器核（部分）/
  `bwmachined`（模块+3 内联测在树，2026-11-25 勘误迁入 §1.4）/
  exporter 族 `animationexporter`/`visualexporter`/`mayavisualexporter`/`bw_world_machine_import`/
  `launcher`（2026-11-26 自 §1.5 并轨迁入）；
  8 个角色目标（单二进制面）、3 个扩展目标（模块面）、`client`/`process_defs`/`plugin_system`。
- **并轨清空（§1.5，2026-11-26）**：原四行+`launcher` 全部入库迁 §1.4；仅余 exporter 族
  lane 候选双交付（animation `e299957`、maya `c37ba81`）待主席择一，不阻塞登记面。
- **未建（§2）**：客户端/渲染/工具库残余、
  `bw_lsp`/`visual_studio`/`exporter_common`——**无模块与测试路径**，各随 BW-11/BW-13 立项解锁。
  （`bwmachined` 已于 2026-11-25 勘误迁入 §1.4——`server-runtime/src/machined.rs` daemon+discovery 在树；
  编辑器 GUI 外壳行已于 2026-11-26 迁入 §1.4——`apps/editor` 可执行外壳 `02bedbb` 建成，
  Windows/macOS 入口仍待 BW-15-03/04。）
- **部分**：3 个扩展（扩展内容）、`entitydef_script`（entity-script 导入面）、
  `navgen`（Windows 入口）、编辑器核（GUI 窗口面）——模块面已有，缺口见各行
  （`db` 后端矩阵已收窄：SQLite/PostgreSQL 在树，MySQL/XML 面=store 四模式 TSV）。

**卡点**：§2 目标的「模块与测试路径」依赖对应席位立项（BW-11/BW-13/BW-15）；§1.2
角色业务逻辑依赖服务端运行时（BW-06..10）。验收「每个目标有模块与测试路径」待 §2 残余
闭合（§1.5 已于 2026-11-26 并轨清空）。

## 4. 边界

本档只**登记**目标映射与测试路径，不新建 crate、不改代码；原版目标清单以
`programming/bigworld/{lib,server,tools,client}/` 目录为准（与 BW-15-06-01/02 的
后端/宿主登记同源）。