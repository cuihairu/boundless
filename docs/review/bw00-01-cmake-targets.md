# BW-00-01-02 CMake 与 Windows 目标清册

> 证据面（BW-00-01-02 验收：清册记录平台及启用宏）。

扫描自参考 `programming/bigworld` 的 CMake 构建（根 `CMakeLists.txt` + `build/cmake/BWConfiguration_*.cmake`），由 `tools/cmake_target_scan.py` 提取。

## 平台与生成方式

- **入口**：根 `CMakeLists.txt` 要求 `-DBW_CMAKE_TARGET=<name>`；从 `build/cmake/BWConfiguration_*.cmake` 文件名收集 `BW_AVAILABLE_TARGETS`，未命中即 `FATAL_ERROR`。
- **平台门**：`BW_IS_REMOTE_ONLY` 时为 Linux 远程构建（`BW_PLATFORM=${BW_REMOTE_PLATFORM}`，由 SSH 执行远端 `build/make/platform_info.py` 得到，恒为 64 位，工程名带 `_remote` 后缀）；`WIN32` 时按 `CMAKE_SIZEOF_VOID_P` 取 `win32` / `win64`；其余平台 `FATAL_ERROR "Only Windows builds are currently supported."`。即 **CMake 面只在 Windows 上生成**，Linux 只走 Make 面。
- **编译器 token**：`MSVC9..MSVC14` 映射 `vc9..vc14`；`MSVC AND CMAKE_C_COMPILER_ID MATCHES Clang` 时置 `CLANG_CL`。
- **工程名**：`${BW_CMAKE_TARGET}_${BW_PLATFORM}`（远程加 `_remote`），输出目录 `game/bin`、`game/res/bigworld`。
- **配置类型**：`CMAKE_CONFIGURATION_TYPES Debug;Hybrid`（`Hybrid` = /MD 无 _DEBUG 的 release 风味）；`CMAKE_MAP_IMPORTED_CONFIG_HYBRID "Release;"` 把 `Hybrid` 的导入库映射到 Release。`client` 与 `bwentity` 额外调 `BW_ADD_CONSUMER_RELEASE_CONFIG`，追加 `Consumer_Release`。
- **BW_IS_&lt;TARGET&gt;**：每个可用目标先置 `OFF`，选中的那个置 `ON`；剥 `_BLOB` 后缀后再比较，故 `client_blob` 与 `client` 共用 `BW_IS_CLIENT`。

## 目标一览（11 个 BWConfiguration_*.cmake）

| 目标 | 库 | 可执行 | Consumer_Release | 编译宏 | 特性开关 |
| --- | --- | --- | --- | --- | --- |
| `assetprocessor` | 25 | 1 | 否 | `BWCLIENT_AS_PYTHON_MODULE=1, PYMODULE` | — |
| `bwentity` | 15 | 2 | 是 | `—` | — |
| `client` | 41 | 17 | 是 | `BW_CLIENT, USE_MEMHOOK, ALLOW_STACK_CONTAINER` | BW_SCALEFORM_SUPPORT=OFF USE_MEMHOOK=ON |
| `client_blob` | 0 | 0 | 否 | `—` | — |
| `exporters` | 33 | 0 | 否 | `BW_EXPORTER, _SECURE_SCL=1` | BW_FMOD_SUPPORT=OFF BW_SPEEDTREE_SUPPORT=OFF BW_UMBRA_SUPPORT=OFF |
| `exporters_blob` | 0 | 0 | 否 | `—` | — |
| `extras` | 28 | 3 | 否 | `USE_MEMHOOK` | USE_MEMHOOK=ON |
| `navgen` | 29 | 1 | 否 | `_NAVGEN` | — |
| `server` | 38 | 39 | 否 | `MF_SERVER, MF_CONFIG="Hybrid"` | BW_FMOD_SUPPORT=OFF BW_PCH_SUPPORT=OFF BW_SCALEFORM_SUPPORT=OFF BW_UMBRA_SUPPORT=OFF |
| `tools` | 53 | 17 | 否 | `EDITOR_ENABLED, ALLOW_STACK_CONTAINER, USE_MEMHOOK` | USE_MEMHOOK=OFF |
| `tools_blob` | 0 | 0 | 否 | `—` | — |

库 / 可执行数为该目标 `BW_LIBRARY_PROJECTS` / `BW_BINARY_PROJECTS` 中登记的项目数（未去重，条件块内也会计入）。

## 库 / Blob 变体

| 目标 | 委托到 | 效果 |
| --- | --- | --- |
| `client_blob` | `BWConfiguration_client` | `SET( BW_BLOB_CONFIG ON )` 后 `INCLUDE` 基座配置，本身不登记项目 |
| `exporters_blob` | `BWConfiguration_exporters` | `SET( BW_BLOB_CONFIG ON )` 后 `INCLUDE` 基座配置，本身不登记项目 |
| `tools_blob` | `BWConfiguration_tools` | `SET( BW_BLOB_CONFIG ON )` 后 `INCLUDE` 基座配置，本身不登记项目 |

`BW_BLOB_CONFIG` 让基座里的资源打包步骤产出 blob 而非散文件，三个变体与各自的基座共享 `BW_IS_<TARGET>` 宏。

## 条件项（IF 守卫下的项目登记）

| 守卫 | 受控项 |
| --- | --- |
| `BW_FMOD_SUPPORT` | `fmodsound` 库（默认 OFF）（assetprocessor:fmodsound、client:fmodsound、extras:fmodsound、navgen:fmodsound、tools:fmodsound） |
| `BW_SCALEFORM_SUPPORT` | `scaleform` 库（默认 OFF，仅 client）（client:scaleform） |
| `BW_SPEEDTREE_SUPPORT` | `speedtree` 库（默认 OFF）（assetprocessor:speedtree、client:speedtree、extras:speedtree、navgen:speedtree、server:speedtree、tools:speedtree） |
| `NOT MSVC` | `cellapp_extension` 只在 clang-cl 构建下编（非 MSVC 真身）（server:cellapp_extension） |
| `NOT PNG_FOUND` | `libpng` 只在 `find_package(PNG)` 未命中时编（server:libpng） |
| `NOT ZLIB_FOUND` | `zip` 只在 `find_package(ZLIB)` 未命中时编（server:zip） |
| `USE_MEMHOOK` | `memhook` 库（默认 OFF；tools 里 `SET( USE_MEMHOOK OFF )` 后 `IF( USE_MEMHOOK )` 才加 `-DUSE_MEMHOOK`）（tools:memhook） |

根 `CMakeLists.txt` 的缺省特性开关：`BW_PYTHON_AS_SOURCE=ON`、`BW_PCH_SUPPORT=ON`；`USE_MEMHOOK`、`BW_FMOD_SUPPORT`、`BW_SCALEFORM_SUPPORT`、`BW_SPEEDTREE_SUPPORT`、`BW_UMBRA_SUPPORT`、`BW_AWESOMIUM_SUPPORT` 均为 `OFF`，各目标可自行覆盖（如 `client` 把 `USE_MEMHOOK` 置 `ON` 并加 `-DUSE_MEMHOOK`）。

另外：`BW_UNIT_TESTS_ENABLED`（缺省 `ON`）门控各目标的 `*_unit_test` 项目；server 目标额外带 `-DMF_SERVER -DMF_CONFIG="Hybrid"`。

## 计数

| 目标 | 库 | 可执行 |
| --- | --- | --- |
| `assetprocessor` | 25 | 1 |
| `bwentity` | 15 | 2 |
| `client` | 41 | 17 |
| `client_blob` | 0 | 0 |
| `exporters` | 33 | 0 |
| `exporters_blob` | 0 | 0 |
| `extras` | 28 | 3 |
| `navgen` | 29 | 1 |
| `server` | 38 | 39 |
| `tools` | 53 | 17 |
| `tools_blob` | 0 | 0 |

生成：`cmake_target_scan.py` + `make_cmake_targets_doc.py`。
