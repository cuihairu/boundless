# BW-00-02-04 最小客户端登录

基线 `4657a17`，产物 `game/bin/server/el7/examples/simple_python_client`（已构建），参考源 `lib/connection/`。对应 todo.md BW-00-02-04：**留存请求和回调序列**。

## 状态：BLOCKED（关键凭据缺件）

最小客户端登录的**完整动态捕获**被双向凭据缺件阻塞：
- **客户端**：`loginapp.pubkey`（LoginApp 公钥，加密登录请求所需）——不存在于资源树，客户端初始化即失败 `Failed to initialise LoginApp public key.`（动态日志见下）。
- **服务器**：`server/bigworld.key`（许可密钥，与 pubkey 成对）——BW-00-02-03 已证，loginapp 硬失败 `No key file server/bigworld.key found.`。

两者均非本仓可自造（需 BigWorld 官方凭据）。请求/回调**序列**已从源码定稿（下方），动态失败证据已留存。

## 请求序列（客户端 → LoginApp）

源码 `lib/connection/loginapp_login_request_protocol.cpp`：

1. 连接建立后，客户端发送 `LoginInterface::login` 请求（RELIABLE_NO）——`sendRequest` `:120` `bundle.startRequest( LoginInterface::login, &request, NULL, timeout*1000000, Mercury::RELIABLE_NO )`。
2. 请求载荷——`addRequestDataToStream` `:132`：
   - `ClientServerProtocolVersion::currentVersion()`（客户端/服务器协议版本）；
   - `LogOnParams::addToStream( LogOnParams::HAS_ALL, ... )`（`log_on_params.hpp:87-91`：`username_`、`password_`、`encryptionKey_`，`digest_` 为实体 def 摘要）。
3. 若此前服务端下发过 challenge：`LoginInterface::challengeResponse` 消息（`challengeCalculationDuration` + challenge 响应 blob）——`sendRequest` `:110-116`。
4. 重试策略——`login_request_protocol.hpp:52/57/62`：`numRequests()=10`、`timeoutInterval()=8.f`、`retryInterval()=1.f`（10 次 × 8s 超时）。

## 回调序列（LoginApp → 客户端）

`onReply` `loginapp_login_request_protocol.cpp:166`：

1. `data >> statusValue` → `LogOnStatus`（`LOGIN_CHALLENGE_ISSUED` / `LOGGED_ON` / 拒绝状态）。
2. **challenge 分支** `:172`：`handleChallengeFromStream` —— 客户端计算 challenge 响应并重发（回请求序列步骤 3）。
3. **拒绝分支** `:177-198`：读可选 `serverMessage` → `request.finish()` → `pHandler->fail( status, serverMessage )` → 触发 `ServerConnectionHandler::onLogOnFailure(status, message)`（`server_connection_handler.hpp:21`）。
4. **成功分支** `:208-245`：`LoginReplyRecord`（`login_reply_record.hpp:16-17`：`Mercury::Address serverAddr` + `uint32 sessionKey`）经块密码解密（`EncryptionFilter::decryptStream`）→ 客户端以 `serverAddr` 连接 baseapp → `onLoggedOn`（`server_connection_handler.hpp:20`）。
5. **无响应耗尽** `:72-96`：`onAttemptsExhausted` → `LogOnStatus::LOGIN_REJECTED_NO_LOGINAPP_RESPONSE` → `handler.fail(...)` → `onLogOnFailure`。
6. **断开**：`logOff()` → `onLoggedOff`（`server_connection_handler.hpp:19`）。

成功登录后（README）：Account base 实体 → `chatMessage` 5 次 → 载入 ClientAvatar 角色实体 → base 转移 → cell 实体 `fromCell` 消息——此为需真实集群才可复现的完整路径。

## 动态证据

命令：`./el7/examples/simple_python_client --res <simple res>:<game/res/bigworld> -server 127.0.0.1:20013 -user MyAccount -password a`（README 口径）。日志 `docs/review/data/bw00-02-04-logs/client.log`：

- res 路径注入成功；`DataDescription` 实体面装载成功（`ClientAvatar: client top-level property count = 5 ...`、`Guard: cell method count = 0 ...`）；
- 网络接口绑定成功（`ens18 (192.168.5.188:45249)`）；
- **失败点**：`Failed to initialise LoginApp public key.`（`loginapp.pubkey` 缺件）。

## 结论与缺口

- 请求/回调序列已定稿（上两节，含精确源码行号），动态失败证据已留存（client.log）。
- **阻塞项**：`loginapp.pubkey`（客户端登录公钥）+ `server/bigworld.key`（服务器许可密钥，二者成对）——均非本仓可自造。
- 次要：`server/development_defaults.xml`（simple 例 bw.xml `parentFile` 引用）亦缺——见 BW-00-02-03。
- 后续如需绿跑：配置凭据后，需真实集群 + 该客户端按 README 启动，方可捕获成功登录的完整回调序列。
