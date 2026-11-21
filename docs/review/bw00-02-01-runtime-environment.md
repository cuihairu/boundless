# BW-00-02-01 采集原版运行环境 — 记录

- **任务编号**: BW-00-02-01（lane BW-00-02 原版运行基准，todo.md 行 100）
- **参考树**: `/home/cui/workspaces/BigWorld`（读采集，未改动）；构建根 `programming/bigworld/build/`。
- **Boundless 模块**: 无（原版运行环境基准，供 BW-00-02 差分验收与 BW-00-05 覆盖台账引用）。
- **采集方式**: 只读命令（git describe/rev-parse、sha256sum、CMakeCache 读取、头文件 grep）。

## 提交锚点

- **HEAD**: `b119c389`（docs: register vscode extension plan (VSX-1..4) in todo ledger）
- **git describe**: `bigworld-14.4.1-320-gb119c389`（基线 tag `bigworld-14.4.1` 后 320 提交）
- **基线 tag**: `bigworld-14.4.1`

## 二进制哈希

参考树当前构建产物共 **389 个**（静态库 `.a`、共享库 `.so`、已构建可执行），分布在 6 个构建目录：

| 构建目录 | 产物数 | 内容 |
|---|---|---|
| `build/cmake-entitydef` | 173 | entitydef 单测 + 库 |
| `build/el7` | 137 | Make 构建树（lib/*.a + third_party Python wheels） |
| `build/cmake-python` | 21 | bwpython313 构建 + python3.13 |
| `build/cmake-network` | 20 | network 单测 + 库 |
| `build/cmake-resmgr` | 19 | resmgr 单测 + 库 |
| `build/cmake-cstdmf` | 19 | cstdmf 单测 + 库 |

**完整哈希清单**: `docs/review/data/bw00-02-01-built-artifacts.sha256`（389 行，路径相对 `programming/bigworld/build`）。运行可执行的哈希（内联）：

| 可执行 | sha256 |
|---|---|
| `cmake-cstdmf/cstdmf_test` | `34c0de67d56b9013d2fe6176860d8899da6b9ab01648d61aa2ba2a8423439ef3` |
| `cmake-resmgr/resmgr_test` | `2a52dfa15f3f9416698a2c81ab1db6297b22b8772abdc2830d35777f89f0ef01` |
| `cmake-entitydef/entitydef_test` | `f2abeaf6e81c93c7311bd06e50dcc93ab850bfa887f80f2566f00f096956c6e1` |

`build/el7/lib/` 关键库（均已入清单）：`libbwpython3.13.a`、`libbwcrypto.a`、`libbwssl.a`、`libbwcurl.a`、`libnetwork.a`、`libdb.a`、`libdb_storage_mysql.a`、`libdb_storage_xml.a`、`libjsoncpp.a`、`libpng.a`、`libentitydef.a`、`libcstdmf.a` 等。

## 构建参数

- **CMake 侧**（`build/cmake-cstdmf/CMakeCache.txt`）：`CMAKE_BUILD_TYPE=Release`；编译器 `/usr/bin/cc`、`/usr/bin/c++`。
- **Make 侧**（`build/el7` 树）：Make 构建树完整（build/dep/lib/obj/third_party）；配置变体来自 `build/bw_internal/scripts/makeServer.py`（`MF_CONFIG` 循环：Debug/Hybrid ×64 + `_SingleThreaded`/`_SystemPython` 变体）。
- **build_revision**: `build/el7/build/build_revision.hpp` 与 `.generated.hpp` 均 `#define REVISION_NUMBER -1`（**未烘焙版本号**——参考构建未注入 Git 版本）。

## Python 版本

- **3.13**：`third_party_python.mak:7` `BW_PYTHON_VERSION := 3.13`；`build/cmake-python/` 已构建 `bwpython313` 与 `python3.13`；`build/el7/lib/libbwpython3.13.a`。
- **site-packages（el7 构建的 Python 3.13 服务端模块，14 个 wheels）**：sqlalchemy-1.4.54、attrs-24.2.0、twisted-24.7.0、constantly-23.10.4、hyperlink-21.0.0、msgpack-1.1.0、pika-1.3.2、zope.interface-7.0、automat-24.8.1、idna-3.10、incremental-24.7.0、typing_extensions-4.12.2、Pympler-1.0、setuptools-75.3.0。

## DB 版本

- **MySQL 客户端**: **8.4.13**（系统 `/usr/bin/mysql_config`；参考构建用系统 MySQL 客户端，未 vendored——`third_party` 无 `mysql_version.h`）。服务端 dbapp 经 `libdb_storage_mysql.a`（el7）链接。
- **SQLite**: **3.6.23.1**（`third_party/sqlite/sqlite3.h` `#define SQLITE_VERSION "3.6.23.1"`；vendored，供 consolidate_dbs/transfer_db 等工具）。

## 边界与已知登记项

1. **服务端进程二进制未建**：参考构建止于库阶段（el7/lib 全部 `.a` 已产出），`baseapp`/`cellapp`/`loginapp`/`bwmachined` 等可执行未在构建树（与 01-01/01-02 一致：Make `everything`/`rpm` 目标在当前树无定义，仅库面可建）。故二进制哈希覆盖库与单测可执行，**服务端进程可执行哈希留待原版全量构建后补采**（BW-00-02-02 复跑单测不依赖进程二进制）。
2. **`build_revision` 为 -1**：参考构建未注入版本，哈希基线以 git 提交锚点为准。
3. **MySQL 8.4 为系统依赖**：非参考树自带；若差分验收需复现，须固定 `mysql_config` 8.4.13。

## 状态

原版运行环境记录完成：提交锚点（b119c389 = bigworld-14.4.1-320-gb119c389）、389 个构建产物哈希（清单 + 3 个单测可执行内联）、构建参数（CMake Release + /usr/bin/{cc,c++}；Make el7 树 + MF_CONFIG 变体；build_revision=-1）、Python 3.13（14 wheels）、DB（MySQL client 8.4.13 系统 + SQLite 3.6.23.1 vendored）。
