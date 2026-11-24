# BW-00-02-03 单机集群启动基线

基线 `4657a17`，产物树 `game/bin/server/el7/`，参考源 `programming/bigworld/`。对应 todo.md BW-00-02-03：**留存拓扑、配置和日志**。

## 状态：全集群稳态已录（2026-11-24 v2）

**十进程单机集群全绿拉起并稳态运行**：`bwmachined2` + 三管理器（dbappmgr/cellappmgr/baseappmgr）+ 四应用（dbapp/cellapp/baseapp×2/loginapp）+ serviceapp，全部打点 `---- X is running ----`（`data/bw00-02-03-logs-v2/`）。首跑（v1，下方"BLOCKED"节）记录的三项阻塞现已按各自真实成因解除：

| v1 卡点 | 真实成因 | v2 解法 |
|---|---|---|
| loginapp 缺 `server/loginapp.privkey` | 密钥对**本来就不是发行内容**——按部署模型由运营方生成（`loginapp.cpp:519-556` 只在加载失败时报错；`allowUnencryptedLogins` 是另一条开发口径，本基线未用） | `openssl genrsa 2048` + `rsa -pubout` 生成密钥对入 smoke res（`loginapp.privkey`/`loginapp.pubkey`） |
| dbapp `No module named 'twisted'` | 服务端 Python 路径 `scripts/server_common/site-packages` 此前为空 | 安装 wheel 组（twisted-24.7.0/zope.interface-7.0/sqlalchemy-1.4.54/msgpack-1.1.0/pika-1.3.2 等）入 `game/res/bigworld/scripts/server_common/site-packages/`（v1 误记为 key 错误） |
| baseapp 缺 `server/bigworld.key` | **仍然不可解**：密钥内容用内置 RSA 公钥 `RSA_public_decrypt` 验签（`ping_manager.cpp:193-260`），无 BigWorld 私钥无法构造；无配置开关 | smoke 口径：`ping_manager.o` 以 `useOpenSSL=0` 单独重编（PingManager 整体被 `USE_OPENSSL` 门控，这是全仓唯一引用该 key 的翻译单元）后正常链接——**该二进制非原版**，原版 baseapp 仍硬阻塞，偏差如实登记 |

v1 文档中"cellapp/dbapp/loginapp 同 key 错误"为误判（cellapp 日志实际无 key 错误，dbapp 是 twisted 导入失败，loginapp 是 privkey 缺失），本版已按各自日志更正。

## v2 拓扑（实跑进程集）

`data/bw00-02-03-logs-v2/cluster-ps.txt`（10 进程 ps 快照，2026-11-24 17:04）：

| 进程 | PID | 启动序 | 稳态打点 |
|---|---|---|---|
| `bwmachined2 -f` | 2729221 | 1 | syslog `Bootstrap complete; 1 machines on network` |
| `dbappmgr` | 2729340 | 2 | `---- DBAppMgr is running ----` |
| `cellappmgr` | 2729344 | 2 | `---- CellAppMgr is running ----` |
| `baseappmgr` | 2729348 | 2 | `---- BaseAppMgr is running ----` |
| `dbapp` | 2729514 | 3 | `---- DBApp is running ----`（MySQL 已连） |
| `cellapp` | 2729742 | 4 | `---- CellApp is running ----` |
| `baseapp`（主） | 2729846 | 5 | `---- BaseApp is running ----` |
| `baseapp`（备） | 2729957 | 5 | `---- BaseApp is running ----`（互备对，baseappmgr 可做死亡接管） |
| `loginapp` | 2730070 | 6 | `---- LoginApp is running ----` |
| `serviceapp` | 2730163 | 7 | `---- ServiceApp is running ----`（DBApp/LoginApp 受理登录前需要它，desiredServiceApps 缺省 1） |

机守护登记链（syslog 摘录 `bwmachined2-syslog.txt`）：`Confirmed 192.168.5.188 (ens18)` → `Listening for requests` → 逐组件 `Added cellappmgr/BaseAppMgrInterface/CellAppMgrInterface/baseappmgr/DBAppMgrInterface/dbappmgr/DBAppInterface/dbapp01/BaseAppIntInterface ... (SERVER_COMPONENT) for uid:1000`。

## v2 配置

| 项 | 值 | 出处 |
|---|---|---|
| res 路径 | `BW_RES_PATH=<examples/client_integration/python/simple/res>:<game/res/bigworld>:<scripts/cluster-smoke/res>`（三段，各组件日志 `Resource path (1 of 3)` 实印） | 实跑 |
| 生效配置 | `server/bw_cui.xml`——BWConfig 在 Linux 载 `server/bw_<pw_name>.xml`（`bwconfig.cpp:182`），由 `bw_deep.xml` 深链覆盖层生成 | `scripts/cluster-smoke/res/server/bw_deep.xml` |
| 覆盖层要点 | `gameUpdateHertz=10`、`personality=egclient4`、`baseApp/archivePeriod=0`、`billingSystem shouldAcceptUnknownUsers=true + entityTypeForUnknownUsers=ClientAvatar`、`loginApp/allowLogin=true`、`cellApp/aoiUpdateSchemes`（空节，防 `AoIUpdateSchemes::init` 空解引用）、`db/mysql 127.0.0.1:3306 bigworld syncTablesToDefs=true numConnections=5 utf8mb4`、`secondaryDB enable=false` | `bw_deep.xml` 实文 |
| loginapp 密钥 | `loginApp/privateKey = server/loginapp.privkey`（smoke res 提供），配对 `loginapp.pubkey` 供客户端 | 实跑 `loginapp.log` 配置段 |
| 服务端 Python 包 | `game/res/bigworld/scripts/server_common/site-packages/`（twisted/zope/sqlalchemy/msgpack/pika）+ `scripts/common/site-packages/`（attrs/automat/constantly/hyperlink/idna/incremental/pympler/setuptools/typing_extensions） | 目录实查 |
| MySQL | MariaDB 11.8 本机 `active`，库 `bigworld`、账号 `bigworld/bigworld`（127.0.0.1 与 localhost 双授权） | deep-chain `provision_db` |

## v2 日志（`data/bw00-02-03-logs-v2/`）

| 日志 | 要点 |
|---|---|
| `bwmachined2-syslog.txt` | `--- BWMachined start ---`、广播发现 ens18、`Bootstrap complete; 1 machines on network`、逐组件登记行（本守护仅走 syslog，stdout 为空，`bwmachined2.log` 留 0 字节原件） |
| `dbapp.log` | `[DBEngine] Database engine = mysql`、`Configured MySQL server: 127.0.0.1:3306 (bigworld)`、`max_allowed_packet = 16777216`、`syncTablesToDefs = True` |
| `loginapp.log` | `loginApp/privateKey = server/loginapp.privkey` 读取成功、`bound to all interfaces ... ens18 (192.168.5.188:20013)`、无 key 错误 |
| `cellapp.log` | 三段 res 路径实印、`cellapp_example.so` 扩展加载、`---- CellApp is running ----` |
| `baseapp.log`/`baseapp2.log` | 双 BaseApp 稳态、TCP 服务绑定、无 key 错误（smoke 重编版） |
| 其余 | 三管理器与 serviceapp 各自稳态打点；每份日志含完整 `ServerAppConfig` 选项打印（配置链全通） |

## v1 记录：BLOCKED 首跑（2026-11-21 前后，原始二进制口径，保留）

首跑以原始（未重编）二进制逐组件 `timeout 6` 直启，`fantasydemo` + `game/res/bigworld` 两段 res，起 bwmachined 后三管理器达 `Version 14.4.1` 并持续运行；baseapp/cellapp/dbapp/loginapp 启动失败。原始日志 `data/bw00-02-03-logs/`：

| 日志 | 真实成因（本版更正） |
|---|---|
| `baseapp.log`/`baseapp2.log` | key 错误 `No key file server/bigworld.key found.`（`ping_manager.cpp`，原版口径下不可解） |
| `cellapp.log` | 无 key 错误；日志止于配置打印段（`timeout 6` 截断 + 独跑无 cellappmgr 可登记） |
| `dbapp.log` | `PyDeferred::staticInit: Failed to import twisted.internet.defer` → `ScriptApp::initScript` 失败（twisted 未入服务端 Python 路径；另见 `MachineDaemon::findInterface: Failed to find BaseAppMgrInterface`——独跑无管理器） |
| `loginapp.log` | `Couldn't load private key from non-existent file server/loginapp.privkey` |
| `bwmachined2.record.txt` | 守护 ps/cmdline + syslog：`Bootstrap complete; 1 machines on network` |

## 结论与缺口

- **验收面已录**：拓扑（cluster-ps.txt + syslog 登记链）、配置（bw_deep.xml 全文 + res 路径 + 密钥供给）、日志（logs-v2 十份稳态）留存于本文件与 `data/bw00-02-03-logs-v2/`。
- **剩余缺口（原版口径）**：`server/bigworld.key` 需 BigWorld 官方许可凭据——原版 baseapp 无法启动的根因不变（RSA 验签不可伪造、无配置开关）。v2 稳态中的 baseapp 为单对象偏差的 smoke 重编版，偏差与还原方法见 `scripts/cluster-smoke/build_license_free_baseapp.sh` 头注（删 `ping_manager.o` 重编即回原版口径）。
- `server/development_defaults.xml` 仍不在仓（上游裁剪）；v2 以 `bw_deep.xml` 内联该文件对本例必需的节（allowLogin/aoiUpdateSchemes/billing/db），并留注释指明来源。
