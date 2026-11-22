# BW-00-02-05 Windows 客户端和工具基线（复测）

> 证据面（BW-00-02-05 验收：逐目标保存结果或缺口）。

由 `tools/windows_baseline_scan.py` 对参考仓逐目标探测产物与宿主工具链，结果 `reference/bw00_02_05_windows_baseline.json`；目标清单取自 BW-00-01-02 的 CMake 目标清单（`reference/bw00_01_cmake_targets.json`）。

| 项 | 值 |
| --- | --- |
| 参考仓提交 | `f3f53898` |
| 探测时间 | `2026-11-23 01:54:54 +0800` |
| 目标数 | 11（缺口 11 个） |
| 产物扫描 | exe=40 / dll=40 / pdb=40，但**全部为 vendored 第三方 Windows 工具**（`third_party/cmake-win32-x86`、`openssl-vs2010..2015`、`openautomate`，共 40 个，且不含任何目标名） |
| 游戏二进制目录 | `game/bin/server`=是（el7 服务端），`game/bin/win32`=否，`game/bin/win64`=否 |
| 宿主 | wine=True / wine64=True，cl.exe=False / msbuild=False / powershell=False（均无） |
| Windows CI | `msvc-compile/root-configure-probe/root-build-probe`，runner `${{ vars.CI_RUNNER || 'windows-latest' }}`，构建目录 `build/win_root_build` |
| 打包脚本 | `package-daily-windows.ps1`（`-BuildDir build/win_root_build`，平铺 `bigworld-windows-server-<ver>.zip`） |

## 逐目标

| 目标 | 侧 | Linux 工程数 | 产物 | 结果 | 缺口原因 |
| --- | --- | --- | --- | --- | --- |
| `assetprocessor` | 工具 | 26 | — | gap | 参考树无产物（`game/bin/server` 仅 Linux 服务端；`game/bin/win32|win64` 不存在），本机无 MSVC/msbuild，有 wine 也无二进制可运行 |
| `bwentity` | 工具 | 17 | — | gap | 参考树无产物（`game/bin/server` 仅 Linux 服务端；`game/bin/win32|win64` 不存在），本机无 MSVC/msbuild，有 wine 也无二进制可运行 |
| `client` | 客户端 | 58 | — | gap | 参考树无产物（`game/bin/server` 仅 Linux 服务端；`game/bin/win32|win64` 不存在），本机无 MSVC/msbuild，有 wine 也无二进制可运行 |
| `client_blob` | 客户端 | 0 | — | gap | 参考树无产物（`game/bin/server` 仅 Linux 服务端；`game/bin/win32|win64` 不存在），本机无 MSVC/msbuild，有 wine 也无二进制可运行 |
| `exporters` | 工具 | 33 | — | gap | 参考树无产物（`game/bin/server` 仅 Linux 服务端；`game/bin/win32|win64` 不存在），本机无 MSVC/msbuild，有 wine 也无二进制可运行 |
| `exporters_blob` | 工具 | 0 | — | gap | 参考树无产物（`game/bin/server` 仅 Linux 服务端；`game/bin/win32|win64` 不存在），本机无 MSVC/msbuild，有 wine 也无二进制可运行 |
| `extras` | 工具 | 31 | — | gap | 参考树无产物（`game/bin/server` 仅 Linux 服务端；`game/bin/win32|win64` 不存在），本机无 MSVC/msbuild，有 wine 也无二进制可运行 |
| `navgen` | 工具 | 30 | — | gap | 参考树无产物（`game/bin/server` 仅 Linux 服务端；`game/bin/win32|win64` 不存在），本机无 MSVC/msbuild，有 wine 也无二进制可运行 |
| `server` | 服务端 | 77 | — | gap | 参考树无产物（`game/bin/server` 仅 Linux 服务端；`game/bin/win32|win64` 不存在），本机无 MSVC/msbuild，有 wine 也无二进制可运行 |
| `tools` | 工具 | 70 | — | gap | 参考树无产物（`game/bin/server` 仅 Linux 服务端；`game/bin/win32|win64` 不存在），本机无 MSVC/msbuild，有 wine 也无二进制可运行 |
| `tools_blob` | 工具 | 0 | — | gap | 参考树无产物（`game/bin/server` 仅 Linux 服务端；`game/bin/win32|win64` 不存在），本机无 MSVC/msbuild，有 wine 也无二进制可运行 |

## Windows 侧可达路径

参考仓把 Windows 基线建在 GitHub Actions 上（`ci-windows.yml`）：

- `msvc-compile` — runner `${{ vars.CI_RUNNER || 'windows-latest' }}`
- `root-configure-probe` — runner `${{ vars.CI_RUNNER || 'windows-latest' }}`
- `root-build-probe` — runner `${{ vars.CI_RUNNER || 'windows-latest' }}`

构建配置：`-DBW_PLATFORM_WINDOWS=ON -DBW_PLATFORM=win64 -DBW_ARCH_64=ON`；`msvc-compile` 按库矩阵 `--target ${{ matrix.library }}` （`lib/<library>` 起点，另跑 `cstdmf_test`），`root-build-probe` 构建根超工程 Hybrid 全目标（`yes`）（`-B build/win_root_build`）。

## 说明

- **缺口性质**：本 Linux 主机既无 MSVC 工具链，也无任何 Windows 目标产物，`game/bin/server` 的 el7 服务端二进制与 Windows 客户端/工具基线无关，因此 11 个目标一律记为缺口——这是宿主条件限制，不是构建失败。
- **wine 不构成结果**：wine 二进制已装（True / True），但树内没有可运行的 Windows 目标产物（40 个 exe 全部是 vendored 第三方工具，且与目标名不匹配）。
- **复测路径**：在 Windows 主机或 `ci-windows.yml` 的 Windows runner 上配置根超工程（Hybrid 全目标）并打包，产物即 `bigworld-windows-server-<ver>.zip`。

生成：`windows_baseline_scan.py` + `windows_baseline_doc.py`。
