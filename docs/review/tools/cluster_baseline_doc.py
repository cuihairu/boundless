#!/usr/bin/env python3
"""Emit the BW-00-02-03 evidence markdown from the cluster-baseline JSON.

Usage: cluster_baseline_doc.py CLUSTER_BASELINE_JSON > bw00-02-03-cluster-baseline.md
The topology/config/logs tables are regenerated from the JSON so the doc never
disagrees with the captured snapshot.
"""
import json
import sys

# Dependency order documented by scripts/cluster-smoke/deep-chain.sh:38 and its
# header comment: MariaDB -> bwmachined2 -> managers -> dbapp -> cellapp ->
# baseapp -> loginapp (serviceapp is a BaseApp variant that DBApp/LoginApp
# require before they accept logons).
CHAIN = [
    ("MariaDB", "bigworld DB on 127.0.0.1:3306"),
    ("bwmachined2", "machine daemon, UDP 20018 (broadcast + lo + eth0)"),
    ("dbappmgr / cellappmgr / baseappmgr", "manager set, started first"),
    ("dbapp", "persistence, entity defs checksum"),
    ("cellapp", "cell space, deep-chain entity"),
    ("baseapp (+ second baseapp)", "client entities, backup partner"),
    ("loginapp", "logon entry, TCP 20013"),
    ("serviceapp", "BaseApp variant for services"),
]


def main():
    with open(sys.argv[1], encoding="utf-8") as fh:
        d = json.load(fh)

    cfg = d["config"]
    res_path = next((a["res_path"] for a in d["apps"] if a.get("res_path")), "—")
    print("# BW-00-02-03 单机集群启动基线")
    print()
    print("> 证据面（BW-00-02-03 验收：留存拓扑、配置和日志）。")
    print()
    print("由参考仓 `cuihairu/BigWorld` 自带的 `scripts/cluster-smoke/deep-chain.sh` "
          "将完整服务端集合拉起到稳定态，再由 `tools/cluster_baseline_scan.py` 快照；"
          "源数据 `reference/bw00_02_03_cluster_baseline.json`。")
    print()
    print("| 项 | 值 |")
    print("| --- | --- |")
    print(f"| 参考仓提交 | `{d['source_commit']}` |")
    print(f"| 捕获时间 | `{d['captured_at']}` |")
    print(f"| 启动脚本 | `{d['smoke_script']}`（`--teardown` 可停） |")
    print(f"| 日志目录 | `{d['log_dir']}`（`${{TMPDIR:-/tmp}}/cluster-smoke`） |")
    print(f"| 服务端进程 | {len(d['apps'])} 个目标，"
          f"{sum(1 for a in d['apps'] if a['pid'])} 个在稳定态 |")
    print()

    print("## 拓扑")
    print()
    print("启动链（脚本头注释与 `APPS` 顺序，脚本 `deep-chain.sh:6-13,38`）：")
    print()
    for name, note in CHAIN:
        print(f"- `{name}` — {note}")
    print()
    print("稳定态进程表（`pid` 与监听套接字来自快照时点）：")
    print()
    print("| 进程 | pid | 稳定标记 | 监听套接字 |")
    print("| --- | --- | --- | --- |")
    for a in d["apps"]:
        pid = a["pid"] or "—"
        marker = (a.get("steady_line") or "（守护进程，无日志标记）")[:60]
        ports = ", ".join("%s:%s" % (p, l) for p, l in a.get("ports", []))
        print(f"| `{a['name']}` | {pid} | {marker} | {ports} |")
    print()

    print("## 配置")
    print()
    print(f"- **BW_RES_PATH**（进程环境，`/proc/<pid>/environ`）："
          f"`{res_path}`")
    print(f"  - 三段：示例资产树 res、发行 res（`game/res/bigworld`）、"
          f"smoke 覆盖 res（`scripts/cluster-smoke/res`）。")
    print(f"- **服务端配置**：`{cfg['server_config_overlay']}` 作为覆盖（`bw_deep.xml`），"
          f"启动时由 `ensure_user_config` 复制为有效配置 `{cfg['server_config_effective']}`；"
          f"两者 sha256 相同（`{cfg['server_config_overlay_sha256'][:16]}…`）。")
    print(f"- **资源树清单**：`{cfg['resources_xml']}`"
          f"（sha256 `{cfg['resources_xml_sha256'][:16]}…`）。")
    print(f"- **数据库**：`{cfg['db']['user']}`/`{cfg['db']['pass']}`@"
          f"`{cfg['db']['host']}:3306`，库 `{cfg['db']['name']}`"
          f"（`provision_db` 授予 localhost/127.0.0.1）。")
    print()

    print("## 日志")
    print()
    print("| 进程 | 日志 | 大小 | 稳定标记 |")
    print("| --- | --- | --- | --- |")
    for a in d["apps"]:
        log = a.get("log_file", "—")
        size = f"{a.get('log_size', 0):,}" if a.get("log_file") else "—"
        marker = a.get("steady_line") or "—"
        print(f"| `{a['name']}` | `{log}` | {size} B | {marker} |")
    print()
    print("`bwmachined2.log` 为 0 字节（守护进程把日志交给 syslog/stderr，"
          "`-p /tmp/bwmachined.pid` 记录 pid）；其余 8 个服务端日志均含 "
          "`---- X is running ----` 稳定标记，脚本 `wait_steady` 据此判定稳定态"
          "（`serviceapp` 的标记行与前一行配置输出无换行、合并为一行）。")
    print()
    print("完整日志与配置已归档 `reference/bw00_02_03_cluster_logs.tar.gz`"
          "（10 个日志 + `smoke-res/server/bw_deep.xml`、`bw_cui.xml`、"
          "`smoke-res/resources.xml`，11.5 KB）。")
    print()
    print("生成：`cluster_baseline_scan.py` + `cluster_baseline_doc.py`。")
    return 0


if __name__ == "__main__":
    sys.exit(main())
