# BW-00-01-03 发行脚本与 RPM 目标清册

基线 `4657a17cf10d55e2fbebf258f96da96a3dbe0c22`，路径相对 `../BigWorld/`。对应 todo.md BW-00-01-03：**登记发行脚本与 RPM 目标，标出独有交付物及版本**。

## 采集方法

1. **RPM 元数据实查**：`rpm -qpi/-qpl/-qpR` 逐字段读取仓库根 3 个原版 RPM（14.4.1.el7.x86_64），非转述。
2. **发行脚本全量读**：`scripts/`（`package-daily.sh` 103 行、`package-daily-windows.ps1` 142 行、`build-windows-server.ps1` 116 行、5 个 `build-*-cmake.sh` 探针、`check-docs-links.py`、`python-modules-smoke-import.py`）。
3. **workflow 面读**：参考仓 `.github/workflows/`（daily-build / ci-windows / ci / deploy-docs）+ 本仓 `.github/workflows/`（交叉引用）。
4. **Make 链核对**：`grep -rn "rpm|RPM" programming/bigworld/build/make/` 确认链内无 RPM 规则（仅 2 处注释）。

## 表 1：原版 RPM 发行物（仅发行物，构建不可复现）

仓库根 3 包，均 `14.4.1-1.r0.el7`、x86_64、License: BigWorld License、Vendor: BigWorld Pty Ltd、Packager: BigWorld Support、Build Date 2022-11-26（localhost）；`fixwork/` 下各有一份 md5 完全相同的副本。

| 包 | 大小 | 独有交付物 | Requires 摘要 |
|---|---|---|---|
| `bigworld-bwmachined` | 26,166,574 B | 12 个文件：`/opt/bigworld/14.4/bwmachined/sbin/bwmachined2`（守护进程二进制）+ `init.d/bwmachined2`（init 脚本）+ `/etc/bwmachined.conf` + `/etc/sysctl.d/bwmachined2.conf` + `/usr/lib/systemd/system/bwmachined2.service` + `/opt/bigworld/current` 符号链接树 | `/bin/sh` |
| `bigworld-devel` | 0 B（空 meta 包） | 无文件 | 21 项：gcc、gcc-c++、gdb、make、python-devel、mariadb-devel、subversion、SDL-devel、MySQL-python、sqlite-devel、readline-devel、gdbm-devel、bzip2(+devel)、ncurses-devel、zlib-devel、strace、binutils-devel、patch、unzip、bc + rpmlib×3 |
| `bigworld-web-integration` | 0 B（空 meta 包） | 无文件 | epel-release、httpd、curl、php >= 5.4、php-gd >= 5.4 + rpmlib×3 |

- Source RPM 名声明为 `bigworld-*-14.4.1-1.r0.el7.src.rpm`，但**树内无 `.src.rpm`、无 `.spec`** → 属仅发行物，构建不可复现。
- **独有交付物口径**：原版 RPM 面**只打包 bwmachined2 守护进程**（服务化：init.d + systemd unit + sysctl + 配置）+ 两个空 meta 包（devel 开发依赖族 / web-integration PHP WebIntegration 依赖族）。**服务器进程本身不在任何 RPM 内**——它们经源码树 `make` 直接交付（与 BW-00-01-01 的 `game/bin/server/el7/` 布局一致）。
- 包名/布局与现行 daily RPM 完全不同：原版分包（bwmachined2 单独包、安装带版本目录 `/opt/bigworld/14.4/` + `current` 链接）；现行单包平铺 `/opt/bigworld`。

## 表 2：现行发行脚本（参考仓 `scripts/`）

| 脚本 | 角色 | 关键口径 |
|---|---|---|
| `package-daily.sh` | 每日三格式打包（tar.gz + deb + rpm） | 输入 = `make` 全量产物 `game/bin/server/el7/{examples,server,tools,unit_tests,third_party}` + `game/res`（已核实布局一致）；版本源 `vcpkg.json "version"`（=14.4.1）；`DEB_VER=<ver>+daily<UTC>.<sha>`，`RPM_REL=1.daily<UTC>.<sha>`；deb control `Package: bigworld` `Depends: libc6>=2.17, libstdc++6, libmysqlclient21|libmariadb3`；rpm spec 动态生成（`Name: bigworld`、`AutoReqProv: no`、`%files /opt/bigworld`），`rpmbuild -bb` 临时 topdir；产物名 `bigworld-<ver>-linux-x86_64.tar.gz`；写 `pkg-version.txt` 供 workflow 命名；删 `*.log` 清理断言转储；**无 tag、无 release**（仓规，工件 retention 14d） |
| `package-daily-windows.ps1` | Windows Hybrid 包 → zip + SHA256 manifest | 11 个 Hybrid exe（server 进程集）+ `python313.dll`/PCbuild pyd/`_bwlog.dll`/`bwengine_xml.dll`/`_bw_profile.dll` + `Lib/` stdlib（2338 文件，仓内拷贝保证自包含）；**平铺布局**（interpreter 从 python313.dll 目录解析 home，不拆分）；核心库为静态，无 cstdmf.dll/network.dll 入包（manifest 如实登记缺失）；产物 `bigworld-windows-server-<ver>.zip`；版本口径与 sh 完全一致 |
| `build-windows-server.ps1` | Windows server 构建（逐目标 msbuild） | `Targets` 表 11 件：cellapp cellappmgr baseapp baseappmgr dbapp dbappmgr loginapp reviver bots transfer_db message_logger；`-DVCPKG_TARGET_TRIPLET=x64-windows-static-md`、`-G "Visual Studio 18 2026" -A x64 -DBW_CMAKE_TARGET=server`；`Config=Hybrid`（根 CMakeLists 只设 Debug;Hybrid，无 Release，MSB8013 教训 run 38745028827）；前置 `$env:DXSDK_DIR`；逐目标单跑避免一次失败掩盖 10 个（run 39013952226）；被 daily-build（交付）与 ci-windows（root-build-probe 证据）双复用 |
| `build-{cstdmf,entitydef,network,python,resmgr}-cmake.sh` | 单库 CMake 构建探针 | 证据用，非发行物 |
| `check-docs-links.py` / `python-modules-smoke-import.py` | 文档链接校验 / Python 模块冒烟 | 验证辅助 |

## 表 3：workflow 面（发行调度）

**参考仓** `.github/workflows/`：`daily-build.yml`（cron `23 21 * * *` 每日 + dispatch 无条件；48h freshness 门（`git rev-list --count --since='48 hours'`），无新提交则跳过；linux job → `package-daily.sh`，windows job → `build-windows-server.ps1` + `package-daily-windows.ps1`；artifacts retention 14d；明示"无 tag、无 release"）；`ci-windows.yml`（root-build-probe 复用同一 ps1，双车道交付/探针同物）；ci.yml、deploy-docs.yml。

**本仓**（boundless2）`.github/workflows/`：`package-daily.yml`（name: Package daily，BW-22-04-01，经 `ci/package.sh` 三平台 leg、Linux 编译先行 Windows/macOS 待 BW-15-03/04，仅 CI 不 gate、仅工件）——**BW-22 席位管辖，此处置交叉引用，不勾其账**；ci.yml、fault-scenarios.yml、load-schedule.yml、report.yml 其余四条。

## 差异与缺口登记

1. 原版 RPM 与现行 daily RPM 包名（bigworld-bwmachined vs bigworld）、布局（`/opt/bigworld/14.4` + current 链接 vs 平铺）、内容（仅 bwmachined2 vs 全 server bin+res）均不同——两者是不同发行体系，不互相派生。
2. make 链内**无 RPM 规则**（`build/make/Makefile:5` 注释提及 "build everything - even non-source stuff (eg: rpms)"，但链内无实现）；RPM 构建完全由脚本层（package-daily.sh 内联 spec）承担。
3. daily 交付面与 BW-00-01-02 cmake 目标差集：Windows daily 仅 11 exe；cmake server 的 17 二进制中 bwmachined（Windows 跳过）、clear_auto_load、consolidate_dbs、snapshot_helper、sync_db、`_bw_profile`、cellapp_extension 不入 daily 包。
4. `fixwork/` 为另一工作副本（含 gate-1027*、docsbuild1027* 日志），3 个 RPM 与仓库根 md5 一致——登记为重复副本，不深究。

## 待验登记（证据边界）

1. RPM 元数据为动态实查（本机有 rpm 工具）；`package-daily.sh` 的 rpmbuild/dpkg-deb 产物**未本机重跑**（脚本静态读 + workflow 注释口径 + 输入布局实地核实）。
2. `package-daily-windows.ps1` 的"11 Hybrid exe"断言来自脚本注释与 `build-windows-server.ps1` Targets 表交叉核对；Windows 面本机未跑。
3. 本仓 `package-daily.yml` 属 BW-22 席位产物，本清册仅交叉引用，其内容以该席位 todo 账为准。

## 结论

验收达成：独有交付物（仅 bwmachined2 守护进程 + 服务化文件 + 2 空 meta 包，14.4.1-1.r0.el7，仅发行物）与版本（14.4.1，vcpkg.json 同源）已标出；现行三格式日更打包脚本（sh + ps1 + 逐目标构建）与 workflow 调度（48h 门、artifacts-only、无 tag/无 release 仓规）全量登记；原版 RPM 与 daily RPM 两套体系差异、make 链无 RPM 实现、daily 交付面 11 exe 与 cmake 17 二进制的差集逐项登记。
