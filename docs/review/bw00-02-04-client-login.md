# BW-00-02-04 最小客户端登录

> 证据面（BW-00-02-04 验收：留存请求和回调序列）。

由 `tools/client_login_scan.py` 驱动参考仓示例客户端（`examples/client_integration/python/simple` → `simple_python_client`）登录运行中的单机集群，抓取客户端请求/回调与服务端脚本回调；源数据 `reference/bw00_02_04_client_login.json`。

| 项 | 值 |
| --- | --- |
| 参考仓提交 | `f3f53898` |
| 捕获时间 | `2026-11-23 01:46:38 +0800` |
| 客户端命令 | `/home/cui/workspaces/BigWorld/game/bin/server/el7/examples/simple_python_client -server 127.0.0.1 -user MyAccount -password a -autoDisconnect` |
| BW_RES_PATH | `/home/cui/workspaces/BigWorld/programming/bigworld/examples/client_integration/python/simple/res:/home/cui/workspaces/BigWorld/game/res/bigworld:/home/cui/workspaces/BigWorld/scripts/cluster-smoke/res` |
| 退出码 / 耗时 | `0` / 2.226s |
| 客户端源地址 | `192.168.5.188:55283` |
| 本机账号 | `MyAccount` / `a`（示例 `db.xml` 的 LogOnMapping，实体 `ClientAvatar`=`MyCharacter`，实体 id 4805） |

## 请求序列

客户端按 `loginapp:20013` UDP → `baseapp` UDP 两跳完成登录（`-autoDisconnect` 使连上后即退出，故运行有界）：

| # | 类型 | 客户端日志 |
| --- | --- | --- |
| 1 | request | `LoginApp@127.0.0.1:20013 over UDP (# requests: 10, retry interval: 1.0s, timeout interval: 8.0s)` |
| 2 | request | `From 192.168.5.188:55283, attempt #1 / 10 to LoginApp 127.0.0.1:20013` |
| 3 | reply | `Attempt #1: Received reply from 127.0.0.1:20013 (after 0.1s)` |
| 4 | callback | `BaseApp@192.168.5.188:51079 over UDP (# requests: 10, retry interval:1.0s, timeout interval: 8.0s)` |
| 5 | request | `From 192.168.5.188:55283, attempt #1 / 10 to BaseApp 192.168.5.188:51079` |
| 6 | reply | `Attempt #1: Received reply from 192.168.5.188:51079 (after 0.1s)` |
| 7 | callback | `Got reply from BaseApp channel udp:192.168.5.188:51079` |
| 8 | callback | `status==LOGGED_ON` |
| 9 | callback | `from: 192.168.5.188:55283` |
| 10 | callback | `to:   192.168.5.188:51079` |
| 11 | callback | `Enabling entities` |
| 12 | callback | `192.168.5.188:51079` |

## 服务端日志匹配（按客户端源端口）

| 服务端 | 日志行 |
| --- | --- |
| `loginapp` | `INFO: LoginApp::login: Attempt from 127.0.0.1:55283` |
| `loginapp` | `INFO: LoginApp::login: Logging in user 'MyAccount' (127.0.0.1:55283)` |
| `baseapp(2)` | `WARNING: LoginHandler::updateStatistics: Addresses are different (due to NAT?). Was 127.0.0.1:55283. Now 192.168.5.188:55283` |
| `baseapp(2)` | `TRACE: BaseApp: Adding proxy 4805 at udp:192.168.5.188:55283` |
| `baseapp(2)` | `INFO: LoginHandler::login: 4805 attached from 192.168.5.188:55283. Attempt 1` |
| `baseapp(2)` | `ERROR: [Network] Exception occurred: 'REASON_NO_SUCH_PORT' on '192.168.5.188:55283'` |
| `baseapp(2)` | `TRACE: BaseApp::removeProxy: Removing proxy id 4805 addr udp:192.168.5.188:55283` |
| `baseapp(2)` | `ERROR: [Network] Exception occurred: 'REASON_NO_SUCH_PORT' on '192.168.5.188:55283'` |

## 服务端回调序列（本运行的实体 id 4805）

`baseapp.log` / `baseapp2.log`（登录可能落在第二个 BaseApp 实例，两个日志一并切分）：

- `INFO: LoginHandler::login: 4805 attached from 192.168.5.188:55283. Attempt 1`
- `DEBUG: Proxy::enableEntities(4805)`
- `DEBUG: Proxy::addCreateBasePlayerToChannelBundle(4805): Creating player on client`
- `INFO: ClientAvatar.onEntitiesEnabled: 4805`
- `ERROR: [Network] Exception occurred: 'REASON_NO_SUCH_PORT' on '192.168.5.188:55283'`
- `TRACE: Proxy::onClientDeath: Client id 4805 has disconnected (Requested by client)`
- `TRACE: BaseApp::removeProxy: Removing proxy id 4805 addr udp:192.168.5.188:55283`
- `INFO: ClientAvatar.onClientDeath: 4805`
- `ERROR: [Network] Exception occurred: 'REASON_NO_SUCH_PORT' on '192.168.5.188:55283'`
- `INFO: Base::~Base(4805): ClientAvatar`

`cellapp.log`：

- `INFO: ClientAvatar.__init__: {}`
- `INFO: Cell::entityDestroyed: ClientAvatar 4805 has been destroyed.`

## 说明

- **两跳协议**：LoginApp 回复后转 BaseApp；`onLoginAppLoginSuccess` 与 `onBaseAppLoginSuccess` 即两次握手的回调，`LOGGED_ON` → `enableEntities` 后实体（ClientAvatar）随 proxy 下发。
- **NAT 观察**：客户端经 `127.0.0.1:20013` 连 LoginApp，baseapp 记 `Addresses are different (due to NAT?)`——同机双路径（回环 + 网卡地址）。
- **`REASON_NO_SUCH_PORT`**：`-autoDisconnect` 断开后客户端端口已释放，服务端再发包时的正常告警，随 `Proxy::onClientDeath`/`removeProxy` 收尾。
- **日志归属**：`baseapp*.log`/`cellapp.log` 为追加式，本运行的回调行按实体 id（`Base::Base(4805): ClientAvatar`）切分。

生成：`client_login_scan.py` + `client_login_doc.py`。
