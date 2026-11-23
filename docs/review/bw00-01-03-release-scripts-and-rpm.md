# BW-00-01-03 登记发行脚本与 RPM 目标 — 清册

- **任务编号**: BW-00-01-03（lane BW-00 构建目标清册，todo.md 行 89）
- **原版构建入口/源码**: `programming/bigworld/build/bw_internal/scripts/`（发行脚本）；`programming/bigworld/build/make/Makefile`（`everything`/`rpm` 目标注释）。
- **Boundless 模块**: 无（参考侧清册，供 BW-00-05 覆盖台账引用）。
- **平台**: 发行脚本分 Linux（makeRPM.py/makeServer.py）与 Windows（win*.bat/py、cmake_all_windows.py、makeSolutions.py）两组。
- **输入**: `build/bw_internal/scripts/` 目录；`build/make/Makefile`；参考 git tag。
- **版本锚点**: 参考仓库 tag `bigworld-14.4.1`；当前检出 `bigworld-14.4.1-320-gb119c389`（tag 后 320 提交）。服务端构建的 Python pin：`build/make/third_party_python.mak:7` `BW_PYTHON_VERSION := 3.13`（`bwpython313`/`libpython313.a`/`pythonSoabiVersion`）。

## 结果：发行脚本清册

### Linux 侧（build/bw_internal/scripts/）

| 脚本 | 用途 |
|---|---|
| `makeRPM.py` | RPM 打包：`make rpm` 于原版布局 `/bigworld/src/server`，`-i/--indie` → `make rpm BIGWORLD_INDIE=1` |
| `makeServer.py` | 服务端构建驱动：`MF_CONFIG`（Debug/Hybrid/…×64 + `_SingleThreaded`/`_SystemPython`）循环 `make`，支持 `clean` 与单测 |
| `build_common.py` | 共享工具（`chdir`/`runCmd`/`getMFRoot`），被其余脚本 import |

### Windows 侧（build/bw_internal/scripts/）

| 脚本 | 用途 |
|---|---|
| `makeSolutions.py` | 生成 VS 解决方案（硬编码 `C:\Python27\python.exe`，**Python 2.7 时代的陈旧路径**） |
| `cmake_all_windows.py` | 批量跑 Windows CMake 构建（走 build_common） |
| `winArchiveBuild.bat` | 归档 Windows 构建产物（`*.exe *.pdb *.dll *.xml *.dle *.mll *.pyd`）用 robocopy 到 `\\bbwindows07\BuildArchive\%JOB_NAME%`——Windows CI 发布归档 |
| `winMakeClient.py` / `winCMakeClient.py` | Windows 客户端构建（ctypes/win32 路径工具） |
| `winCMakeShaders.py` | Windows CMake 着色器编译 |
| `winCMakeUnitTests.py` / `winMakeUnitTests.py` | Windows 单元测试构建 |
| `perforce_synchronizer.py` / `project_manager.py` | 版本控制同步 / VS 工程管理支持 |

`testing/` 子目录另含测试脚本（供 BW-00-02-02 复跑上游单测时引用）。

## RPM 目标（登记在案缺口）

- `makeRPM.py` 调用 `make rpm`（可选 `BIGWORLD_INDIE=1`），**但当前参考树未定义 `rpm` 目标**：`build/make/Makefile:5` 只在注释里提「everything - build absolutely everything, even non-source stuff (eg: rpms)」，`everything` 与 `rpm` 两个目标在现有 Makefiles 中均无定义（全树 `grep -rn '^rpm:'` 仅命中 `third_party/curl` 自带打包）。
- 结论：参考快照的 Make 树是**裁剪版**——RPM 打包目标不在其中，仅发行脚本 `makeRPM.py` 存在；原版布局为 `/bigworld/src/server` 下 `make rpm`。此缺口登记为 01-03 的已知项，供 BW-00-01-04 / BW-00-05 追溯。

## 独有交付物与版本

- **独有交付物**：RPM 包（`make rpm` 产物，Linux）；Windows 构建归档（`winArchiveBuild.bat` 的 `*.exe/*.dll/*.pyd` 等）；VS 解决方案（`makeSolutions.py`）。
- **版本**：参考基线 `bigworld-14.4.1`（tag），检出 `bigworld-14.4.1-320-gb119c389`；`BW_PYTHON_VERSION := 3.13`（服务端构建 Python pin）；`makeSolutions.py` 的 Python 2.7 是陈旧时代残留（Windows VS 工程生成脚本未随 3.13 迁移）。

## 状态

发行脚本清册完成（Linux 3 脚本 + Windows 8 脚本 + testing 子目录）；RPM 目标登记为缺口（`rpm`/`everything` 未在当前树定义）；版本锚点记录（bigworld-14.4.1 tag + 检出 320 提交 + BW_PYTHON_VERSION=3.13）。
