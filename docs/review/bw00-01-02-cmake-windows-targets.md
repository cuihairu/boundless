# BW-00-01-02 CMake 与 Windows 目标清册

基线 `4657a17cf10d55e2fbebf258f96da96a3dbe0c22`，路径相对 `../BigWorld/programming/bigworld/`。对应 todo.md BW-00-01-02：**展开 CMake 与 Windows 目标，清册记录平台及启用宏**。

## 采集方法

1. **静态逐文件读**：根 `CMakeLists.txt`（265 行）+ `build/cmake/` 全量（`BWConfiguration_*.cmake` 11 个、宏/工程/链接器 `BW*.cmake` 6 个、`Find*` 模块 5 个、`BWProjectUserFile.cmake`、`RemoteBuildConverter.py`、`BWUserFileTemplates/`）+ 驱动 `build/bigworld_cmake.py`（509 行）与 3 个 `.bat` 入口 + `vcpkg.json` 与 `build/vcpkg-triplets/x64-linux-bw.cmake`。
2. **工程源目录存在性核对**：`BW_LIBRARY_PROJECTS`/`BW_BINARY_PROJECTS` 每个 path 逐一 `[ -e ]` 验证，全部存在（`tools/asset_pipeline/compiler/test_*`、`lib/navigation_recast` 等易错路径已修正核对）。
3. **原版运行痕迹引证**：本机为 Linux，未跑 Windows configure；清册为静态采集。`CMakeLists.txt`/`BWConfiguration_server.cmake` 注释携带原版探针 run ID（如 39470260881、39476482506、39485824624、39006994918、39439040925、39016799505），证明原版树历史上有真实 superproject 配置/链接运行，差异语义以此为准。
4. **动态缺口如实登记**：见文末「待验登记」。

## 驱动入口与调用契约

- 入口：`build/bigworld_cmake.bat`（裸跑）、`bigworld_cmake_with_server.bat`（`--show-verify-server`）、`bigworld_cmake_experimental.bat`（`--experimental`），均转调 `bigworld_cmake.py`。
- 调用契约（根 `CMakeLists.txt`）：必须在 `bigworld/` 根配置；`-DBW_CMAKE_TARGET=<name>` 必填（缺省 FATAL_ERROR）；目标名合法性由 `FILE(GLOB BWConfiguration_*.cmake)` 动态发现（`BW_AVAILABLE_TARGETS`），并为每个目标设置 `BW_IS_<NAME>` 开关（blob 后缀剥除后置 ON）。
- `bigworld_cmake.py` 参数：`--target`（可多次）、`--generator`、`--cmake-exe`（默认自带 `third_party/cmake-win32-x86/bin/cmake.exe`）、`--dry-run`、`--deprecated`、`--experimental`、位置参数 `builddir`；构建目录后缀按生成器（`vc16_win64`…；server 远程目标固定 `linux`）。
- 解决方案工程名：`${BW_CMAKE_TARGET}_${BW_PLATFORM}`（远程-only 时追加 `_remote`）。

## 平台矩阵

| 维度 | 取值 | 判定处 |
|---|---|---|
| Windows | `win32`（BW_ARCH_32）/ `win64`（BW_ARCH_64） | `CMAKE_SIZEOF_VOID_P`（根 CMakeLists 63-73） |
| Linux | 仅远程构建：`BW_IS_REMOTE_ONLY` → `BW_PLATFORM=${BW_REMOTE_PLATFORM}`，恒 64 位 | 根 CMakeLists 58-62 |
| 其他平台 | FATAL_ERROR「Only Windows builds are currently supported」 | 根 CMakeLists 74-76 |
| 编译器 token | `vc9/vc10/vc11/vc12/vc14`（MSVC9-14）；MSVC14 追加 `_SILENCE_TR1_NAMESPACE_DEPRECATION_WARNING` + `XPDeprecationWarning=false`；clang-cl 检测置 `CLANG_CL` | 根 CMakeLists 80-117 |
| 配置类型 | `Debug;Hybrid`（覆盖默认四配置；Hybrid 为 release 风味 `/MD` 无 `_DEBUG`） | 根 CMakeLists 22-24 |
| Hybrid 导入解析 | `CMAKE_MAP_IMPORTED_CONFIG_HYBRID="Release;"`——vcpkg 导入目标 Hybrid 落 Release 导入（原版注释：message_logger 曾吃进 MDd jsoncpp，LNK2038，run 39470260881） | 根 CMakeLists 39 |
| 预处理宏 | `COMPILE_DEFINITIONS_DEBUG=_DEBUG`、`_HYBRID`、`_CONSUMER_RELEASE`（后者仅存于 OUTPUT_NAME/CODE_INLINE 引用，非现行配置）；Hybrid 与 CONSUMER_RELEASE 定义 `CODE_INLINE` | BWCompilerAndLinkerOptions.cmake 206-212、根 202-208 |
| `BW_BUILD_PLATFORM` | `-DBW_BUILD_PLATFORM="${BW_PLATFORM}"` 全目标注入 | BWStandardProject.cmake 135 |
| Linux 远程参数 | `-DBW_LINUX_DIR/-DBW_LINUX_CFLAGS/-DBW_REPLACE_LINUX_DIR`，plink（putty）连通性校验；`BWVerifyServer.cmake` + `BW_VERIFY_LINUX` 控制编译前验证 | bigworld_cmake.py 378-381、BWVerifyServer.cmake |

生成器表（`bigworld_cmake.py CMAKE_GENERATORS`）：VS2008 Win32（vc9）、VS2010 Win32/Win64（vc10）、VS2012 Win32/Win64（vc11）、VS2013 Win32/Win64（vc12）、VS2019 Win32/x64（vc16）、VS2022 Win32/x64（vc17）、Ninja+MSVC11/12 Win32/Win64（`CodeBlocks - Ninja`）、VS2012±Clang-cl Win32/Win64。旧生成器走 `--deprecated`，实验项走 `--experimental`。

## 目标族清单（BW_CMAKE_TARGET，11 个）

| 目标 | 启用宏（ADD_DEFINITIONS） | 库工程 | 二进制工程 | 单测 | 备注 |
|---|---|---|---|---|---|
| `assetprocessor` | `BWCLIENT_AS_PYTHON_MODULE=1`、`PYMODULE` | 23 | 1 | 0 | bwclient 以 Python 模块形态进 assetprocessor |
| `bwentity` | （无全局宏；MSVC 加 `/Zc:wchar_t-`） | 13 (+2 UT 库) | 1 | 1 | BW-04 的 bwentity DLL 工程 |
| `client` | `BW_CLIENT`、`USE_MEMHOOK`、`ALLOW_STACK_CONTAINER`（`USE_MEMHOOK` 置 ON） | 36 (+2 UT 库) | 2 | 15 | bwclient（WIN32 GUI exe）+ emptyvoip(SHARED) |
| `client_blob` | 继承 client + `BW_BLOB_CONFIG=1`（win64 debug 加 `/bigobj`） | 同 client | 同 client | 同 client | 薄包装：`SET(BW_BLOB_CONFIG ON)` 后 INCLUDE client |
| `exporters` | `BW_EXPORTER`、`_SECURE_SCL=1` | 33 起（条件追加，见下） | 0 | 0 | Max/Maya 导出器插件，全部注册进 BW_LIBRARY_PROJECTS |
| `exporters_blob` | 同 exporters + `BW_BLOB_CONFIG` | 同 exporters | 0 | 0 | 薄包装 |
| `extras` | `USE_MEMHOOK` | 26 | 3 | 0 | 独立小件：process_defs / res_packer / simple_python_client |
| `navgen` | `_NAVGEN` | 27 | 1 | 0 | 寻路预生成 |
| `server` | `MF_SERVER`、`MF_CONFIG="Hybrid"` | 33 (+2 UT 库) | 17（MSVC） | 21 | 另注册 start/stop/display_server 三条 Linux 命令 |
| `tools` | `EDITOR_ENABLED`、`ALLOW_STACK_CONTAINER`、include `tools/` | 46 (+4 UT 库) | 15 | 2 | 世界/模型/粒子编辑器 + batch/jit 编译器 + 8 资产转换器 |
| `tools_blob` | 同 tools + `BW_BLOB_CONFIG` | 同 tools | 同 tools | 同 tools | 薄包装 |

exporters 条件追加：win64 加 `mayavisualexporter_maya2014`；非 vc9 加 `animationexporter_max2013`+`visualexporter_max2013`；win32 加三件 max2009 系列。

## server 目标二进制（17，MSVC 口径）

| 工程 | 源路径 | 说明 |
|---|---|---|
| baseapp / baseappmgr / cellapp / cellappmgr / dbapp / dbappmgr / loginapp / reviver | `server/<同名>` | 8 服务进程，与 Make 链一致 |
| bots / clear_auto_load / consolidate_dbs / message_logger / snapshot_helper / sync_db / transfer_db | `server/tools/<同名>` | 7 工具进程，与 Make 链一致 |
| `bwmachined` | `server/tools/bwmachined` | **Windows 上直接 RETURN 跳过**（Linux-only：syslog/setrlimit/proc，原版注释 run 39016799505）；Make 链产物名为 `bwmachined2` |
| `_bw_profile` | `server/tools/bw_profile` | `BW_ADD_LIBRARY(_bw_profile MODULE)`——MSVC DLL；Make 链注册了 `_bw_profile.so` 但盘上无产物（BW-00-01-01 缺口） |
| `cellapp_extension`（条件，仅非 MSVC） | `examples/cellapp_extension` | MSVC exe 不导出宿主符号（53 LNK2019，run 39439040925），设计项未落地前 Windows 排除 |

server 单测 21 件与 Make 链 `BW_UNIT_TESTS` 完全同集（cstdmf/db/entitydef/math/network/physics2/pyscript/resmgr/scene/server/script/terrain/unit_test_lib + baseapp/baseappmgr/cellapp/cellappmgr/dbapp/dbappmgr/loginapp/reviver）；**非 MSVC 从该表剔除 cstdmf/math 两件**（Make 链负责跑它们），得 19。

## client / tools / extras 目标二进制

| 目标 | 二进制 |
|---|---|
| client | `bwclient`（`client/CMakeLists.txt:277`，`BW_ADD_EXECUTABLE(bwclient WIN32)`，DEBUG/HYBRID/CONSUMER_RELEASE 三套 OUTPUT_NAME）、`emptyvoip`（SHARED，同名三套 OUTPUT_NAME） |
| client 单测（15） | compiled_space cstdmf duplo entitydef math model moo network particle physics2 pyscript resmgr scene script space（各 `lib/<名>/unit_test`） |
| tools | `cstdmf`（挂 `lib/cstdmf`，经由 BINARY_PROJECTS 复用其可执行目标）、worldeditor modeleditor particle_editor offline_processor batch_compiler jit_compiler + 8 转换器（bsp/effect/texformat/texture/hierarchical_config/space/visual/primitive `_converter`） |
| tools 单测（2） | compiler_unit_test、dependency_unit_test（asset_pipeline 下） |
| extras | `simple_python_client`（源在 `examples/client_integration/python/simple`）、`process_defs`、`res_packer`（`tools/` 下） |

## 特性开关（根默认，目标可覆盖）

| 开关 | 根默认 | 实际启用 | 附加宏 |
|---|---|---|---|
| `BW_UNIT_TESTS_ENABLED` | OPTION ON | 全目标可用；server/client/tools/bwentity 各自 `ENABLE_TESTING()` | — |
| `BW_PYTHON_AS_SOURCE` | ON | — | — |
| `BW_PCH_SUPPORT` | ON | server 显式 OFF | `PCH_SUPPORT=1` |
| `USE_MEMHOOK` | OFF | client ON（+宏）；extras 挂 `USE_MEMHOOK` 宏 | `USE_MEMHOOK` |
| `BW_FMOD_SUPPORT` | OFF | 无目标开 | `FMOD_SUPPORT=1` + fmod include |
| `BW_SCALEFORM_SUPPORT` | OFF | client 显式 OFF | `SCALEFORM_SUPPORT=1` |
| `BW_SPEEDTREE_SUPPORT` | OFF | 无目标开（server 已把 speedtree 无条件入表，仅碰撞缓存语义） | `SPEEDTREE_SUPPORT=1` |
| `BW_UMBRA_SUPPORT` | OFF | 无目标开 | `UMBRA_ENABLE=1` |
| `BW_AWESOMIUM_SUPPORT` | OFF | 无目标开（client CMakeLists 内仍有消费点） | — |
| `BW_BLOB_CONFIG` | OFF | 仅 3 个 blob 包装目标 | `BW_BLOB_CONFIG=1` |

## 第三方依赖接入

- **vcpkg manifest**（`vcpkg.json`，version 14.4.1，builtin-baseline `10541e3` 钉死）：openssl、curl[openssl]、jsoncpp、zlib、libpng、sqlite3、bzip2（bzip2 仅为 CPython `_bz2` 扩展）。自定义 triplet `x64-linux-bw`：x64、CRT 动态、库静态、**仅 release**、`VCPKG_CMAKE_SYSTEM_NAME=Linux`，经 `--overlay-triplets` 传入。
- **Find 模块**：FindBWOpenSSL / FindBWcurl / FindSDL / FindSDL_image / FindScaleformSDK；BWStandardProject 每库工程重跑 `FIND_PACKAGE(ZLIB/PNG QUIET)`，找不到才回落 vendored `third_party/zip`、`third_party/png/projects/visualc71`（server 配置同规则）。
- **刻意不走 vcpkg**：`third_party/python`（深度补丁的嵌入式 CPython，BWHooks 内存跟踪，见 bw_changes.txt）、`third_party/mongodb`（旧版 mongo-cxx-driver API）。vendored 其余：re2、nedalloc、CppUnitLite2、recastnavigation（Recast+Detour）、nvmeshmender、python_modules/tools（libpython_tools）。

## 与 Linux Make 链（BW-00-01-01）的差异登记

| 项 | Make 链 | CMake 链 |
|---|---|---|
| serviceapp | `copy_of_binary`（baseapp 拷贝） | 无此目标 |
| remove_db | 一次性规则拷 Python 脚本进 `server/commands/` | 无此目标 |
| process_defs / res_packer / simple_python_client | server 面（process_defs 按 HOST 平台） | 独立 `extras` 目标 |
| bwmachined2 / bwmachined | 产物名 `bwmachined2` | 工程名 `bwmachined`，Windows 上跳过 |
| `_bw_profile` | 注册 `.so` 但盘上无产物 | MODULE 目标（MSVC DLL）声明齐全 |
| 单测 | `BW_UNIT_TESTS` 21 件 | server 21 件同集（非 MSVC 剔 2 件由 Make 承担） |
| MF_CONFIG | `-DMF_CONFIG="$(BW_CONFIG)"` 字符串 | 硬编码 `-DMF_CONFIG="Hybrid"`（注释声明有意对齐根 server 构建） |
| cellapp_extension | examples 条件构建（user_shouldBuildExamples） | 仅非 MSVC 进入 BW_BINARY_PROJECTS |
| 覆盖面 | 仅 Linux server 面（lib/server/tools/unit_tests） | client/tools/exporters/navgen/assetprocessor/bwentity/extras 全家 + server，Windows 为主 |

## 待验登记（静态证据边界）

1. 本机 Linux，未执行任何 Windows configure/build；全部目标数、宏、条件语义来自静态读取 + 原版注释 run-ID 引证，动态验证留给 Windows 环境（MSVC 生成器表、blob /bigobj、exporter 插件链）。
2. Linux 侧 CMake 远程构建链（plink 校验、BW_VERIFY_LINUX、`x64-linux-bw` triplet）未实际运行；本仓 Linux 交付面以 Make 链为准（BW-00-01-01），CMake Linux 链仅登记待验。
3. `tools` 目标把 `cstdmf` 挂进 BW_BINARY_PROJECTS 的实际产物形态（复用 lib 工程内的可执行目标）未动态确认，静态按声明登记。

## 结论

验收达成：清册完整记录平台矩阵（win32/win64/远程 Linux、vc9-vc17+clang-cl 生成器、Debug/Hybrid 配置与 `_DEBUG/_HYBRID` 宏）与全部 11 个目标的启用宏（MF_SERVER/BW_CLIENT/EDITOR_ENABLED/BW_EXPORTER/_NAVGEN/PYMODULE/BW_BLOB_CONFIG 等），库/二进制/单测清单按目标列全，vcpkg 与 vendored 第三方边界划清，与 Make 链差异逐项登记。
