# BW-00-01-02 展开 CMake 与 Windows 目标 — 清册

- **任务编号**: BW-00-01-02（lane BW-00 构建目标清册，todo.md 行 88）
- **原版构建入口/源码**: `programming/bigworld/CMakeLists.txt`（265 行，根驱动）；配置分派文件 `programming/bigworld/build/cmake/BWConfiguration_*.cmake`；目标工程 `programming/bigworld/**/CMakeLists.txt`（全树 192 个）。
- **Boundless 模块**: 无（参考侧清册，供 BW-00-05 覆盖台账与 BW-15 平台矩阵引用）。
- **平台**: Windows（本地 MSVC）；Linux 仅经 CMake 的远端 SSH 通道（Windows VS 工程作驱动，PRE_BUILD 调 `_remote_build.bat`→SSH→make）。**macOS 无路径**。
- **输入**: 根 CMakeLists.txt + 全部 `BWConfiguration_*.cmake`（10 个 + 3 个 blob 包装）+ 各目标 CMakeLists.txt；采集用只读 grep/find，未改动参考树。

## 1. BW_CMAKE_TARGET 取值全集（11 个）

取值非手写枚举，由 glob 发现配置文件名（`CMakeLists.txt:127-132`：`FILE(GLOB BWConfiguration_*.cmake)` → `BW_AVAILABLE_TARGETS`），校验与单一分派点（`:135-140` 未知值 `FATAL_ERROR`；`:170` `INCLUDE(BWConfiguration_${BW_CMAKE_TARGET})`）。**无 IF/ELSEIF 链**——每个 target 一个配置文件，填充 `BW_LIBRARY_PROJECTS`/`BW_BINARY_PROJECTS` 两个二维数组，根 `:217-251` 逐个 `ADD_SUBDIRECTORY`。

| target | 配置文件 | 说明 |
|---|---|---|
| `client` | BWConfiguration_client.cmake | Windows-only，含 `bwclient` |
| `server` | BWConfiguration_server.cmake | 跨平台（本地 MSVC + 远端 make） |
| `tools` | BWConfiguration_tools.cmake | Windows-only 编辑器/工具 |
| `assetprocessor` | BWConfiguration_assetprocessor.cmake | Windows-only DLL |
| `bwentity` | BWConfiguration_bwentity.cmake | Windows-only DLL |
| `exporters` | BWConfiguration_exporters.cmake | Windows-only 3dsMax/Maya 插件 |
| `navgen` | BWConfiguration_navgen.cmake | Windows-only（MFC） |
| `extras` | BWConfiguration_extras.cmake | process_defs/res_packer/simple_python_client（Make 覆盖） |
| `client_blob` | 包装文件 | 仅 `SET(BW_BLOB_CONFIG ON)` + `INCLUDE(BWConfiguration_client)` |
| `tools_blob` | 包装文件 | 同上（tools） |
| `exporters_blob` | 包装文件 | 同上（exporters） |

## 2. 平台矩阵

平台**在根一次性决定**（`CMakeLists.txt:57-76`）：`BW_IS_REMOTE_ONLY`→Linux（远端）；`WIN32`→Windows（win32/win64 按指针宽）；否则 `FATAL_ERROR("Only Windows builds are currently supported.")`。

- **macOS 无路径**：`APPLE` 仅出现在 vendored `FindSDL.cmake`，构建逻辑零出现；`CMAKE_SYSTEM_NAME`/`UNIX`/`MINGW`/`CYGWIN` 在 `build/cmake/*.cmake` 中缺失。
- **Linux 原生 CMake 构建不存在**：Linux 仅经 `BW_IS_REMOTE_ONLY`（`BWVerifyServer.cmake:13-23`，`BW_LINUX_CONN_TYPE != "NONE"` 且 `BW_VERIFY_LINUX=OFF`）；远端构建把 Windows VS 工程当驱动，源标记 `HEADER_FILE_ONLY`，`PRE_BUILD` 调 `_remote_build.bat`→SSH→make（`BWMacros.cmake:362-384`）。编译器令牌块（`:80-98`）仍要求 MSVC，故 CMake 进程恒在 Windows。
- 各目标可构建平台：**server** Windows+Linux（远端，恒 64 位）；**client/tools/assetprocessor/bwentity/exporters/navgen** 仅 Windows；**extras** Windows+Linux（Make 覆盖）。

整树显式门控：`IF(NOT MSVC)` 才加 `cellapp_extension`（`BWConfiguration_server.cmake:198-202`，Linux 共享模块模型支持、MSVC exe 导出不支持）；`IF(${BW_PLATFORM} STREQUAL "win64"/"win32"/"vc9")` 门控单个 exporters（无 `else()`→非 Windows 平台零构建，与根门控一致）；`AnimationExporter.cmake:10-19` 的 win32/win64 `LINK_DIRECTORIES` 亦无 `else()`。

## 3. 配置类型与启用宏

- **配置类型**：`CMAKE_CONFIGURATION_TYPES = Debug;Hybrid`（`:22-24`）；`Consumer_Release` 由 `BW_ADD_CONSUMER_RELEASE_CONFIG()`（`BWMacros.cmake:2-14`）为 **client**（`BWConfiguration_client.cmake:2`）与 **bwentity**（`BWConfiguration_bwentity.cmake:2`）追加。
- 按配置编译定义：`CODE_INLINE`（Hybrid/Consumer_Release，`:203-208`）；`CONSUMER_CLIENT`+`_RELEASE`（Consumer_Release，`client:4-7`）。**`BW_CONFIG`/`BW_CONFIG_*` 在 CMake 轨道无定义**——服务端硬编码 `ADD_DEFINITIONS(-DMF_CONFIG="Hybrid")`（`server:9`），注释声明 make 链传 `-DMF_CONFIG="$(BW_CONFIG)"`（`build/make/Makefile:210`）。`BW_CONFIG_*` 是 Make-only（`build/make/Makefile:18-19`）。
- **CMake 变量开关族**（根默认块 `:160-167` + 各 target 覆盖）：

| 变量 | 根默认 | 覆盖 | 效果 |
|---|---|---|---|
| `BW_PYTHON_AS_SOURCE` | ON | assetprocessor OFF | 静态链接 Python 源 vs 预编译 DLL |
| `BW_PCH_SUPPORT` | ON | server OFF | `-DPCH_SUPPORT=1` |
| `USE_MEMHOOK` | OFF | client ON / extras ON / tools OFF | `-DUSE_MEMHOOK` |
| `BW_FMOD_SUPPORT` | OFF | server/exporters OFF | `-DFMOD_SUPPORT=1` + FMOD 库目录 |
| `BW_SCALEFORM_SUPPORT` | OFF | client/server/exporters OFF | `-DSCALEFORM_SUPPORT=1` |
| `BW_SPEEDTREE_SUPPORT` | OFF | — | `-DSPEEDTREE_SUPPORT=1` |
| `BW_UMBRA_SUPPORT` | OFF | server/exporters OFF | `-DUMBRA_ENABLE=1` |
| `BW_AWESOMIUM_SUPPORT` | OFF | — | 由 `client/CMakeLists.txt:29-31` 消费 |
| `BW_USE_CSTDMF_DLL`/`BW_USE_CSTDMF_IN_DLL` | — | 仅 tools/offlineprocessor | `-DCSTDMF_IMPORT`，cstdmf 变 SHARED |
| `BW_USE_BWENTITY_DLL` | — | bwentity | `BWENTITY_DLL_EXPORT`，bwentity SHARED |
| `BW_BLOB_CONFIG` | — | blob 包装 | `-DBW_BLOB_CONFIG=1` |

- **每目标身份宏**（CMake 轨道唯一设置的目标标识 define）：`MF_SERVER`（server）、`BW_CLIENT`（client）、`EDITOR_ENABLED`（tools）、`BW_EXPORTER`+`_SECURE_SCL=1`（exporters）、`_NAVGEN`（navgen）、`BWCLIENT_AS_PYTHON_MODULE=1`+`PYMODULE`（assetprocessor）；另有 `SCRIPT_PYTHON`、`BW_BUILD_PLATFORM`、`ALLOW_STACK_CONTAINER`、`UNICODE`/`_UNICODE`、`NTDDI_VERSION`/`_WIN32_WINNT`/`WIN32_LEAN_AND_MEAN`（Windows-only）。
- **重要负发现**：`BW_USE_OPENSSL`、`BW_USE_CURL`、`BW_USE_SDL`、`BW_USE_SCALEFORM`、`BW_USE_MYSQL`、`BW_USE_MONGO`、`BW_USE_V8`、`BW_USE_UMBRA`、`BW_ENABLE_*` 构建变量**全树零命中**（`BW_ENABLE_` 仅 C++ 源码宏 `BW_ENABLE_STACKTRACE`）。CMake 特性旋钮是 `BW_*_SUPPORT`/`USE_MEMHOOK`/`BW_USE_CSTDMF_*`/`BW_USE_BWENTITY_DLL` 族。Make 用**不同命名**（`-DUSE_OPENSSL`、`-DENABLE_MEMTRACKER -DUSE_MEMHOOK`、`-DENABLE_PROTECTED_ALLOCATOR -DUSE_MEMHOOK`，`common_footer_config.mak:191`、`Makefile:214/218`）——两套构建系统宏词汇表**非 1:1 映射**。

## 4. 可执行/库/模块清单

`BW_ADD_LIBRARY` 无关键字（`BWMacros.cmake:263-269`）→ CMake 默认 STATIC（`BUILD_SHARED_LIBS` 未设置）；仅显式 SHARED/MODULE 列下。

### SERVER target

**可执行**（`BWConfiguration_server.cmake:164-186`）：baseapp、baseappmgr、cellapp、cellappmgr、dbapp、dbappmgr、loginapp、reviver（8 个服务进程）+ bwmachined、bots、clear_auto_load、consolidate_dbs、message_logger、snapshot_helper、sync_db、transfer_db（工具）。

**Python MODULE**：`bwengine_mysql`（dbapp_extensions/bwengine_mysql:24）、`bwengine_xml`（:12）、`_bw_profile`（tools/bw_profile:11）、`cellapp_extension`（examples/cellapp_extension:23，**Linux-only**，`IF(NOT MSVC)`）。

**静态库**（`:66-138`）：network、resmgr、math、cstdmf、connection_model、memhook、build、server、db、db_storage、db_storage_mysql、db_storage_xml、sqlite、connection、waypoint、terrain、physics2、moo、chunk_loading、delegate_interface、entitydef_script、scene、speedtree、entitydef、pyscript、chunk、urlrequest、script + libpython、re2、nedalloc（vcpkg 缺失时加 zip/libpng，`:151-162`）。

### CLIENT target

**可执行**：`bwclient`（client/CMakeLists.txt:277，`BW_ADD_EXECUTABLE(bwclient WIN32)`，Windows GUI）。
**共享库**：`emptyvoip`（lib/emptyvoip:18）。
**静态库**（`BWConfiguration_client.cmake:47-90`）：memhook、ashes、asset_pipeline、camera、chunk、chunk_scene_adapter、connection、connection_model、cstdmf、duplo、entitydef、input、math、model、moo、network、open_automate、particle、physics2、post_processing、pyscript、resmgr、romp、scene、compiled_space、space、script、terrain、urlrequest、waypoint、web_render + libpng、nedalloc、zip、libpython、re2（开关开时加 fmodsound/scaleform/speedtree，`:92-108`）。
**Python MODULE：无**（`bwclient_as_python_module` 仅在 assetprocessor 目标经 `BWCLIENT_AS_PYTHON_MODULE` 出现）。
**单测**：`BW_UNIT_TESTS_ENABLED`（默认 ON，`:123`）折叠进两 target（client `:18-44`、server `:19-64`，每 `*_unit_test` 各一 `BW_ADD_EXECUTABLE`+`BW_ADD_TEST`）。

## 5. 与 Make 构建的关系（交叉引用 BW-00-01-01）

- **双建（重叠）**：8 个服务进程全部（均有 `server/<name>/Makefile.rules`）；CMake 列表中的服务端工具全部（`remove_db` 有 `Makefile.rules` 但**不在** CMake 列表→Make-only）；服务端库与 Python 模块全部；`extras` 三二进制（process_defs/res_packer/simple_python_client）全部。
- **CMake-only（无 Makefile.rules）**：client（bwclient、emptyvoip、asset_pipeline、open_automate、web_render、client 侧 urlrequest 及 client 专属 lib）；tools（worldeditor、modeleditor、particle_editor、offline_processor、batch_compiler、jit_compiler、asset-pipeline 库与 8 转换器）；assetprocessor（DLL）、bwentity（DLL）、exporters（插件）、navgen。
- **边界总结**：Make 链覆盖**服务端运行时+其库+服务端工具+服务端 Python 模块**；CMake 覆盖 Make 全部**再加 Windows-only 表面**（bwclient、全部编辑器/工具、asset-pipeline DLL+转换器、exporters、navgen、bwentity、extras）。共享接缝=extras 三二进制与服务端核心。宏词汇两套（Make `USE_OPENSSL`/`USE_MEMHOOK`/`ENABLE_*` vs CMake `BW_*_SUPPORT`/`MF_CONFIG`）不可 1:1 映射，仅**目标/目录边界**可交叉引用。

## 状态

清册完整：11 个 BW_CMAKE_TARGET 取值、平台矩阵（Windows 本地 + Linux 远端 SSH；macOS 无路径）、配置类型（Debug/Hybrid + client/bwentity 的 Consumer_Release）、启用宏族（BW_*_SUPPORT/USE_MEMHOOK/BW_USE_CSTDMF_*/BW_USE_BWENTITY_DLL；`BW_USE_OPENSSL` 等 CMake 变量不存在）、SERVER/CLIENT 可执行/库/模块清单、Make/CMake 重叠与边界交叉引用。参考树只读未改。
