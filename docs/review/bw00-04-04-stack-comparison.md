# BW-00-04-04 原版与新版各自全栈的对照运行

> 证据面（BW-00-04-04 验收：同一输入产生两份证据）。

由 `tools/stack_comparison_run.py` 执行：一份输入（`reference/bw00_04_04_input.json`：角色集 + 资源路径 + 配置）同时驱动两栈，各自产出证据文件——原版侧 `reference/bw00_04_04_reference_evidence.json`，新版侧 `reference/bw00_04_04_new_evidence.json`。

| 项 | 原版 | 新版 |
| --- | --- | --- |
| 提交 | `4b637c8d` | `09666ef` |
| 捕获时间 | `2026-11-23 03:00:26 +0800` | `2026-11-23 03:00:26 +0800` |
| 驱动 | 全栈二进制 `/home/cui/workspaces/BigWorld/game/bin/server/el7/server`（模式 `live`） | `/home/cui/workspaces/boundless5/target/debug/boundless-server` |
| 输入 | 8 角色，`server/bw.xml` | 同左（同一文件） |
| 角色证据 | 9 份 | 8 份 |

输入（两栈共用，逐字节相同）：

```json
{
 "roles": [
  "dbappmgr",
  "cellappmgr",
  "baseappmgr",
  "dbapp",
  "cellapp",
  "baseapp",
  "loginapp",
  "serviceapp"
 ],
 "config": "server/bw.xml",
 "res_path": "/home/cui/workspaces/BigWorld/programming/bigworld/examples/client_integration/python/simple/res:/home/cui/workspaces/BigWorld/game/res/bigworld:/home/cui/workspaces/BigWorld/scripts/cluster-smoke/res",
 "log_dir": "/tmp/cluster-smoke",
 "reference_bin_dir": "/home/cui/workspaces/BigWorld/game/bin/server/el7/server"
}
```

## 原版全栈证据

集群为他人所有（mode=live）：只旁听、不驱动，只读轮询（1s 一次）直至全角色在位且各自日志出现`---- <App> is running ----` 稳态标记（本次 full_steady=true，轮询 0.0s）。

| 角色 | 进程在位 | 稳态标记 | 日志 |
| --- | --- | --- | --- |
| `baseapp` | 是 | 找到 | `/tmp/cluster-smoke/baseapp.log` |
| `baseappmgr` | 是 | 找到 | `/tmp/cluster-smoke/baseappmgr.log` |
| `cellapp` | 是 | 找到 | `/tmp/cluster-smoke/cellapp.log` |
| `cellappmgr` | 是 | 找到 | `/tmp/cluster-smoke/cellappmgr.log` |
| `dbapp` | 是 | 找到 | `/tmp/cluster-smoke/dbapp.log` |
| `dbappmgr` | 是 | 找到 | `/tmp/cluster-smoke/dbappmgr.log` |
| `loginapp` | 是 | 找到 | `/tmp/cluster-smoke/loginapp.log` |
| `serviceapp` | 是 | 找到 | `/tmp/cluster-smoke/serviceapp.log` |
| `baseapp(2)` | 是 | 找到 | `/tmp/cluster-smoke/baseapp2.log` |

## 新版全栈证据

`boundless-server <role>` 以同一输入逐角色运行（`BW_RES_PATH` 与原版侧相同），记录接受性、退出码与启动行。

| 角色 | 接受 | rc | 启动行 |
| --- | --- | --- | --- |
| `baseapp` | 是 | 0 | `[baseapp] starting (boundless minimal entry)` |
| `baseappmgr` | 是 | 0 | `[baseappmgr] starting (boundless minimal entry)` |
| `cellapp` | 是 | 0 | `[cellapp] starting (boundless minimal entry)` |
| `cellappmgr` | 是 | 0 | `[cellappmgr] starting (boundless minimal entry)` |
| `dbapp` | 是 | 0 | `[dbapp] starting (boundless minimal entry)` |
| `dbappmgr` | 是 | 0 | `[dbappmgr] starting (boundless minimal entry)` |
| `loginapp` | 是 | 0 | `[loginapp] starting (boundless minimal entry)` |
| `serviceapp` | 否 | 1 | `boundless-server: unknown server role 'serviceapp' (expected one of: baseapp, baseappmgr, cellapp, cellappmgr, dbapp, dbappmgr, loginapp, reviver)` |

## 对照结论

- 双栈均有证据的角色：7/8（baseapp, baseappmgr, cellapp, cellappmgr, dbapp, dbappmgr, loginapp）。
- 新版尚不认识的角色：serviceapp——`KNOWN_ROLES`（`crates/server-runtime/src/app.rs`）无此项，拒绝并 rc=1（证据本身记录了这个缺口）。
- 深度差（预期内）：原版到达完整稳态（machined/管理器/dbapp/cellapp/baseapp/loginapp 各自打印运行标记），新版入口当前只到达进程启动（校验角色 + 启动行 + rc）；更深的 init 链（bw.xml 全量、网络接口、信号）由 BW-01-04 落地。对照运行如实记录两侧各自到达的深度。

## 可追溯性

- 输入：`reference/bw00_04_04_input.json`（两份证据均内嵌同一输入）。
- 原版证据：`reference/bw00_04_04_reference_evidence.json`（提交 `4b637c8d`）。
- 新版证据：`reference/bw00_04_04_new_evidence.json`（提交 `09666ef`）。

生成：`stack_comparison_run.py` + `stack_comparison_doc.py`。
