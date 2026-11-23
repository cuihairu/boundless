# BW-00-02-03 单机集群启动基线

> 证据面（BW-00-02-03 验收：留存拓扑、配置和日志）。

由参考仓 `cuihairu/BigWorld` 自带的 `scripts/cluster-smoke/deep-chain.sh` 将完整服务端集合拉起到稳定态，再由 `tools/cluster_baseline_scan.py` 快照；源数据 `reference/bw00_02_03_cluster_baseline.json`。

| 项 | 值 |
| --- | --- |
| 参考仓提交 | `916dc72a` |
| 捕获时间 | `2026-11-23 01:18:25 +0800` |
| 启动脚本 | `/home/cui/workspaces/BigWorld/scripts/cluster-smoke/deep-chain.sh`（`--teardown` 可停） |
| 日志目录 | `/tmp/cluster-smoke`（`${TMPDIR:-/tmp}/cluster-smoke`） |
| 服务端进程 | 10 个目标，10 个在稳定态 |

## 拓扑

启动链（脚本头注释与 `APPS` 顺序，脚本 `deep-chain.sh:6-13,38`）：

- `MariaDB` — bigworld DB on 127.0.0.1:3306
- `bwmachined2` — machine daemon, UDP 20018 (broadcast + lo + eth0)
- `dbappmgr / cellappmgr / baseappmgr` — manager set, started first
- `dbapp` — persistence, entity defs checksum
- `cellapp` — cell space, deep-chain entity
- `baseapp (+ second baseapp)` — client entities, backup partner
- `loginapp` — logon entry, TCP 20013
- `serviceapp` — BaseApp variant for services

稳定态进程表（`pid` 与监听套接字来自快照时点）：

| 进程 | pid | 稳定标记 | 监听套接字 |
| --- | --- | --- | --- |
| `bwmachined2` | 2854199 | （守护进程，无日志标记） | udp:127.0.0.1:20018, udp:192.168.5.188:20018, udp:255.255.255.255:20018 |
| `dbappmgr` | 2854667 | INFO: [Server] ---- DBAppMgr is running ---- | tcp:192.168.5.188:43083, udp:192.168.5.188:43083, udp:192.168.5.188:60778 |
| `cellappmgr` | 2854671 | INFO: [Server] ---- CellAppMgr is running ---- | tcp:0.0.0.0:38345, tcp:192.168.5.188:37761, udp:192.168.5.188:37761, udp:192.168.5.188:52644 |
| `baseappmgr` | 2854673 | INFO: [Server] ---- BaseAppMgr is running ---- | tcp:192.168.5.188:50901, udp:192.168.5.188:37660, udp:192.168.5.188:50901 |
| `dbapp` | 2855002 | INFO: [Server] ---- DBApp is running ---- | tcp:192.168.5.188:52039, tcp:192.168.5.188:57493, udp:192.168.5.188:38457, udp:192.168.5.188:57493 |
| `cellapp` | 2855345 | INFO: [Server] ---- CellApp is running ---- | tcp:0.0.0.0:39023, tcp:192.168.5.188:46015, tcp:192.168.5.188:50001, udp:192.168.5.188:46015, udp:192.168.5.188:57152 |
| `baseapp` | 2855879 | INFO: [Server] ---- BaseApp is running ---- | tcp:192.168.5.188:40001, tcp:192.168.5.188:54301, tcp:192.168.5.188:57477, udp:0.0.0.0:57477, udp:192.168.5.188:39253, udp:192.168.5.188:54301 |
| `loginapp` | 2856519 | INFO: [Server] ---- LoginApp is running ---- | tcp:192.168.5.188:20013, tcp:192.168.5.188:49171, udp:0.0.0.0:20013, udp:192.168.5.188:49171, udp:192.168.5.188:55303 |
| `serviceapp` | 2857019 | INFO: [Config] Found 0 service fragments in [Services] secti | tcp:192.168.5.188:37101, tcp:192.168.5.188:54005, tcp:192.168.5.188:60001, udp:0.0.0.0:37101, udp:192.168.5.188:50379, udp:192.168.5.188:54005 |
| `baseapp(2)` | 2856191 | INFO: [Server] ---- BaseApp is running ---- | tcp:192.168.5.188:38103, tcp:192.168.5.188:40002, tcp:192.168.5.188:40535, udp:0.0.0.0:38103, udp:192.168.5.188:40535, udp:192.168.5.188:43112 |

## 配置

- **BW_RES_PATH**（进程环境，`/proc/<pid>/environ`）：`/home/cui/workspaces/BigWorld/programming/bigworld/examples/client_integration/python/simple/res:/home/cui/workspaces/BigWorld/game/res/bigworld:/home/cui/workspaces/BigWorld/scripts/cluster-smoke/res`
  - 三段：示例资产树 res、发行 res（`game/res/bigworld`）、smoke 覆盖 res（`scripts/cluster-smoke/res`）。
- **服务端配置**：`/home/cui/workspaces/BigWorld/scripts/cluster-smoke/res/server/bw_deep.xml` 作为覆盖（`bw_deep.xml`），启动时由 `ensure_user_config` 复制为有效配置 `/home/cui/workspaces/BigWorld/scripts/cluster-smoke/res/server/bw_cui.xml`；两者 sha256 相同（`d39deb1f59b60de3…`）。
- **资源树清单**：`/home/cui/workspaces/BigWorld/scripts/cluster-smoke/res/resources.xml`（sha256 `c853583cbd2c5837…`）。
- **数据库**：`bigworld`/`bigworld`@`127.0.0.1:3306`，库 `bigworld`（`provision_db` 授予 localhost/127.0.0.1）。

## 日志

| 进程 | 日志 | 大小 | 稳定标记 |
| --- | --- | --- | --- |
| `bwmachined2` | `/tmp/cluster-smoke/bwmachined2.log` | 0 B | — |
| `dbappmgr` | `/tmp/cluster-smoke/dbappmgr.log` | 5,576 B | INFO: [Server] ---- DBAppMgr is running ---- |
| `cellappmgr` | `/tmp/cluster-smoke/cellappmgr.log` | 12,834 B | INFO: [Server] ---- CellAppMgr is running ---- |
| `baseappmgr` | `/tmp/cluster-smoke/baseappmgr.log` | 9,005 B | INFO: [Server] ---- BaseAppMgr is running ---- |
| `dbapp` | `/tmp/cluster-smoke/dbapp.log` | 11,361 B | INFO: [Server] ---- DBApp is running ---- |
| `cellapp` | `/tmp/cluster-smoke/cellapp.log` | 15,890 B | INFO: [Server] ---- CellApp is running ---- |
| `baseapp` | `/tmp/cluster-smoke/baseapp.log` | 14,395 B | INFO: [Server] ---- BaseApp is running ---- |
| `loginapp` | `/tmp/cluster-smoke/loginapp.log` | 10,786 B | INFO: [Server] ---- LoginApp is running ---- |
| `serviceapp` | `/tmp/cluster-smoke/serviceapp.log` | 14,032 B | INFO: [Config] Found 0 service fragments in [Services] section in /etc/bwmachined.confINFO: [Server] ---- ServiceApp is running ---- |
| `baseapp(2)` | `/tmp/cluster-smoke/baseapp2.log` | 14,214 B | INFO: [Server] ---- BaseApp is running ---- |

`bwmachined2.log` 为 0 字节（守护进程把日志交给 syslog/stderr，`-p /tmp/bwmachined.pid` 记录 pid）；其余 8 个服务端日志均含 `---- X is running ----` 稳定标记，脚本 `wait_steady` 据此判定稳定态（`serviceapp` 的标记行与前一行配置输出无换行、合并为一行）。

完整日志与配置已归档 `reference/bw00_02_03_cluster_logs.tar.gz`（10 个日志 + `smoke-res/server/bw_deep.xml`、`bw_cui.xml`、`smoke-res/resources.xml`，11.5 KB）。

生成：`cluster_baseline_scan.py` + `cluster_baseline_doc.py`。
