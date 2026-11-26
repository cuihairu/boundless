# BW-00-02-04 最小客户端登录录制

基线 `4657a17` 产物树的 `simple_python_client`（`game/bin/server/el7/examples/`，example 自源构建）对 **BW-00-02-03 v2 在线集群**实跑登录。对应 todo.md BW-00-02-04：**留存请求和回调序列**。证据目录 `data/bw00-02-04-client-login-v2/`（client-drive.log + 四份服务端日志 + mysql-state.txt）。

## 环境

- 集群：BW-00-02-03 v2 十进程稳态（bwmachined2 + 三管理器 + dbapp/cellapp/baseapp×2/loginapp/serviceapp），loginapp 外部口 `192.168.5.188:20013`，baseapp2 外部口 `192.168.5.188:52918`。
- 密钥：`loginapp.privkey`/`loginapp.pubkey`（openssl RSA-2048，运营方供给模型；pubkey 在 smoke res 段，客户端经 BW_RES_PATH 找到后走加密登录）。
- 账号面：`billingSystem/shouldAcceptUnknownUsers=true` + `entityTypeForUnknownUsers=ClientAvatar`（bw_deep.xml）；`tbl_Account` 为空，故 MyAccount 走**未知用户受理**路径，Avatar 为临时实体。
- 驱动命令：`simple_python_client -server 192.168.5.188:20013 -user MyAccount -password a`，BW_RES_PATH 三段同集群，**exit 0 干净退出**。

## 请求序列（client-drive.log 行号为证据锚）

| # | 请求 | 证据（client-drive.log） |
|---|---|---|
| 1 | 实体定义装载（ClientAvatar 5 顶层属性/2 client 方法/1 base 方法） | L5-L9 `Highest exposed counts` |
| 2 | 客户端绑定 UDP `192.168.5.188:54388` | L11-L12 |
| 3 | `logOnTo` → `ServerConnection::logOnBegin: Using server: 192.168.5.188:20013, transport: UDP, username: MyAccount` | L13-L14 |
| 4 | LoginHandler 起重试机（# requests: 10, retry 1.0s, timeout 8.0s）→ Attempt #1 连 LoginApp | L15-L17 |
| 5 | LoginApp 回复（0.1s）携带 BaseApp 地址 | L21 + L22 |
| 6 | Attempt #1 连 BaseApp `192.168.5.188:52918`（UDP） | L23-L24 |
| 7 | BaseApp 回复（0.1s）→ 登录成立 | L25-L26 |
| 8 | `ServerConnection::logOn: status==LOGGED_ON` → `enableEntities` | L27-L30 |
| 9 | 断开（演示结束）→ 干净退出 exit 0 | L31 `Connected to 192.168.5.188:52918` 后退出 |

## 回调序列（两侧对照）

客户端侧（client-drive.log）：

1. `LoginRequest::handleMessage: Attempt #1: Received reply from 192.168.5.188:20013`（L21）
2. `LoginHandler::onLoginAppLoginSuccess: BaseApp@192.168.5.188:52918 over UDP`（L22）
3. `LoginRequest::handleMessage: ... Received reply from ...:52918`（L25）
4. `LoginHandler::onBaseAppLoginSuccess: Got reply from BaseApp channel udp:192.168.5.188:52918`（L26）
5. `ServerConnection::logOn: status==LOGGED_ON`（L27）→ `enableEntities: Enabling entities`（L30）

服务端侧（各组件日志）：

1. loginapp：`LoginApp::login: Logging in user 'MyAccount' (192.168.5.188:54388)`（loginapp.log）
2. dbapp：`DBApp::logOn: MyAccount`（billing 未知用户受理，entityTypeForUnknownUsers=ClientAvatar；dbapp.log）
3. baseapp2（接洽实例，外口 52918）：`Base::Base(4802): ClientAvatar` → `BaseApp: Adding proxy 4802 at udp:192.168.5.188:54388` → `LoginHandler::login: 4802 attached ... Attempt 1` → `Proxy::enableEntities(4802)` → `Proxy::addCreateBasePlayerToChannelBundle(4802): Creating player on client` → `ClientAvatar.onEntitiesEnabled: 4802`（baseapp2.log；另见引导 `Base::Base(4801): Space` + `py_registerGlobally 'DefaultSpace'`）
4. cellapp：`Cell::createEntityInternal: New ClientAvatar (4802)` → `ClientAvatar.__init__: {}`（cellapp.log）

拆链（客户端退出后，baseapp2.log/cellapp.log）：

1. `Proxy::onClientDeath: Client id 4802 has disconnected (Requested by client)`
2. `BaseApp::removeProxy: Removing proxy id 4802 addr udp:192.168.5.188:54388`
3. `Base::~Base(4802): ClientAvatar`
4. cellapp：`Cell::entityDestroyed: ClientAvatar 4802 has been destroyed.`

## 持久化观察（mysql-state.txt）

`syncTablesToDefs=true` 已按实体定义建全表集（tbl_Account/tbl_ClientAvatar + 属性 blob 表/tbl_Guard/tbl_Space/bigworldLogOnMapping 等）；本次登录为未知用户受理路径，断开即毁（`tbl_ClientAvatar` 行数 0，`tbl_Account` 行数 0）——临时实体未持久化，符合 billing 受理口径。

## 口径与缺口

- 基础二进制口径同 BW-00-02-03 v2：baseapp2 为 smoke 单对象偏差重编版（`ping_manager.o` 无 USE_OPENSSL），**loginapp/dbapp/cellapp/客户端均为原版产物**；`server/bigworld.key` 缺口不变（见 00-02-03 文档）。
- 加密登录生效：客户端持 `loginapp.pubkey` 加密登录参数，一次 Attempt #1 即获回复；更细的 challenge 分步（challengeRequest/challengeResponse）在本运行日志详级别下未单独打印，序列按实印留存，不推测补行。
- 参考方法名对照（源码口径）：`LoginHandler` 的请求/回调族与参考 `client_interface`/`login_interface` 一致（logOnTo→login→LoginReply；onLoginAppLoginSuccess→onBaseAppLoginSuccess→enableEntities），实跑序列为上述源码面在 14.4.1 二进制上的运行实例。
