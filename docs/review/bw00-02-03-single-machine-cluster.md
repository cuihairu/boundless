# BW-00-02-03 单机集群启动基线

基线 `4657a17`，产物树 `game/bin/server/el7/`，参考源 `programming/bigworld/`。对应 todo.md BW-00-02-03：**留存拓扑、配置和日志**。

## 状态：BLOCKED（关键凭据缺件）

单机集群**全绿拉起**被 `server/bigworld.key`（BigWorld 服务器许可密钥）硬阻塞：baseapp/cellapp/dbapp/loginapp 四个组件均在 `ServerApp::init` 硬失败——`ERROR: No key file server/bigworld.key found.` + `ERROR: [Server] Failed to initialise <App>`（日志见下）。该文件为 BigWorld 商业授权凭据，**不存在于本仓，且不可自造**（需向 BigWorld 官方申领）。缺件：`<res>/server/bigworld.key`。

拓扑、配置与可采集的启动日志已如实留存（下方三节），以佐证"除许可密钥外其余环节可达"。

## 拓扑（单机集群进程集）

产物树实查（`game/bin/server/el7/`）给出单机集群的进程集与角色：

| 进程 | 角色 | 二进制 |
|---|---|---|
| `bwmachined2` | 机守护：广播发现、端口登记、进程看护 | `tools/bwmachined2` |
| `baseappmgr` | base 应用管理器 | `server/baseappmgr` |
| `baseapp` | base 应用（客户端会话/脚本） | `server/baseapp` |
| `cellappmgr` | cell 应用管理器 | `server/cellappmgr` |
| `cellapp` | cell 应用（空间逻辑） | `server/cellapp` |
| `dbappmgr` | DB 应用管理器 | `server/dbappmgr` |
| `dbapp` | DB 应用（持久化） | `server/dbapp` |
| `loginapp` | 登录应用 | `server/loginapp` |
| `reviver` | 组件复活看护 | `server/reviver` |
| `serviceapp` | 服务应用 | `server/serviceapp` |
| `bots` | 机器人（压测/自动测试客户端） | `server/bots` |
| `process_defs` | 实体 def → Python 过程定义代码生成器 | `tools/process_defs` |
| `message_logger` | 日志收集器 | `tools/message_logger` |
| `res_packer` | 资源打包 | `tools/res_packer` |

- 组件配置装载（`lib/server/bwconfig.cpp:182`）：每个进程以 `server/bw.xml` 为主配置，先尝试 `server/bw_<username>.xml`（个性化），沿 `parentFile` 链回退。
- 机守护配置（`server/tools/bwmachined/linux_machine_guard.cpp:31-32`）：`/etc/bwmachined.conf` + `/etc/bigworld.conf`（INI 式 `[tag]`/`key=value`，`bwmachined.cpp:553-611`）；**两个文件本机均不存在，bwmachined2 仅 WARN 继续**（见日志）。
- 实跑拓扑（本机 ens18 = 192.168.5.188）：bwmachined2 广播发现 → 确认 ens18 为默认广播路由接口 → `Listening for requests` → `Bootstrap complete; 1 machines on network`（`bwmachined2.record.txt` 转自 syslog）。

## 配置

| 项 | 值 | 出处 |
|---|---|---|
| 主配置链 | `server/bw.xml`（可 `parentFile` 串联），个性化优先 `server/bw_<user>.xml` | `bwconfig.cpp:182-233` |
| 机守护配置 | `/etc/bwmachined.conf`、`/etc/bigworld.conf`（INI） | `linux_machine_guard.cpp:31-32` |
| 仓内可用样例 | `programming/fantasydemo/res/server/bw.xml`（最小自含）、`programming/bigworld/examples/client_integration/python/simple/res/server/bw.xml` | 实查 |
| 仓内缺失 | `server/development_defaults.xml`（上游 `bigworld/res` 的开发缺省配置）——fantasydemo bw.xml 注释自述"does not exist in this repository" | `fantasydemo/res/server/bw.xml` 头注 |
| 仓内缺失 | `server/bigworld.key`（许可密钥，硬阻塞） | 实跑错误 |
| res 路径 | `BW_RES_PATH` 逗号/冒号串联；bwconfig 以 `server/bw.xml` 为入口 | 实跑 |

- 组件脚本面（`game/res/bigworld/scripts/`：`server_common/`、`common/`、`lib-dynload-el7/`）与 fantasydemo 实体面（`entities.xml`、`db.xml`、`entity_defs/`、base/cell/client 脚本）均在场。
- baseapp 完整打印 `ServerAppConfig` 选项（`baseapp2.log`：serverMode=standalone、production=true、timeSyncPeriod=60、maxOpenFileDescriptors=-1、numStartupRetries=60、hitchDetection 阈值 1.5 等）——配置装载路径全通。

## 日志

实跑日志入仓 `docs/review/data/bw00-02-03-logs/`（命令：`timeout 6 env BW_RES_PATH="<fantasydemo res>:<game/res/bigworld>" <el7>/server/<组件>`）：

| 日志 | 要点 |
|---|---|
| `baseapp.log` | `BWConfig::init: Adding server/bw.xml`；`MachineDaemon::queryForInternalInterface` 超时（未起 bwmachined 时） |
| `baseapp2.log` | 起 bwmachined 后：`bound to all interfaces ... ens18 (192.168.5.188)`、`TCPServer::initWithPort`、`Internal address`、`ServerApp::init: Run from bwmachined = false`、**key 错误** |
| `cellapp.log` | 同 key 错误（init 失败后崩溃回收，exit 139） |
| `dbapp.log` | 同 key 错误（exit 1） |
| `loginapp.log` | 同 key 错误（exit 1） |
| `bwmachined2.record.txt` | 守护 ps/cmdline + syslog：`Bootstrap complete; 1 machines on network`、`Dropping dead listener (for BaseAppMgrInterface's)` |

## 结论与缺口

- 拓扑、配置、可采集日志已留存（本文件 + `data/bw00-02-03-logs/`）。
- **阻塞项**：`server/bigworld.key` 缺失——四组件均硬失败于 `ServerApp::init`，无任何配置开关可绕过（源码无开发口径）。需 BigWorld 官方许可凭据。
- 次要观察：init 失败后的析构在 baseapp/cellapp 触发段错误（exit 139），属失败路径回收缺陷，非主因；dbapp/loginapp 干净退出（exit 1）。
- 后续如需绿跑：申领/配置 `server/bigworld.key`，补 `server/development_defaults.xml`，并按 bw.xml 缺省模板补全组件节。
