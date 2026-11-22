# BW-15-02-05 验证旧 EL7 互通 — 基线配置与结果记录

- **任务编号**: BW-15-02-05（lane BW-15 Linux/Windows/macOS 原生支持，todo.md 行 1227）
- **验收**: 记录该基线配置和结果。
- **参考树**: `/home/cui/workspaces/BigWorld`（读采集，未改动）。
- **采集方式**: 只读命令（`ls`、`ldd`、`process_defs --help/-r`、`git log`）。
- **结论**: EL7 基线**配置与部分结果**已在本机记录并实测——EL7 分发二进制在现代 Linux 上可执行，`process_defs` 借助其 res 根的 `bw_site` 站点进入实体定义解析。**互通结果**（新服务端 ↔ EL7 工具）未采集：依赖新服务端登录链路（BW-06/07/09）先落地。

## 1. 版本与基线锚点

| 项 | 值 |
|---|---|
| 参考树 HEAD | `dc3b0b59`（`docs: close R3 row with recorded decisions`） |
| 基线 tag | `bigworld-14.4.1`（`bigworld-14.4.1-320-gb119c389` 为构建树采集锚点，见 `bw00-02-01-runtime-environment.md`） |
| RPM 版本 | `14.4.1.el7.x86_64`（`fixwork/bigworld-{bwmachined,devel,web-integration}-14.4.1.el7.x86_64.rpm`） |
| 目标平台 | Linux x86-64（EL7，Red Hat Enterprise Linux 7 系） |

## 2. EL7 基线配置

### 2.1 分发二进制树

`game/bin/server/el7/`（EL7 构建产物，非构建树）：

| 目录 | 内容 |
|---|---|
| `server/` | 服务端角色 ELF ×25（baseapp、baseappmgr、cellapp、cellappmgr、dbapp、dbappmgr、loginapp、reviver + 二进制扩展 cellapp-extensions/dbapp-extensions + commands 工具组） |
| `tools/` | `process_defs`、`res_packer`、`bwmachined2`、`message_logger`、`_bwlog.so`、`_bw_profile.so` |
| `unit_tests/` | 原版单测 ELF |
| `third_party/python/` | 内嵌 Python 3.13 |
| `examples/` | 客户端/扩展示例 |

### 2.2 RPM payload（bwmachined，真 payload）

`fixwork/bigworld-bwmachined-14.4.1.el7.x86_64.rpm` 的布局（已由 `bw14-04-installable-artifact-layout.md` §2 逐路径登记，此处复述）：

- `opt/bigworld/14.4/bwmachined/{sbin,init.d}`
- `etc/bwmachined.conf`（machined 主配置，`BWResource` 的 res 路径兜底）
- `etc/sysctl.d/bwmachined2.conf`
- `usr/lib/systemd/system/bwmachined2.service`

`devel` 与 `web-integration` RPM 为**占位**（Size=0 无 payload），头信息在（`bw14-03-01-web-and-rpm-inventory.md`）。

### 2.3 运行库依赖（本机 ldd 实测）

| 可执行 | 动态依赖（除 vDSO/loader 外） |
|---|---|
| `tools/process_defs` | `libstdc++.so.6`、`libm.so.6`、`libgcc_s.so.1`、`libc.so.6` |
| `server/baseapp` | `libm.so.6`、`libstdc++.so.6`、`libgcc_s.so.1`、`libc.so.6` |

两者均只依赖 glibc + libstdc++ + libm + libgcc_s——**本机现代 Linux 即可满足**，无需 EL7 专属系统库。

### 2.4 res 根与 Python 站点

`BWResource` 的 res 路径发现顺序（`--res`/`-r` → `BW_RES_PATH` → `~/.bwmachined.conf`）。`Script::init` 需要 res 根携带 `scripts/common/bw_site.py`（`bw_site` 模块），缺则 `Failed to initialise Python`。参考树中 `game/res/bigworld/scripts/common/bw_site.py` 即该站点。

## 3. 基线结果（本机实测）

1. **二进制可执行**：`process_defs --help` 在本机运行，usage 输出到 stderr、**退出码 0**（rc=0）。
2. **站点可载**：`BW_RES_PATH=game/res/bigworld ./tools/process_defs -r game/res/bigworld` 成功导入 `bw_site`（不再报 `Failed to initialise Python`），进入 `EntityDescriptionMap::parse`。
3. **解析面**：该 res 根（`game/res/bigworld`）无 `.def` 实体定义，故 `pSection is NULL` / `Failed to parse .def files`（rc=1）——**预期**：实体定义根与 `bw_site` 站点根需为同一 res 根。带 `.def` 的 `programming/fantasydemo/res` 无 `bw_site.py`，二者未合成，故**完整 defs dump 未在本机复现**。
4. **server/baseapp**：EL7 二进制在现代 Linux 上动态解析通过（见 2.3），具备启动的链接前提。

## 4. 卡点

- **互通结果（新服务端 ↔ EL7 工具）未采集**：互通需要新服务端 loginapp/baseapp 登录链路（BW-06-01-02 / BW-07-01 / BW-09-01-04/05）先落地；当前 `apps/server` 为最小入口，无登录处理。
- **完整 defs dump**：需一个同时含 `bw_site.py` 与 `.def` 实体定义的 res 根（参考侧应为同一游戏 res 树）；本机未合成该根，未复现完整 dump。
- **EL7 独立运行面**：完整 EL7 服务端进程面（baseapp 等全角色在 EL7 库上跑）未在本机复现——本机为现代 Linux，仅验证了链接前提与工具的可执行性，非完整 EL7 运行环境。

## 5. 边界

本项只**记录** EL7 基线配置与可验证结果，不产出新功能；新服务端的 EL7 互通验收随 BW-06/07/09 落地后复测。
