# BW-15-02-05 验证旧 EL7 互通 — 基线配置与结果记录

- **任务编号**: BW-15-02-05（lane BW-15 Linux/Windows/macOS 原生支持，todo.md 行 1227）
- **验收**: 记录该基线配置和结果。
- **参考树**: `/home/cui/workspaces/BigWorld`（读采集，未改动）。
- **采集方式**: 只读命令（`ls`、`ldd`、`process_defs --help/-r`、`git log`）。
- **结论**: EL7 基线配置与结果**已在本机记录并实测闭环**——EL7 分发二进制在现代 Linux
  上可执行；完整 defs dump 已复现（2026-11-25 补录，见 §3.5/3.6：EL7 `process_defs`
  与本仓 `process_defs` 对同一夹具输出**逐结构相等**，digest
  `18D00DC69AC756433D091F537DA7030E`）；EL7 `res_packer` 产物经本仓
  `PackedSection` 回读与源 XML **逐树相等**（§3.7，永久测试钉住）；
  EL7 服务端进程面基线由 `bw00-02-03-cluster-baseline.md` 的 deep-chain
  十进程稳定态记录承载（§3.8）。剩余唯一未采集面=新服务端登录链路 ↔ EL7 工具
  互通（BW-06/07/09 落地后复测，见 §4）。

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
3. **解析面**：该 res 根（`game/res/bigworld`）无 `.def` 实体定义，故 `pSection is NULL` / `Failed to parse .def files`（rc=1）——**预期**：实体定义根与 `bw_site` 站点根需为同一 res 根。带 `.def` 的 `programming/fantasydemo/res` 无 `bw_site.py`，二者未合成，故**完整 defs dump 未在本机复现**。（2026-11-25 此项已闭环，见 3.5。）
4. **server/baseapp**：EL7 二进制在现代 Linux 上动态解析通过（见 2.3），具备启动的链接前提。
5. **完整 defs dump（2026-11-25 复现闭环）**：res 根合成不再需要拷贝——`BW_RES_PATH`
   冒号分隔双根（夹具根 `docs/review/fixtures/fantasydemo_def/res` +
   `game/res/bigworld`）即可，引擎把两根的 `scripts/common` 都挂进 `sys.path`
   （`bw_site.py` 自述），夹具根补一个 personality 转储模块即完成。EL7
   `process_defs` 经 `Personality::import` 找到的默认模块名 `ProcessDefs`
   （`main.cpp:537`），回调 `ProcessDefs.process(description)` 收到
   `{"entityTypes": [...], "constants": {...}}` 纯 dict/tuple/str/int 结构
   （`main.cpp:486-523`）。转储脚本 `scripts/el7-interop/run-el7-process-defs.sh`
   （personality 内联，结果落 JSON）。实测 5 实体全解析：

   | index | 实体 | C/B/S | 属性 | client/base/cell 方法 | persistent |
   |---|---|---|---|---|---|
   | 0 | Account | 1/1/0 | 2 | 1/0/0 | 真 |
   | 1 | Avatar | 1/1/1 | 4 | 2/1/0 | 真 |
   | 2 | Guard | 1/1/1 | 1 | 0/0/0 | 真 |
   | 3 | Space | 1/1/1 | 0 | 0/0/0 | 真 |
   | 4 | ClientOnly | 1/0/0 | 2 | 1/0/0 | 假 |

   constants：`digest=18D00DC69AC756433D091F537DA7030E`、
   `maxExposedClientMethodCount=2`、`maxExposedBaseMethodCount=1`、
   `maxExposedCellMethodCount=0`、`maxClientServerPropertyCount=4`。
6. **新旧工具逐位对照（2026-11-25）**：本仓 `process_defs`
   （`cargo run -p entitydef --bin process_defs -- -r <夹具根> -r <bigworld 根> -m ProcessDefs`）
   对同一夹具输出的 JSON 与 EL7 转储**深比较逐结构相等**——每实体的
   `allProperties`/三面方法/五组 properties、四项 constants、digest 全等。
   这是 defs 工具族新↔旧互通的直接记录。
   **参照二进制收尾怪癖**：personality 返回后 Python 终结化阶段 abort
   （`PyOutputStream` 无 `flush` + `_Py_GetConfig` GIL 错误，rc=134）；空
   personality（仅 `return True`）同样 abort——**参照二进制自身怪癖**，与
   回调内容无关；故回调须自行落盘结果（转储脚本即如此）。
7. **res_packer 产物互通（2026-11-25）**：EL7 `res_packer -r . bw.xml bw.packed`
   的产物（177 字节）magic `45 4E A1 62` = 本仓 `PACKED_SECTION_MAGIC`
   `0x62a14e45` 小端逐字节相同；本仓 `PackedSection::parse` 回读该产物与源
   XML 逐树相等（节点名/子数/叶值全等）。产物以参考工具夹具入库
   （`docs/review/fixtures/el7_res_packer/bw.packed`，同 waypoint navmesh bin
   先例），永久测试
   `an_el7_res_packer_file_reads_back_identical_to_its_xml_source`
   （`crates/resources/src/packed.rs`）钉住；复现脚本
   `scripts/el7-interop/run-el7-res-packer.sh`。反方向（本仓 pack → EL7 读）
   无 EL7 读取器可证（`res_packer` 只写不读），如实记录不声称。
8. **EL7 服务端进程面**：完整 EL7 服务端集合（MariaDB → bwmachined2 → 三 mgr →
   dbapp → cellapp → baseapp×2 → loginapp → serviceapp 十进程）在参考仓
   `scripts/cluster-smoke/deep-chain.sh` 拉起到稳定态、8 角色
   `---- X is running ----` 的基线记录见 `bw00-02-03-cluster-baseline.md`
   （含逐进程 PID/端口快照）——本项基线结果记录以该档为准。

## 4. 卡点（2026-11-25 复核后仅余一条）

- **新服务端 ↔ EL7 登录链路互通未采集**：需要新服务端 loginapp/baseapp 登录链路
  （BW-06-01-02 / BW-07-01 / BW-09-01-04/05）先落地；当前 `apps/server` 为最小入口，
  无登录处理。工具与产物面的互通（3.6/3.7）不受此影响，已采集。
- ~~完整 defs dump~~（已闭环，见 3.5/3.6）；~~EL7 独立运行面~~（deep-chain 十进程
  基线已记录，见 3.8）。

## 5. 边界

本项只**记录** EL7 基线配置与可验证结果，不产出新功能；新服务端的 EL7 互通验收随
BW-06/07/09 落地后复测。res_packer 反方向互通（本仓 pack → EL7 读）无 EL7 读取器
可证，不作声称（见 3.7）。
