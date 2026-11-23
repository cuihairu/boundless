# BW-00-02-05 Windows 客户端和工具基线复测

基线 `4657a17`，参考树 `programming/bigworld/`（CMake 配置 + 产物树 `game/bin/server/el7/`）。对应 todo.md BW-00-02-05：**逐目标保存结果或缺口**。

## 状态：已完成（记录为缺口 + Linux 侧可达基线）

本机（x86_64 Linux，Ubuntu GLIBC 2.43，cmake 4.x + ninja 在场，`cl.exe` 缺席）**无法构建或运行任何 Windows 目标**——CMake 配置在平台门禁处即 FATAL_ERROR。所有 11 个 `BW_CMAKE_TARGET` 记为 **GAP**，缺口原因已逐层定位；Linux 侧对应工具已实跑采集（下文）。

## Windows 目标矩阵（11 个 BW_CMAKE_TARGET 全 GAP）

平台门禁链（`programming/bigworld/CMakeLists.txt`，本机实跑配置，命令 `cmake -G Ninja -DCMAKE_POLICY_VERSION_MINIMUM=3.5 -DBW_CMAKE_TARGET=client <bigworld>`，日志 `data/bw00-02-05-logs/cmake_linux_configure.txt`）：

| 行 | 门禁 | 本机结果 |
|---|---|---|
| `:7` | `CMAKE_MINIMUM_REQUIRED(VERSION 2.8.12)` | 现代 cmake（≥3.5）直接拒绝，需 `-DCMAKE_POLICY_VERSION_MINIMUM=3.5` 才能继续 |
| `:17` | `IF(NOT BW_CMAKE_TARGET)` FATAL_ERROR | 必须经 `bigworld_cmake.bat` 或 `-DBW_CMAKE_TARGET=` 传入；未传即 FATAL |
| `:47-49` | `IF(NOT CMAKE_CXX_FLAGS)` → FATAL_ERROR "CMake failed to find a valid compiler!" | **真正的 Windows-only 门禁**：MSVC 会填充 `CMAKE_CXX_FLAGS`（`/DWIN32 ...`），GNU/Clang 在 Linux 上留空 → 守卫命中 → FATAL |

- 门禁措辞并非显式 "Only Windows"，而是 `CMAKE_CXX_FLAGS` 非空守卫——MSVC 必然填充、Linux 编译器必然留空，故 Linux 上配置 100% FATAL。GNU 15.2 编译器检测本身成功（`-- The CXX compiler identification is GNU 15.2.0`），随后即被该守卫拦截。
- **仓内无 Windows 产物**：`game/bin/` 仅含 `server/el7/`（Linux）；全树 `.exe` 仅 third_party（putty/plink/pageant、cmake-win32-x86、openssl-vs2015）与 `build/cmake-entitydef/bwpython313/python.exe`，**均非 BigWorld 产品**。
- 结论：11 目标（assetprocessor、bwentity、client、client_blob、exporters、exporters_blob、extras、navgen、server、tools、tools_blob）全部 **GAP**，缺口 = 平台门禁 + 无 Windows 工具链 + 无 Windows 产物树。复测只能在 Windows + MSVC 环境进行。

## Linux 侧工具基线（实跑采集）

命令环境：`env BW_RES_PATH="<fantasydemo/res>:<game/res/bigworld>"`。日志入 `data/bw00-02-05-logs/`。

| 工具 | 场景 | 结果 | 日志 |
|---|---|---|---|
| `res_packer` | 无 res 路径（`--help`） | exit 1：`BWResource::BWResource: No res path set.`（资源系统无法初始化） | `res_packer_no_res.txt` |
| `res_packer` | 无参（BW_RES_PATH 置） | exit 1：完整用法双模式（兼容单文件 / `--list` 批量） | `res_packer_usage.txt` |
| `res_packer` | 单文件字符串表示（`crab.bmp`） | **exit 0**：`ImageFile: <crab.bmp>`（bmp 走 ImageFile 资产类型） | `res_packer_repr.txt` |
| `res_packer` | 打包写文件（`crab_norms.bmp` → `/tmp/.../out.res`） | 报 `Processing ... ...succeeded`，exit 0；但目标文件未在请求路径出现（写盘位置未落观察，注记） | `res_packer_pack.txt` |
| `process_defs` | `--use-stdout`（BW_RES_PATH 置） | exit 1：`ModuleNotFoundError: No module named 'ProcessDefs'`（personality 模块未入树） | `process_defs.txt` |
| `process_defs` | `--help`（无 res） | exit 1：`No res path set.` → `ModuleNotFoundError: No module named 'bw_site'` → `Failed to initialise Python`（脚本面缺件） | `process_defs.txt` |
| `message_logger` | 裸跑 / `--help`，5s | **无任何输出**，exit 1（守护式静默；未在 5s 内落日志文件） | `message_logger.txt` |
| `simple_python_client` | 登录路径 | 已由 BW-00-02-04 覆盖（`el7/examples/`，Linux 最小客户端示例） | — |

- host 快照：`data/bw00-02-05-logs/host.txt`（x86_64、GLIBC 2.43、cmake/ninja 在场、`cl.exe` 缺席、`python2` 缺席）。

## 结论与缺口

- 11 个 Windows 目标全 GAP，缺口已逐层定位（`CMAKE_CXX_FLAGS` 守卫 + 无 Windows 工具链/产物树），仅 Windows + MSVC 环境可复测。
- Linux 侧 `res_packer` 可达绿（ImageFile 表示 + 打包 succeeded）；`process_defs` 缺 scripting personality 模块（`ProcessDefs`/`bw_site`，未入仓）不可达；`message_logger` 守护式静默，无输出基线。
- 次要观察：res_packer 打包写文件模式报 succeeded 但产物未出现在请求路径，写盘位置与 base_path 语义待核（非阻塞，记录为注记）。
