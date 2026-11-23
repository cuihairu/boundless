# BW-00-02-01 原版运行环境采集

基线 `4657a17cf10d55e2fbebf258f96da96a3dbe0c22`（2026-11-12 17:41:26 +0800，"docs: note doc-gif scope and blockers"），参考树 `../BigWorld/`。对应 todo.md BW-00-02-01：**记录提交、二进制哈希、构建参数、Python 和 DB 版本**。

## 采集方法（全动态实查，2026-11-21）

`git rev-parse` 定版本；`sha256sum` 全量哈希（manifest 见下）；`file`/`readelf -p .comment` 定构建形态与编译器；`ldd` 定动态闭包；嵌入解释器以自带 `python3.13.exe` 自报 + `strings` 交叉核对；DB/SSL 版本以解释器模块自报（`sqlite3.sqlite_version`/`ssl.OPENSSL_VERSION`）。

## 提交与产物清单

- 提交：`4657a17cf10d55e2fbebf258f96da96a3dbe0c22`（上述日期/主题）。
- 完整 SHA256 manifest：`docs/review/data/bw00-02-01-el7-manifest.sha256`（本仓内，逐文件全量哈希 47 件：server 10 进程 + cellapp/dbapp extensions 3 件 + commands 6 件（含 `_helpers/snapshot_helper`）+ tools 5 件（含 `_bwlog.so`，**仅 tools 有、server/ 无**）+ examples 1 件 + `libpython3.13.a` + unit_tests 21 件）。
- 代表性哈希（前 16 位，全量见 manifest）：`baseapp 067335432b211e8d`、`cellapp 06ac88f023f24cc5`、`dbapp 40146e961b2bd563`、`loginapp 147cf9c77f143522`、`bwmachined2 6499c0a4d0d118d6`、`process_defs 3da7255c8f06673b`。
- 产物形态：ELF x86-64 PIE，**带 debug_info 未 strip**；构建时间戳 2026-09-29～10-18（与迁移工程时间线一致）。

## 构建参数

| 项 | 值 | 证据 |
|---|---|---|
| 构建链 | GNU Make 4.4.1 + `bigworld/build/make/`（BW-00-01-01 清册） | `make --version`；清册 |
| 编译器 | GCC (Ubuntu 15.2.0-16ubuntu1) 15.2.0 | `readelf -p .comment baseapp` → `GCC: (Ubuntu 15.2.0-16ubuntu1) 15.2.0`；与嵌入解释器自报 `[GCC 15.2.0]` 一致 |
| 配置 | `BW_CONFIG` 缺省 = `hybrid`（`BW_CONFIG_DEFAULT := $(BW_CONFIG_HYBRID)`，Makefile:39） | make 链 |
| 第三方接线 | vcpkg manifest（baseline `10541e3`）+ 自定义 triplet `x64-linux-bw`（静态、仅 release） | vcpkg.json + BW-00-01-04 |
| 动态闭包 | server 进程仅依赖 libc 族：`libstdc++.so.6/libm.so.6/libgcc_s.so.1/libc.so.6` + ld-linux——**第三方（含 OpenSSL/curl/mysql 客户端/CPython）全部静态链接**（`ldd dbapp` 实查） | ldd |
| 运行目录约定 | `BW_RES_PATH`/`~/.bwmachined.conf` 未设时 baseapp 报 `No res path set`（错误口径留存） | 实跑 |

## Python 版本

| 面 | 版本 | 证据 |
|---|---|---|
| 安装的宿主解释器（`third_party/python/bin/python3.13.exe`，Linux ELF） | **Python 3.13.15**（main, Sep 28 2026, GCC 15.2.0） | `--version`/`sys.version` 自报 |
| 链接进 server 的静态库（`libpython3.13.a`，69,054,000 B） | **3.13.15**（strings） | `strings libpython3.13.a` |
| baseapp 内嵌字符串 | 另见 `3.13.16` 字样 | `strings baseapp` |
| 携带 stdlib | `third_party/python/lib/python3.13/`（含 LICENSE.txt） | 目录实查 |
| ssl / sqlite3 | OpenSSL **3.6.1**（27 Jan 2026）/ SQLite **3.46.1** | 解释器模块自报 |

版本差登记：vcpkg-migration 文档称 vendored 树为 3.13.16，实链产物与宿主 exe 均自报 **3.13.15**，baseapp 内另有 `3.13.16` 字符串——以二进制自报为准（3.13.15），差异登记不强行归一。

## DB 版本

| 项 | 值 | 证据 |
|---|---|---|
| 系统客户端 | mysql Ver 15.2 Distrib **11.8.6-MariaDB**（debian-linux-gnu, EditLine） | `mysql --version` |
| dbapp 链接形态 | mysql 客户端**静态**入包（ldd 无任何 mysql/mariadb 动态依赖；dbapp 内含 `BW::DBConfig::MySQLConfigBlock`、`mysql>` 字符串） | ldd + strings |
| XML 后备 | `bwengine_xml.so`（dbapp-extensions）随树交付 | 目录实查 |
| SQLite | vcpkg 3.53.4（迁移决策）+ 解释器内 3.46.1（见上，两口径分开登记） | vcpkg-migration §1 / 模块自报 |

## 差距登记

1. sqlite 双口径（vcpkg 3.53.4 vs python 内嵌 3.46.1）并存是原版树实态：引擎侧走 vcpkg、解释器扩展走自带构建，如实记录。
2. `python3.13.exe` 命名（Linux ELF 带 .exe 后缀）为 make 链 install 规则原样输出，非笔误。
3. 单测二进制 21 件与 make 清册一致；`unit_tests/` 另含历史 assert log（哈希重复、非产物），manifest 已排除。

## 结论

验收达成：提交（4657a17）、全量二进制哈希（manifest 落仓）、构建参数（GCC 15.2.0/hybrid/静态三方/仅 libc 族动态）、Python（3.13.15 实链 + 3.13.16 字样差登记 + OpenSSL 3.6.1/SQLite 3.46.1）、DB（MariaDB 11.8.6 客户端、静态入包、XML 后备）逐项记录在案，证据可复跑。
