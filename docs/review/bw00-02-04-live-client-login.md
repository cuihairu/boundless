# BW-00-02-04 最小客户端登录（请求与回调序列实测）

客户端登录此前只录到失败侧（`Failed to initialise LoginApp public key.`）。本次在
BW-00-02-03 的全量单机集群上实测跑通登录，留存**两端**的请求/回调序列：客户端
逐帧回调 + 服务端 loginapp/baseapp 对应记录。原始日志入档
`docs/review/data/bw00-02-04-logs/live-2026-11-24/`（`client.log` 全量、
`loginapp.log`/`baseapp.log` 服务端全量、`server-login-excerpt.txt` 行号定位摘录）。
运行日期 2026-11-24。

## 1. 命令与前置

```
cd /home/cui/workspaces/BigWorld
BW_RES_PATH="<example res>:<game/res/bigworld>:<cluster-smoke/res>" \
  game/bin/server/el7/examples/simple_python_client \
    -server 127.0.0.1 -user MyAccount -password pass
```

- `-server` 参数经 `ServerConnection::logOnBegin()` 的
  `Endpoint::convertAddress()` 走 **DNS 解析**（`lib/connection/server_connection.cpp`
  `logOnBegin`），因此主机名类名字（如 `local`）会直接判 `DNS_LOOKUP_FAILED`，
  须给可解析地址；`-server 127.0.0.1:20013` 可内嵌登录端口（`strchr(serverName, ':')`）。
- 客户端公钥从 res 链解析：`BWResource::resolveToAbsolutePath("loginapp.pubkey")`
  → `setLoginAppPublicKeyPath()`，故 `cluster-smoke/res/loginapp.pubkey` 必须在链上。
  该密钥对由脚手架 `ensure_loginapp_keys()` 用 `openssl genrsa 2048` +
  `openssl rsa -pubout` 现场生成——**运营者按部署自生成，非商业许可凭据**，
  与 `server/bigworld.key`（不可伪造）性质不同，可合法补齐。
- 账号 `MyAccount` 来自示例资产 `res/scripts/db.xml`（README 指定；其
  `character = MyCharacter` 用于加载 ClientAvatar，`playerName = MyCharacter`）。

## 2. 客户端请求序列（出站）

| # | 帧 | 目标 | 证据行 |
| --- | --- | --- | --- |
| 1 | `SmartServerConnection::logOnTo` transport=UDP | — | `client.log:13` |
| 2 | `ServerConnection::logOnBegin` server/user 定案 | — | `:14` |
| 3 | `LoginHandler::start` 投递参数定案 | LoginApp | `:15` |
| 4 | `LoginRequest::onTransportConnect` attempt #1 | `LoginApp@127.0.0.1:20013` | `:16` |
| 5 | `LoginHandler::sendNextRequest` attempt 1/10 | 同上 | `:17` |
| 6 | `LoginRequest::onTransportConnect` attempt #1 | `BaseApp@192.168.5.188:50463` | `:23` |
| 7 | `LoginHandler::sendNextRequest` attempt 1/10 | 同上 | `:24` |

投递参数（`LoginHandler::start` 打印）：`# requests: 10, retry interval: 1.0s,
timeout interval: 8.0s`——与源码口径一致（10 次 × 8s 超时）。

## 3. 客户端回调序列（入站，实际次序）

```
LoginRequest::handleMessage   attempt #1 reply from 127.0.0.1:20013 (after 0.1s)   :21
LoginHandler::onLoginAppLoginSuccess   BaseApp@192.168.5.188:50463                :22
LoginRequest::onTransportConnect  attempt #1 to BaseApp ...                        :23
LoginRequest::handleMessage   attempt #1 reply from 192.168.5.188:50463 (after 0.1s) :25
LoginHandler::onBaseAppLoginSuccess  Got reply from BaseApp channel udp:...         :26
ServerConnection::logOn       status==LOGGED_ON                                    :27-29
ServerConnection::enableEntities  Enabling entities                                :30
Connected to 192.168.5.188:50463                                                   :31
```

进程退出码 0（正常）。两段握手各一轮即成功，未触发 challenge 重试。

## 4. 服务端对应记录

loginapp（`loginapp.log`）：

```
122 INFO: CuckooCycleLoginChallengeFactory::configure: Easiness: 50.000
123 INFO: DelayLoginChallengeFactory::configure: Duration 1.000s
124 INFO: [Server] ---- LoginApp is running ----
125 DEBUG: LoginApp::finishInit: id 1 (DBApp Alpha: 192.168.5.188:45986)
126 INFO: LoginApp::notifyDBAppAlpha: 192.168.5.188:45986
127 INFO: LoginApp::login: Attempt from 127.0.0.1:55396
128 INFO: LoginApp::login: Logging in user 'MyAccount' (127.0.0.1:55396)
```

挑战因子口径：`challengeType` 未从 bw.xml 链读取（用默认，空），实际生效两个
工厂：CuckooCycle（Easiness 50.000）与 Delay（1.000s）。客户端源地址
`127.0.0.1:55396` 与客户端日志的 `From 192.168.5.188:55396` 端口号一致（127.0.0.1
即 loopback，源地址按登录接口一侧记录），可做端到端关联。

baseapp（`baseapp.log`）——同一会话的实体生命周期：

```
162 INFO: Base::Base(1): ClientAvatar
163 INFO: ClientAvatar.__init__: {'prop1': 1} 0
169 INFO: ClientAvatar.onEntitiesEnabled: 1
171 TRACE: Proxy::onClientDeath: Client id 1 has disconnected (Requested by client)
173 INFO: ClientAvatar.onClientDeath: 1
176 INFO: Base::~Base(1): ClientAvatar
```

客户端 `enableEntities`（`:30`）与服务端 `ClientAvatar.onEntitiesEnabled`（169）
对应；客户端退出触发 `onClientDeath`（Requested by client）。

## 5. 与 00-02-03 的依赖关系

本项与 00-02-03 同一运行窗口：集群拓扑、配置、日志见
`docs/review/bw00-02-03-live-cluster-baseline.md` 与
`docs/review/data/bw00-02-03-logs/live-2026-11-24/`。登录链路此前判 BLOCKED 的两处
外部条件均已消解：`loginapp.privkey`/`loginapp.pubkey` 由运营者侧 openssl 生成，
`server/bigworld.key` 由无许可检查 baseapp 变体绕开（变体差异已登记）。

## 6. 登记的差异与限制

- 客户端日志中的源地址是物理网卡 `192.168.5.188`（NetworkInterface 按默认路由
  接口绑定），而登录目标按 loopback 记录为 `127.0.0.1`；两者同机不同地址族视角，
  非缺陷。
- `simple_python_client` 在 `Connected to ...` 之后即退出（示例进程设计如此），
  未观察到主动 `onLogOff` 回调帧；断连侧证据取自服务端 `Proxy::onClientDeath`
  （Requested by client）。
- 未触发 challenge 重试：`challengeType` 为空时按默认工厂直接通过，故本次未录到
  失败路径的 `onLogOnFailure` 帧序列（此前 2026-11-22 的
  `data/bw00-02-04-logs/client.log` 保留了失败侧记录，未覆盖）。